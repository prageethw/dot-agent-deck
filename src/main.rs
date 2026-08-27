use std::io::Write as _;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use tokio::sync::RwLock;

use dot_agent_deck::agent_pty::{
    DOT_AGENT_DECK_AGENT_ID, DOT_AGENT_DECK_DAEMON_BOOT_ID, DOT_AGENT_DECK_PANE_ID,
    DOT_AGENT_DECK_REGISTRATION_GENERATION,
};
use dot_agent_deck::build_version_handshake;
use dot_agent_deck::config::{DashboardConfig, attach_socket_path, socket_path, state_dir};
use dot_agent_deck::daemon::{Daemon, run_daemon_with};
use dot_agent_deck::daemon_attach::ensure_external_daemon_or_die;
use dot_agent_deck::daemon_client::DaemonClient;
use dot_agent_deck::embedded_pane::EmbeddedPaneController;
use dot_agent_deck::hook::handle_hook;
use dot_agent_deck::pane::PaneController;
use dot_agent_deck::reconnect::{HydrationGate, spawn_event_subscriber};
use dot_agent_deck::state::AppState;
use dot_agent_deck::ui::run_tui;

/// Fork issue #36 backstop: how long the reconnect hydration gate may hold
/// live events before it is opened regardless.
///
/// The normal path releases it in well under a second (one `ListAgents`
/// round-trip plus the per-agent attaches). This bound only matters when the
/// hydration path never runs at all or dies before signalling; firing it
/// restores the pre-fix behaviour rather than leaving the TUI permanently
/// blind to live events. Generous relative to `embedded_pane`'s 5s list
/// timeout so a merely-slow daemon is not mistaken for a missing hydrator.
const HYDRATION_GATE_WATCHDOG: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Parser)]
#[command(name = "dot-agent-deck", about = "AI agent session dashboard", version = env!("DAD_VERSION"))]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
enum CliAgent {
    #[default]
    ClaudeCode,
    Opencode,
    /// PRD #20 W1: Codex ships a Claude-Code-compatible hooks engine, so its
    /// native command hooks shell `dot-agent-deck hook --agent codex`. Ingested
    /// by the [`dot_agent_deck::hook`] `"codex"` arm.
    Codex,
    /// Devin CLI, likewise Claude-Code-hook-compatible: its native command hooks
    /// shell `dot-agent-deck hook --agent devin` and are ingested by the
    /// [`dot_agent_deck::hook`] `"devin"` arm.
    Devin,
}

impl CliAgent {
    /// Map the CLI-surface agent selector to the registry's typed identity, so
    /// hook install/uninstall dispatch reads the integration STRATEGY from the
    /// agent registry (PRD #20 M2) instead of hardcoding which per-agent module
    /// to call for each variant.
    fn agent_type(self) -> dot_agent_deck::event::AgentType {
        match self {
            CliAgent::ClaudeCode => dot_agent_deck::event::AgentType::ClaudeCode,
            CliAgent::Opencode => dot_agent_deck::event::AgentType::OpenCode,
            CliAgent::Codex => dot_agent_deck::event::AgentType::Codex,
            CliAgent::Devin => dot_agent_deck::event::AgentType::Devin,
        }
    }
}

#[derive(Subcommand)]
enum Commands {
    /// Handle an agent hook event (reads stdin, sends to socket)
    Hook {
        /// Agent type
        #[arg(long, value_enum, default_value_t = CliAgent::ClaudeCode)]
        agent: CliAgent,
    },
    /// Manage hook installation
    Hooks {
        #[command(subcommand)]
        action: HooksAction,
    },
    /// Get or set configuration values
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Inspect the resolved experimental-feature-flag state (fork issue #303)
    Features {
        #[command(subcommand)]
        action: FeaturesAction,
    },
    /// Generate a .dot-agent-deck.toml template in the current or specified directory
    Init {
        /// Target directory (defaults to current directory)
        #[arg(short, long, default_value = ".")]
        path: std::path::PathBuf,
    },
    /// Validate a .dot-agent-deck.toml configuration file
    Validate {
        /// Target directory (defaults to current directory)
        #[arg(short, long, default_value = ".")]
        path: std::path::PathBuf,
    },
    /// Execute a command repeatedly at a fixed interval (like Linux watch)
    Watch {
        /// Refresh interval in seconds
        #[arg(long)]
        interval: u64,
        /// Command to execute
        command: String,
    },
    /// Delegate work to one or more worker roles (orchestrator only)
    Delegate {
        /// Task description with context, file paths, and constraints.
        /// Mutually exclusive with --task-file.
        #[arg(long, conflicts_with = "task_file")]
        task: Option<String>,
        /// Read the task text verbatim from a file (or `-` for stdin). The
        /// shell-safe way to pass a task containing backticks, quotes, `$VAR`,
        /// or newlines, which --task would otherwise let the caller's shell
        /// mangle. Mutually exclusive with --task.
        #[arg(long = "task-file", value_name = "PATH")]
        task_file: Option<String>,
        /// Role name(s) to delegate to (repeatable)
        #[arg(long)]
        to: Vec<String>,
        /// Subject tag (issue/PR number, or a short opaque token) this delegation
        /// is for — issue #586 M4. Optional; when both this and the worker's own
        /// `work-done --subject` are supplied and they don't match, the daemon
        /// flags it visibly rather than silently trusting a wrong-content report.
        #[arg(long)]
        subject: Option<String>,
    },
    /// Create a git worktree and start an isolated line of work inside it.
    /// Agent-callable, one step (PRD #220).
    Dispatch {
        /// Short name for the dispatch unit (used for worktree naming).
        /// Omit it only with --list-targets.
        #[arg(required_unless_present = "list_targets")]
        name: Option<String>,
        /// Task description with context, file paths, and constraints.
        /// Mutually exclusive with --task-file.
        #[arg(long, conflicts_with = "task_file")]
        task: Option<String>,
        /// Read the task text verbatim from a file (or `-` for stdin).
        /// Mutually exclusive with --task.
        #[arg(long = "task-file", value_name = "PATH")]
        task_file: Option<String>,
        /// Start ONE agent, even where this repo defines `[[orchestrations]]`.
        /// Mutually exclusive with --orchestration.
        #[arg(long, conflicts_with = "orchestration")]
        single: bool,
        /// Start a full orchestration by name (`--orchestration review`), or this
        /// repo's first role-bearing one (`--orchestration=` with an empty value).
        /// Mutually exclusive with --single.
        ///
        /// The value is REQUIRED rather than optional: with `num_args = 0..=1` clap
        /// consumes the next bare token, so `dispatch --orchestration my-unit
        /// --task "…"` silently bound the UNIT NAME as the orchestration name and
        /// then aborted for a missing positional. Requiring it makes that
        /// invocation unambiguous.
        #[arg(long, value_name = "NAME")]
        orchestration: Option<String>,
        /// Print the spawn targets available in this repo, then exit. Ask the
        /// user which one they want before dispatching.
        ///
        /// Conflicts with every dispatch argument: combined, it used to print the
        /// listing and exit 0 WITHOUT dispatching, so an agent that merged the two
        /// usage lines reported a unit as started that never existed.
        #[arg(
            long,
            conflicts_with_all = ["name", "task", "task_file", "single", "orchestration"]
        )]
        list_targets: bool,
    },
    /// Signal task completion back to the orchestrator
    WorkDone {
        /// Summary of what was accomplished. Mutually exclusive with
        /// --task-file.
        #[arg(long, conflicts_with = "task_file")]
        task: Option<String>,
        /// Read the summary text verbatim from a file (or `-` for stdin). The
        /// shell-safe way to pass a summary containing backticks, quotes,
        /// `$VAR`, or newlines. Mutually exclusive with --task.
        #[arg(long = "task-file", value_name = "PATH")]
        task_file: Option<String>,
        /// Signal that the entire orchestration is complete (orchestrator only)
        #[arg(long)]
        done: bool,
        /// Subject tag (issue/PR number, or a short opaque token) this report is
        /// for — issue #586 M4, echoing back the delegation's own `--subject`.
        /// Optional; when both this and the delegation's `--subject` are
        /// supplied and they don't match, the daemon flags it visibly rather
        /// than silently trusting a wrong-content report.
        #[arg(long)]
        subject: Option<String>,
    },
    /// Report an agent lifecycle state so the pane's card status updates
    /// (PRD #201 M1.2). Used by an agent's extension (e.g. the bundled Pi
    /// extension) to drive status with NO hook installed: it rides the
    /// existing raw-`AgentEvent` socket path.
    AgentEvent {
        /// Lifecycle state: one of `running`, `waiting`, `finished`.
        #[arg(long = "type")]
        r#type: String,
    },
    /// Print the seed/prompt the daemon prepared for this pane, then clear it
    /// (PRD #201 native prompt delivery). READ-ONLY: it asks the daemon over
    /// the hook socket for the pane's pending seed and prints it to stdout
    /// (empty output = no seed). The bundled Pi extension shells this on
    /// `session_start` and, if the output is non-empty, delivers it natively
    /// via `pi.sendUserMessage` — so a Pi pane's first prompt no longer needs
    /// PTY keystroke injection. Uses `DOT_AGENT_DECK_PANE_ID` to scope the
    /// request, exactly like `agent-event`.
    GetSeed,
    /// Set up the Pi orchestrator integration (PRD #201). Detects `pi` on
    /// PATH, materializes the bundled orchestrator extension into Pi's global
    /// extension dir, and enables it (Pi auto-discovers the dir). Prints the
    /// one-line install hint and exits non-zero if `pi` is absent.
    Orchestrator {
        #[command(subcommand)]
        cmd: OrchestratorCmd,
    },
    /// Daemon-side subcommands. Used internally by remote transports — not
    /// part of the everyday user surface.
    Daemon {
        #[command(subcommand)]
        cmd: DaemonCmd,
    },
    /// Manage registered remote agent environments (PRD #76).
    Remote {
        #[command(subcommand)]
        cmd: RemoteCmd,
    },
    /// Attach a local TUI to a remote daemon (PRD #76, M2.4). With no
    /// argument, runs an interactive picker over the configured remotes.
    Connect {
        /// Friendly name from `dot-agent-deck remote list`. If omitted, the
        /// picker runs.
        name: Option<String>,
    },
    /// Manage cron-scheduled prompts (PRD #127). The single validated writer
    /// for the global `~/.config/dot-agent-deck/schedules.toml`: every
    /// mutating subcommand validates the cron, expands `~`/`$VAR`, writes the
    /// global file atomically regardless of cwd, and triggers a live daemon
    /// reload.
    Schedule {
        #[command(subcommand)]
        action: ScheduleAction,
    },
    /// Manage the local saved-session snapshot (PRD #89). Auto-restore reads
    /// this on-disk snapshot on every TUI startup; this group is the local
    /// fresh-start escape hatch. A subcommand group (not a bare flag) so future
    /// snapshot operations can be added without changing the surface.
    Snapshot {
        #[command(subcommand)]
        cmd: SnapshotCmd,
    },
    /// Reclaim git worktrees whose PR is merged, whose tree is clean, and
    /// which the deck can prove it created. Never inspects git ancestry for
    /// merge state — squash-merges never enter `main`'s ancestry, and an
    /// ancestor branch with no PR must never be removed. The branch always
    /// survives; only the worktree directory is removed.
    Worktree {
        #[command(subcommand)]
        cmd: WorktreeCmd,
    },
    /// Claim a GitHub issue against concurrent agents — cooperative
    /// coordination, not an adversarial lock (PRD fork#235, fork-only —
    /// layered over PRD #421's claim record). Refuses with a non-zero exit
    /// when a DIFFERENT identity already holds the issue, naming the
    /// holder's worktree (path and branch, or the claiming human), rather
    /// than merely recording who claimed it.
    Issue {
        #[command(subcommand)]
        cmd: IssueCmd,
    },
    /// Declare or clear a monitored external wait for the calling pane
    /// (PRD #499, reopened) — a role invokes `wait start <label>` when it
    /// becomes responsible for noticing an external dependency resolve (CI,
    /// another agent/worker, an approval), and `wait done <label> --outcome
    /// <...>` once it does, so the pane reads `Working` for the whole span
    /// even across polling gaps or after its own delegated task has already
    /// reported done. A wait that is never explicitly cleared self-heals
    /// after a TTL (`DOT_AGENT_DECK_WAIT_TTL_SECS`) rather than wedging the
    /// pane `Working` forever.
    Wait {
        #[command(subcommand)]
        cmd: WaitCmd,
    },
    /// Wrap an agent command, passing its stdio through transparently while
    /// tee-ing output through pattern detection into `AgentEvent`s (PRD #20 M6
    /// — the generic stdout-wrapper integration strategy). The child stays
    /// fully interactive; recognised output lines drive the pane's card status,
    /// and the child's exit code becomes the wrapper's exit code. Usage:
    /// `dot-agent-deck wrap [--agent <name>] -- <command> <args...>`.
    Wrap {
        /// Optional agent identity override (a registry basename, e.g.
        /// `claude`). When omitted, the type is inferred from the wrapped
        /// command's binary.
        #[arg(long)]
        agent: Option<String>,
        /// The agent command and its arguments, taken verbatim after `--`.
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
}

#[derive(Subcommand)]
enum ScheduleAction {
    /// Add a new scheduled task.
    Add {
        #[arg(long)]
        name: String,
        #[arg(long)]
        cron: String,
        #[arg(long = "working-dir")]
        working_dir: String,
        #[arg(long)]
        command: Option<String>,
        #[arg(long)]
        prompt: String,
        // PRD #127 B1: accept an explicit `<true|false>` value (ArgAction::Set),
        // consistent with `update` and what the authoring seed prompt + docs
        // tell the agent to pass. A bare `SetTrue` flag here would reject the
        // value the primary agent-driven path supplies.
        #[arg(long = "new-tab-per-fire", action = clap::ArgAction::Set, default_value_t = false)]
        new_tab_per_fire: bool,
        #[arg(long, action = clap::ArgAction::Set, default_value_t = true)]
        enabled: bool,
        // PRD #120: issue-dispatch knobs. When `--repo` is present this `add`
        // authors an ISSUE-DISPATCH task (writes `[scheduled_tasks.issue_dispatch]`,
        // and `--command` is optional — the per-issue command comes from each
        // cloned repo's config). `--repo` is validated as a strict `owner/name`
        // slug.
        #[arg(long)]
        repo: Option<String>,
        #[arg(long = "max-per-run")]
        max_per_run: Option<usize>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        query: Option<String>,
        // PRD #421 M3.0: opt-in triage. Only meaningful alongside the other
        // issue_dispatch flags above; like them, threaded through with no
        // extra validation of that relationship.
        #[arg(long)]
        triage: bool,
    },
    /// Update fields of an existing task. Rename is forbidden — there is no
    /// name-change flag; `name` selects the task to edit.
    Update {
        #[arg(long)]
        name: String,
        #[arg(long)]
        cron: Option<String>,
        #[arg(long = "working-dir")]
        working_dir: Option<String>,
        #[arg(long)]
        command: Option<String>,
        #[arg(long)]
        prompt: Option<String>,
        #[arg(long = "new-tab-per-fire")]
        new_tab_per_fire: Option<bool>,
        #[arg(long)]
        enabled: Option<bool>,
    },
    /// Remove a task definition (does not kill an open tab for it).
    Remove {
        #[arg(long)]
        name: String,
    },
    /// List scheduled tasks with their enabled/disabled state and next-fire.
    List,
    /// Enable a task.
    Enable {
        #[arg(long)]
        name: String,
    },
    /// Disable a task (keeps the definition; stops it firing).
    Disable {
        #[arg(long)]
        name: String,
    },
    /// Fire a task now via the running daemon.
    RunNow {
        #[arg(long)]
        name: String,
    },
    /// Ask the running daemon to re-read the global config.
    Reload,
}

#[derive(Subcommand)]
enum OrchestratorCmd {
    /// Detect `pi`, then materialize + enable the bundled orchestrator
    /// extension in Pi's global extension dir. Idempotent (re-run to refresh a
    /// stale copy). Exits non-zero with the install hint when `pi` is absent.
    Setup,
}

#[derive(Subcommand)]
enum DaemonCmd {
    /// Run the daemon as a foreground process, binding the hook-ingestion
    /// and streaming-attach sockets but **not** launching the TUI. Used
    /// internally by lazy-spawn-on-attach (PRD #76, M4.3) and by callers
    /// that want a long-lived daemon to outlive the spawning shell. Not
    /// part of the everyday user surface.
    Serve,
    /// Print the binary's attach-protocol version as JSON. Used by the
    /// laptop-side `connect` flow (PRD #76 M2.21) to detect wire-format skew
    /// across an ssh hop without spawning the remote daemon: the protocol
    /// version is compiled into the binary, so a static print is equivalent
    /// to a Hello round-trip against a running daemon. Output is a JSON
    /// `AttachResponse` carrying `server_version` so the client side can
    /// reuse its existing deserializer.
    Hello,
    /// Stop the local daemon gracefully (SIGTERM, then poll for it to
    /// stop accepting connections). PRD #103 Phase 3 — documented
    /// alternative to `kill -9` after upgrading the binary. Refuses
    /// without `--force` when managed agents are still running.
    Stop {
        /// Terminate even when managed agents are running, and escalate
        /// to SIGKILL if SIGTERM doesn't take effect within the grace
        /// window. Data-loss guard — only pass this when you have
        /// already detached anything you cared about.
        #[arg(long)]
        force: bool,
    },
    /// Stop the local daemon (same flags as `stop`). The next
    /// `dot-agent-deck` invocation lazy-spawns a fresh daemon.
    Restart {
        /// See `stop --force`.
        #[arg(long)]
        force: bool,
    },
    /// Print a read-only snapshot of the daemon's managed agents: pane id,
    /// label, cwd, orchestration role, live status, and active tool. Fork
    /// #47: a CLI consumer of the existing `AttachRequest::ListAgents` — it
    /// never starts, stops, attaches to, resizes, writes to, or subscribes
    /// to any agent, and a missing/unreachable daemon is reported rather
    /// than lazily spawned.
    Status {
        /// Emit a versioned JSON document (`{schema_version, agents}`)
        /// instead of the human table.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum WorktreeCmd {
    /// List every linked worktree with its resolved PR state, cleanliness,
    /// ownership, and gate verdict (remove/ask/keep) with a reason. Read-only
    /// — never removes anything.
    List {
        /// Emit a versioned JSON document (`{schema_version, worktrees}`)
        /// instead of the human table.
        #[arg(long)]
        json: bool,
        /// Fork #166 M3.0: list only worktrees this orchestration created,
        /// determined by matching each worktree's marker owner against
        /// `DOT_AGENT_DECK_WORKTREE_OWNER`. Fails loudly (non-zero exit) if
        /// the variable is absent or set to the `orchestration:unknown`
        /// sentinel, rather than silently returning everything or nothing —
        /// a wrong answer here hands one orchestration another's worktree.
        #[arg(long)]
        mine: bool,
    },
    /// Remove every merged, clean worktree the deck can PROVE it created and
    /// which holds no gitignored content; list the rest for confirmation. A
    /// worktree the deck can prove it created AND successfully marked (a
    /// bare `git worktree add` it ran itself, with its ownership marker
    /// written), with no gitignored content, is removed by a bare
    /// `reclaim`, no `--yes` needed. One the deck cannot prove it both
    /// created and marked — including one it created but whose marker write
    /// failed, which is reported as a warning at creation time — OR one that
    /// still holds gitignored content (e.g. `target/`, `.env`), is instead
    /// reported as reclaimable-pending-confirmation and left alone unless
    /// `--yes` is passed — and once passed, `--yes` removes it regardless of
    /// provenance or ignored content, exactly like a deck-created one; the
    /// flag is the user vouching for what the pending report already showed
    /// them, not a request for the deck to trust more.
    /// A dirty worktree, an open/closed-unmerged PR, or an unresolvable PR
    /// state always keeps, `--yes` or not. Never deletes the branch.
    Reclaim {
        /// Also remove worktrees the deck did NOT prove it created (the
        /// `ask` verdict) that were just named as reclaimable-pending-
        /// confirmation — removed regardless of provenance once passed. Has
        /// no effect on worktrees the gate already keeps for another reason
        /// (dirty, PR not merged, PR state unresolvable).
        #[arg(long)]
        yes: bool,
    },
    /// Pin an isolated clone against `reclaim`'s automatic removal (fork
    /// issue #546 hazard 2, #597): once pinned, `reclaim` treats it exactly
    /// like a dirty/unmerged worktree — reported, never removed, `--yes` or
    /// not — until explicitly unpinned.
    Pin {
        /// Path to the isolated clone to pin.
        path: std::path::PathBuf,
    },
    /// Clear a pin set by `worktree pin` (fork issue #597), or explicitly
    /// record "not pinned" on a clone that was never pinned — both succeed.
    Unpin {
        /// Path to the isolated clone to unpin.
        path: std::path::PathBuf,
    },
}

#[derive(Subcommand)]
enum IssueCmd {
    /// Claim issue `<n>`, refusing (non-zero exit) when a DIFFERENT identity
    /// already holds it. Idempotent when the caller already holds it. See
    /// `dot_agent_deck::issue_claim`'s module doc for the full decision
    /// table.
    Claim {
        /// The GitHub issue number.
        issue: u64,
        /// `owner/name`; derived from the current directory's `origin`
        /// remote when omitted.
        #[arg(long)]
        repo: Option<String>,
        /// Signal intent to take over from a different holder. Alone, this
        /// still refuses — deliberate friction, so an agent can't satisfy
        /// the override in the same breath it discovers the conflict. Pass
        /// `--confirm-stopped` too, once you have confirmed the other
        /// agent has stopped.
        #[arg(long)]
        takeover: bool,
        /// Confirms the previous holder's agent has been stopped. Only
        /// takes effect together with `--takeover`; nothing verifies the
        /// assertion.
        #[arg(long = "confirm-stopped")]
        confirm_stopped: bool,
    },
    /// Report whether the caller is clear to act on issue `<n>`, WITHOUT
    /// writing anything — the read-only counterpart to `claim`, built to
    /// back a `PreToolUse` hook (issue #286) that gates `gh issue
    /// comment`/`close`/`edit` and a closing `gh pr merge` on the same
    /// identity lock `claim` enforces. No `--takeover`/`--confirm-stopped`:
    /// this command never writes, so takeover is meaningless here — resolve
    /// a refusal with `issue claim --takeover --confirm-stopped` instead.
    ClaimCheck {
        /// The GitHub issue number.
        issue: u64,
        /// `owner/name`; derived from the current directory's `origin`
        /// remote when omitted.
        #[arg(long)]
        repo: Option<String>,
    },
    /// Release issue `<n>`'s claim (issue #326) — the missing release half
    /// of `claim`'s lock: removes the `in-progress` label and posts a
    /// comment recording the release. Refuses on an unclaimed issue, and
    /// refuses on an issue held by a DIFFERENT identity or one whose
    /// holder identity is unknown unless BOTH `--force` and
    /// `--confirm-stopped` are passed. See
    /// `dot_agent_deck::issue_claim`'s module doc for the full release
    /// decision table.
    Release {
        /// The GitHub issue number.
        issue: u64,
        /// `owner/name`; derived from the current directory's `origin`
        /// remote when omitted.
        #[arg(long)]
        repo: Option<String>,
        /// Signal intent to release an issue held by a different identity,
        /// or whose holder identity is unknown. Alone, this still refuses —
        /// deliberate friction mirroring `claim`'s own
        /// `--takeover`-alone-still-refuses behavior, so an agent can't
        /// satisfy the override in the same breath it discovers the
        /// conflict. Pass `--confirm-stopped` too, once you have confirmed
        /// the other agent has stopped.
        #[arg(long)]
        force: bool,
        /// Confirms the previous holder's agent has been stopped. Only
        /// takes effect together with `--force`; nothing verifies the
        /// assertion.
        #[arg(long = "confirm-stopped")]
        confirm_stopped: bool,
        /// Optional free-text reason, included verbatim (after
        /// sanitization, wrapped in a code span) in the posted release
        /// comment.
        #[arg(long)]
        reason: Option<String>,
    },
}

#[derive(Subcommand)]
enum WaitCmd {
    /// Declare a monitored external wait for this pane, identified by
    /// `<label>` (a short opaque token, e.g. `ci-check`). Idempotent-ish:
    /// re-running `start` before a matching `done` just resets the TTL
    /// clock and re-records the label.
    Start {
        /// Short label naming what this wait is for. Not interpreted by the
        /// daemon beyond attribution/logging.
        label: String,
    },
    /// Clear a previously declared monitored wait for this pane.
    Done {
        /// The same label passed to `start`. A pane carries at most one
        /// monitored wait at a time, so this clears it regardless of an
        /// exact match — a mismatch is logged daemon-side, not refused.
        label: String,
        /// The terminal outcome of the external dependency this wait was
        /// for. All four clear the wait identically — a role reports which
        /// one occurred for its own bookkeeping / logging, not because the
        /// daemon's status computation treats them differently.
        #[arg(long, value_enum)]
        outcome: CliWaitOutcome,
    },
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum CliWaitOutcome {
    Success,
    Failure,
    Cancelled,
    Timeout,
}

impl From<CliWaitOutcome> for dot_agent_deck::event::WaitOutcome {
    fn from(value: CliWaitOutcome) -> Self {
        match value {
            CliWaitOutcome::Success => Self::Success,
            CliWaitOutcome::Failure => Self::Failure,
            CliWaitOutcome::Cancelled => Self::Cancelled,
            CliWaitOutcome::Timeout => Self::Timeout,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
enum CliRemoteType {
    #[default]
    Ssh,
    Kubernetes,
}

#[derive(Subcommand)]
enum RemoteCmd {
    /// Register a remote ssh-reachable host as a deck environment.
    Add {
        /// Friendly name for the registry (e.g. hetzner-1). Must be unique.
        name: String,
        /// ssh target: `[user@]host`.
        target: String,
        /// Remote type. Defaults to `ssh` (the only transport implemented today);
        /// `kubernetes` is planned in PRD #81.
        #[arg(long = "type", value_enum, default_value_t = CliRemoteType::Ssh)]
        kind: CliRemoteType,
        /// ssh port.
        #[arg(long, default_value_t = dot_agent_deck::remote::DEFAULT_SSH_PORT)]
        port: u16,
        /// ssh identity file. Optional; if omitted, ssh's default key search applies.
        #[arg(long)]
        key: Option<std::path::PathBuf>,
        /// Daemon binary version to install on the remote.
        #[arg(long, default_value = env!("DAD_VERSION"))]
        version: String,
        /// Skip binary install. Pre-flight will run `dot-agent-deck --version`
        /// on the remote and require version match.
        #[arg(long = "no-install")]
        no_install: bool,
    },
    /// Print the configured remotes from the local registry. Offline metadata
    /// only — does not probe remote hosts.
    List,
    /// Remove a remote from the local registry. Does not touch the remote
    /// host (the binary and hooks remain installed there until you ssh in
    /// and clean them up explicitly).
    Remove {
        /// Friendly name of the registry entry to remove.
        name: String,
    },
    /// Re-run the binary install flow against an existing entry, then bump
    /// the registry's version field.
    Upgrade {
        /// Friendly name of the registry entry to upgrade.
        name: String,
        /// Target version. Defaults to the local client's version.
        #[arg(long, default_value = env!("DAD_VERSION"))]
        version: String,
        /// Skip binary install. Useful when the user has already swapped the
        /// binary on the remote and just wants the registry's version field
        /// updated.
        #[arg(long = "no-install")]
        no_install: bool,
    },
}

#[derive(Subcommand)]
enum SnapshotCmd {
    /// Delete the local saved-session snapshot. With auto-restore on by
    /// default (PRD #89), this is the one obvious "start fresh" action for the
    /// local deck: the next `dot-agent-deck` startup begins from an empty
    /// dashboard instead of restoring the previous workspace. Registry-only
    /// `remote remove` intentionally does NOT touch this global snapshot.
    Clear,
}

#[derive(Subcommand)]
enum HooksAction {
    /// Install hooks for an agent
    Install {
        /// Agent type
        #[arg(long, value_enum, default_value_t = CliAgent::ClaudeCode)]
        agent: CliAgent,
    },
    /// Remove hooks for an agent
    Uninstall {
        /// Agent type
        #[arg(long, value_enum, default_value_t = CliAgent::ClaudeCode)]
        agent: CliAgent,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Get a configuration value
    Get {
        /// Configuration key (e.g., default_command, bell.on_idle)
        key: String,
    },
    /// Set a configuration value
    Set {
        /// Configuration key (e.g., default_command, bell.on_idle)
        key: String,
        /// Value to set
        value: String,
    },
}

/// Fork issue #303: an on-demand diagnostic for the experimental-feature-flag
/// resolution, so a user does not need `lsof` + a running process + log-file
/// archaeology to find out why every experimental surface is off.
#[derive(Subcommand)]
enum FeaturesAction {
    /// Print the resolved `.dot-agent-deck.toml` path, whether it exists, the
    /// resolved `experimental` value, and which source won. Works whether or
    /// not the deck is running.
    Status,
}

/// Resolve the task/summary text for `delegate` / `work-done` from the mutually
/// exclusive `--task` / `--task-file` inputs.
///
/// `--task-file <path>` reads the text **verbatim** from a file — no shell is
/// involved, so backticks, quotes, `$VAR`, and newlines survive unmangled
/// (the whole point: `--task "…`code`…"` lets the caller's shell
/// command-substitute the backticks before we ever run). `--task-file -` reads
/// stdin instead. clap's `conflicts_with` already rejects passing *both*; this
/// function rejects passing *neither* and surfaces file/stdin read errors.
fn resolve_task(
    task: Option<String>,
    task_file: Option<String>,
    stdin: impl std::io::Read,
) -> Result<String, String> {
    match (task, task_file) {
        (Some(t), None) => Ok(t),
        (None, Some(path)) => read_task_file(&path, stdin),
        // clap `conflicts_with` normally prevents this; kept as a defensive
        // guard so the invariant holds even if the two are ever resolved
        // outside clap parsing.
        (Some(_), Some(_)) => {
            Err("--task and --task-file are mutually exclusive; pass exactly one".to_string())
        }
        (None, None) => Err(
            "provide the task via --task <text> or --task-file <path> (use `-` for stdin)"
                .to_string(),
        ),
    }
}

/// Does `task_file` resolve into the daemon's own `.dot-agent-deck/work-done-*.md`
/// output namespace ([`dot_agent_deck::state::work_done_file_name`])?
///
/// Upstream #331's own proposed fix: a `work-done --task-file <path>` whose
/// path already sits where `handle_work_done` is about to write its own
/// summary is a literal, mechanically-preventable setup for that summary to
/// clobber the very file being reported — refused client-side, before ever
/// reading the file or contacting the daemon. A glob on the immediate parent
/// directory + filename prefix, not an exact match against today's filename
/// shape: the daemon's own name now carries a per-pane digest suffix (fork
/// #76), and this check must stay correct if that suffix ever changes again
/// without knowing what it is.
///
/// PR #90 review P1 (b): a check on the SUPPLIED path's own lexical name and
/// immediate parent is defeated by a symlink — a file symlink whose target
/// resolves into the namespace (fork #76's `delegate_026`), or an
/// intermediate directory symlink that aliases into `.dot-agent-deck` itself
/// (`delegate_027`) — since neither changes what the argument itself looks
/// like. [`resolve_work_done_candidate`] resolves as much of `task_file` as
/// the filesystem allows before classification.
///
/// PR #90 re-review: comparing the resolved parent against
/// `current_dir()/.dot-agent-deck` is itself wrong, because that compares
/// against the CLI process's own cwd, not the pane's cwd the daemon actually
/// writes under (`handle_work_done` keys off `pane_cwd_map[pane_id]`,
/// captured at `StartAgent` time, which the client can never see). A worker
/// that `cd`s before invoking the CLI makes that comparison diverge from the
/// real output file in both directions — so there is no cwd this check can
/// anchor to. The rule below anchors to nothing: it refuses whenever the
/// resolved parent directory is literally named `.dot-agent-deck` and the
/// filename matches the `work-done-*.md` glob, anywhere on disk, regardless
/// of which cwd produced it. The accepted trade is a same-named decoy
/// `.dot-agent-deck/work-done-*.md` elsewhere also being refused (a harmless
/// false positive — rename the file) in exchange for never missing the real
/// output file (the false negative that silently destroys a report). When
/// nothing on disk can be resolved (the target file doesn't exist yet, or —
/// as in this module's own unit tests — the path is a pure string exercising
/// the glob shape with no backing filesystem state at all),
/// [`resolve_work_done_candidate`] returns the original unresolved path and
/// the same parent-name check applies to it directly, pinning that shape
/// independent of resolution.
fn is_work_done_output_path(task_file: &str) -> bool {
    if task_file == "-" {
        return false;
    }
    let path = std::path::Path::new(task_file);
    let (resolved, _was_resolved) = resolve_work_done_candidate(path);

    let is_work_done_name = resolved
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| name.starts_with("work-done-") && name.ends_with(".md"));
    if !is_work_done_name {
        return false;
    }
    let Some(resolved_parent) = resolved.parent() else {
        return false;
    };

    resolved_parent.file_name().and_then(|n| n.to_str()) == Some(".dot-agent-deck")
}

/// Resolve `path` as far as the real filesystem allows, following every
/// symlink along the way, and report whether any resolution actually
/// happened.
///
/// Tries the full path first (handles both a symlinked file and a path
/// reached through a symlinked intermediate directory, since
/// [`std::path::Path::canonicalize`] follows symlinks in every component
/// including the last). Falls back to resolving just the parent when the
/// final component doesn't exist yet — a legitimate not-yet-written
/// `--task-file` reached through an aliased directory must still resolve
/// against the real directory, not the symlink's own name. Returns the
/// original, unresolved path with `false` when neither resolves (nothing on
/// disk backs any part of it), so callers can tell "genuinely outside the
/// namespace" apart from "not on disk to check" and choose a lexical
/// fallback instead of misclassifying the latter as a resolution failure.
fn resolve_work_done_candidate(path: &std::path::Path) -> (std::path::PathBuf, bool) {
    if let Ok(resolved) = path.canonicalize() {
        return (resolved, true);
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && let Ok(resolved_parent) = parent.canonicalize()
        && let Some(name) = path.file_name()
    {
        return (resolved_parent.join(name), true);
    }
    (path.to_path_buf(), false)
}

/// Read the registration generation and daemon boot id THIS WORKER WAS
/// SPAWNED UNDER from its own environment — injected at spawn time, sibling
/// to `DOT_AGENT_DECK_PANE_ID` — rather than asking the live daemon what the
/// pane's generation/boot currently are.
///
/// Fork #358 (M2 redesign, M4): asking the daemon at send time answers "what
/// generation does this pane currently hold", which is the SAME question
/// `handle_work_done` asks again microseconds later at delivery — a signal
/// produced that way can never disagree with itself, so a worker that
/// outlives its own orchestration's teardown (the actual #358 repro) would
/// ask the *new* tenant's daemon state and get delivered into the *new*
/// tenant's worktree unchanged. Reading it from the env instead means the
/// value genuinely travels with the worker process from spawn, so a mismatch
/// at delivery means what it's supposed to mean: the pane was re-registered
/// since THIS worker began.
///
/// Missing or unparseable (an old CLI predating these variables, or a caller
/// that didn't go through a dot-agent-deck-managed spawn) degrades to
/// `(0, String::new())` — `0` never matches a real registration (those start
/// at `1`), and `""` is never a real `daemon_boot_id` (see
/// `DaemonBootId::default`) — so a report built from a defaulted context is
/// refused at delivery rather than silently delivered unchecked. See
/// `WorkDoneSignal::generation`'s doc for the cross-version cost of that
/// choice.
fn read_registration_context() -> (u64, String) {
    let generation: u64 = std::env::var(DOT_AGENT_DECK_REGISTRATION_GENERATION)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let daemon_boot_id: String = std::env::var(DOT_AGENT_DECK_DAEMON_BOOT_ID).unwrap_or_default();
    (generation, daemon_boot_id)
}

/// Read task text verbatim from `path`, or from `stdin` when `path` is `-`.
fn read_task_file(path: &str, mut stdin: impl std::io::Read) -> Result<String, String> {
    if path == "-" {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut stdin, &mut buf)
            .map_err(|e| format!("failed to read task from stdin: {e}"))?;
        Ok(buf)
    } else {
        std::fs::read_to_string(path).map_err(|e| format!("failed to read task file '{path}': {e}"))
    }
}

/// What `dot-agent-deck delegate` should print and exit with, for one daemon
/// reply. See [`delegate_verdict`].
#[derive(Debug, PartialEq, Eq)]
struct DelegateVerdict {
    /// `true` → `ExitCode::FAILURE`. Under this command's contract that means
    /// "nothing landed", which is what makes a retry safe.
    failed: bool,
    /// Printed to stderr verbatim when set. `None` only for a delegate that
    /// reached every role it named without anything worth flagging (see
    /// [`Self::info`] for the one exception).
    message: Option<String>,
    /// Printed to STDOUT verbatim when set — deliberately a different stream
    /// than [`Self::message`], because this is a confirmation of what
    /// happened, not a warning or an error.
    ///
    /// fork #92 P2: `--to coder --to coder` de-duplicates to one target
    /// before dispatch (`AppState::delegate_targets`), so exactly one worker
    /// pane is armed for two requested copies of the role. An ordinary full
    /// delivery stays silent (see `delegate_verdict_reports_a_full_delivery_
    /// silently`), but silence here would be indistinguishable from a SECOND
    /// worker actually having been armed — the caller has no way to tell "one
    /// pane, dedup collapsed the rest" from "two panes, both armed". Set only
    /// for a role requested more than once whose delegate still fully
    /// resolved; ordinary full deliveries (no repeated role) get `None` here
    /// too, same as `message`.
    info: Option<String>,
}

/// Parse one line of daemon reply into a [`DelegateResponse`], or `None` when
/// the line is not a delegate reply this build understands.
///
/// PR #466 review: `None` covers BOTH a line that fails to parse and a line that
/// parses but carries no [`DELEGATE_RESPONSE_KIND`] marker. Every field of
/// `DelegateResponse` is `#[serde(default)]`, so without the marker check `{}`
/// and `{"seed":null}` both parse into a pristine "nothing failed" response and
/// the caller reports success it has no evidence for. Callers treat `None` as
/// [`dot_agent_deck::hook::SocketReply::NoReply`] — delivered, unverifiable.
fn parse_delegate_reply(line: &str) -> Option<dot_agent_deck::event::DelegateResponse> {
    serde_json::from_str::<dot_agent_deck::event::DelegateResponse>(line)
        .ok()
        .filter(|r| r.is_delegate_reply())
}

/// Decide what `delegate` reports for a daemon reply it does understand.
///
/// Pure, and separate from the `Delegate` arm, so the contract below is pinned
/// by unit tests in this file — the tier that actually gates a merge. The e2e
/// assertions that cover it live in `tests/e2e_dispatcher_mode.rs`, which CI
/// compiles to nothing (`#![cfg(feature = "e2e")]` + no `--features e2e` in any
/// build job), so a refactor that made the rejection silent again would
/// otherwise pass every gate (PR #466 review).
///
/// Three outcomes, and the middle one is the whole point:
///
/// * `error` — routing failed outright, nothing was dispatched. **Failure.**
/// * `unresolved_roles` with an EMPTY `delivered` — every named role missed.
///   **Failure**, and the message must not assert a cause it has not
///   established: a role can be missing from the toml, BE the sending
///   orchestrator (which `delegate_targets` excludes by design), or have had its
///   worker pane closed. "Check the role names" is right for only the first.
/// * `unresolved_roles` with a NON-EMPTY `delivered` — the delegate HALF landed.
///   **Not a failure**: the task really is in the delivered panes' PTYs and
///   their idle-worker records are armed, so an orchestrator applying the
///   contract "non-zero ⇒ it did not land" would retry and dispatch those panes
///   a second time, arming two records for one pane. The message names both
///   sides so a retry can be aimed at just the roles that missed.
///
/// `requested_roles` is the caller's raw `--to` list, BEFORE
/// `AppState::delegate_targets` de-duplicates it — needed only for
/// [`DelegateVerdict::info`]'s fork #92 P2 case below; every other branch
/// ignores it.
fn delegate_verdict(
    pane_id: &str,
    requested_roles: &[String],
    resp: &dot_agent_deck::event::DelegateResponse,
) -> DelegateVerdict {
    if let Some(error) = resp.error.as_deref() {
        return DelegateVerdict {
            failed: true,
            message: Some(format!(
                "Error: delegate from pane {pane_id} failed: {error}"
            )),
            info: None,
        };
    }
    if resp.unresolved_roles.is_empty() {
        // fork #92 P2: a role named more than once in `requested_roles` but
        // present in `delivered` (necessarily once — `delivered` is already
        // de-duplicated) collapsed via `delegate_targets`'s dedup. Naming it
        // here is the ONE exception to an ordinary full delivery's silence —
        // without it the caller cannot tell "one pane, dedup collapsed the
        // rest" from "two panes, both armed".
        let mut requested_counts: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        for role in requested_roles {
            *requested_counts.entry(role.as_str()).or_insert(0) += 1;
        }
        let collapsed: Vec<&str> = resp
            .delivered
            .iter()
            .filter(|role| requested_counts.get(role.as_str()).is_some_and(|&n| n > 1))
            .map(|role| role.as_str())
            .collect();
        return DelegateVerdict {
            failed: false,
            message: None,
            info: if collapsed.is_empty() {
                None
            } else {
                Some(format!(
                    "Delegated to: {}. (Requested more than once in this delegate; \
                     exactly one worker pane exists for the role, so it was armed \
                     once, not once per request.)",
                    collapsed.join(", ")
                ))
            },
        };
    }
    let unresolved = resp.unresolved_roles.join(", ");
    // The three causes, stated as the three causes rather than as the one that
    // happens to be most common.
    let causes = "(A role reaches no worker when it is absent from \
                  .dot-agent-deck.toml, when it is the delegating orchestrator \
                  itself — an orchestrator cannot delegate to itself — or when \
                  its worker pane has been closed.)";
    if resp.delivered.is_empty() {
        return DelegateVerdict {
            failed: true,
            message: Some(format!(
                "Error: delegate from pane {pane_id} reached no worker for role(s): \
                 {unresolved}. No role in this orchestration received it. {causes}"
            )),
            info: None,
        };
    }
    DelegateVerdict {
        failed: false,
        message: Some(format!(
            "Warning: delegate from pane {pane_id} reached no worker for role(s): \
             {unresolved}. It WAS delivered to: {}. Retry only the roles that \
             missed — re-sending the whole delegate would dispatch the delivered \
             roles a second time. {causes}",
            resp.delivered.join(", ")
        )),
        info: None,
    }
}

fn main() -> ExitCode {
    // PRD #89 M3.4: the `--continue` flag was removed — auto-restore is now the
    // default. Intercept a stale invocation before clap parsing so the user
    // gets a guiding message ("auto-restore is the default; just run
    // `dot-agent-deck`") instead of clap's bare "unexpected argument" error.
    // The exit is non-zero so wrapper scripts still fail loudly until updated.
    // Review-fix F8: also match the `--continue=<value>` form (e.g. a wrapper
    // that passed `--continue=true`) so it keeps the friendly message instead of
    // falling through to clap's generic error.
    if std::env::args().any(|a| a == "--continue" || a.starts_with("--continue=")) {
        eprintln!(
            "error: the `--continue` flag has been removed. Auto-restore is now the default — \
             just run `dot-agent-deck` (no flag) and your previous session is restored \
             automatically."
        );
        return ExitCode::FAILURE;
    }

    let keys_help = dot_agent_deck::config::config_keys_help();
    let cmd = Cli::command().mut_subcommand("config", |c| {
        c.mut_subcommand("get", |g| {
            g.long_about(format!("Get a configuration value\n\n{keys_help}"))
        })
        .mut_subcommand("set", |s| {
            s.long_about(format!("Set a configuration value\n\n{keys_help}"))
        })
    });
    let cli = Cli::from_arg_matches(&cmd.get_matches())
        .expect("clap arg matches should be valid for Cli struct");

    match cli.command {
        None => run_dashboard(),
        Some(Commands::Hook { agent }) => {
            let agent_str = match agent {
                CliAgent::ClaudeCode => "claude-code",
                CliAgent::Opencode => "opencode",
                CliAgent::Codex => "codex",
                CliAgent::Devin => "devin",
            };
            handle_hook(agent_str)
        }
        Some(Commands::Hooks { action }) => {
            // PRD #20 finding #15: dispatch through the SPEC's own handler rather
            // than a strategy-keyed hardcoded incumbent. Behaviour is unchanged
            // for the two CLI agents — ClaudeCode installs its native hooks,
            // Opencode its plugin — but a FUTURE agent (even one reusing an
            // existing strategy) installs correctly from just its own registry
            // handler, never another agent's module.
            use dot_agent_deck::agent_registry;
            match action {
                HooksAction::Install { agent } => {
                    let spec = agent_registry::spec(&agent.agent_type());
                    match spec.hook_install {
                        Some(install) => {
                            if let Err(e) = install() {
                                eprintln!("Failed to install {} hooks: {e}", spec.label);
                                return ExitCode::FAILURE;
                            }
                        }
                        None => {
                            eprintln!("No hook installer for agent {}", spec.label);
                            return ExitCode::FAILURE;
                        }
                    }
                }
                HooksAction::Uninstall { agent } => {
                    let spec = agent_registry::spec(&agent.agent_type());
                    match spec.hook_uninstall {
                        Some(uninstall) => {
                            if let Err(e) = uninstall() {
                                eprintln!("Failed to uninstall {} hooks: {e}", spec.label);
                                return ExitCode::FAILURE;
                            }
                        }
                        None => {
                            eprintln!("No hook uninstaller for agent {}", spec.label);
                            return ExitCode::FAILURE;
                        }
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Some(Commands::Config { action }) => match action {
            ConfigAction::Get { key } => {
                let config = DashboardConfig::load();
                match config.get_field(&key) {
                    Ok(value) => {
                        println!("{value}");
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("{e}");
                        ExitCode::FAILURE
                    }
                }
            }
            ConfigAction::Set { key, value } => {
                let mut config = DashboardConfig::load();
                if let Err(e) = config.set_field(&key, &value) {
                    eprintln!("{e}");
                    return ExitCode::FAILURE;
                }
                if let Err(e) = config.save() {
                    eprintln!("{e}");
                    return ExitCode::FAILURE;
                }
                ExitCode::SUCCESS
            }
        },
        Some(Commands::Features { action }) => match action {
            FeaturesAction::Status => {
                use dot_agent_deck::config::{
                    EXPERIMENTAL_ENV, FeaturesFileOutcome, describe_features_file,
                    features_config_path_with_diagnostics, load_features_file, resolve_features,
                };
                use dot_agent_deck::features::Features;
                use dot_agent_deck::project_config::CONFIG_FILE_NAME;
                use dot_agent_deck::terminal_sanitize::sanitize_path_for_terminal_display;

                // The ON/OFF answer below is produced by the exact same
                // resolution functions the real startup path calls
                // (`features_config_path_with_diagnostics`,
                // `load_features_file`, `resolve_features`) — no
                // reimplementation, so it can never disagree with what the
                // deck applies from this same path.
                //
                // The SOURCE label is a separate question and can genuinely
                // differ from a running deck's own explanation: this command
                // passes `Features::default()` as `load_features_file`'s
                // `previous`, while the live watcher passes the deck's
                // current value. On a load failure (malformed TOML, an
                // unreadable or non-regular target, an oversized file) both
                // return their respective `previous` — so the value printed
                // here can read OFF while a running deck, which fell back to
                // a previously-loaded ON, still shows the experimental
                // surfaces. `describe_features_file` mirrors the same
                // branching `load_features_file` took (without recomputing
                // the value) so that distinction is named instead of
                // silently collapsing into "(project file)".
                //
                // Round 2 (reviewer M-2): before this, this command called
                // the discarding `features_config_path()` wrapper, so it was
                // the one diagnostic surface that could not see a declined
                // ancestor and reported "no config found" when one was found
                // and declined — the opposite of the truth. Calling the
                // `_with_diagnostics` variant directly and printing each
                // declined message closes that.
                let (path, declined) = features_config_path_with_diagnostics(&launch_project_dir());
                for message in &declined {
                    println!("declined: {message}");
                }
                let is_override = std::env::var("DOT_AGENT_DECK_FEATURES_CONFIG").is_ok();
                let path_source = if is_override {
                    "DOT_AGENT_DECK_FEATURES_CONFIG override"
                } else {
                    "ancestor walk from the current directory"
                };
                let file_kind = if is_override {
                    "override target"
                } else {
                    "project file"
                };

                let Some(path) = path else {
                    // No ancestor was trustworthy at all — see
                    // `features_config_path_with_diagnostics`'s doc. This is
                    // the same resolution `init_and_watch` reaches; there is
                    // no path to describe or load, only the env override
                    // (which needs no file) to check.
                    let resolved = resolve_features(Features::default());
                    let value_source = if std::env::var(EXPERIMENTAL_ENV).is_ok() {
                        format!("{EXPERIMENTAL_ENV} env override")
                    } else {
                        "default (no ancestor directory could be trusted)".to_string()
                    };
                    println!("config path: none (no ancestor directory could be trusted)");
                    println!("config path exists: false");
                    println!(
                        "experimental: {} ({value_source})",
                        if resolved.experimental { "on" } else { "off" }
                    );
                    return ExitCode::SUCCESS;
                };

                let file_outcome = describe_features_file(&path);
                let exists = !matches!(file_outcome, FeaturesFileOutcome::NotFound);
                let file_value = load_features_file(&path, Features::default());
                let resolved = resolve_features(file_value);
                let value_source = if std::env::var(EXPERIMENTAL_ENV).is_ok() {
                    format!("{EXPERIMENTAL_ENV} env override")
                } else {
                    match file_outcome {
                        FeaturesFileOutcome::Parsed => file_kind.to_string(),
                        FeaturesFileOutcome::NotFound => {
                            if declined.is_empty() {
                                format!("default (no {CONFIG_FILE_NAME} found)")
                            } else {
                                format!(
                                    "default (all candidate {CONFIG_FILE_NAME} files were declined)"
                                )
                            }
                        }
                        FeaturesFileOutcome::NotRegular => {
                            format!("default ({file_kind} is not a regular file)")
                        }
                        FeaturesFileOutcome::Oversized => {
                            format!("default ({file_kind} exceeds the size cap)")
                        }
                        FeaturesFileOutcome::Unreadable => {
                            format!("default ({file_kind} exists but could not be read)")
                        }
                        FeaturesFileOutcome::Malformed => format!(
                            "default ({file_kind} has malformed TOML; a running deck would keep its previous value instead)"
                        ),
                    }
                };

                println!(
                    "config path: {} ({path_source})",
                    sanitize_path_for_terminal_display(&path)
                );
                println!("config path exists: {exists}");
                println!(
                    "experimental: {} ({value_source})",
                    if resolved.experimental { "on" } else { "off" }
                );
                ExitCode::SUCCESS
            }
        },
        Some(Commands::Init { path }) => dot_agent_deck::init::run_init(&path),
        Some(Commands::Watch { interval, command }) => {
            dot_agent_deck::watch::run_watch(interval, &command)
        }
        Some(Commands::Delegate {
            task,
            task_file,
            to,
            subject,
        }) => {
            let pane_id = match std::env::var(DOT_AGENT_DECK_PANE_ID) {
                Ok(id) => id,
                Err(_) => {
                    eprintln!(
                        "Error: DOT_AGENT_DECK_PANE_ID environment variable not set.\nThis command should be run from within a worker-agent-deck managed pane."
                    );
                    return ExitCode::FAILURE;
                }
            };
            if to.is_empty() {
                eprintln!("Error: at least one --to <role> is required.");
                return ExitCode::FAILURE;
            }
            let task = match resolve_task(task, task_file, std::io::stdin().lock()) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("Error: {e}");
                    return ExitCode::FAILURE;
                }
            };
            // Kept for the error messages below — the signal is moved into the
            // wire message, and a failure has to name the pane and the roles it
            // could not reach.
            let pane_id_for_report = pane_id.clone();
            let signal_roles = to.clone();
            let signal = dot_agent_deck::event::DelegateSignal {
                pane_id,
                task,
                to,
                timestamp: chrono::Utc::now(),
                subject,
            };
            let msg = dot_agent_deck::event::DaemonMessage::Delegate(signal);
            let json = match serde_json::to_string(&msg) {
                Ok(j) => j,
                Err(e) => {
                    eprintln!("Failed to serialize delegate signal: {e}");
                    return ExitCode::FAILURE;
                }
            };
            // A REQUEST, not a fire-and-forget send. The daemon is the only place
            // that knows whether this delegate resolved to a worker, and until it
            // answered, `delegate` printed nothing and exited 0 no matter what
            // happened on the other side — so an orchestrator whose delegation was
            // dropped announced that its worker was working and then waited
            // forever for a `work-done` that could not arrive.
            //
            // `send_and_await_reply`, not `request_from_socket`: the latter folds
            // "no daemon" and "old daemon that does not answer this verb" into one
            // `None`, and those must not be reported the same way — the first is a
            // real failure, the second has to stay a success or every delegate
            // against an older daemon reports a phantom error.
            use dot_agent_deck::hook::SocketReply;
            let line = match dot_agent_deck::hook::send_and_await_reply(&json) {
                SocketReply::Unreachable => {
                    eprintln!(
                        "Error: could not reach the dot-agent-deck daemon socket, so the \
                         delegate to {} was NOT delivered.",
                        signal_roles.join(", ")
                    );
                    return ExitCode::FAILURE;
                }
                // Handed to the socket of a daemon that answered nothing
                // readable in `DELEGATE_REPLY_TIMEOUT` — usually one predating
                // this response. Pre-response contract: unverifiable, and the
                // caller must not turn that into a phantom failure. See
                // `SocketReply::NoReply`.
                SocketReply::NoReply => return ExitCode::SUCCESS,
                SocketReply::Line(line) => line,
            };
            let Some(resp) = parse_delegate_reply(&line) else {
                // Same reasoning as `NoReply`: a line we cannot parse — or one
                // that never identifies itself as a delegate reply — is a daemon
                // we do not understand, not a proven failure.
                return ExitCode::SUCCESS;
            };
            let verdict = delegate_verdict(&pane_id_for_report, &signal_roles, &resp);
            if let Some(info) = verdict.info {
                println!("{info}");
            }
            if let Some(message) = verdict.message {
                eprintln!("{message}");
            }
            if verdict.failed {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Some(Commands::Dispatch {
            name,
            task,
            task_file,
            single,
            orchestration,
            list_targets,
        }) => {
            let pane_id = match std::env::var(DOT_AGENT_DECK_PANE_ID) {
                Ok(id) => id,
                Err(_) => {
                    eprintln!(
                        "Error: DOT_AGENT_DECK_PANE_ID environment variable not set.\n\
                         This command should be run from within a worker-agent-deck managed pane."
                    );
                    return ExitCode::FAILURE;
                }
            };
            // `--list-targets` is a READ-ONLY daemon round-trip (the `get-seed`
            // pattern): the daemon answers from the PANE's cwd and config, which is
            // the same basis the dispatch itself resolves from. Computing it here
            // from the CLI's own `current_dir()` diverged whenever the agent had
            // `cd`'d, and offered targets the dispatch could not start.
            //
            // Exits after printing. clap's `conflicts_with_all` guarantees no
            // dispatch arguments were supplied, so this cannot silently swallow a
            // real dispatch and still exit 0.
            if list_targets {
                let req = dot_agent_deck::event::DaemonMessage::ListTargets(
                    dot_agent_deck::event::ListTargetsRequest { pane_id },
                );
                let json = match serde_json::to_string(&req) {
                    Ok(j) => j,
                    Err(e) => {
                        eprintln!("Failed to serialize list-targets request: {e}");
                        return ExitCode::FAILURE;
                    }
                };
                match dot_agent_deck::hook::request_from_socket(&json) {
                    Some(line) if !line.trim().is_empty() => {
                        match serde_json::from_str::<dot_agent_deck::event::ListTargetsResponse>(
                            &line,
                        ) {
                            Ok(resp) => {
                                print!("{}", resp.rendered);
                                // A broken config is reported as a FAILURE so the
                                // agent cannot read "no orchestrations here" out of
                                // an error it never noticed.
                                if resp.error.is_some() {
                                    return ExitCode::FAILURE;
                                }
                                ExitCode::SUCCESS
                            }
                            Err(e) => {
                                eprintln!("Failed to parse the daemon's list-targets reply: {e}");
                                ExitCode::FAILURE
                            }
                        }
                    }
                    // No reply: no daemon, or one predating this verb. Say so rather
                    // than printing a confident empty list the caller would act on.
                    _ => {
                        eprintln!(
                            "Error: the daemon did not answer list-targets (not running, or an \
                             older build). Dispatch `--single` to start one agent, or \
                             `--orchestration <name>` if you know the name."
                        );
                        ExitCode::FAILURE
                    }
                }
            } else {
                // `required_unless_present = "list_targets"` guarantees this.
                let Some(name) = name else {
                    eprintln!("Error: a dispatch name is required.");
                    return ExitCode::FAILURE;
                };
                let task_text = match resolve_task(task, task_file, std::io::stdin().lock()) {
                    Ok(t) => t,
                    Err(e) => {
                        eprintln!("Error: {e}");
                        return ExitCode::FAILURE;
                    }
                };
                // clap's `conflicts_with` already rejects both flags together. A bare
                // `--orchestration` arrives as `Some("")` via `default_missing_value`
                // and means "this repo's first (role-bearing) one".
                //
                // The retained name is TRIMMED: an LLM-emitted `--orchestration "review "`
                // otherwise travels to the daemon with its whitespace, fails the exact
                // name comparison, and is refused with "no orchestration named 'review ';
                // available: review" — after a full worktree round trip.
                let shape = match (single, orchestration) {
                    (true, _) => Some(dot_agent_deck::event::DispatchShape::SingleAgent),
                    (false, Some(n)) => Some(dot_agent_deck::event::DispatchShape::Orchestration {
                        name: {
                            let n = n.trim();
                            if n.is_empty() {
                                None
                            } else {
                                Some(n.to_string())
                            }
                        },
                    }),
                    (false, None) => None,
                };
                let signal = dot_agent_deck::event::DispatchSignal {
                    pane_id,
                    name,
                    task: Some(task_text),
                    shape,
                    timestamp: chrono::Utc::now(),
                };
                let msg = dot_agent_deck::event::DaemonMessage::Dispatch(signal);
                let json = match serde_json::to_string(&msg) {
                    Ok(j) => j,
                    Err(e) => {
                        eprintln!("Failed to serialize dispatch signal: {e}");
                        return ExitCode::FAILURE;
                    }
                };
                if dot_agent_deck::hook::send_to_socket(&json).is_none() {
                    eprintln!("Failed to send dispatch signal to daemon socket.");
                    return ExitCode::FAILURE;
                }
                ExitCode::SUCCESS
            }
        }
        Some(Commands::WorkDone {
            task,
            task_file,
            done,
            subject,
        }) => {
            let pane_id = match std::env::var(DOT_AGENT_DECK_PANE_ID) {
                Ok(id) => id,
                Err(_) => {
                    eprintln!(
                        "Error: DOT_AGENT_DECK_PANE_ID environment variable not set.\nThis command should be run from within a worker-agent-deck managed pane."
                    );
                    return ExitCode::FAILURE;
                }
            };
            if let Some(ref path) = task_file
                && is_work_done_output_path(path)
            {
                eprintln!(
                    "Error: --task-file {path:?} resolves into dot-agent-deck's own \
                     work-done output namespace (.dot-agent-deck/work-done-*.md). Sending \
                     it back to `work-done` would let the daemon overwrite this exact file \
                     with this call's own summary. Write your report somewhere else (e.g. \
                     .dot-agent-deck/report-<role>-<summary-slug>.md) and pass that path \
                     instead."
                );
                return ExitCode::FAILURE;
            }
            let task = match resolve_task(task, task_file, std::io::stdin().lock()) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("Error: {e}");
                    return ExitCode::FAILURE;
                }
            };
            // Fork #358 (M2 redesign, M4) / issue #461: see
            // `read_registration_context`'s doc for why these are read from
            // this process's own env rather than asked of the live daemon.
            let (generation, daemon_boot_id) = read_registration_context();
            let signal = dot_agent_deck::event::WorkDoneSignal {
                pane_id,
                task,
                done,
                timestamp: chrono::Utc::now(),
                generation,
                daemon_boot_id,
                subject,
            };
            let msg = dot_agent_deck::event::DaemonMessage::WorkDone(signal);
            let json = match serde_json::to_string(&msg) {
                Ok(j) => j,
                Err(e) => {
                    eprintln!("Failed to serialize work-done signal: {e}");
                    return ExitCode::FAILURE;
                }
            };
            if dot_agent_deck::hook::send_to_socket(&json).is_none() {
                eprintln!("Failed to send work-done signal to daemon socket.");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Some(Commands::AgentEvent { r#type }) => {
            let pane_id = match std::env::var(DOT_AGENT_DECK_PANE_ID) {
                Ok(id) => id,
                Err(_) => {
                    eprintln!(
                        "Error: DOT_AGENT_DECK_PANE_ID environment variable not set.\nThis command should be run from within a worker-agent-deck managed pane."
                    );
                    return ExitCode::FAILURE;
                }
            };
            // Optional — the daemon injects this on spawn (same pattern as the
            // hook path); a pane spawned before agent-id tagging has none.
            let agent_id = std::env::var(DOT_AGENT_DECK_AGENT_ID).ok();
            let event_type = match dot_agent_deck::event::agent_event_type_from_state(&r#type) {
                Some(et) => et,
                None => {
                    eprintln!(
                        "Error: unknown agent-event --type {:?}. Expected one of: running, waiting, finished.",
                        r#type
                    );
                    return ExitCode::FAILURE;
                }
            };
            // Ride the EXISTING raw-`AgentEvent` socket path (zero new wire):
            // a bare AgentEvent with no `message_type` envelope, keyed on a
            // stable session id derived from the pane so repeated events update
            // the same card. The daemon's `run_hook_loop` falls back to
            // `AgentEvent` and `apply_event` drives the status.
            let event = dot_agent_deck::event::AgentEvent {
                session_id: format!("{pane_id}-session"),
                // TODO(companion PRD): derive agent type from the pane instead
                // of hard-coding Pi. Safe today because the daemon's
                // `apply_event` only UPGRADES `None` → a concrete type (never
                // downgrades), so a hard-coded `Pi` from the `agent-event`
                // subcommand can't clobber an already-known type.
                agent_type: dot_agent_deck::event::AgentType::Pi,
                event_type,
                tool_name: None,
                tool_detail: None,
                cwd: None,
                timestamp: chrono::Utc::now(),
                user_prompt: None,
                metadata: Default::default(),
                pane_id: Some(pane_id),
                agent_id,
                agent_version: None,
                schema_version: None,
                live_target: None,
                model: None,
            };
            let json = match serde_json::to_string(&event) {
                Ok(j) => j,
                Err(e) => {
                    eprintln!("Failed to serialize agent-event: {e}");
                    return ExitCode::FAILURE;
                }
            };
            if dot_agent_deck::hook::send_to_socket(&json).is_none() {
                eprintln!("Failed to send agent-event to daemon socket.");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Some(Commands::GetSeed) => {
            let pane_id = match std::env::var(DOT_AGENT_DECK_PANE_ID) {
                Ok(id) => id,
                Err(_) => {
                    eprintln!(
                        "Error: DOT_AGENT_DECK_PANE_ID environment variable not set.\nThis command should be run from within a worker-agent-deck managed pane."
                    );
                    return ExitCode::FAILURE;
                }
            };
            // Ask the daemon (over the hook socket) for the seed it prepared
            // for this pane. READ-ONLY request/response — the one hook-socket
            // verb that reads a reply. A missing daemon / older daemon that
            // doesn't answer → `None` → we print nothing and exit 0, so the
            // extension no-sends and the daemon's PTY-injection safety net
            // still delivers (graceful cross-version degradation, no
            // PROTOCOL_VERSION dependency).
            let req = dot_agent_deck::event::DaemonMessage::GetSeed(
                dot_agent_deck::event::GetSeedRequest { pane_id },
            );
            let json = match serde_json::to_string(&req) {
                Ok(j) => j,
                Err(e) => {
                    eprintln!("Failed to serialize get-seed request: {e}");
                    return ExitCode::FAILURE;
                }
            };
            match dot_agent_deck::hook::request_from_socket(&json) {
                Some(line) if !line.trim().is_empty() => {
                    match serde_json::from_str::<dot_agent_deck::event::GetSeedResponse>(&line) {
                        Ok(resp) => {
                            if let Some(seed) = resp.seed {
                                // Print the seed verbatim (no trailing newline)
                                // so the extension captures exactly the prepared
                                // text. Empty seed → print nothing.
                                print!("{seed}");
                            }
                            ExitCode::SUCCESS
                        }
                        // A reply we can't parse is treated as "no seed": print
                        // nothing, exit 0 — the fallback still covers delivery.
                        Err(_) => ExitCode::SUCCESS,
                    }
                }
                // No reply (no daemon / older daemon / no seed) → no seed.
                _ => ExitCode::SUCCESS,
            }
        }
        Some(Commands::Orchestrator { cmd }) => match cmd {
            // PRD #201 M3.2: thin wrapper — wire real PATH-detection + the real
            // `~/.pi/agent/extensions/dot-agent-deck` dir to the pure
            // `run_setup` core, then render its report to stdout/stderr + exit.
            OrchestratorCmd::Setup => {
                use dot_agent_deck::orchestrator_ext;
                // HOME-unset-safe (matching the auto-materialize path): the
                // strict resolver yields `None` when HOME is unset OR empty.
                // Because this is an EXPLICIT user command it ERRORS (non-zero)
                // rather than silently guessing a `/tmp`/`./` location Pi will
                // never discover — do NOT materialize, do NOT report success.
                match orchestrator_ext::default_extension_dir() {
                    None => {
                        eprintln!(
                            "orchestrator setup: HOME is not set — cannot locate Pi's extension \
                             directory (~/.pi/agent/extensions/dot-agent-deck). Set HOME and \
                             re-run `dot-agent-deck orchestrator setup`."
                        );
                        ExitCode::FAILURE
                    }
                    Some(target_dir) => {
                        let pi_present = orchestrator_ext::pi_on_path();
                        match orchestrator_ext::run_setup(pi_present, &target_dir) {
                            Ok(report) if report.success => {
                                println!("{}", report.message);
                                ExitCode::SUCCESS
                            }
                            Ok(report) => {
                                eprintln!("{}", report.message);
                                ExitCode::FAILURE
                            }
                            Err(e) => {
                                eprintln!(
                                    "orchestrator setup: failed to materialize the Pi extension into {}: {e}",
                                    target_dir.display()
                                );
                                ExitCode::FAILURE
                            }
                        }
                    }
                }
            }
        },
        Some(Commands::Daemon { cmd }) => match cmd {
            DaemonCmd::Serve => {
                // PRD #170 M1.2: capture the login-shell PATH and apply it to
                // the daemon's OWN environment HERE — in the synchronous `main`
                // dispatch, BEFORE `run_daemon_serve_cli` builds its tokio
                // runtime (`#[tokio::main]`) and any worker threads exist. That
                // single-threaded window is the PRD's stated `set_var`
                // soundness condition. This covers BOTH the `daemon serve` path
                // and the lazy-spawned daemon, since the deck lazy-spawns by
                // fork-exec'ing this exact subcommand. Logging is initialized
                // first so the capture result is recorded; `run_daemon_serve_cli`
                // therefore no longer initializes it.
                init_logging_from_env();
                dot_agent_deck::login_shell::apply_login_shell_path();
                // PRD #201: materialize the bundled Pi orchestrator extension ONCE
                // at daemon startup — parity with claude/opencode installing their
                // hooks/plugin at startup. This covers both the lazy-spawned daemon
                // and a headless `daemon serve`, and is command-agnostic (works for
                // `pi`, an absolute path, or a wrapper like `devbox run pi-big`),
                // since it does not look at any spawn command. Runs AFTER the
                // login-shell PATH is applied so pi-presence is detected against the
                // daemon's real PATH. Self-guards on pi being installed; a no-op
                // otherwise. It honors `PI_CODING_AGENT_DIR` (else `~/.pi/agent`),
                // so it lands where pi will look — see `orchestrator_ext`.
                dot_agent_deck::orchestrator_ext::auto_materialize(&[]);
                // PRD #20 §4.2.1: same precedent for Codex — install the deck's
                // `hooks.json` into the active Codex home and record SCOPED,
                // hash-pinned trust for exactly those entries, ONCE at daemon
                // startup. Command-agnostic on purpose: the spawn seam can only
                // detect a `codex` basename, so a launcher (`devbox run codex-big`,
                // `run_codex.sh`) previously got no hooks at all. With the home
                // prepared here, its hook events reach the pane through the
                // inherited `DOT_AGENT_DECK_PANE_ID` regardless of launch method.
                // Runs AFTER the login-shell PATH is applied so codex-presence is
                // detected against the daemon's real PATH. Self-guards on codex
                // being installed and a resolvable home; a no-op otherwise.
                dot_agent_deck::codex_hooks_manage::auto_install_and_trust_at_startup();
                // Same precedent for Devin, which is also a native-hooks agent:
                // merge the deck's hooks into Devin's user config ONCE at daemon
                // startup, command-agnostically, so a headless daemon and a
                // launcher whose basename isn't `devin` are covered too. Runs
                // AFTER the login-shell PATH is applied so devin-presence is
                // detected against the daemon's real PATH. Self-guards on devin
                // being on PATH and a resolvable config dir; a no-op otherwise.
                dot_agent_deck::devin_hooks_manage::auto_install();
                run_daemon_serve_cli()
            }
            DaemonCmd::Hello => run_daemon_hello_cli(),
            DaemonCmd::Stop { force } => run_daemon_stop_cli(force),
            DaemonCmd::Restart { force } => run_daemon_restart_cli(force),
            DaemonCmd::Status { json } => run_daemon_status_cli(json),
        },
        Some(Commands::Remote { cmd }) => match cmd {
            RemoteCmd::Add {
                name,
                target,
                kind,
                port,
                key,
                version,
                no_install,
            } => {
                let opts = dot_agent_deck::remote::AddOptions {
                    name,
                    remote_type: match kind {
                        CliRemoteType::Ssh => "ssh".to_string(),
                        CliRemoteType::Kubernetes => "kubernetes".to_string(),
                    },
                    target,
                    port,
                    key,
                    version,
                    no_install,
                    release_base: dot_agent_deck::remote::RELEASE_BASE.to_string(),
                };
                let path = dot_agent_deck::remote::default_remotes_path();
                let executor = dot_agent_deck::remote::SystemSshExecutor::new();
                match dot_agent_deck::remote::add(&opts, &executor, &path) {
                    Ok(_) => ExitCode::SUCCESS,
                    Err(e) => {
                        eprintln!("{e}");
                        ExitCode::FAILURE
                    }
                }
            }
            RemoteCmd::List => {
                let path = dot_agent_deck::remote::default_remotes_path();
                let mut stdout = std::io::stdout().lock();
                match dot_agent_deck::remote::list(&path, &mut stdout) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(e) => {
                        eprintln!("{e}");
                        ExitCode::FAILURE
                    }
                }
            }
            RemoteCmd::Remove { name } => {
                let path = dot_agent_deck::remote::default_remotes_path();
                match dot_agent_deck::remote::remove(&name, &path) {
                    Ok(_) => {
                        println!(
                            "Removed remote '{name}' from local registry. The dot-agent-deck binary on the remote and its hooks are unaffected; if you want to clean those up, ssh in and run `dot-agent-deck hooks uninstall` and `rm ~/.local/bin/dot-agent-deck`."
                        );
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("{e}");
                        ExitCode::FAILURE
                    }
                }
            }
            RemoteCmd::Upgrade {
                name,
                version,
                no_install,
            } => {
                let opts = dot_agent_deck::remote::UpgradeOptions {
                    name,
                    version,
                    no_install,
                    release_base: dot_agent_deck::remote::RELEASE_BASE.to_string(),
                };
                let path = dot_agent_deck::remote::default_remotes_path();
                let executor = dot_agent_deck::remote::SystemSshExecutor::new();
                match dot_agent_deck::remote::upgrade(&opts, &executor, &path) {
                    Ok(_) => ExitCode::SUCCESS,
                    Err(e) => {
                        eprintln!("{e}");
                        ExitCode::FAILURE
                    }
                }
            }
        },
        Some(Commands::Worktree { cmd }) => match cmd {
            WorktreeCmd::List { json, mine } => run_worktree_list_cli(json, mine),
            WorktreeCmd::Reclaim { yes } => run_worktree_reclaim_cli(yes),
            WorktreeCmd::Pin { path } => run_worktree_pin_cli(path),
            WorktreeCmd::Unpin { path } => run_worktree_unpin_cli(path),
        },
        Some(Commands::Issue { cmd }) => match cmd {
            IssueCmd::Claim {
                issue,
                repo,
                takeover,
                confirm_stopped,
            } => run_issue_claim_cli(issue, repo, takeover, confirm_stopped),
            IssueCmd::ClaimCheck { issue, repo } => run_issue_claim_check_cli(issue, repo),
            IssueCmd::Release {
                issue,
                repo,
                force,
                confirm_stopped,
                reason,
            } => run_issue_release_cli(issue, repo, force, confirm_stopped, reason),
        },
        Some(Commands::Wait { cmd }) => {
            let pane_id = match std::env::var(DOT_AGENT_DECK_PANE_ID) {
                Ok(id) => id,
                Err(_) => {
                    eprintln!(
                        "Error: DOT_AGENT_DECK_PANE_ID environment variable not set.\nThis command should be run from within a worker-agent-deck managed pane."
                    );
                    return ExitCode::FAILURE;
                }
            };
            let msg = match cmd {
                WaitCmd::Start { label } => {
                    let ttl_secs = dot_agent_deck::agent_pty::wait_ttl_secs();
                    dot_agent_deck::event::DaemonMessage::WaitStart(
                        dot_agent_deck::event::WaitStartSignal {
                            pane_id,
                            label,
                            ttl_secs,
                            timestamp: chrono::Utc::now(),
                        },
                    )
                }
                WaitCmd::Done { label, outcome } => dot_agent_deck::event::DaemonMessage::WaitDone(
                    dot_agent_deck::event::WaitDoneSignal {
                        pane_id,
                        label,
                        outcome: outcome.into(),
                        timestamp: chrono::Utc::now(),
                    },
                ),
            };
            let json = match serde_json::to_string(&msg) {
                Ok(j) => j,
                Err(e) => {
                    eprintln!("Failed to serialize wait signal: {e}");
                    return ExitCode::FAILURE;
                }
            };
            if dot_agent_deck::hook::send_to_socket(&json).is_none() {
                eprintln!("Failed to send wait signal to daemon socket.");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Some(Commands::Connect { name }) => run_connect(name),
        Some(Commands::Schedule { action }) => run_schedule_cli(action),
        Some(Commands::Snapshot { cmd }) => match cmd {
            // PRD #89 M4.2 — local fresh-start escape hatch. Reuses the same
            // `SavedSession::clear()` the TUI calls at teardown, so it honors
            // the `DOT_AGENT_DECK_SESSION` override and deletes the one global
            // snapshot at `config::session_path()`.
            SnapshotCmd::Clear => match dot_agent_deck::config::SavedSession::clear() {
                Ok(()) => {
                    println!(
                        "Cleared the local saved-session snapshot. The next `dot-agent-deck` startup will begin from an empty dashboard."
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("Failed to clear the saved-session snapshot: {e}");
                    ExitCode::FAILURE
                }
            },
        },
        Some(Commands::Validate { path }) => {
            use dot_agent_deck::config_validation::{has_errors, validate_config};
            use dot_agent_deck::project_config::load_project_config;

            match load_project_config(&path) {
                Ok(None) => {
                    eprintln!("No .dot-agent-deck.toml found in {}", path.display());
                    ExitCode::FAILURE
                }
                Ok(Some(config)) => {
                    let issues = validate_config(&config);
                    if issues.is_empty() {
                        println!("Config is valid.");
                        ExitCode::SUCCESS
                    } else {
                        for issue in &issues {
                            eprintln!("{issue}");
                        }
                        if has_errors(&issues) {
                            ExitCode::FAILURE
                        } else {
                            ExitCode::SUCCESS
                        }
                    }
                }
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some(Commands::Wrap { agent, command }) => {
            dot_agent_deck::wrap::run_wrap(agent.as_deref(), &command)
        }
    }
}

/// The deck's project directory for the process-global config reads that have
/// no narrower directory to key off — today just the `[features]` table
/// (issue #577).
///
/// Resolved ONCE here, at the entry point, and handed to
/// `features::init_and_watch` as an explicit directory — the same shape as
/// `examine_worktrees(&cwd)` and `run_reclaim(&cwd, …)` below, and as
/// `load_project_config(dir)` everywhere else. `features_config_path` no
/// longer reaches for the process cwd itself, so nothing downstream of this
/// call silently depends on where the process happens to be running.
///
/// The launch directory is where the search STARTS, not where it ends:
/// `resolve_project_dir` walks up to the nearest ancestor holding a trusted
/// `.dot-agent-deck.toml`, so a deck started at `repo/src` finds `repo`'s
/// flags instead of silently finding none. With no config at or above the
/// launch directory it returns that directory unchanged, which is the
/// pre-#577 path exactly.
fn launch_project_dir() -> std::path::PathBuf {
    let start = std::env::current_dir().unwrap_or_else(|e| {
        // Not fatal: `.` preserves the pre-#577 fallback, and a deck that
        // cannot resolve its own cwd still starts with the flag OFF.
        tracing::warn!(
            "failed to resolve the launch directory ({e}); reading [features] relative to \".\""
        );
        std::path::PathBuf::from(".")
    });
    dot_agent_deck::config::resolve_project_dir(&start)
}

#[tokio::main]
async fn run_dashboard() -> ExitCode {
    init_logging_from_env();
    run_tui_session().await
}

/// Optional file-based logging from `DOT_AGENT_DECK_LOG`. Pulled out of the
/// dashboard entry point so the `connect` subcommand (which builds its own
/// tokio runtime) can call it once before launching the TUI body.
///
/// PRD #170 (Auditor-2): this MUST stay synchronous — a plain `std::fs::File`
/// writer, NEVER a `tracing_appender::non_blocking` / worker-thread appender.
/// On the `daemon serve` path it runs immediately before the pre-runtime
/// `apply_login_shell_path` `set_var` (main.rs); a logging thread spawned here
/// would land inside that single-threaded window and break the `set_var`
/// soundness invariant the login-shell PATH capture relies on.
fn init_logging_from_env() {
    if let Ok(log_val) = std::env::var("DOT_AGENT_DECK_LOG") {
        let is_default = log_val.is_empty() || log_val == "1";
        let log_path = if is_default {
            let dir = state_dir();
            if let Err(e) = dot_agent_deck::platform::fsperm::ensure_owner_only_dir(&dir) {
                warn_log_setup_failure("create owner-only state dir", &dir, e);
                return;
            }
            dir.join("deck.log")
        } else {
            std::path::PathBuf::from(log_val)
        };
        let open_result = if is_default {
            open_deck_log_file(&log_path)
        } else {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
        };
        match open_result {
            Ok(log_file) => {
                tracing_subscriber::fmt()
                    .with_env_filter(dot_agent_deck::logging::env_filter_from_env())
                    .with_writer(log_file)
                    .with_ansi(false)
                    .init();
            }
            Err(e) => {
                warn_log_setup_failure("open log file", &log_path, e);
            }
        }
    }
}

/// Opens the default `deck.log` with the same symlink/mode hardening
/// `daemon.log` already has (`platform/detach/unix.rs`): `O_NOFOLLOW` +
/// mode `0o600`, refusing rather than following a pre-planted symlink at the
/// exact log path. Applies only to the default `state_dir()`-derived path —
/// `init_logging_from_env`'s explicit `DOT_AGENT_DECK_LOG=<path>` branch is
/// user-specified and stays on the unhardened open, matching how the rest of
/// this fix draws that line.
#[cfg(unix)]
fn open_deck_log_file(log_path: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o600)
        .open(log_path)
    {
        Ok(f) => Ok(f),
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => Err(std::io::Error::other(format!(
            "log path {} is a symlink — refusing to follow (someone may have planted it to redirect log output)",
            log_path.display()
        ))),
        Err(e) => Err(e),
    }
}

#[cfg(not(unix))]
fn open_deck_log_file(log_path: &std::path::Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
}

fn warn_log_setup_failure(action: &str, path: &std::path::Path, e: impl std::fmt::Display) {
    eprintln!("Warning: failed to {action} {}: {e}", path.display());
}

/// The TUI body extracted from `run_dashboard` so `connect` can reuse it.
/// PRD #93 Phase 2: every fresh `dot-agent-deck` invocation lazy-spawns a
/// per-user daemon on the `attach_socket_path()` Unix socket and
/// attaches to it via the streaming protocol. The legacy in-process
/// daemon path (and its env-var escape hatch) is gone — the daemon is
/// always external.
///
/// Returns `ExitCode::FAILURE` when the external-daemon bootstrap fails
/// (spawn error, start timeout, or trust-check rejection). Successful TUI
/// runs return `ExitCode::SUCCESS` — including TUI-task errors, which are
/// already surfaced to stderr.
async fn run_tui_session() -> ExitCode {
    // PRD #139 M1.2/M1.3: initialize the process-global experimental flag from
    // `.dot-agent-deck.toml` `[features]` (env override wins) and start the
    // live re-read watcher. The startup state is recorded via a single
    // `tracing::info!` line, which surfaces only when file logging is enabled
    // (`DOT_AGENT_DECK_LOG`); it is never printed to the terminal. The project
    // directory is resolved HERE, at the entry point, and passed down (issue
    // #577) — see `launch_project_dir`.
    //
    // Fork issue #303: when no `.dot-agent-deck.toml` was found in the
    // project directory or any ancestor, and fork issue #309: when an
    // ancestor was declined as world-writable, `init_and_watch` returns
    // diagnosability warnings. Print them here, before
    // `ensure_external_daemon_or_die`/`run_tui`'s `ratatui::init()` flips
    // into the alternate screen, so they land on stderr in the normal
    // terminal — mirroring how `KeybindingConfig::load()` below prints its
    // own malformed-config warnings ahead of the alt-screen switch. No
    // `DOT_AGENT_DECK_LOG` and no restart flag required, which is the whole
    // point: today's `tracing::warn!` above is invisible without both.
    for warning in dot_agent_deck::features::init_and_watch(&launch_project_dir()) {
        eprintln!("Warning: {warning}");
    }

    let state = Arc::new(RwLock::new(AppState::default()));
    let attach_path = attach_socket_path();

    // If the attach socket is missing, `ensure_external_daemon_or_die`
    // fork-execs `dot-agent-deck daemon serve` detached under
    // flock-serialized contention (so two simultaneous TUIs can't both
    // win the bind — M1.3) and trust-checks any existing socket
    // (uid + 0o600 + is-socket) before the TUI's DaemonClient touches it.
    if let Err(e) = ensure_external_daemon_or_die(&attach_path).await {
        eprintln!(
            "failed to connect to daemon at {}: {e}",
            attach_path.display()
        );
        return ExitCode::FAILURE;
    }
    // PRD #103 Phase 2 / PRD #161 Part A: build-version handshake against
    // the running daemon. Runs unconditionally — including the
    // freshly-spawned case where the build-ids are necessarily equal (PRD
    // M2.3). The cost is one extra Unix-socket round-trip on cold start;
    // the upside is a smoke test of the handshake on every launch, which
    // catches regressions in `ensure_external_daemon_or_die` itself (wrong
    // socket / wrong binary) or in the wire encoding of the `build_version`
    // field.
    //
    // PRD #161 D2 (option A — consent-based always-restart) decides the
    // mismatch path by agents-present + TTY:
    //   - No agents: the daemon is SIGTERM'd silently (`Recovered`); we
    //     fall through and re-spawn a fresh daemon at the current build.
    //   - Agents + TTY: an interactive prompt names the live agents; a
    //     single `s` restarts (`Recovered`, re-spawn), any dismiss key
    //     declines (`ProceedOnExisting`, keep the existing daemon — D4
    //     never-strand).
    //   - Agents + non-TTY: prints the recovery hint to stderr and exits
    //     non-zero (the only non-zero-exit path).
    // Errors are already user-visible inside the helper, so we render no
    // further message here.
    //
    // Fork issue #17: the helper ALSO enforces `PROTOCOL_VERSION` before any of
    // that, and a skew returns `HandshakeError::ProtocolMismatch`. That variant
    // carries its whole (multi-line, actionable) message in `Display`, so the
    // catch-all `Err(e)` arm below is what renders it — no special-casing here.
    let handshake_outcome =
        match build_version_handshake::ensure_compatible_daemon_or_die(&attach_path).await {
            Ok(outcome) => outcome,
            Err(build_version_handshake::HandshakeError::MismatchAborted) => {
                return ExitCode::FAILURE;
            }
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::FAILURE;
            }
        };
    // After a `Recovered` outcome the old daemon was just SIGTERM'd; the
    // next attach lazy-spawns a fresh one. Re-run the bootstrap so the
    // socket is back before any client (DaemonClient::list_agents,
    // spawn_event_subscriber, the embedded-pane controller) touches it.
    // On `Match` (compatible) or `ProceedOnExisting` (user declined the
    // restart, keeping the existing daemon) the daemon is already running —
    // re-running the bootstrap would just be wasted I/O.
    if matches!(
        handshake_outcome,
        build_version_handshake::HandshakeOutcome::Recovered
    ) && let Err(e) = ensure_external_daemon_or_die(&attach_path).await
    {
        eprintln!(
            "failed to re-spawn daemon at {} after version-mismatch recovery: {e}",
            attach_path.display()
        );
        return ExitCode::FAILURE;
    }
    // Test-only escape hatch (PRD #103 M4.2): integration tests in
    // tests/build_version_handshake.rs need to exercise the handshake
    // path (including SIGTERM + lazy re-spawn) without entering the
    // full TUI. Setting `DOT_AGENT_DECK_EXIT_AFTER_HANDSHAKE` causes
    // the TUI to exit cleanly here, after the handshake completed and
    // the daemon socket is back up. Production code never sets it; the
    // env-var name is grep-ably explicit so a future audit can confirm.
    if std::env::var_os("DOT_AGENT_DECK_EXIT_AFTER_HANDSHAKE").is_some() {
        return ExitCode::SUCCESS;
    }
    // PRD #76 M2.17: subscribe to the daemon's `AgentEvent` broadcast so
    // the TUI's `AppState` mirrors live agent activity.
    //
    // Fork issue #36: the subscription and the `ListAgents` hydration snapshot
    // below used to race, and an edge-triggered event that fell between them
    // (the shell-activity monitor's paired `ShellIdle`) was delivered to
    // nobody — leaving the reconnected pane reading `Working` forever. The
    // gate orders the two: hydration waits for the subscription to be
    // confirmed before snapshotting, and the subscriber holds events back
    // until hydration has seeded its cards. See `dot_agent_deck::reconnect`.
    let hydration_gate = HydrationGate::armed();
    spawn_event_subscriber(attach_path.clone(), state.clone(), hydration_gate.clone());
    // Safety net: hydration is what opens the gate, and it only runs under the
    // embedded-pane (external-daemon) controller. If that path is skipped, or
    // dies before signalling, nothing else would ever release the held events.
    // Opening the gate late is a return to the pre-fix behaviour (the window
    // stays open for this attach), never a stall.
    {
        let gate = hydration_gate.clone();
        tokio::spawn(async move {
            tokio::time::sleep(HYDRATION_GATE_WATCHDOG).await;
            if !gate.is_seeded() {
                tracing::warn!(
                    held_secs = HYDRATION_GATE_WATCHDOG.as_secs(),
                    "hydration gate never released by the reconnect path; opening it so live \
                     events resume"
                );
                gate.mark_seeded();
            }
        });
    }

    let version_state = state.clone();
    tokio::spawn(async move {
        if let Some(latest) = dot_agent_deck::version::check_for_update().await {
            version_state.write().await.update_available = Some(latest);
        }
    });

    let config = dot_agent_deck::config::DashboardConfig::load();

    // PRD #40: resolve keybindings client-side, *before* entering the
    // alternate screen, so any malformed-config / conflict / unknown-action
    // warnings land on stderr in the normal terminal (and, under a PTY, in
    // the byte stream that precedes the alt-screen switch) where they are
    // actually visible. `run_tui` (via `ratatui::init`) is what flips into
    // the alt-screen, so loading here keeps the warnings ahead of it.
    let keybindings = dot_agent_deck::keybindings::KeybindingConfig::load();

    // Auto-install hooks/plugins for detected agents (silent, best-effort).
    // PRD #20 M2 / R20-010: driven from the agent registry — iterate the shipped
    // agents and run each spec's OWN startup auto-install action. Order is stable
    // (`ALL` order). Dispatching per-spec (rather than mapping the reusable
    // `IntegrationStrategy` enum to a hardcoded incumbent) means a future agent
    // reusing `NativeHooks`/`Plugin` runs ITS OWN installer, not Claude's or
    // OpenCode's. Claude installs native hooks and OpenCode its plugin at
    // startup; Codex installs its native `hooks.json` AND records scoped,
    // hash-pinned trust for it here too (PRD #20 §4.2.1 — command-agnostic, so a
    // launcher like `devbox run codex-big` that the spawn seam can't detect is
    // still covered); Pi (`Extension`) materializes at spawn-time (see
    // `agent_pty`), so its `startup_auto_install` is `None` and it is skipped.
    {
        use dot_agent_deck::agent_registry::ALL;
        for spec in ALL {
            if let Some(install) = spec.startup_auto_install {
                install();
            }
        }
    }

    let pane_controller: Arc<dyn PaneController> = Arc::new(
        EmbeddedPaneController::new(attach_path.clone(), tokio::runtime::Handle::current())
            // Fork issue #36: hydration waits on this gate before snapshotting
            // and releases it once every rebuilt pane is seeded.
            .with_hydration_gate(hydration_gate),
    );
    let tui_state = state.clone();
    let tui_result = tokio::task::spawn_blocking(move || {
        run_tui(tui_state, pane_controller, config, keybindings)
    })
    .await;

    // TUI exited — clean up. The daemon was fork-execed detached by
    // ensure_external_daemon_or_die (setsid'd into its own session) so
    // it is intentionally outside this process tree: we do not abort
    // the daemon and do not unlink its sockets. Agents must survive
    // TUI exit (PRD #76 line 199).

    if let Err(e) = tui_result {
        eprintln!("TUI task error: {e}");
    } else if let Ok(Err(e)) = tui_result {
        eprintln!("TUI error: {e}");
    }
    ExitCode::SUCCESS
}

/// `dot-agent-deck connect [name]` — PRD #76 M2.9.
///
/// Resolves the remote (via lookup or picker), probes the remote
/// `dot-agent-deck` for reachability + version sanity, then exec's
/// `ssh -t` to run the deck TUI on the remote in M2.8 external-daemon
/// mode. The laptop process blocks until ssh exits and propagates the
/// exit code.
fn run_connect(name: Option<String>) -> ExitCode {
    let registry_path = dot_agent_deck::remote::default_remotes_path();

    let entry = match name {
        Some(n) => match dot_agent_deck::connect::lookup_remote(&n, &registry_path) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::FAILURE;
            }
        },
        None => {
            let stdin = std::io::stdin();
            let mut input = stdin.lock();
            let stdout = std::io::stdout();
            let mut output = stdout.lock();
            match dot_agent_deck::connect::pick_remote(&registry_path, &mut input, &mut output) {
                Ok(e) => e,
                Err(e) => {
                    let _ = output.flush();
                    eprintln!("{e}");
                    return ExitCode::FAILURE;
                }
            }
        }
    };

    let local_version = env!("DAD_VERSION");
    match dot_agent_deck::connect::run_connect_default(&entry, &registry_path, local_version) {
        Ok(0) => ExitCode::SUCCESS,
        // ExitCode::from(u8) is the closest we can get to "propagate ssh's
        // exit code." Codes outside 0..=255 saturate to 255, which is also
        // the value ssh itself uses for its own transport errors — that
        // collision is harmless because we already classified those as
        // typed RemoteConnectError before reaching the spawn.
        Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// `dot-agent-deck daemon hello` — PRD #76 M2.21 protocol-version handshake.
/// Prints a JSON-encoded [`dot_agent_deck::daemon_protocol::AttachResponse`]
/// carrying `server_version = PROTOCOL_VERSION` (and, per PRD #103 M1.3,
/// `build_version = env!("DAD_BUILD_ID")`) and exits.
///
/// Used by the laptop-side `connect` flow over ssh: the remote binary's
/// compiled-in `PROTOCOL_VERSION` is what its daemon would speak, so a static
/// print here is equivalent to a Hello round-trip against a running daemon —
/// and avoids lazy-spawning the daemon just to answer a version probe.
///
/// The wire shape mirrors what the daemon dispatcher returns for an
/// [`dot_agent_deck::daemon_protocol::AttachRequest::Hello`] in the
/// in-process attach path, so the client-side deserializer is the same in
/// both flows. Keep this helper in lockstep with that dispatcher arm and
/// with `AttachResponse::hello` — any divergence silently breaks the
/// handshake.
fn run_daemon_hello_cli() -> ExitCode {
    let resp = dot_agent_deck::daemon_protocol::AttachResponse::hello(
        dot_agent_deck::daemon_protocol::PROTOCOL_VERSION,
    );
    let json = match serde_json::to_string(&resp) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("Failed to serialize hello response: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("{json}");
    ExitCode::SUCCESS
}

/// `dot-agent-deck daemon status [--json]`. Read-only CLI
/// consumer of the existing `AttachRequest::ListAgents`
/// ([`dot_agent_deck::daemon_client::DaemonClient::list_agents`]) — no new
/// attach request type, and therefore no `PROTOCOL_VERSION` bump: this command
/// puts nothing new on the wire, so an older daemon answers a newer CLI's
/// status query exactly as it always did (issue #459 — this rationale used to
/// cite a design note under the gitignored `.dot-agent-deck/`, which no reader
/// of the merged source could open). The `--json` document has its own,
/// separate [`dot_agent_deck::daemon_status::SCHEMA_VERSION`]; that is what
/// moves when the document shape changes. Row
/// shaping lives in [`dot_agent_deck::daemon_status`]; this wrapper only
/// bounds the round trip with [`dot_agent_deck::daemon_status::STATUS_REQUEST_TIMEOUT`]
/// and translates the outcome into stdout/stderr text and an exit code.
///
/// "Unavailable" (no daemon, a transport error, or a timed-out request) is
/// reported as failure — a status query that got no answer learned nothing,
/// unlike `daemon stop`'s idempotent "no daemon running" — but deliberately
/// never with clap's own exit code 2, so a caller can tell "this build
/// doesn't understand the request" apart from "the daemon didn't answer".
/// Never spawns, retries, or otherwise perturbs the daemon it's asking
/// about: a timeout abandons the query rather than looping.
#[tokio::main]
async fn run_daemon_status_cli(json: bool) -> ExitCode {
    use dot_agent_deck::daemon_status::{
        STATUS_REQUEST_TIMEOUT, StatusDocument, build_status_agents, format_human,
    };

    let client = DaemonClient::new(attach_socket_path());
    let records = match tokio::time::timeout(STATUS_REQUEST_TIMEOUT, client.list_agents()).await {
        Ok(Ok(records)) => records,
        Ok(Err(e)) => {
            eprintln!("daemon status: unavailable ({e})");
            return ExitCode::FAILURE;
        }
        Err(_elapsed) => {
            eprintln!(
                "daemon status: unavailable (no response within {}s)",
                STATUS_REQUEST_TIMEOUT.as_secs()
            );
            return ExitCode::FAILURE;
        }
    };

    let agents = build_status_agents(records);
    if json {
        match serde_json::to_string(&StatusDocument::new(agents)) {
            Ok(j) => {
                println!("{j}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("daemon status: failed to serialize JSON: {e}");
                ExitCode::FAILURE
            }
        }
    } else {
        print!("{}", format_human(&agents));
        ExitCode::SUCCESS
    }
}

/// `dot-agent-deck worktree list [--json] [--mine]` — PRD #422, `--mine` added
/// by fork #166 M3.0. Pure CLI-subprocess operation over `git`/`gh` in the
/// current directory's repo — no daemon involved, so unlike the `daemon
/// status`/`daemon stop` wrappers below this is plain synchronous code, no
/// `#[tokio::main]`. `--mine` must keep this property (M3.0's own
/// correctness bar is "correct immediately after a daemon restart"), so it
/// filters on the on-disk marker owner against `DOT_AGENT_DECK_WORKTREE_OWNER`
/// rather than querying the daemon. Row shaping and the gate itself live in
/// [`dot_agent_deck::worktree_reclaim`]; this wrapper only translates the
/// outcome into stdout/stderr text and an exit code.
fn run_worktree_list_cli(json: bool, mine: bool) -> ExitCode {
    use dot_agent_deck::agent_pty::{
        DOT_AGENT_DECK_WORKTREE_OWNER, ORCHESTRATION_UNKNOWN_SENTINEL,
    };
    use dot_agent_deck::terminal_sanitize::sanitize_for_terminal_display;
    use dot_agent_deck::worktree_reclaim::{
        WorktreeListDocument, examine_worktrees, format_disagreement_warning,
        format_excluded_unknown_owner_warning, format_list_error_for_cli, format_list_human,
        is_mine, owner_disagreements, sanitize_marker_creator, shallow_repo_warning,
    };

    let owner_filter = if mine {
        match std::env::var(DOT_AGENT_DECK_WORKTREE_OWNER) {
            // PR #215 round-3 fixup (reviewer F4): an exported-but-empty
            // (or whitespace-only) variable is exactly as meaningless as an
            // absent one -- `std::env::var` returns `Ok("")` for it, so
            // without this arm it fell through to "use it as the filter"
            // and produced a definitive-looking `no worktrees owned by `
            // with a blank subject and exit 0. This must run on the RAW
            // value and precede sanitization (round-4 fixup, R4-1):
            // `sanitize_marker_creator("")` returns its `"unknown"` floor,
            // not an empty string, so sanitizing first would let an empty
            // variable slip past this guard as a non-empty, non-sentinel
            // value.
            Ok(v) if v.trim().is_empty() => {
                eprintln!(
                    "worktree list --mine: {DOT_AGENT_DECK_WORKTREE_OWNER} is set but empty -- \
                     cannot determine which worktrees belong to this orchestration; supply it or \
                     drop --mine"
                );
                return ExitCode::FAILURE;
            }
            // Round-4 fixup (R4-1): compare the SANITIZED value against the
            // sentinel, not the merely-trimmed one -- `trim` strips
            // whitespace, not control characters, so a value like
            // `"orchestration:unknown\u{7}"` used to trim to itself, fail
            // this check, and reach the filter as an unmatchable near-miss
            // of the sentinel instead of being refused by it.
            Ok(v) if sanitize_marker_creator(&v) == ORCHESTRATION_UNKNOWN_SENTINEL => {
                eprintln!(
                    "worktree list --mine: {DOT_AGENT_DECK_WORKTREE_OWNER} is set to the \
                     `{ORCHESTRATION_UNKNOWN_SENTINEL}` sentinel, which is never a real \
                     identity -- refusing rather than matching another nameless orchestration's \
                     worktrees"
                );
                return ExitCode::FAILURE;
            }
            // Round-4 fixup (R4-1): filter on the SAME sanitized value used
            // above, not the raw one -- `read_marker_owner` always
            // sanitizes the on-disk marker, so a raw filter value could
            // never match a legitimate identity that carried stray
            // whitespace, and would silently report "no worktrees owned by
            // ..." for an identity that in fact owns one.
            Ok(v) => Some(sanitize_marker_creator(&v)),
            Err(_) => {
                eprintln!(
                    "worktree list --mine: {DOT_AGENT_DECK_WORKTREE_OWNER} is not set -- cannot \
                     determine which worktrees belong to this orchestration; supply it or drop \
                     --mine"
                );
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };

    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!(
                "worktree list: failed to resolve current directory: {}",
                sanitize_for_terminal_display(&e.to_string())
            );
            return ExitCode::FAILURE;
        }
    };
    let mut reports = match examine_worktrees(&cwd) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{}", format_list_error_for_cli(&e));
            return ExitCode::FAILURE;
        }
    };
    // Fork issues #230 / #231: everything a `--json` consumer would
    // otherwise only learn from a stderr line it never reads (or never
    // learn at all, for #231's mirror case) -- attached to the document
    // below via `with_warnings`, reporting-only, never changing which rows
    // `is_mine` retains.
    let mut json_warnings: Vec<String> = Vec::new();
    // Fork issue #325 M2: name a shallow enumerating repo and its repair
    // BEFORE the `--mine` filtering below, so the warning fires regardless
    // of whether this run is filtered -- unconditional, unlike the
    // `--mine`-only warnings that follow.
    if let Some(warning) = shallow_repo_warning(&cwd) {
        eprintln!("{warning}");
        json_warnings.push(warning);
    }
    if let Some(owner) = &owner_filter {
        // Issue #221 / #230: before filtering, name any row where the marker
        // names this owner but the independent `owned` resolution disagrees
        // -- otherwise that disagreement becomes indistinguishable from
        // "nothing found" once the retain below drops the row. Stderr for a
        // human, AND collected here so a `--json` consumer (which reads
        // stdout only) sees it too.
        for path in owner_disagreements(&reports, owner) {
            let warning = format_disagreement_warning(path, owner);
            eprintln!("{warning}");
            json_warnings.push(warning);
        }

        // Issue #231: the mirror case -- `owned: true` but `owner: None`
        // (most often a legacy pre-fork#166 marker, `owner_kind: "unknown"`
        // as of PRD fork#298) -- is unconditionally excluded by the retain
        // below regardless of which owner was filtered on, since it can
        // never match ANY `owner` string. Represented in the JSON document
        // rather than dropped silently; deliberately NOT also printed to
        // stderr (see `format_excluded_unknown_owner_warning`'s own doc --
        // issue #231 itself warns that a blanket per-row stderr warning here
        // would fire on every legacy worktree, every time).
        for report in reports.iter().filter(|r| r.owned && r.owner.is_none()) {
            json_warnings.push(format_excluded_unknown_owner_warning(&report.real_path));
        }

        // PR #215 fixup (reviewer F4 / auditor L1 item 3): `owned` must be a
        // conjunct, not just a non-`None` `owner`. `owner_of`'s own doc
        // records that `owned=false` can land alongside a non-`None`
        // `owner` (two independent `owned_git_dir` resolutions), and
        // explicitly accepted that divergence as cosmetic ONLY because "no
        // consumer treats `owner`'s mere presence as an ownership signal."
        // `--mine` is now such a consumer, so the conjunct restores the
        // precondition that comment relies on. `is_mine` (issue #221 review
        // round) is the same predicate `worktree/reclaim/030` asserts
        // against, so this retain and that test cannot silently drift apart.
        reports.retain(|r| is_mine(r, owner));
    }

    if json {
        let doc = WorktreeListDocument::new(reports).with_warnings(json_warnings);
        match serde_json::to_string(&doc) {
            Ok(j) => {
                println!("{j}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("worktree list: failed to serialize JSON: {e}");
                ExitCode::FAILURE
            }
        }
    } else {
        // PR #215 fixup (reviewer F7 / auditor L1): a filtered-to-empty
        // `--mine` used to fall through to `format_list_human`'s generic
        // "no worktrees found", which conflates "this repo has no
        // worktrees" with "none are yours" and with "yours failed to
        // match" -- the last of which is a wrong answer arriving
        // silently, against this feature's own fail-loud bar. Name the
        // identity that was filtered on instead.
        if reports.is_empty()
            && let Some(owner) = &owner_filter
        {
            // PR #215 round-3 fixup (auditor M1): `owner` is the ONE string
            // in this feature that reaches here with no sanitizer applied --
            // round 1 verified "the failure messages never print the
            // variable's value" as load-bearing, and printing it raw
            // reintroduced that terminal-escape / forged-line sink.
            // Round-4 fixup (R4-1): `owner` is already sanitized at
            // construction (the single `Ok(v) => Some(sanitize_marker_creator(&v))`
            // arm above), which guarantees this is the exact string the
            // filter compared against -- not a second, possibly-divergent
            // normalization of it. Issue #232 round 2 (gap 3) corrects M1's
            // premise, though: `sanitize_marker_creator` strips Unicode
            // category `Cc` but deliberately preserves `Cf` (bidi/format)
            // chars, so "already sanitized" does not mean "safe to print to
            // a terminal" -- this is the same terminal-display sink
            // `format_disagreement_warning` has, so it goes through the same
            // display sanitizer immediately before printing.
            println!(
                "no worktrees owned by {}",
                sanitize_for_terminal_display(owner)
            );
            return ExitCode::SUCCESS;
        }
        print!("{}", format_list_human(&reports));
        ExitCode::SUCCESS
    }
}

/// `dot-agent-deck worktree reclaim [--yes]` — PRD #422. Removes every
/// worktree the gate marks `remove` (deck-owned, merged PR, clean tree, no
/// gitignored content) unconditionally, and — only with `--yes` — also those
/// it marks `ask` (merged and clean, but either the deck cannot prove it
/// created them or the tree still holds gitignored content); once `--yes` is
/// passed, removal is unconditional on provenance or ignored content, same
/// as a deck-created worktree. Without `--yes`, `ask`-verdict worktrees are
/// left alone and reported as a pending decision that leads the output,
/// naming their exact paths and the ready-to-copy `--yes` command. Always
/// exits successfully once it has finished examining and acting on every
/// worktree; only a failure to enumerate worktrees at all (e.g. not a git
/// repo) is reported as failure.
fn run_worktree_reclaim_cli(yes: bool) -> ExitCode {
    use dot_agent_deck::issue_claim::resolve_remover_identity;
    use dot_agent_deck::terminal_sanitize::sanitize_for_terminal_display;
    use dot_agent_deck::worktree_reclaim::{
        format_reclaim_error_for_cli, format_reclaim_human, run_reclaim,
    };

    // issue #325 / reviewer NEW-1 / auditor P1: without this, the
    // `tracing::info!("worktree removed")` in `remove_worktree_dir` is
    // silently dropped -- `run_worktree_reclaim_cli` is a distinct top-level
    // `Commands::Worktree` arm, mutually exclusive with `Commands::Daemon`'s
    // `Serve` arm and `run_dashboard()` (the two existing call sites), so
    // this can never double-install a subscriber and panic on `.init()`.
    init_logging_from_env();

    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!(
                "worktree reclaim: failed to resolve current directory: {}",
                sanitize_for_terminal_display(&e.to_string())
            );
            return ExitCode::FAILURE;
        }
    };
    // issue #325 / reviewer P2-1 / auditor F2: attribute this reclaim's own
    // removals to whoever is actually running it. `resolve_remover_identity`
    // (not the bare `resolve_caller_identity` the claim LOCK uses) is
    // deliberate: `worktree reclaim`'s dominant caller is an orchestration
    // running it from the root checkout, which `resolve_caller_identity`
    // refuses to attribute (correctly, for the lock it was built for) --
    // this is diagnostic data, not a lock, so it degrades to whatever local
    // signal is available (pane id + host + cwd) rather than the bare
    // "unknown" a naive reuse of the lock resolver would produce for
    // exactly this case.
    let remover = resolve_remover_identity(&cwd);
    match run_reclaim(&cwd, yes, &remover) {
        Ok(outcome) => {
            print!("{}", format_reclaim_human(&outcome));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{}", format_reclaim_error_for_cli(&e));
            ExitCode::FAILURE
        }
    }
}

/// `dot-agent-deck worktree pin <path>` (fork issue #597) — pin an isolated
/// clone against `worktree reclaim`'s automatic removal. Synchronous, no
/// daemon socket involved, same shape as [`run_worktree_reclaim_cli`] above.
///
/// Review-findings fix round (F1): `path` is absolutized here, at the CLI
/// boundary, before it ever reaches [`pin_isolated_clone`] --
/// `canonicalize_best_effort` (`issue_dispatch_run.rs`) only canonicalizes a
/// path's *parent* and rejoins the raw final component, which is correct for
/// its own callers (all of which construct an already-absolute path
/// internally) but leaves a bare relative name's parent as `""`, whose
/// `canonicalize()` fails -- so the raw relative string gets hashed instead
/// of the resolved path, and `worktree pin <bare-name>` (the everyday
/// invocation, and what shell tab-completion produces as `<bare-name>/`)
/// missed the provenance artifact entirely. Resolving the whole path here
/// with a real `canonicalize()` sidesteps that: every component, including
/// the last, is followed, so the result agrees with whatever
/// `canonicalize_best_effort` would derive from it (re-canonicalizing an
/// already-canonical parent is a no-op).
fn run_worktree_pin_cli(path: std::path::PathBuf) -> ExitCode {
    use dot_agent_deck::issue_dispatch_run::pin_isolated_clone;
    use dot_agent_deck::terminal_sanitize::{
        sanitize_for_terminal_display, sanitize_path_for_terminal_display,
    };

    let path = match path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "worktree pin: {} does not resolve to an existing path: {}",
                sanitize_path_for_terminal_display(&path),
                sanitize_for_terminal_display(&e.to_string())
            );
            return ExitCode::FAILURE;
        }
    };

    match pin_isolated_clone(&path) {
        Ok(()) => ExitCode::SUCCESS,
        // Review-findings fix round (F2): the only realistic failure past
        // the resolution above is "this is not an isolated clone this deck
        // provisioned", which surfaces as a bare `read_to_string` NotFound
        // naming nothing the caller can act on. Say so explicitly instead
        // of echoing the raw IO error.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "worktree pin: {} is not an isolated clone provisioned by this deck (no pin/\
                 provenance record exists for it) -- `worktree pin` applies only to isolated \
                 clones, not to linked worktrees",
                sanitize_path_for_terminal_display(&path)
            );
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!(
                "worktree pin: failed to pin {}: {}",
                sanitize_path_for_terminal_display(&path),
                sanitize_for_terminal_display(&e.to_string())
            );
            ExitCode::FAILURE
        }
    }
}

/// `dot-agent-deck worktree unpin <path>` (fork issue #597) — clear a pin
/// set by `worktree pin`. Same shape as [`run_worktree_pin_cli`] above,
/// including its F1/F2 handling.
fn run_worktree_unpin_cli(path: std::path::PathBuf) -> ExitCode {
    use dot_agent_deck::issue_dispatch_run::unpin_isolated_clone;
    use dot_agent_deck::terminal_sanitize::{
        sanitize_for_terminal_display, sanitize_path_for_terminal_display,
    };

    let path = match path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "worktree unpin: {} does not resolve to an existing path: {}",
                sanitize_path_for_terminal_display(&path),
                sanitize_for_terminal_display(&e.to_string())
            );
            return ExitCode::FAILURE;
        }
    };

    match unpin_isolated_clone(&path) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "worktree unpin: {} is not an isolated clone provisioned by this deck (no pin/\
                 provenance record exists for it) -- `worktree unpin` applies only to isolated \
                 clones, not to linked worktrees",
                sanitize_path_for_terminal_display(&path)
            );
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!(
                "worktree unpin: failed to unpin {}: {}",
                sanitize_path_for_terminal_display(&path),
                sanitize_for_terminal_display(&e.to_string())
            );
            ExitCode::FAILURE
        }
    }
}

/// `dot-agent-deck issue claim <n> [--repo owner/name] [--takeover]
/// [--confirm-stopped]` — PRD fork#235 M3. Pure CLI-subprocess operation
/// over `git`/`gh` in the current directory — no daemon involved (rule 12),
/// synchronous like the `worktree` verbs above. The exit code is the
/// mechanism: a refusal and an operational failure both map to
/// `ExitCode::FAILURE` here (distinguished only by the printed message) —
/// `dot_agent_deck::issue_claim::run_issue_claim` is where the actual
/// decision lives.
fn run_issue_claim_cli(
    issue: u64,
    repo: Option<String>,
    takeover: bool,
    confirm_stopped: bool,
) -> ExitCode {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("issue claim: failed to resolve current directory: {e}");
            return ExitCode::FAILURE;
        }
    };
    match dot_agent_deck::issue_claim::run_issue_claim(
        &cwd,
        repo.as_deref(),
        issue,
        takeover,
        confirm_stopped,
    ) {
        Ok(message) => {
            print!("{message}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("issue claim: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Same shape as [`run_issue_claim_cli`] — a refusal and an operational
/// failure both map to `ExitCode::FAILURE` here (distinguished only by the
/// printed message); `dot_agent_deck::issue_claim::run_issue_release` is
/// where the actual decision lives.
fn run_issue_release_cli(
    issue: u64,
    repo: Option<String>,
    force: bool,
    confirm_stopped: bool,
    reason: Option<String>,
) -> ExitCode {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("issue release: failed to resolve current directory: {e}");
            return ExitCode::FAILURE;
        }
    };
    match dot_agent_deck::issue_claim::run_issue_release(
        &cwd,
        repo.as_deref(),
        issue,
        force,
        confirm_stopped,
        reason.as_deref(),
    ) {
        Ok(message) => {
            print!("{message}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("issue release: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The exit code IS the mechanism here, more than usually: PR #573's
/// fix-round hook (`.claude/hooks/check-issue-claim.sh`) reads exactly these
/// four codes to tell a confident lock refusal apart from a merely
/// ambiguous state and from an operational failure it cannot answer at all
/// — see `dot_agent_deck::issue_claim::ClaimCheckOutcome`'s doc table for
/// the full mapping. Do not renumber these without updating that hook.
///
/// Code 2 is deliberately SKIPPED (round-2 fix, reviewer B5 / auditor R3):
/// it is clap's own reserved usage-error code, so any `worker-agent-deck`
/// binary predating this subcommand answers `claim-check` with exit 2 from
/// a `clap` usage error, not from this function at all — colliding with
/// whatever tier claimed 2 and fabricating a claim-state reason that was
/// never actually determined. `Clear=0, RefusedByLock=1, (2 reserved by
/// clap, never assigned here), CouldNotDetermine=3, Ambiguous=4`.
fn run_issue_claim_check_cli(issue: u64, repo: Option<String>) -> ExitCode {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("issue claim-check: failed to resolve current directory: {e}");
            return ExitCode::from(3);
        }
    };
    use dot_agent_deck::issue_claim::ClaimCheckOutcome;
    match dot_agent_deck::issue_claim::run_issue_claim_check(&cwd, repo.as_deref(), issue) {
        ClaimCheckOutcome::Clear(message) => {
            print!("{message}");
            ExitCode::SUCCESS
        }
        ClaimCheckOutcome::RefusedByLock(message) => {
            eprintln!("issue claim-check: {message}");
            ExitCode::from(1)
        }
        ClaimCheckOutcome::CouldNotDetermine(message) => {
            eprintln!("issue claim-check: {message}");
            ExitCode::from(3)
        }
        ClaimCheckOutcome::Ambiguous(message) => {
            eprintln!("issue claim-check: {message}");
            ExitCode::from(4)
        }
    }
}

/// `dot-agent-deck daemon stop [--force]` — PRD #103 Phase 3 (M3.2).
/// Documented, non-`kill -9` way to recycle the local daemon after a
/// binary upgrade. Idempotent (no-op exit 0 when no daemon is running)
/// and safe-by-default (refuses when managed agents are alive unless
/// `--force` is passed). The recovery flow is in
/// [`dot_agent_deck::daemon_stop::run_daemon_stop`]; this function
/// only translates outcomes into stdout/stderr text and exit codes.
#[tokio::main]
async fn run_daemon_stop_cli(force: bool) -> ExitCode {
    let attach_path = attach_socket_path();
    match dot_agent_deck::daemon_stop::run_daemon_stop(&attach_path, force).await {
        Ok(dot_agent_deck::daemon_stop::StopOutcome::NoDaemonRunning) => {
            println!("no daemon running");
            ExitCode::SUCCESS
        }
        Ok(dot_agent_deck::daemon_stop::StopOutcome::Stopped { pid }) => {
            println!("daemon stopped (pid {pid})");
            ExitCode::SUCCESS
        }
        Ok(dot_agent_deck::daemon_stop::StopOutcome::ForceKilled { pid }) => {
            println!("daemon force-killed via SIGKILL (pid {pid})");
            ExitCode::SUCCESS
        }
        Err(dot_agent_deck::daemon_stop::StopError::LiveAgents { ids }) => {
            eprint!(
                "{}",
                dot_agent_deck::daemon_stop::format_live_agents_refusal(&ids)
            );
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("daemon stop: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `dot-agent-deck daemon restart [--force]` — PRD #103 Phase 3 (M3.3).
/// Thin wrapper over `daemon stop`: the next TUI invocation lazy-spawns
/// a fresh daemon per PRD #93. Shares the same `--force` semantics as
/// `daemon stop`.
#[tokio::main]
async fn run_daemon_restart_cli(force: bool) -> ExitCode {
    let attach_path = attach_socket_path();
    match dot_agent_deck::daemon_stop::run_daemon_restart(&attach_path, force).await {
        Ok(dot_agent_deck::daemon_stop::StopOutcome::NoDaemonRunning) => {
            println!("no daemon running; next invocation will spawn one");
            ExitCode::SUCCESS
        }
        Ok(dot_agent_deck::daemon_stop::StopOutcome::Stopped { pid }) => {
            println!("daemon stopped (pid {pid}); next invocation will spawn a fresh daemon");
            ExitCode::SUCCESS
        }
        Ok(dot_agent_deck::daemon_stop::StopOutcome::ForceKilled { pid }) => {
            println!(
                "daemon force-killed via SIGKILL (pid {pid}); next invocation will spawn a fresh daemon"
            );
            ExitCode::SUCCESS
        }
        Err(dot_agent_deck::daemon_stop::StopError::LiveAgents { ids }) => {
            eprint!(
                "{}",
                dot_agent_deck::daemon_stop::format_live_agents_refusal(&ids)
            );
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("daemon restart: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `dot-agent-deck daemon serve` — PRD #76 M4.3. Runs the daemon (hook
/// ingestion + streaming-attach servers) in the foreground without a TUI.
/// The body mirrors the in-process spawn used by `run_tui_session`
/// (Daemon::with_attach + run_daemon_with) so a remote running this
/// subcommand binds the same two sockets a local TUI would.
///
/// Hook auto-install is skipped here on purpose: `remote add` already runs
/// `hooks install` on the remote, and the on-disk hook scripts only need
/// to be (re)installed when the binary version changes — not every time
/// the daemon starts.
#[tokio::main]
async fn run_daemon_serve_cli() -> ExitCode {
    // NOTE: logging is initialized by the `DaemonCmd::Serve` dispatch arm in
    // `main`, before the login-shell PATH capture and before this runtime is
    // built — so it is intentionally NOT initialized again here (a second
    // `tracing` global-default init would panic).
    // PRD #139 M1.2/M2.1: the daemon reads the experimental flag from the same
    // `.dot-agent-deck.toml` source of truth and watches it independently of
    // the TUI (the file is the contract; no cross-process sync). The detached
    // spawn in `platform::detach` sets no `current_dir`, so the daemon
    // inherits the launching TUI's directory and the two agree on the file by
    // construction.
    //
    // The `Vec<String>` diagnosability warnings (fork issues #303 and #309)
    // are deliberately discarded here, unlike the TUI's `run_tui_session`
    // call: a detached daemon has no terminal — `platform::detach::unix`
    // sends both its stdout and stderr to `<state_dir>/daemon.log` — so
    // there is nowhere useful to `eprintln!` them, and the paired
    // `tracing::warn!` calls inside `init_and_watch` already land in that
    // same log file.
    dot_agent_deck::features::init_and_watch(&launch_project_dir());
    let state = Arc::new(RwLock::new(AppState::default()));
    let path = socket_path();
    let attach_path = attach_socket_path();

    let daemon = Daemon::with_attach(state, attach_path.clone());
    if let Err(e) = run_daemon_with(&path, daemon).await {
        eprintln!("Daemon error: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// `dot-agent-deck schedule <subcommand>` — PRD #127 M1.5. The single
/// validated writer for the global `schedules.toml`. Mutating subcommands
/// (add/update/remove/enable/disable) load the current file, apply the change
/// through the `schedule_cli` helpers (cron validation + `~`/`$VAR` expansion +
/// rename guard), write the global path atomically regardless of cwd, then
/// trigger a live daemon reload (a daemon that isn't running is fine — the
/// change loads on next `daemon serve`). `run-now` and `reload` send control
/// messages to the daemon; `list` prints the current file.
#[tokio::main]
async fn run_schedule_cli(action: ScheduleAction) -> ExitCode {
    use dot_agent_deck::config::{LoadedSchedules, schedules_path};
    use dot_agent_deck::schedule_cli;

    // Subcommands that purely talk to the daemon (no file write).
    match &action {
        ScheduleAction::RunNow { name } => {
            use dot_agent_deck::daemon_client::RunNowOutcome;
            let client = DaemonClient::new(attach_socket_path());
            return match client.run_now(name).await {
                // PRD #127 C5: report skipped distinctly (still exit 0 — the
                // task is registered and the request succeeded).
                Ok(RunNowOutcome::Started) => {
                    println!("ran {name}");
                    ExitCode::SUCCESS
                }
                Ok(RunNowOutcome::SkippedStillRunning) => {
                    println!("skipped {name}: previous run still active");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("run-now failed: {e}");
                    ExitCode::FAILURE
                }
            };
        }
        ScheduleAction::Reload => {
            let client = DaemonClient::new(attach_socket_path());
            return match client.reload_schedules().await {
                Ok(names) => {
                    println!("reloaded; registered: {}", names.join(", "));
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("reload failed: {e}");
                    ExitCode::FAILURE
                }
            };
        }
        ScheduleAction::List => {
            let loaded = LoadedSchedules::load();
            for err in &loaded.errors {
                eprintln!("warning: skipped malformed entry: {}", err.message);
            }
            print!("{}", schedule_cli::format_list(&loaded.tasks));
            return ExitCode::SUCCESS;
        }
        _ => {}
    }

    // Mutating subcommands: load → apply → atomic write → reload trigger.
    let loaded = LoadedSchedules::load();
    for err in &loaded.errors {
        eprintln!(
            "warning: skipped malformed entry while loading: {}",
            err.message
        );
    }
    let mut tasks = loaded.tasks;

    let apply_result = match action {
        ScheduleAction::Add {
            name,
            cron,
            working_dir,
            command,
            prompt,
            new_tab_per_fire,
            enabled,
            repo,
            max_per_run,
            label,
            query,
            triage,
        } => {
            // PRD #120: `--repo` turns this into an issue-dispatch `add`. Build
            // the sub-table here (defaulting `max_per_run` to the documented 3
            // when omitted); `schedule_cli::add` validates the slug + relaxes the
            // `--command` requirement.
            use dot_agent_deck::config::{IssueDispatchConfig, default_max_per_run};
            let issue_dispatch = repo.map(|repo| IssueDispatchConfig {
                repo,
                max_per_run: max_per_run.unwrap_or_else(default_max_per_run),
                label,
                query,
                triage,
            });
            schedule_cli::add(
                &mut tasks,
                schedule_cli::AddArgs {
                    name,
                    cron,
                    working_dir,
                    command,
                    prompt,
                    new_tab_per_fire,
                    enabled,
                    issue_dispatch,
                },
            )
        }
        ScheduleAction::Update {
            name,
            cron,
            working_dir,
            command,
            prompt,
            new_tab_per_fire,
            enabled,
        } => schedule_cli::update(
            &mut tasks,
            schedule_cli::UpdateArgs {
                name,
                cron,
                working_dir,
                command,
                prompt,
                new_tab_per_fire,
                enabled,
            },
        ),
        ScheduleAction::Remove { name } => schedule_cli::remove(&mut tasks, &name),
        ScheduleAction::Enable { name } => schedule_cli::set_enabled(&mut tasks, &name, true),
        ScheduleAction::Disable { name } => schedule_cli::set_enabled(&mut tasks, &name, false),
        // RunNow/Reload/List handled above.
        ScheduleAction::RunNow { .. } | ScheduleAction::Reload | ScheduleAction::List => {
            unreachable!("daemon-only / read-only subcommands handled above")
        }
    };

    if let Err(e) = apply_result {
        eprintln!("{e}");
        return ExitCode::FAILURE;
    }

    let path = schedules_path();
    if let Err(e) = schedule_cli::write_atomic(&path, &tasks) {
        eprintln!("{e}");
        return ExitCode::FAILURE;
    }

    // Trigger a live reload so a running daemon picks the change up. A daemon
    // that isn't running is not an error — the change loads on next serve.
    let client = DaemonClient::new(attach_socket_path());
    match client.reload_schedules().await {
        Ok(_) => {}
        Err(e) => {
            eprintln!(
                "note: wrote {} but could not reload the daemon ({e}); it will load on next `daemon serve`",
                path.display()
            );
        }
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    // --- PRD #220: the dispatch shape selector's parsing ---

    fn parse_dispatch(
        args: &[&str],
    ) -> (Option<String>, Option<String>, bool, Option<String>, bool) {
        let mut argv = vec!["dot-agent-deck", "dispatch"];
        argv.extend_from_slice(args);
        match Cli::try_parse_from(argv)
            .expect("dispatch args should parse")
            .command
            .expect("a subcommand")
        {
            Commands::Dispatch {
                name,
                task,
                single,
                orchestration,
                list_targets,
                ..
            } => (name, task, single, orchestration, list_targets),
            // `Commands` deliberately derives no `Debug`, so this cannot print the
            // variant it got — the arm is unreachable anyway, since the argv above
            // always names `dispatch`.
            _ => panic!("expected the Dispatch subcommand"),
        }
    }

    /// `--orchestration` REQUIRES its value, so it can never consume the unit name.
    ///
    /// With `num_args = 0..=1` clap consumed the next bare token, so
    /// `dispatch --orchestration my-unit --task "…"` bound the UNIT NAME as the
    /// orchestration and aborted for a missing positional. A required value makes
    /// both orderings unambiguous.
    #[test]
    fn orchestration_value_is_required_so_it_cannot_eat_the_unit_name() {
        // Flag-first with a name still binds correctly: `probe` is the VALUE, and
        // the missing positional is a real error rather than a silent mis-bind.
        assert!(
            Cli::try_parse_from([
                "dot-agent-deck",
                "dispatch",
                "--orchestration",
                "probe",
                "--task",
                "t",
            ])
            .is_err(),
            "no positional NAME was supplied, so this must be rejected outright"
        );

        // A bare `--orchestration` with nothing after it is now an error, not a
        // silent \"this repo\'s first\".
        assert!(
            Cli::try_parse_from(["dot-agent-deck", "dispatch", "unit", "--orchestration"]).is_err(),
            "--orchestration now requires a value"
        );

        // The explicit empty value is how \"this repo\'s first\" is requested.
        let (name, _, _, orch, _) = parse_dispatch(&["unit", "--orchestration="]);
        assert_eq!(name.as_deref(), Some("unit"));
        assert_eq!(orch.as_deref(), Some(""));

        // And --task is never swallowed.
        let (name, task, _, orch, _) =
            parse_dispatch(&["unit", "--orchestration=review", "--task", "hello"]);
        assert_eq!(name.as_deref(), Some("unit"));
        assert_eq!(task.as_deref(), Some("hello"));
        assert_eq!(orch.as_deref(), Some("review"));
    }

    /// `--list-targets` cannot be combined with dispatch arguments. Combined, the
    /// early branch printed the listing and exited 0 WITHOUT dispatching, so an
    /// agent that merged the seed\'s two usage lines reported a unit as started
    /// that never existed.
    #[test]
    fn list_targets_conflicts_with_every_dispatch_argument() {
        for extra in [
            vec!["unit"],
            vec!["unit", "--task", "t"],
            vec!["--single"],
            vec!["--orchestration=review"],
        ] {
            let mut argv = vec!["dot-agent-deck", "dispatch", "--list-targets"];
            argv.extend(extra.iter().copied());
            assert!(
                Cli::try_parse_from(argv.clone()).is_err(),
                "--list-targets must conflict with {extra:?}"
            );
        }
        // Alone, it parses and needs no name.
        let (name, _, _, _, list) = parse_dispatch(&["--list-targets"]);
        assert!(name.is_none() && list);
    }

    #[test]
    fn dispatch_named_orchestration_and_single_parse_as_expected() {
        let (_, _, single, orch, _) = parse_dispatch(&["unit", "--orchestration=review"]);
        assert!(!single);
        assert_eq!(orch.as_deref(), Some("review"));

        let (_, _, single, orch, _) = parse_dispatch(&["unit", "--single", "--task", "t"]);
        assert!(single);
        assert_eq!(orch, None);
    }

    /// The two shape flags are mutually exclusive, so a caller can never express
    /// an ambiguous choice.
    #[test]
    fn dispatch_rejects_single_and_orchestration_together() {
        assert!(
            Cli::try_parse_from([
                "dot-agent-deck",
                "dispatch",
                "unit",
                "--single",
                "--orchestration=review",
            ])
            .is_err(),
            "--single and --orchestration must conflict"
        );
    }

    /// `--list-targets` is the one form that needs no name; every other form does,
    /// so a missing name can never be read as an empty dispatch name.
    #[test]
    fn dispatch_name_is_required_except_for_list_targets() {
        assert!(
            Cli::try_parse_from(["dot-agent-deck", "dispatch", "--task", "t"]).is_err(),
            "a dispatch with no name and no --list-targets must be rejected"
        );
    }

    // PRD #127 B1 — `schedule add --new-tab-per-fire` must accept an explicit
    // `<true|false>` value (ArgAction::Set), matching `update`, the authoring
    // seed prompt, and the docs. A bare SetTrue flag would reject the value.
    fn parse_add_new_tab(value: &str) -> bool {
        let cli = Cli::try_parse_from([
            "dot-agent-deck",
            "schedule",
            "add",
            "--name",
            "t",
            "--cron",
            "0 9 * * *",
            "--working-dir",
            "/tmp",
            "--prompt",
            "p",
            "--new-tab-per-fire",
            value,
        ])
        .expect("schedule add must accept --new-tab-per-fire <true|false>");
        match cli.command {
            Some(Commands::Schedule {
                action:
                    ScheduleAction::Add {
                        new_tab_per_fire, ..
                    },
            }) => new_tab_per_fire,
            _ => panic!("expected `schedule add`"),
        }
    }

    #[test]
    fn schedule_add_new_tab_per_fire_takes_a_value() {
        assert!(parse_add_new_tab("true"));
        assert!(!parse_add_new_tab("false"));
    }

    #[test]
    fn schedule_add_new_tab_per_fire_defaults_false() {
        let cli = Cli::try_parse_from([
            "dot-agent-deck",
            "schedule",
            "add",
            "--name",
            "t",
            "--cron",
            "0 9 * * *",
            "--working-dir",
            "/tmp",
            "--prompt",
            "p",
        ])
        .expect("parse without --new-tab-per-fire");
        match cli.command {
            Some(Commands::Schedule {
                action:
                    ScheduleAction::Add {
                        new_tab_per_fire, ..
                    },
            }) => assert!(!new_tab_per_fire, "default must be false"),
            _ => panic!("expected `schedule add`"),
        }
    }

    // ---- PRD #201: shell-safe `--task-file` for delegate / work-done --------
    //
    // The task text may contain backticks, quotes, `$VAR`, and newlines. Passed
    // as `--task "…"` those are mangled by the caller's shell *before*
    // dot-agent-deck runs; `--task-file` reads the bytes verbatim off disk (or
    // stdin) so they survive. `resolve_task` is the pure seam under both
    // `delegate` and `work-done`, tested directly here.

    // A payload that exercises every character class the shell would otherwise
    // corrupt: backticks (command substitution), single/double quotes, a
    // `$VAR`, an escaped `\`, and multiple lines.
    const TRICKY_TASK: &str =
        "Fix `compute()` in \"src/lib.rs\" for $USER\nsecond 'line' with $HOME & a \\ backslash\n";

    #[test]
    fn task_file_reads_task_verbatim_from_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("task.txt");
        std::fs::write(&path, TRICKY_TASK).expect("write task file");

        // Empty stdin — the file path branch must not touch it.
        let got = resolve_task(
            None,
            Some(path.to_str().unwrap().to_string()),
            std::io::empty(),
        )
        .expect("resolve_task should read the file");
        assert_eq!(
            got, TRICKY_TASK,
            "a task with backticks/quotes/$VAR/newlines must round-trip VERBATIM via --task-file"
        );
    }

    #[test]
    fn task_file_dash_reads_task_verbatim_from_stdin() {
        let got = resolve_task(None, Some("-".to_string()), TRICKY_TASK.as_bytes())
            .expect("resolve_task should read stdin for `-`");
        assert_eq!(
            got, TRICKY_TASK,
            "`--task-file -` must read the task VERBATIM from stdin"
        );
    }

    #[test]
    fn task_plain_string_passes_through() {
        let got = resolve_task(Some("hello".to_string()), None, std::io::empty())
            .expect("plain --task should pass through");
        assert_eq!(got, "hello");
    }

    #[test]
    fn task_file_missing_errors_clearly() {
        let err = resolve_task(
            None,
            Some("/no/such/task-file.txt".to_string()),
            std::io::empty(),
        )
        .expect_err("a missing --task-file must error");
        assert!(
            err.contains("failed to read task file") && err.contains("/no/such/task-file.txt"),
            "missing-file error should name the path: {err}"
        );
    }

    #[test]
    fn task_and_task_file_both_set_is_rejected() {
        // Defensive guard inside resolve_task (clap also rejects this at parse
        // time — see the parse test below).
        let err = resolve_task(
            Some("x".to_string()),
            Some("y".to_string()),
            std::io::empty(),
        )
        .expect_err("--task + --task-file must conflict");
        assert!(
            err.contains("mutually exclusive"),
            "conflict error should be clear: {err}"
        );
    }

    #[test]
    fn task_neither_set_is_rejected() {
        let err = resolve_task(None, None, std::io::empty())
            .expect_err("neither --task nor --task-file must error");
        assert!(
            err.contains("--task") && err.contains("--task-file"),
            "neither-given error should mention both flags: {err}"
        );
    }

    // `is_work_done_output_path` is the pure seam under the client-side
    // refusal in the `work-done` CLI arm (upstream #331's own proposed fix),
    // tested directly here so the glob shape (parent `.dot-agent-deck/` +
    // `work-done-*.md` filename, not an exact match on today's filename) is
    // pinned independent of the daemon-side digest suffix.

    #[test]
    fn work_done_output_path_matches_own_namespace() {
        assert!(is_work_done_output_path(
            ".dot-agent-deck/work-done-coder.md"
        ));
        assert!(is_work_done_output_path(
            ".dot-agent-deck/work-done-coder-1a2b3c4d.md"
        ));
        assert!(is_work_done_output_path(
            "sub/dir/.dot-agent-deck/work-done-reviewer-deadbeef.md"
        ));
    }

    #[test]
    fn work_done_output_path_rejects_files_outside_the_namespace() {
        // Wrong directory.
        assert!(!is_work_done_output_path("work-done-coder.md"));
        assert!(!is_work_done_output_path("other-dir/work-done-coder.md"));
        // Right directory, wrong filename shape.
        assert!(!is_work_done_output_path(
            ".dot-agent-deck/report-coder-abc123-my-summary.md"
        ));
        assert!(!is_work_done_output_path(
            ".dot-agent-deck/worker-task-coder.md"
        ));
        // stdin sentinel must never be treated as a path.
        assert!(!is_work_done_output_path("-"));
    }

    #[test]
    fn delegate_parses_task_file_and_conflicts_with_task() {
        // --task-file parses into `task_file` with `task` empty.
        let cli = Cli::try_parse_from([
            "dot-agent-deck",
            "delegate",
            "--task-file",
            "/tmp/t.txt",
            "--to",
            "coder",
        ])
        .expect("delegate --task-file should parse");
        match cli.command {
            Some(Commands::Delegate {
                task,
                task_file,
                to,
                subject,
            }) => {
                assert_eq!(task, None);
                assert_eq!(task_file.as_deref(), Some("/tmp/t.txt"));
                assert_eq!(to, vec!["coder".to_string()]);
                assert_eq!(subject, None);
            }
            _ => panic!("expected `delegate`"),
        }

        // Passing both --task and --task-file is rejected at parse time.
        // (`Cli` isn't `Debug`, so match rather than `expect_err`.)
        let err = match Cli::try_parse_from([
            "dot-agent-deck",
            "delegate",
            "--task",
            "x",
            "--task-file",
            "/tmp/t.txt",
            "--to",
            "coder",
        ]) {
            Ok(_) => panic!("--task + --task-file must conflict at parse time"),
            Err(e) => e,
        };
        assert_eq!(
            err.kind(),
            clap::error::ErrorKind::ArgumentConflict,
            "expected a clap ArgumentConflict, got: {err}"
        );
    }

    #[test]
    fn work_done_parses_task_file_and_conflicts_with_task() {
        let cli = Cli::try_parse_from(["dot-agent-deck", "work-done", "--task-file", "-"])
            .expect("work-done --task-file - should parse");
        match cli.command {
            Some(Commands::WorkDone {
                task,
                task_file,
                done,
                subject,
            }) => {
                assert_eq!(task, None);
                assert_eq!(task_file.as_deref(), Some("-"));
                assert!(!done);
                assert_eq!(subject, None);
            }
            _ => panic!("expected `work-done`"),
        }

        let err = match Cli::try_parse_from([
            "dot-agent-deck",
            "work-done",
            "--task",
            "x",
            "--task-file",
            "y",
        ]) {
            Ok(_) => panic!("--task + --task-file must conflict at parse time"),
            Err(e) => e,
        };
        assert_eq!(
            err.kind(),
            clap::error::ErrorKind::ArgumentConflict,
            "expected a clap ArgumentConflict, got: {err}"
        );
    }

    // ---- PR #466 review: what `delegate` reports for a daemon reply --------
    //
    // The e2e assertions that cover this (`orchestration/dispatch/001`) live
    // behind `#![cfg(feature = "e2e")]`, and no CI build job passes
    // `--features e2e`, so they compile to nothing where it counts. These pin
    // the same contract in the tier that gates a merge.

    use dot_agent_deck::event::{DELEGATE_RESPONSE_KIND, DelegateResponse};

    fn reply(delivered: &[&str], unresolved: &[&str], error: Option<&str>) -> DelegateResponse {
        DelegateResponse {
            delivered: delivered.iter().map(|s| s.to_string()).collect(),
            unresolved_roles: unresolved.iter().map(|s| s.to_string()).collect(),
            error: error.map(str::to_string),
            ..Default::default()
        }
    }

    /// `&[&str]` to the `Vec<String>` `delegate_verdict`'s `requested_roles`
    /// takes — the caller's raw, pre-dedup `--to` list.
    fn requested(roles: &[&str]) -> Vec<String> {
        roles.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn delegate_verdict_reports_a_full_delivery_silently() {
        let v = delegate_verdict(
            "pane-1",
            &requested(&["coder", "tester"]),
            &reply(&["coder", "tester"], &[], None),
        );
        assert!(!v.failed, "every named role resolved — this is a success");
        assert_eq!(v.message, None, "a clean delegate prints nothing");
        assert_eq!(
            v.info, None,
            "no role was requested more than once, so there is nothing to flag"
        );
    }

    #[test]
    fn delegate_verdict_fails_a_routing_error() {
        let v = delegate_verdict(
            "xcaller",
            &requested(&[]),
            &reply(&[], &[], Some("no orchestration role")),
        );
        assert!(v.failed, "a routing error means nothing was dispatched");
        let msg = v.message.expect("a routing error must be reported");
        assert!(
            msg.contains("xcaller") && msg.contains("no orchestration role"),
            "the message must name the pane and the daemon's reason: {msg}"
        );
    }

    #[test]
    fn delegate_verdict_fails_when_nothing_landed() {
        let v = delegate_verdict(
            "pane-1",
            &requested(&["ghost"]),
            &reply(&[], &["ghost"], None),
        );
        assert!(v.failed, "no role received the task — non-zero is correct");
        let msg = v.message.expect("an unreached delegate must be reported");
        assert!(
            msg.contains("ghost"),
            "the message must name the role that missed: {msg}"
        );
        // The three causes, not the one that happens to be most common: the
        // old message told the user to go check role names in the toml even
        // when the role was sitting there correctly and was simply the
        // orchestrator itself, or had had its worker pane closed.
        assert!(
            msg.contains(".dot-agent-deck.toml")
                && msg.contains("orchestrator cannot delegate to itself")
                && msg.contains("worker pane has been closed"),
            "the message must state all three causes, not assert one: {msg}"
        );
    }

    // THE blocker of the PR #466 review. `--to coder --to tester` with only a
    // `coder` pane really does write the task into the coder's PTY and arm its
    // idle-worker record. Reporting that as a failure invites the orchestrator
    // to retry — under this command's own new contract, non-zero means it did
    // not land — and the coder gets the same task twice, arming two records for
    // one pane.
    #[test]
    fn delegate_verdict_does_not_fail_a_partial_delivery() {
        let v = delegate_verdict(
            "pane-1",
            &requested(&["coder", "tester"]),
            &reply(&["coder"], &["tester"], None),
        );
        assert!(
            !v.failed,
            "a delegate that half landed must NOT exit non-zero: a retry would \
             dispatch `coder` a second time"
        );
        let msg = v
            .message
            .expect("a partial delivery must still be reported");
        assert!(
            msg.contains("tester") && msg.contains("coder"),
            "a partial delivery must name BOTH what missed and what landed, or \
             a retry cannot be aimed safely: {msg}"
        );
    }

    #[test]
    fn parse_delegate_reply_requires_the_delegate_marker() {
        let good = serde_json::to_string(&reply(&["coder"], &[], None)).expect("serialize");
        assert!(
            good.contains(DELEGATE_RESPONSE_KIND),
            "the daemon's own reply must carry the marker: {good}"
        );
        let parsed = parse_delegate_reply(&good).expect("a real delegate reply must parse");
        assert_eq!(parsed.delivered, vec!["coder".to_string()]);

        // Every field is `#[serde(default)]`, so each of these DESERIALIZES
        // fine and yields a pristine "nothing failed" response. Accepting one
        // is how the verb whose purpose is answering "did this land?" answers
        // yes when it cannot tell.
        for line in [
            "{}",
            r#"{"seed":null}"#,
            r#"{"kind":"get-seed"}"#,
            "",
            "not json",
        ] {
            assert!(
                parse_delegate_reply(line).is_none(),
                "a reply that does not identify itself as a delegate response \
                 must be treated as unverifiable, not as success: {line}"
            );
        }
    }

    // --- issue #467 F1: `open_deck_log_file`'s symlink refusal + file mode ---

    /// Scenario: pre-create the log path as a symlink to another file, call
    /// `open_deck_log_file` directly against it, and confirm it returns an
    /// error — the `O_NOFOLLOW`/`ELOOP` refusal — instead of silently
    /// opening and writing through the symlink.
    #[test]
    #[cfg(unix)]
    fn open_deck_log_file_refuses_a_preexisting_symlink() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("elsewhere.log");
        std::fs::write(&target, b"not the deck log\n").expect("write symlink target");
        let log_path = dir.path().join("deck.log");
        std::os::unix::fs::symlink(&target, &log_path).expect("create symlink");

        let err = open_deck_log_file(&log_path)
            .expect_err("a pre-planted symlink at the log path must be refused, not followed");
        assert!(
            err.to_string().contains("symlink"),
            "the refusal error should name the symlink as the cause: {err}"
        );

        // Refusing means never opening through the symlink at all, not
        // opening-then-erroring — the target must be untouched.
        let target_contents =
            std::fs::read_to_string(&target).expect("symlink target must still be readable");
        assert_eq!(
            target_contents, "not the deck log\n",
            "the symlink target must not have been truncated or appended to"
        );
    }

    /// Scenario: call `open_deck_log_file` against a fresh, non-symlinked
    /// path and confirm the file it creates is mode `0o600` — the
    /// file-mode half `lifecycle/log-path/001` doesn't cover, since that
    /// e2e test only asserts the parent directory's mode (`0o700`).
    #[test]
    #[cfg(unix)]
    fn open_deck_log_file_creates_a_fresh_file_at_mode_0o600() {
        use std::os::unix::fs::MetadataExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let log_path = dir.path().join("deck.log");

        let file =
            open_deck_log_file(&log_path).expect("a fresh, non-symlinked path must open cleanly");
        drop(file);

        let mode = std::fs::metadata(&log_path)
            .expect("the log file must exist after a successful open")
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "expected the log file to be created owner-only (mode 0o600), got {mode:#o}"
        );
    }

    // --- issue #461: `read_registration_context`'s reader-seam extraction ---
    //
    // `std::env::set_var`/`remove_var` are process-global, and all three
    // tests below mutate the same two vars, so they serialize against this
    // module-level lock. As with `issue_claim.rs`'s `PANE_ID_ENV_LOCK`, real
    // soundness rests on `cargo nextest`'s process-per-test isolation
    // (CLAUDE.md rule 5's actual gate) — this mutex only protects this
    // module's own tests from each other under a plain thread-per-test
    // `cargo test` run.
    static WORK_DONE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Scenario: set both `DOT_AGENT_DECK_REGISTRATION_GENERATION` and
    /// `DOT_AGENT_DECK_DAEMON_BOOT_ID` to real values, call
    /// `read_registration_context`, and confirm it returns exactly what was
    /// set — the parsed generation and the boot id verbatim.
    #[test]
    fn read_registration_context_reads_both_vars_when_present() {
        let _g = WORK_DONE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_gen = std::env::var(DOT_AGENT_DECK_REGISTRATION_GENERATION).ok();
        let prev_boot = std::env::var(DOT_AGENT_DECK_DAEMON_BOOT_ID).ok();

        // SAFETY: lock held for the duration; restored below.
        unsafe {
            std::env::set_var(DOT_AGENT_DECK_REGISTRATION_GENERATION, "3");
            std::env::set_var(DOT_AGENT_DECK_DAEMON_BOOT_ID, "boot-abc123");
        }

        assert_eq!(read_registration_context(), (3, "boot-abc123".to_string()));

        // SAFETY: same lock; restore.
        unsafe {
            match prev_gen {
                Some(v) => std::env::set_var(DOT_AGENT_DECK_REGISTRATION_GENERATION, v),
                None => std::env::remove_var(DOT_AGENT_DECK_REGISTRATION_GENERATION),
            }
            match prev_boot {
                Some(v) => std::env::set_var(DOT_AGENT_DECK_DAEMON_BOOT_ID, v),
                None => std::env::remove_var(DOT_AGENT_DECK_DAEMON_BOOT_ID),
            }
        }
    }

    /// Scenario: with both env vars unset, call `read_registration_context`
    /// and confirm it falls back to `(0, String::new())` — the defaults an
    /// old CLI predating these variables, or a caller that didn't go through
    /// a dot-agent-deck-managed spawn, must degrade to so the report is
    /// refused at delivery rather than silently accepted.
    #[test]
    fn read_registration_context_defaults_when_vars_are_unset() {
        let _g = WORK_DONE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_gen = std::env::var(DOT_AGENT_DECK_REGISTRATION_GENERATION).ok();
        let prev_boot = std::env::var(DOT_AGENT_DECK_DAEMON_BOOT_ID).ok();

        // SAFETY: lock held for the duration; restored below.
        unsafe {
            std::env::remove_var(DOT_AGENT_DECK_REGISTRATION_GENERATION);
            std::env::remove_var(DOT_AGENT_DECK_DAEMON_BOOT_ID);
        }

        assert_eq!(read_registration_context(), (0, String::new()));

        // SAFETY: same lock; restore.
        unsafe {
            match prev_gen {
                Some(v) => std::env::set_var(DOT_AGENT_DECK_REGISTRATION_GENERATION, v),
                None => std::env::remove_var(DOT_AGENT_DECK_REGISTRATION_GENERATION),
            }
            match prev_boot {
                Some(v) => std::env::set_var(DOT_AGENT_DECK_DAEMON_BOOT_ID, v),
                None => std::env::remove_var(DOT_AGENT_DECK_DAEMON_BOOT_ID),
            }
        }
    }

    /// Scenario: set `DOT_AGENT_DECK_REGISTRATION_GENERATION` to a
    /// non-numeric value with the boot id unset, and confirm
    /// `read_registration_context` silently falls back to `0` rather than
    /// panicking — matching `.and_then(|v| v.parse().ok())`'s existing
    /// silent-fallback behavior for a malformed value.
    #[test]
    fn read_registration_context_falls_back_to_zero_on_unparseable_generation() {
        let _g = WORK_DONE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_gen = std::env::var(DOT_AGENT_DECK_REGISTRATION_GENERATION).ok();
        let prev_boot = std::env::var(DOT_AGENT_DECK_DAEMON_BOOT_ID).ok();

        // SAFETY: lock held for the duration; restored below.
        unsafe {
            std::env::set_var(DOT_AGENT_DECK_REGISTRATION_GENERATION, "not-a-number");
            std::env::remove_var(DOT_AGENT_DECK_DAEMON_BOOT_ID);
        }

        assert_eq!(read_registration_context(), (0, String::new()));

        // SAFETY: same lock; restore.
        unsafe {
            match prev_gen {
                Some(v) => std::env::set_var(DOT_AGENT_DECK_REGISTRATION_GENERATION, v),
                None => std::env::remove_var(DOT_AGENT_DECK_REGISTRATION_GENERATION),
            }
            match prev_boot {
                Some(v) => std::env::set_var(DOT_AGENT_DECK_DAEMON_BOOT_ID, v),
                None => std::env::remove_var(DOT_AGENT_DECK_DAEMON_BOOT_ID),
            }
        }
    }
}
