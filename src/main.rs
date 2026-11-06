use std::io::Write as _;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use tokio::sync::RwLock;

use dot_agent_deck::agent_pty::{DOT_AGENT_DECK_AGENT_ID, DOT_AGENT_DECK_PANE_ID};
use dot_agent_deck::bounded_read::read_task_input;
use dot_agent_deck::build_version_handshake;
use dot_agent_deck::config::{DashboardConfig, state_dir};
use dot_agent_deck::daemon::{Daemon, run_daemon_with};
use dot_agent_deck::daemon_attach::ensure_external_daemon_or_die;
use dot_agent_deck::daemon_client::{DaemonClient, LocalEndpoint};
use dot_agent_deck::embedded_pane::EmbeddedPaneController;
use dot_agent_deck::endpoint_resolve::client_attach_socket_path;
use dot_agent_deck::hook::handle_hook;
use dot_agent_deck::pane::PaneController;
use dot_agent_deck::reconnect::HydrationGate;
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
        /// mangle. PATH must be a regular file, not a FIFO or a device; pass
        /// `-` to read a pipe. At most 1 MiB, from a file or from stdin.
        /// Mutually exclusive with --task.
        #[arg(long = "task-file", value_name = "PATH")]
        task_file: Option<String>,
        /// Role name(s) to delegate to (repeatable)
        #[arg(long)]
        to: Vec<String>,
        /// Dispatch even to a worker that still owes a work-done for an
        /// earlier delegation. Without it the daemon refuses that worker and
        /// says how many delegations it still owes. Use it when the earlier
        /// task is abandoned and this one replaces it.
        #[arg(long)]
        supersede: bool,
    },
    /// Create a git worktree and start an isolated line of work inside it.
    /// Agent-callable, one step (PRD #220).
    ///
    /// Exit status when starting a unit: 0 means the daemon admitted the
    /// request, or gave no answer this build can check (an older daemon, or
    /// none within 5 seconds). It does not mean the worktree was created, that
    /// the unit started, or that its task reached the agent: the daemon answers
    /// before it does any of that work. Non-zero means the request did not get
    /// that far: no daemon was reachable, the daemon refused it (the reason is
    /// printed), or the command line itself was unusable.
    ///
    /// What happened arrives afterwards as messages in this agent's inbox: first the
    /// daemon's reply to the dispatch, beginning `dispatch:`. A reply beginning
    /// `dispatch: spawned isolated` names what was started and where; a reply
    /// with any other opening is a failure and says why. A spawned unit has
    /// still not been confirmed to have received its task. The report it sends
    /// later, when it finishes, beginning `dispatch: a unit you dispatched has
    /// completed`, is the first message that indicates it
    /// did — an indication, not proof, since the deck does not tie that report
    /// to the task's delivery.
    ///
    /// With --list-targets the exit status means something else: 0 means the
    /// listing was printed, and non-zero means no listing could be trusted —
    /// the daemon did not answer, or could not read this repo's config or this
    /// pane's directory. The reason is printed.
    Dispatch {
        /// Short name for the dispatch unit (used for worktree naming).
        /// Omit it only with --list-targets.
        #[arg(required_unless_present = "list_targets")]
        name: Option<String>,
        /// Task description with context, file paths, and constraints.
        /// Mutually exclusive with --task-file.
        #[arg(long, conflicts_with = "task_file")]
        task: Option<String>,
        /// Read the task text verbatim from a file (or `-` for stdin). PATH
        /// must be a regular file, not a FIFO or a device; pass `-` to read a
        /// pipe. At most 1 MiB, from a file or from stdin. Mutually exclusive
        /// with --task.
        #[arg(long = "task-file", value_name = "PATH")]
        task_file: Option<String>,
        /// Start ONE agent, even where this repo defines `[[orchestrations]]`.
        /// Mutually exclusive with --orchestration.
        #[arg(long, conflicts_with = "orchestration")]
        single: bool,
        /// Start a full orchestration by name (`--orchestration review`), or this
        /// repo's DEFAULT one (`--orchestration=` with an empty value) — the block
        /// carrying `default = true`, else the first with roles.
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
        /// `$VAR`, or newlines. PATH must be a regular file, not a FIFO or a
        /// device; pass `-` to read a pipe. At most 1 MiB, from a file or from
        /// stdin. Mutually exclusive with --task.
        #[arg(long = "task-file", value_name = "PATH")]
        task_file: Option<String>,
        /// Signal that the entire orchestration is complete (orchestrator only)
        #[arg(long, conflicts_with = "task_id")]
        done: bool,
        /// The id of the delegated task this report completes, as your task
        /// names it. Without it, the report is a note to your orchestrator
        /// that completes no task.
        #[arg(long = "task-id", value_name = "ID")]
        task_id: Option<String>,
    },
    /// Close dispatched units, one agent, or one orchestration.
    ///
    /// Name units by the name they were dispatched under, or by the unit id a
    /// listing printed with --unit-id, or close one pane with --pane, or every
    /// role of an orchestration with --orchestration-of. --all lists every
    /// unit you may close and closes nothing; --all --yes lists them, then
    /// closes exactly those.
    ///
    /// Refused unless --force: a dispatched unit that has not reported
    /// `work-done --done`, an agent that is mid-turn, and one role of a live
    /// orchestration closed on its own. Run from an agent's pane, `close` acts
    /// only on units that agent dispatched, never on its own pane or
    /// orchestration, and --force does not change that.
    ///
    /// Exit status: 0 everything named was closed (or listed); 1 something was
    /// refused or failed; 2 the running daemon is too old for `close`; 3 no
    /// daemon is running.
    Close {
        /// Dispatched units to close, by the name given to `dispatch`.
        #[arg(conflicts_with_all = ["unit_ids", "pane", "orchestration_of", "all"])]
        units: Vec<String>,
        /// Dispatched units to close, by the unit id `close --all` (or an
        /// `ambiguous` refusal) printed. Repeat it, or give several ids.
        #[arg(
            long = "unit-id",
            value_name = "ID",
            num_args = 1..,
            conflicts_with_all = ["pane", "orchestration_of", "all"]
        )]
        unit_ids: Vec<String>,
        /// Close the one agent in this pane.
        #[arg(long, value_name = "PANE_ID", conflicts_with_all = ["orchestration_of", "all"])]
        pane: Option<String>,
        /// Close every role of the orchestration this pane belongs to.
        #[arg(
            long = "orchestration-of",
            value_name = "PANE_ID",
            conflicts_with = "all"
        )]
        orchestration_of: Option<String>,
        /// List every unit you may close. Closes nothing unless --yes.
        #[arg(long)]
        all: bool,
        /// With --all: close the units it listed.
        #[arg(long, requires = "all")]
        yes: bool,
        /// Show what would be closed, and what would be refused, without
        /// closing anything.
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// Close even a unit that has not reported, an agent that is mid-turn,
        /// or one role of a live orchestration.
        #[arg(long)]
        force: bool,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// List the live agents with their folder, orchestration and role, and
    /// which one is each orchestration's orchestrator — the agent to message
    /// with `inbox send`.
    ///
    /// --folder, --orchestration, --name and --pane select what `inbox send`
    /// would address with the same flags (all the agents of each matching
    /// orchestration) when run on the same machine as the deck, which is how
    /// agents in deck panes run it, and combine as an intersection.
    Roles {
        /// Print a JSON array instead of the table.
        #[arg(long)]
        json: bool,
        /// Only the orchestrations in this folder or, when none is, its
        /// nearest parent folder that has one.
        #[arg(long, value_name = "PATH")]
        folder: Option<String>,
        /// Only the orchestrations with this config name or title.
        #[arg(long, value_name = "NAME", conflicts_with = "name")]
        orchestration: Option<String>,
        /// Only the standalone agents with this name (the NAME column).
        #[arg(long, value_name = "NAME", allow_hyphen_values = true)]
        name: Option<String>,
        /// Only the standalone agent or orchestrator in this pane (the PANE
        /// column).
        #[arg(long, value_name = "PANE", allow_hyphen_values = true)]
        pane: Option<String>,
    },
    /// Acknowledge a message the deck delivered to you (`m-…`). Exits non-zero
    /// only when the deck says it holds no such message for you; any other
    /// failure to acknowledge is harmless, and the task goes on.
    Ack {
        /// The message id (`m-…`).
        delivery_id: String,
        /// Print the deck's answer as one JSON line, for the agent-side
        /// components that confirm their acknowledgements.
        #[arg(long, hide = true)]
        json: bool,
    },
    /// Read and manage this agent's dot-agent-deck inbox.
    Inbox {
        #[command(subcommand)]
        action: InboxAction,
    },
    /// Report an agent lifecycle state, a submitted prompt or a tool call so
    /// the pane's card updates (PRD #201 M1.2, issue #622). Used by an agent's
    /// extension (e.g. the bundled Pi extension) to drive its card with NO
    /// hook installed: it rides the existing raw-`AgentEvent` socket path.
    AgentEvent {
        /// One of `running`, `waiting`, `finished` (lifecycle), `prompt`,
        /// `tool-start`, `tool-end` (card detail).
        #[arg(long = "type")]
        r#type: String,
        /// The agent's working directory, shown as the card's `Dir:`.
        #[arg(long, allow_hyphen_values = true)]
        cwd: Option<String>,
        /// The prompt being submitted (with `--type prompt`).
        #[arg(long, allow_hyphen_values = true)]
        prompt: Option<String>,
        /// The tool starting or finishing (with `--type tool-start|tool-end`).
        #[arg(long = "tool-name", allow_hyphen_values = true)]
        tool_name: Option<String>,
        /// A short description of the tool call, e.g. its command or path.
        #[arg(long = "tool-detail", allow_hyphen_values = true)]
        tool_detail: Option<String>,
        /// Accepted and ignored. Pi extensions from before PRD #1666 pass it on
        /// every report to declare that they report every prompt; nothing reads
        /// that any more, so the flag does nothing.
        #[arg(long = "reports-prompts", hide = true)]
        _reports_prompts: bool,
        /// Read the final reply of the turn that just settled (with `--type
        /// finished`) from stdin, for the deck's reading mode (PRD #1497). Sent
        /// by the bundled Pi extension, which writes the reply to stdin so it
        /// never appears on a command line; the daemon passes it only to
        /// clients reading this agent, and never shows or stores it.
        #[arg(long = "turn-reply-stdin")]
        turn_reply_stdin: bool,
        /// The turn whose reply `--turn-reply-stdin` reads ended in an error.
        #[arg(long = "turn-reply-failed")]
        turn_reply_failed: bool,
    },
    /// Report a question an agent is asking and wait for the deck's answer
    /// (PRD #1631). Run by the deck's OpenCode plugin and Pi extension, not by
    /// hand: prints one line — what the producer should answer the agent with —
    /// or nothing, and always exits 0.
    #[command(hide = true)]
    AwaitAnswer {
        /// The producer: `opencode` (the question on stdin) or `pi`.
        #[arg(long)]
        agent: String,
        /// The question, for a producer that cannot write stdin (Pi).
        #[arg(long)]
        question: Option<String>,
        /// After printing the answer, read one line from stdin — whether the
        /// agent took it — and pass it to the deck (the OpenCode plugin). The
        /// question is then the first line of stdin rather than all of it.
        #[arg(long)]
        ack: bool,
    },
    /// Print the user documentation for this version, embedded in the binary
    /// (no network, no daemon). With no topic, list the topics; with a topic
    /// (e.g. `orchestration`, `desktop/voice`), print that page's Markdown;
    /// with `--all`, print every page in reading order.
    Docs {
        /// The page to print: its path under the docs without `.md`, as
        /// listed by `dot-agent-deck docs`.
        topic: Option<String>,
        /// Print every page in reading order.
        #[arg(long, conflicts_with = "topic")]
        all: bool,
    },
    /// Check for a newer release and upgrade this machine's dot-agent-deck
    /// and Agent Deck desktop app, each through the way it was installed.
    /// Prints what it would do and asks before each upgrade (issue #1635).
    Upgrade {
        /// Only report what is installed and what an upgrade would do.
        #[arg(long)]
        check: bool,
        /// Upgrade without asking for confirmation.
        #[arg(long, conflicts_with = "check")]
        yes: bool,
    },
    /// Set up the Pi orchestrator integration (PRD #201). Detects `pi` on
    /// PATH, materializes the bundled orchestrator extension into Pi's global
    /// extension dir, and enables it (Pi auto-discovers the dir). Prints the
    /// one-line install hint and exits non-zero if `pi` is absent.
    Orchestrator {
        #[command(subcommand)]
        cmd: OrchestratorCmd,
    },
    /// Manage the calling pane's own worker panes within its orchestration
    /// (issue #868).
    Pane {
        #[command(subcommand)]
        cmd: PaneCmd,
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
    /// Add a new schedule.
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
        /// Issue #835: force the fire's spawn shape instead of deriving it from
        /// `--working-dir`'s config: `single` (ONE agent running `--command`, even
        /// where the dir defines `[[orchestrations]]`), `orchestration` (that
        /// dir's default one) or `orchestration:<name>`. Omit to keep the
        /// config-derived behaviour.
        #[arg(long)]
        shape: Option<String>,
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
    /// Update fields of an existing schedule. Rename is forbidden — there is no
    /// name-change flag; `name` selects the schedule to edit.
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
        /// Issue #835: set the fire's spawn shape (`single`, `orchestration`,
        /// `orchestration:<name>`). Pass an EMPTY value (`--shape ""`) to clear it
        /// and go back to deriving the shape from the target dir's config;
        /// omitting the flag leaves the stored value unchanged.
        #[arg(long)]
        shape: Option<String>,
    },
    /// Remove a schedule (does not kill an open tab for it).
    Remove {
        #[arg(long)]
        name: String,
    },
    /// List schedules with their enabled/disabled state and next-fire.
    List,
    /// Enable a schedule.
    Enable {
        #[arg(long)]
        name: String,
    },
    /// Disable a schedule (keeps the definition; stops it firing).
    Disable {
        #[arg(long)]
        name: String,
    },
    /// Fire a schedule now via the running daemon.
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
enum PaneCmd {
    /// Restart a worker role's pane within the calling orchestrator's own
    /// orchestration (issue #868). Recovery for a role marked `crashed`;
    /// refused for a healthy role unless `--force` is given.
    Restart {
        /// Worker role name to restart (as declared in .dot-agent-deck.toml).
        role: String,
        /// Restart even when the target pane's current agent hasn't
        /// crashed. Without this, restarting a healthy pane is refused.
        #[arg(long)]
        force: bool,
    },
    /// Spawn a worker role that is declared in .dot-agent-deck.toml but was
    /// never spawned into this running orchestration instance (issue #868).
    Spawn {
        /// Worker role name to spawn (as declared in .dot-agent-deck.toml).
        role: String,
    },
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
    /// Run the daemon as a user service (systemd on Linux, launchd on
    /// macOS), so it starts at login and comes back after a crash or an
    /// upgrade. Starts it now unless a daemon is already running; never stops
    /// one (PRD #143).
    Install,
    /// Remove the service `install` set up, so the daemon no longer starts at
    /// login. A daemon it started keeps running until `daemon stop`.
    Uninstall,
    /// Print the attach endpoint this host's daemon is listening on, and
    /// print it **only** when a live daemon answered there.
    ///
    /// The remote half of endpoint discovery (issue #1174). The desktop runs
    /// this over ssh on the far host so that the answer comes from a build of
    /// this program running as that user, rather than from a shell snippet
    /// restating the rules from outside — see
    /// [`dot_agent_deck::remote_tunnel::REMOTE_SOCKET_PROBE`].
    ///
    /// Three things have to hold before anything is printed: the path resolves
    /// the same way the daemon's own `bind(2)` resolves it, it passes
    /// `verify_endpoint_trusted` (not a symlink, a socket, owned by this uid,
    /// mode `0o600`), and a bounded attach-protocol `Hello` round trip
    /// succeeds against it. Otherwise nothing goes to stdout and the exit
    /// status is non-zero.
    ///
    /// Read-only, and it never lazy-spawns: "no daemon here" is the answer, not
    /// a reason to start one.
    Endpoint,
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
    /// Plumbing (PRD #1487): report whether a daemon is running at this host's
    /// endpoint, and its `Hello` reply when one is. Never lazy-spawns. Run over
    /// ssh by the laptop's `remote upgrade`, because `daemon hello` prints this
    /// binary's own static hello rather than the running daemon's.
    #[command(hide = true)]
    Probe {
        /// Print one JSON line (`{running, hello}`).
        #[arg(long)]
        json: bool,
    },
    /// Plumbing (PRD #1487): ask the running daemon at this host's endpoint to
    /// restart onto the build installed at its own path. Run over ssh by the
    /// laptop's `remote upgrade`, through the freshly installed binary.
    #[command(hide = true)]
    RestartInstalled {
        /// Print one JSON line (`{running, reply, unsupported}`).
        #[arg(long)]
        json: bool,
        /// The version the caller just installed; the daemon refuses if the
        /// installed build reports anything else.
        #[arg(long)]
        expect_version: Option<String>,
        /// The confirmed stop set, as hex-encoded JSON.
        #[arg(long, conflicts_with = "confirm_stdin")]
        confirm_hex: Option<String>,
        /// Read the confirmed stop set's JSON from stdin, to EOF: for a set
        /// too large for the command line (issue #1619). `daemon probe --json`
        /// reports `confirm_stdin: true` for a build that accepts it.
        #[arg(long)]
        confirm_stdin: bool,
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
        /// the variable is set to the `orchestration:unknown` sentinel, or
        /// if it is absent while running inside an orchestration pane
        /// (issue #300: detected via `DOT_AGENT_DECK_PANE_ID`). If it is
        /// absent outside an orchestration pane -- a human at an
        /// interactive terminal -- falls back to this caller's own
        /// `gh`-resolved identity instead, refusing only if that
        /// resolution also fails. Never silently returns everything or
        /// nothing — a wrong answer here hands one orchestration another's
        /// worktree.
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
    /// Catch up every sibling isolated clone after a merge (fork issue #744,
    /// wiring PRD fork#544 M7's `sync_merged_workspace_to_main` /
    /// `fetch_other_live_workspace` to a real call site for the first time).
    /// No path argument — like `reclaim`, it discovers every isolated clone
    /// on its own via the same [`dot_agent_deck::worktree_reclaim::examine_worktrees`]
    /// enumeration `list`/`reclaim` already use. For each one: if its own
    /// branch's PR is MERGED (resolved the same way `reclaim`'s M4c
    /// auto-reclaim rule already does — `gh pr list`, never git ancestry),
    /// auto-switch it onto the repository's real default branch (resolved
    /// via `gh repo view`) when doing so is safe — `sync_merged_workspace_to_main`
    /// refuses on its own when the tree carries anything beyond the merge,
    /// so this command needs no confirmation flag the way `reclaim` does.
    /// Every other isolated clone gets a read-only `git fetch origin` only,
    /// leaving its checked-out branch and working tree untouched.
    Sync,
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
    /// Diagnose a remote's ssh setup: reachability, the deck's install, the
    /// forwards ssh actually resolved, and the remote sshd policy behind them
    /// (PRD #345). Read-only — it never edits ssh config, sshd config, the
    /// registry, or anything on the remote. Exits 0 when the diagnosis is
    /// clear, 1 when a check failed, and 2 when a check could not be
    /// determined.
    Doctor {
        /// Friendly name of the registry entry to diagnose.
        name: String,
    },
    /// Install a new build on a remote and restart its daemon onto it. On a
    /// host whose deck Homebrew installed, runs `brew upgrade dot-agent-deck`
    /// there instead of downloading a copy. An idle daemon restarts without a
    /// question; when agents or orchestration roles would stop, it asks first
    /// on a terminal and, with no terminal to ask on, installs and keeps the
    /// running daemon. Exits non-zero only when a stage fails.
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
        /// Print the outcome as one JSON object on stdout (progress goes to
        /// stderr).
        #[arg(long)]
        json: bool,
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
enum InboxAction {
    /// Print every message waiting for you, framed as from the deck, and
    /// acknowledge them.
    Fetch {
        /// Print the deck's JSON answer instead.
        #[arg(long)]
        json: bool,
        /// For an agent-side component that caught the doorbell naming this
        /// message: fetch without acknowledging, and only while that message
        /// is still waiting. Implies `--json`.
        #[arg(long, hide = true, value_name = "ID")]
        doorbell: Option<String>,
        /// The integration the component running a doorbell fetch declares
        /// (PRD #1666 §11). Run by that component, not by hand.
        #[arg(long, hide = true, value_name = "N")]
        adapter_integration: Option<u32>,
        /// The receiver incarnation a doorbell fetch's attempts are bound to:
        /// the component's own stream (PRD #1666 §4).
        #[arg(long, hide = true, value_name = "RX")]
        rx: Option<u64>,
    },
    /// List your messages and their delivery states.
    List {
        /// Print the deck's JSON answer instead.
        #[arg(long)]
        json: bool,
        /// From an orchestrator pane: every inbox in its orchestration.
        #[arg(long)]
        all: bool,
    },
    /// Wait for messages on behalf of an agent's hook, plugin or extension.
    /// Run by those components, not by hand.
    Wait {
        /// The agent the component belongs to (`claude-code`, `opencode`,
        /// `pi`, `codex`, `devin`).
        #[arg(long)]
        agent: String,
        /// Stay connected and print each message as a JSON line, reading the
        /// component's receipt controls from stdin.
        #[arg(long)]
        stream: bool,
        /// The deck integration the component that runs this watcher
        /// declares; with it the stream carries deck message receipts.
        #[arg(long, value_name = "N")]
        adapter_integration: Option<u32>,
        /// Register a receiver that only carries the component's doorbell
        /// substitutions.
        #[arg(long, hide = true)]
        doorbell_only: bool,
        /// The component's callbacks survived since its previous watcher.
        #[arg(long, hide = true)]
        resumed: bool,
    },
    /// Tell the deck an agent's native inbox component is about to arm. Run
    /// by those components, not by hand.
    #[command(hide = true)]
    Announce {
        /// The agent the component belongs to.
        #[arg(long)]
        agent: String,
    },
    /// Announce something to every agent in your orchestration (or dispatch
    /// tree) subscribed to TOPIC. They see it as another agent's text, to
    /// use as information.
    Publish {
        /// The topic: lowercase letters, digits and hyphens.
        topic: String,
        #[command(flatten)]
        body: InboxBody,
    },
    /// From an orchestrator pane: offer a job to the first free worker whose
    /// role takes jobs from POOL. One worker gets it; you are told when it is
    /// claimed, completed, or its claim expires.
    Offer {
        /// The pool: lowercase letters, digits and hyphens.
        pool: String,
        #[command(flatten)]
        body: InboxBody,
        /// How long the worker that takes the job may hold it before it is
        /// offered to another, e.g. `30m`, `1h30m`, `90s`.
        #[arg(long, value_name = "DURATION")]
        claim_ttl: Option<String>,
    },
    /// Message an orchestration's orchestrator from outside it, by folder,
    /// orchestration name, or both. With a folder alone that holds no
    /// orchestration, the one standalone agent there is messaged instead,
    /// which is how an orchestrator answers a standalone agent. --name and
    /// --pane reach one standalone agent (or, by pane, an orchestrator) as
    /// `dot-agent-deck roles` lists it, combined with --folder. Your text
    /// arrives as UNTRUSTED data, with the deck naming your pane and how to
    /// reply. A worker reports to its own orchestrator with `work-done`.
    ///
    /// Exit status: 0 sent; 1 refused (nothing matched, the address was
    /// ambiguous, or the orchestrator is not live); 2 the running daemon is
    /// too old for `inbox send`; 3 no daemon is running.
    #[command(group(clap::ArgGroup::new("address").required(true).multiple(true).args(["folder", "orchestration", "name", "pane"])))]
    Send {
        /// The orchestration in this folder or, when none is, in its nearest
        /// parent folder that has one.
        #[arg(long, value_name = "PATH")]
        folder: Option<String>,
        /// The orchestration with this config name or title.
        #[arg(long, value_name = "NAME", conflicts_with = "name")]
        orchestration: Option<String>,
        /// The standalone agent with this name (the NAME column of
        /// `dot-agent-deck roles`), such as one of two in the same folder.
        /// A name may start with `-`.
        #[arg(long, value_name = "NAME", allow_hyphen_values = true)]
        name: Option<String>,
        /// The standalone agent, or orchestration's orchestrator, in this pane
        /// (the PANE column of `dot-agent-deck roles`). Never a worker.
        #[arg(long, value_name = "PANE", allow_hyphen_values = true)]
        pane: Option<String>,
        #[command(flatten)]
        body: InboxBody,
    },
    /// Report a job you were offered as done.
    Complete {
        /// The job's message id.
        id: String,
    },
    /// Hand a job you were offered back, so another worker can take it.
    Release {
        /// The job's message id.
        id: String,
    },
}

/// The text of an `inbox publish` or `inbox offer`.
#[derive(clap::Args)]
#[group(required = true, multiple = false)]
struct InboxBody {
    /// The text.
    #[arg(long)]
    text: Option<String>,
    /// Read the text from this file (`-` reads stdin).
    #[arg(long, value_name = "PATH")]
    file: Option<String>,
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
    /// Update every installed agent's hooks to this copy, as the deck does
    /// when it starts, and say what they run now. Another live install the
    /// hooks run is left in place, and the command to replace it is shown.
    Refresh {
        /// Print the report as JSON
        #[arg(long)]
        json: bool,
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
///
/// Both reads are size-bounded at
/// [`MAX_TASK_BYTES`](dot_agent_deck::bounded_read::MAX_TASK_BYTES) and
/// refused — never
/// truncated — past it, and the path branch additionally requires a **regular
/// file** (issue #328): a FIFO with no writer would otherwise block the CLI
/// forever inside `open`, and a character device such as `/dev/zero` never
/// ends. See [`read_task_input`].
fn resolve_task(
    task: Option<String>,
    task_file: Option<String>,
    stdin: impl std::io::Read,
) -> Result<String, String> {
    match (task, task_file) {
        (Some(t), None) => Ok(t),
        (None, Some(path)) => read_task_input(&path, stdin),
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

/// Parse one line of daemon reply into a [`dot_agent_deck::event::SignalAck`],
/// or `None` when the line is not an ack this build understands.
///
/// Same marker discipline as [`parse_delegate_reply`], and the same direction of
/// failure matters more here: `SignalAck`'s fields are all `#[serde(default)]`
/// over an `accepted: bool`, so `{}` and another verb's reply both parse into
/// `accepted: false` — a refusal invented out of a line that is not an ack.
/// Filtering on the marker is what keeps an unrecognised daemon reported as
/// success rather than as a failure the sender did not have.
fn parse_signal_ack(line: &str) -> Option<dot_agent_deck::event::SignalAck> {
    serde_json::from_str::<dot_agent_deck::event::SignalAck>(line)
        .ok()
        .filter(|a| a.is_signal_ack())
}

/// Issue #1383: what `ack` got back, including the two cases it decides without
/// asking the daemon.
enum AckReply {
    /// The argument is not shaped like a delivery id.
    Malformed,
    /// No `DOT_AGENT_DECK_PANE_ID`: not run from a deck-managed pane.
    NoPane,
    Socket(dot_agent_deck::hook::SocketReply),
}

#[derive(Debug, PartialEq, Eq)]
enum AckStream {
    Stdout,
    Stderr,
}

/// Issue #1383: what `ack` prints, and where. Its exit code is 0 for every
/// reply, which is the one deliberate divergence from
/// [`send_signal_and_report_ack`]: a worker told to acknowledge before anything
/// else must never be derailed from its task by a failed acknowledgement. The
/// daemon keeps re-sending the pointer in that case, which the task file tells
/// the worker to expect.
///
/// Only a recognisable ack with `accepted: true` whose `delivery` says the id
/// named the pane's current delivery prints "Acknowledged" (PR #1414 review:
/// admission alone is not a match). An id the deck does not know — mistyped, or
/// an earlier task's — is said so on stderr, still with exit 0. `NoReply` —
/// which is how a daemon older than `ack` answers, because it predates the verb
/// — an admission that carries no `delivery`, and a line that is not an ack
/// this build understands are all reported on stderr as "could not confirm",
/// still with exit 0: nothing proves the deck matched the ack, so the worker
/// must not be told it did, and nothing proves it failed either, so the worker
/// carries on.
/// PRD #1666: whether the deck positively said it holds no message under the
/// id for this agent — the one `ack` outcome that exits non-zero, so an agent
/// that acknowledged the wrong id learns it. Every other failure to confirm
/// stays a success, for the reason [`ack_report`] gives.
fn ack_was_refused_by_id(reply: &AckReply) -> bool {
    use dot_agent_deck::event::AckDelivery;
    use dot_agent_deck::hook::SocketReply;
    let AckReply::Socket(SocketReply::Line(line)) = reply else {
        return false;
    };
    parse_signal_ack(line).is_some_and(|ack| {
        ack.accepted
            && matches!(
                ack.delivery,
                Some(AckDelivery::Unknown | AckDelivery::WrongAgent | AckDelivery::Undeliverable)
            )
    })
}

/// PR #1735 second review: `ack --json`'s one line, for an agent-side
/// component that must know whether the deck recorded its acknowledgement (the
/// Pi extension confirms each one it sends over its watcher, which answers
/// nothing). `final` is true when the deck refused the id itself, so asking
/// again changes nothing; every other unrecorded answer may be asked again.
fn ack_json(reply: &AckReply) -> serde_json::Value {
    use dot_agent_deck::hook::{AckConfirmation, ack_confirmation};
    if let AckReply::Socket(socket) = reply {
        match ack_confirmation(socket) {
            AckConfirmation::Recorded => return serde_json::json!({"acknowledged": true}),
            AckConfirmation::Refused(reason) => {
                return serde_json::json!({"acknowledged": false, "final": true, "reason": reason});
            }
            AckConfirmation::Unconfirmed => {}
        }
    }
    let (_, message) = ack_report(reply);
    serde_json::json!({"acknowledged": false, "final": false, "reason": message})
}

fn ack_report(reply: &AckReply) -> (AckStream, String) {
    use dot_agent_deck::event::AckDelivery;
    use dot_agent_deck::hook::SocketReply;
    const CARRY_ON: &str = "this is harmless — carry on with your task.";
    let not_confirmed = |why: &str| {
        (
            AckStream::Stderr,
            format!("Could not confirm receipt with the deck ({why}); {CARRY_ON}"),
        )
    };
    match reply {
        AckReply::Malformed => not_confirmed("that is not a message id"),
        AckReply::NoPane => not_confirmed("not running inside a dot-agent-deck managed pane"),
        AckReply::Socket(SocketReply::Unreachable) => not_confirmed("the deck is not reachable"),
        AckReply::Socket(SocketReply::NoReply) => {
            not_confirmed("the deck did not answer; it may predate acknowledgements")
        }
        AckReply::Socket(SocketReply::Line(line)) => match parse_signal_ack(line) {
            Some(ack) if ack.accepted => match ack.delivery {
                Some(delivery) if delivery.recorded() => (
                    AckStream::Stdout,
                    "Acknowledged. Carry on with your task.".to_string(),
                ),
                Some(AckDelivery::Unknown) => (
                    AckStream::Stderr,
                    format!(
                        "Unknown id: the deck has no delivery under that id for this pane \
                         (mistyped, or from an earlier task), so it may send the task pointer \
                         again; {CARRY_ON}"
                    ),
                ),
                Some(AckDelivery::WrongAgent) => (
                    AckStream::Stderr,
                    "That message is addressed to another agent (wrong_agent); it was not \
                     acknowledged."
                        .to_string(),
                ),
                Some(AckDelivery::Undeliverable) => (
                    AckStream::Stderr,
                    "That message is no longer deliverable (undeliverable); it was not \
                     acknowledged."
                        .to_string(),
                ),
                Some(_) => not_confirmed("the deck's reply was not one this build understands"),
                None => not_confirmed("the deck did not say whether the delivery id matched"),
            },
            Some(ack) => not_confirmed(&format!(
                "the deck refused it{}",
                ack.reason
                    .as_deref()
                    .map(|r| format!(": {r}"))
                    .unwrap_or_default()
            )),
            None => not_confirmed("the deck's reply was not one this build understands"),
        },
    }
}

/// Send one fire-and-forget hook-socket signal — `work-done` or `dispatch` —
/// and turn the daemon's acknowledgement into this process's exit code.
///
/// Issue #1129. Before this, both verbs went through
/// `dot_agent_deck::hook::send_to_socket` and exited 0 on any successful write,
/// so a message the provenance gate refused looked identical to one the daemon
/// acted on. The legitimate sender that trips that gate is a `dot-agent-deck`
/// binary in the pane older than the daemon that spawned it: it forwards no
/// token, is refused as `missing_token`, and used to report a completion that
/// went nowhere as a clean success.
///
/// `verb` names the signal in the two messages below; `subject` is how a message
/// refers to it in prose ("work-done report", "dispatch").
///
/// The four outcomes, and why only one of them is a failure the caller did not
/// already have:
///
/// * [`SocketReply::Unreachable`] — never sent, and since issue #434 that is
///   established by the byte count rather than assumed. Exactly what a `None`
///   from `send_to_socket` meant, reported the same way.
/// * [`SocketReply::NoReply`] — usually a daemon that does not write an ack,
///   i.e. one predating issue #1129; since issue #434 also a request write
///   that broke with bytes already gone. **Success** in both cases, for the
///   reason `delegate`'s own `NoReply` arm is: the verb was fire-and-forget
///   before the daemon answered it, so a daemon that does not answer must not
///   become a phantom failure on every mixed-version pair — and a broken write
///   may still have handed the daemon a complete line, so reporting a failure
///   the sender would then retry is the one outcome that can double-send it.
/// * a line that is not a recognisable ack — treated as `NoReply`, per
///   [`parse_signal_ack`].
/// * an ack with `accepted: false` — the one new failure. The daemon refused
///   the signal and said why.
///
/// An `accepted: true` ack says the gate admitted the message, **not** that the
/// handler behind it succeeded — for `dispatch` the daemon writes it before the
/// handler runs, and for `work-done` once the report is queued, before it is
/// written into any pane. See `dot_agent_deck::event::SignalAck` for what
/// stays invisible.
/// PRD #1666 part 3, round 5 (plan D1, D10): `work-done`, sent only once the
/// daemon confirms on the same connection that it serves task-scoped
/// completion, and answered in the words of `work_done_text`.
fn run_work_done(pane_id: String, report: String, done: bool, task_id: Option<String>) -> ExitCode {
    use dot_agent_deck::event::{
        DaemonMessage, HookCapabilitiesRequest, WorkDoneMode, WorkDoneTaskReply,
        WorkDoneTaskRequest,
    };
    use dot_agent_deck::hook::Gated;
    use dot_agent_deck::work_done_text as text;
    let token = dot_agent_deck::hook_provenance::token_from_env();
    let mode = match (&task_id, done) {
        (Some(task_id), _) => WorkDoneMode::Task {
            task_id: task_id.clone(),
        },
        (None, true) => WorkDoneMode::Terminal,
        (None, false) => WorkDoneMode::Uncorrelated,
    };
    let question = DaemonMessage::HookCapabilities(HookCapabilitiesRequest {
        pane_id: pane_id.clone(),
        token: token.clone(),
    });
    let mutation = DaemonMessage::WorkDoneTask(WorkDoneTaskRequest {
        pane_id,
        token,
        mode,
        report,
        timestamp: chrono::Utc::now(),
    });
    let (Ok(question), Ok(mutation)) = (
        serde_json::to_string(&question),
        serde_json::to_string(&mutation),
    ) else {
        eprintln!("Failed to serialize work-done signal");
        return ExitCode::FAILURE;
    };
    let reply = match dot_agent_deck::hook::send_gated(
        &question,
        &mutation,
        dot_agent_deck::hook::confirms_task_scoped_completion,
    ) {
        Gated::NotConfirmed {
            refusal: Some((code, message)),
            ..
        } => {
            // The code is the word the daemon also put in its own `warn!`
            // line, as `dispatch`'s refusal carries it.
            eprintln!("Error: the daemon did not accept this work-done report: {message} [{code}]");
            return ExitCode::FAILURE;
        }
        Gated::NotConfirmed { why, .. } => {
            eprintln!("{}", text::NOT_CONFIRMED);
            eprintln!("({why})");
            return ExitCode::FAILURE;
        }
        Gated::Unconfirmed(why) => {
            eprintln!("{}", text::unconfirmed(task_id.as_deref(), done));
            eprintln!("({why})");
            return ExitCode::FAILURE;
        }
        Gated::Reply(line) => line,
    };
    let Ok(reply) = serde_json::from_str::<WorkDoneTaskReply>(reply.trim()) else {
        eprintln!("{}", text::unconfirmed(task_id.as_deref(), done));
        eprintln!("(the daemon's answer could not be read)");
        return ExitCode::FAILURE;
    };
    let answer = text::cli_answer(&reply, task_id.as_deref(), done);
    if let Some(line) = answer.stdout {
        println!("{line}");
    }
    if let Some(line) = answer.stderr {
        eprintln!("{line}");
    }
    if answer.success {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn send_signal_and_report_ack(json: &str, verb: &str, subject: &str) -> ExitCode {
    use dot_agent_deck::hook::SocketReply;
    match dot_agent_deck::hook::send_and_await_signal_ack(json) {
        SocketReply::Unreachable => {
            eprintln!("Failed to send {verb} signal to daemon socket.");
            ExitCode::FAILURE
        }
        SocketReply::NoReply => ExitCode::SUCCESS,
        SocketReply::Line(line) => match parse_signal_ack(&line) {
            Some(ack) if !ack.accepted => {
                // The daemon's sentence carries the remedy; the code is the word
                // it also put in its own `warn!` line, so an operator reading
                // both has one string to grep for.
                eprintln!(
                    "Error: the daemon did not accept this {subject}: {}{}",
                    ack.error.as_deref().unwrap_or(
                        "refused, and the daemon gave no reason this build understands."
                    ),
                    ack.reason
                        .as_deref()
                        .map(|r| format!(" [{r}]"))
                        .unwrap_or_default()
                );
                ExitCode::FAILURE
            }
            _ => ExitCode::SUCCESS,
        },
    }
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
///
/// Issue #580 adds `busy` — roles refused because their worker still owes a
/// `work-done` — under the same contract. When NOTHING was dispatched the daemon
/// also sets `error`, so the first arm above reports it and fails. When some role
/// WAS dispatched, a busy role is a partial miss exactly like an unresolved one:
/// a warning, exit 0. And `superseded` — a `--supersede` that did replace an
/// unanswered delegation — is reported rather than silent, but is not a miss.
fn delegate_verdict(
    pane_id: &str,
    requested_roles: &[String],
    resp: &dot_agent_deck::event::DelegateResponse,
) -> DelegateVerdict {
    let blocked_note = blocked_workers_note(resp);
    if let Some(error) = resp.error.as_deref() {
        let mut message = format!("Error: delegate from pane {pane_id} failed: {error}");
        if let Some(note) = blocked_note {
            message.push('\n');
            message.push_str(&note);
        }
        return DelegateVerdict {
            failed: true,
            message: Some(message),
            info: None,
        };
    }
    let mut notes: Vec<String> = Vec::new();
    if let Some(note) = blocked_note {
        notes.push(note);
    }
    if !resp.superseded.is_empty() {
        notes.push(format!(
            "Note: dispatched to worker(s) that still owed a work-done for an earlier \
             delegation: {}. That earlier task is not cancelled — its work-done, if it comes, is \
             credited like any other.",
            dot_agent_deck::state::describe_busy_workers(&resp.superseded)
        ));
    }
    if !resp.busy.is_empty() {
        let delivered = if resp.delivered.is_empty() {
            String::new()
        } else {
            format!(" It WAS delivered to: {}.", resp.delivered.join(", "))
        };
        notes.push(format!(
            "Warning: delegate from pane {pane_id} was NOT sent to worker(s) that still owe a \
             work-done for an earlier delegation: {}.{delivered} {}",
            dot_agent_deck::state::describe_busy_workers(&resp.busy),
            dot_agent_deck::state::busy_worker_remedy()
        ));
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
            // A reply with `busy` and an empty `delivered` but no `error` is not
            // one this daemon writes; if one arrives, nothing landed.
            failed: resp.delivered.is_empty() && !resp.busy.is_empty(),
            message: (!notes.is_empty()).then(|| notes.join("\n")),
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
    // The five causes, stated as the five causes rather than as the one that
    // happens to be most common. Issue #554 added the fourth: the daemon routes
    // by the role name a pane was started with, so a role renamed in the toml
    // after the orchestration started is present in the file and still reaches
    // nobody — the first cause alone sent the user to a file that looked right.
    // Issue #524 added the fifth: a worker that exited on its own is still in
    // the role maps, and only a `clear = true` role is respawned by a delegate.
    let causes = "(A role reaches no worker when it is absent from \
                  .dot-agent-deck.toml, when it is the delegating orchestrator \
                  itself — an orchestrator cannot delegate to itself — when \
                  its worker pane has been closed, when its worker has exited \
                  and the role is not `clear = true` — `dot-agent-deck pane \
                  restart <role>` starts it again — or when the role was renamed \
                  or added in .dot-agent-deck.toml after this orchestration \
                  started — running panes keep the role names they were started \
                  with until the orchestration is restarted.)";
    if resp.delivered.is_empty() {
        notes.insert(
            0,
            format!(
                "Error: delegate from pane {pane_id} reached no worker for role(s): \
                 {unresolved}. No role in this orchestration received it. {causes}"
            ),
        );
        return DelegateVerdict {
            failed: true,
            message: Some(notes.join("\n")),
            info: None,
        };
    }
    notes.insert(
        0,
        format!(
            "Warning: delegate from pane {pane_id} reached no worker for role(s): \
             {unresolved}. It WAS delivered to: {}. Retry only the roles that \
             missed — re-sending the whole delegate would dispatch the delivered \
             roles a second time. {causes}",
            resp.delivered.join(", ")
        ),
    );
    DelegateVerdict {
        failed: false,
        message: Some(notes.join("\n")),
        info: None,
    }
}

/// Issue #714: the warning `delegate` prints for
/// [`dot_agent_deck::event::DelegateResponse::blocked`], split by what happened
/// to each blocked role. Never a failure on its own: a blocked worker that was
/// delivered to really has the task (a windowed limit may even have reset), so
/// the exit code stays governed by the rules above. Fixed text plus role labels,
/// ages and resets only — no error text, which is agent-controlled.
fn blocked_workers_note(resp: &dot_agent_deck::event::DelegateResponse) -> Option<String> {
    if resp.blocked.is_empty() {
        return None;
    }
    let (busy, delivered): (Vec<_>, Vec<_>) = resp
        .blocked
        .iter()
        .cloned()
        .partition(|b| resp.busy.iter().any(|w| w.role == b.role));
    let mut lines = Vec::new();
    if !delivered.is_empty() {
        lines.push(format!(
            "Warning: worker(s) {} appear BLOCKED by a provider usage limit or credit pool. The task WAS \
             delivered but will likely not be worked on while that lasts — if the worker's card \
             still shows Blocked, reassign it to a role backed by a different provider or \
             account, or restore the quota and re-delegate.",
            dot_agent_deck::state::describe_blocked_workers(&delivered)
        ));
    }
    if !busy.is_empty() {
        lines.push(format!(
            "Warning: busy worker(s) {} also appear BLOCKED by a provider usage limit or credit pool, so \
             --supersede will likely not get the task worked on while that lasts — if the \
             worker's card still shows Blocked, reassign it to a role backed by a different \
             provider or account.",
            dot_agent_deck::state::describe_blocked_workers(&busy)
        ));
    }
    Some(lines.join("\n"))
}

/// A delegated-task delivery id from before PRD #1666 (`d-` and eight lowercase
/// hex digits). Still sent, so the deck can answer that it holds no such
/// message instead of the CLI calling a task file's id malformed.
fn is_retired_delivery_id(id: &str) -> bool {
    id.strip_prefix("d-").is_some_and(|hex| {
        hex.len() == 8
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn main() -> ExitCode {
    // PRD #1741: the telemetry PATH shim, first, so the agent's arguments it
    // carries are never read as the deck's.
    if let Some(code) = dot_agent_deck::telemetry::shim::run_if_invoked() {
        return code;
    }
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
            // Issue #730 (auditor S-C): the hook installers LOG. Until this call
            // they did not — this branch never installed a subscriber, so every
            // `tracing::warn!` on the install and uninstall paths was dropped on
            // the floor by the no-op global subscriber (the same failure mode
            // documented for `wrap` below). That silenced both halves of the
            // Codex trust diagnosis: the "we could not recognise our own entry"
            // warning and the "trust write failed" arm, each of which otherwise
            // reaches the user only as an exit 0 with nothing said.
            //
            // Safe for a CLI whose stdout is the documented output.
            // `init_logging_from_env` installs a subscriber ONLY when
            // `DOT_AGENT_DECK_LOG` is set, and the subscriber it installs writes
            // only to that file — so this command's own stdout is unchanged
            // whether logging is on or off, and nothing a hook installer logs
            // can end up in what a caller parses.
            init_logging_from_env();
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
                HooksAction::Refresh { json } => {
                    let report = dot_agent_deck::hooks_refresh::refresh();
                    if json {
                        match serde_json::to_string(&report) {
                            Ok(line) => println!("{line}"),
                            Err(e) => {
                                eprintln!("Failed to write the hooks report: {e}");
                                return ExitCode::FAILURE;
                            }
                        }
                    } else {
                        for line in report.render() {
                            println!("{line}");
                        }
                    }
                    if !report.ok {
                        return ExitCode::FAILURE;
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
            supersede,
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
                supersede,
                timestamp: chrono::Utc::now(),
                token: dot_agent_deck::hook_provenance::token_from_env(),
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
                // this response — or, since issue #434, a request write that
                // broke with bytes already gone. Pre-response contract:
                // unverifiable, and the caller must not turn that into a
                // phantom failure. See `SocketReply::NoReply`.
                //
                // Deliberately silent as well as successful, and the broken
                // write is why that is worth stating rather than inheriting.
                // The orchestrator reading this is an agent: a stderr line
                // saying the delegate might not have landed is an invitation
                // to send it again, and the daemon may already be holding a
                // complete line (`read_capped_line` treats a trailing
                // unterminated line as a line), so the retry is what
                // double-dispatches the worker. Exit 0 with no output is the
                // "do not resend" signal; the record of the broken write is
                // the `warn!` `request_from_socket_at_detailed` logs.
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
            // `--list-targets` is a READ-ONLY daemon round-trip: the daemon
            // answers from the PANE's cwd and config, which is the same basis
            // the dispatch itself resolves from. Computing it here
            // from the CLI's own `current_dir()` diverged whenever the agent had
            // `cd`'d, and offered targets the dispatch could not start.
            //
            // Exits after printing. clap's `conflicts_with_all` guarantees no
            // dispatch arguments were supplied, so this cannot silently swallow a
            // real dispatch and still exit 0.
            if list_targets {
                let req = dot_agent_deck::event::DaemonMessage::ListTargets(
                    dot_agent_deck::event::ListTargetsRequest {
                        pane_id,
                        token: dot_agent_deck::hook_provenance::token_from_env(),
                    },
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
                    token: dot_agent_deck::hook_provenance::token_from_env(),
                };
                let msg = dot_agent_deck::event::DaemonMessage::Dispatch(signal);
                let json = match serde_json::to_string(&msg) {
                    Ok(j) => j,
                    Err(e) => {
                        eprintln!("Failed to serialize dispatch signal: {e}");
                        return ExitCode::FAILURE;
                    }
                };
                send_signal_and_report_ack(&json, "dispatch", "dispatch")
            }
        }
        Some(Commands::WorkDone {
            task,
            task_file,
            done,
            task_id,
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
            run_work_done(pane_id, task, done, task_id)
        }
        Some(Commands::Inbox { action }) => match action {
            InboxAction::Fetch {
                doorbell: Some(id),
                adapter_integration,
                rx,
                ..
            } => dot_agent_deck::inbox_cli::fetch_for_doorbell(&id, adapter_integration, rx),
            InboxAction::Fetch {
                json,
                doorbell: None,
                ..
            } => dot_agent_deck::inbox_cli::fetch(json),
            InboxAction::List { json, all } => dot_agent_deck::inbox_cli::list(json, all),
            InboxAction::Wait {
                agent,
                stream,
                adapter_integration,
                doorbell_only,
                resumed,
            } => dot_agent_deck::inbox_cli::wait(
                &agent,
                stream,
                dot_agent_deck::inbox_cli::Receipt {
                    adapter_integration,
                    doorbell_only,
                    resumed,
                },
            ),
            InboxAction::Announce { agent } => dot_agent_deck::inbox_cli::announce(&agent),
            InboxAction::Publish { topic, body } => {
                dot_agent_deck::inbox_cli::publish(&topic, body.text, body.file)
            }
            InboxAction::Offer {
                pool,
                body,
                claim_ttl,
            } => dot_agent_deck::inbox_cli::offer(&pool, body.text, body.file, claim_ttl),
            InboxAction::Send {
                folder,
                orchestration,
                name,
                pane,
                body,
            } => run_inbox_send_cli(absolute_address(folder, orchestration, name, pane), body),
            InboxAction::Complete { id } => dot_agent_deck::inbox_cli::settle(&id, true),
            InboxAction::Release { id } => dot_agent_deck::inbox_cli::settle(&id, false),
        },
        Some(Commands::Ack { delivery_id, json }) => {
            let reply = match (
                dot_agent_deck::inbox::is_valid_message_id(&delivery_id)
                    || is_retired_delivery_id(&delivery_id),
                std::env::var(DOT_AGENT_DECK_PANE_ID),
            ) {
                (false, _) => AckReply::Malformed,
                (true, Err(_)) => AckReply::NoPane,
                (true, Ok(pane_id)) => {
                    let msg = dot_agent_deck::event::DaemonMessage::Ack(
                        dot_agent_deck::event::AckSignal {
                            pane_id,
                            delivery_id,
                            agent_id: std::env::var(DOT_AGENT_DECK_AGENT_ID).ok(),
                            token: dot_agent_deck::hook_provenance::token_from_env(),
                        },
                    );
                    match serde_json::to_string(&msg) {
                        Ok(json) => {
                            AckReply::Socket(dot_agent_deck::hook::send_and_await_signal_ack(&json))
                        }
                        Err(_) => AckReply::Malformed,
                    }
                }
            };
            if json {
                println!("{}", ack_json(&reply));
            } else {
                let (stream, message) = ack_report(&reply);
                match stream {
                    AckStream::Stdout => println!("{message}"),
                    AckStream::Stderr => eprintln!("{message}"),
                }
            }
            if ack_was_refused_by_id(&reply) {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Some(Commands::AgentEvent {
            r#type,
            cwd,
            prompt,
            tool_name,
            tool_detail,
            _reports_prompts,
            turn_reply_stdin,
            turn_reply_failed,
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
            // Optional — the daemon injects this on spawn (same pattern as the
            // hook path); a pane spawned before agent-id tagging has none.
            let agent_id = std::env::var(DOT_AGENT_DECK_AGENT_ID).ok();
            let event_type = match dot_agent_deck::event::agent_event_type_from_state(&r#type) {
                Some(et) => et,
                None => {
                    eprintln!(
                        "Error: unknown agent-event --type {:?}. Expected one of: {}.",
                        r#type,
                        dot_agent_deck::event::AGENT_EVENT_TYPES.join(", ")
                    );
                    return ExitCode::FAILURE;
                }
            };
            // Ride the EXISTING raw-`AgentEvent` socket path (zero new wire):
            // a bare AgentEvent with no `message_type` envelope, keyed on a
            // stable session id derived from the pane so repeated events update
            // the same card. The daemon's `run_hook_loop` falls back to
            // `AgentEvent` and `apply_event` drives the card.
            // PRD #1497: the settled turn's reply rides the line beside the
            // event, never inside it, exactly as a hook's does. It arrives on
            // stdin, read only for a turn end and never for longer than
            // `TURN_REPLY_STDIN_TIMEOUT`. A flagged turn end whose stdin gave
            // nothing is a turn that ended with nothing to read (audit A2).
            let turn_reply = (turn_reply_stdin
                && event_type == dot_agent_deck::event::EventType::Idle)
                .then(|| {
                    dot_agent_deck::hook::read_turn_reply_stdin(
                        std::io::stdin(),
                        dot_agent_deck::hook::TURN_REPLY_STDIN_TIMEOUT,
                    )
                    .unwrap_or_default()
                });
            let reply = dot_agent_deck::hook::agent_event_cli_turn_reply(
                &event_type,
                turn_reply,
                turn_reply_failed,
            );
            let event = dot_agent_deck::hook::build_agent_event_cli(
                pane_id,
                agent_id,
                event_type,
                dot_agent_deck::hook::AgentEventDetail {
                    cwd,
                    prompt,
                    tool_name,
                    tool_detail,
                },
            );
            // Issue #318: present this pane's hook capability token.
            let token = dot_agent_deck::hook_provenance::token_from_env();
            let json = match dot_agent_deck::hook::agent_event_cli_line(
                &event,
                token.as_deref(),
                reply.as_ref(),
            ) {
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
        Some(Commands::Roles {
            json,
            folder,
            orchestration,
            name,
            pane,
        }) => run_roles_cli(json, absolute_address(folder, orchestration, name, pane)),
        Some(Commands::Close {
            units,
            unit_ids,
            pane,
            orchestration_of,
            all,
            yes,
            dry_run,
            force,
            json,
        }) => {
            let args = dot_agent_deck::close_cli::CloseArgs {
                units,
                unit_ids,
                pane,
                orchestration_of,
                all,
                yes,
                dry_run,
                force,
                json,
            };
            run_close_cli(args)
        }
        Some(Commands::Docs { topic, all }) => {
            use dot_agent_deck::embedded_docs::{self, DocsOutput};
            match embedded_docs::run(embedded_docs::PAGES, topic.as_deref(), all) {
                DocsOutput::Stdout(text) => {
                    match embedded_docs::write_output(std::io::stdout(), &text) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(e) => {
                            eprintln!("error: cannot write the docs to stdout: {e}");
                            ExitCode::FAILURE
                        }
                    }
                }
                DocsOutput::Error(message) => {
                    eprint!("{message}");
                    ExitCode::FAILURE
                }
            }
        }
        Some(Commands::AwaitAnswer {
            agent,
            question,
            ack,
        }) => dot_agent_deck::hook::handle_await_answer(&agent, question.as_deref(), ack),
        Some(Commands::Upgrade { check, yes }) => {
            dot_agent_deck::self_upgrade::cli::main(dot_agent_deck::self_upgrade::cli::Args {
                check,
                yes,
            })
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
        Some(Commands::Pane { cmd }) => match cmd {
            PaneCmd::Restart { role, force } => {
                let pane_id = match std::env::var(DOT_AGENT_DECK_PANE_ID) {
                    Ok(id) => id,
                    Err(_) => {
                        eprintln!(
                            "Error: DOT_AGENT_DECK_PANE_ID environment variable not set.\nThis command should be run from within a dot-agent-deck managed pane."
                        );
                        return ExitCode::FAILURE;
                    }
                };
                let signal = dot_agent_deck::event::RestartRoleSignal {
                    pane_id,
                    role: role.clone(),
                    force,
                    timestamp: chrono::Utc::now(),
                    token: dot_agent_deck::hook_provenance::token_from_env(),
                };
                let msg = dot_agent_deck::event::DaemonMessage::RestartRole(signal);
                let json = match serde_json::to_string(&msg) {
                    Ok(j) => j,
                    Err(e) => {
                        eprintln!("Failed to serialize restart-role signal: {e}");
                        return ExitCode::FAILURE;
                    }
                };
                use dot_agent_deck::hook::SocketReply;
                // Issue #868: `pane restart`'s own, larger reply budget —
                // see `RESTART_ROLE_REPLY_TIMEOUT`'s doc for why `delegate`'s
                // 5s is too small for a respawn's worst case.
                let line = match dot_agent_deck::hook::send_and_await_restart_role_reply(&json) {
                    SocketReply::Unreachable => {
                        eprintln!(
                            "Error: could not reach the dot-agent-deck daemon socket, so the \
                             restart of role {role} was NOT delivered."
                        );
                        return ExitCode::FAILURE;
                    }
                    // Unlike `delegate`'s fire-and-forget `NoReply`, `pane
                    // restart` must not read a silent close as success — but
                    // `NoReply` folds several distinct `NoReplyCause`s (see
                    // its doc comment above), only one of which is "old
                    // daemon"; a `DeadlineExpired` here means the restart may
                    // well have already succeeded, so the message below stays
                    // cause-agnostic rather than asserting "old daemon" and
                    // telling the agent to restart the whole daemon, which
                    // would be the worst possible advice in that case (PR
                    // #918 review). Issue #434 added a cause the wording had
                    // to lose two words for: a write that broke with bytes
                    // already gone did not fail "in time", and the daemon may
                    // still act on what it holds — which is the same "check
                    // before retrying" this arm already advises.
                    SocketReply::NoReply => {
                        eprintln!(
                            "Error: the daemon did not answer `pane restart {role}` — \
                             either it does not support this command (an older build; restart \
                             the daemon to pick up the new one) or the restart is still in \
                             flight and may have already succeeded. Check the pane before \
                             retrying — retrying a successful restart will kill and respawn it \
                             again."
                        );
                        return ExitCode::FAILURE;
                    }
                    SocketReply::Line(line) => line,
                };
                let resp =
                    serde_json::from_str::<dot_agent_deck::event::RestartRoleResponse>(&line)
                        .ok()
                        .filter(|r| r.is_restart_role_reply());
                let Some(resp) = resp else {
                    eprintln!(
                        "Error: the running daemon sent an unexpected reply to \
                         `pane restart {role}` — it may not support this command; restart \
                         the daemon to pick up the new build."
                    );
                    return ExitCode::FAILURE;
                };
                if resp.restarted {
                    println!("Restarted role {role}");
                    ExitCode::SUCCESS
                } else {
                    eprintln!(
                        "Error: restart of role {role} failed: {}",
                        resp.error.as_deref().unwrap_or("unknown error")
                    );
                    ExitCode::FAILURE
                }
            }
            PaneCmd::Spawn { role } => {
                let pane_id = match std::env::var(DOT_AGENT_DECK_PANE_ID) {
                    Ok(id) => id,
                    Err(_) => {
                        eprintln!(
                            "Error: DOT_AGENT_DECK_PANE_ID environment variable not set.\nThis command should be run from within a dot-agent-deck managed pane."
                        );
                        return ExitCode::FAILURE;
                    }
                };
                let signal = dot_agent_deck::event::SpawnRoleSignal {
                    pane_id,
                    role: role.clone(),
                    timestamp: chrono::Utc::now(),
                    token: dot_agent_deck::hook_provenance::token_from_env(),
                };
                let msg = dot_agent_deck::event::DaemonMessage::SpawnRole(signal);
                let json = match serde_json::to_string(&msg) {
                    Ok(j) => j,
                    Err(e) => {
                        eprintln!("Failed to serialize spawn-role signal: {e}");
                        return ExitCode::FAILURE;
                    }
                };
                use dot_agent_deck::hook::SocketReply;
                // PR #783 fix round (auditor L1): `pane spawn`'s own, larger
                // reply budget — see `SPAWN_ROLE_REPLY_TIMEOUT`'s doc for why
                // `delegate`'s 5s is too small now that a timeout is a hard
                // failure rather than a silent success.
                let line = match dot_agent_deck::hook::send_and_await_spawn_role_reply(&json) {
                    SocketReply::Unreachable => {
                        eprintln!(
                            "Error: could not reach the dot-agent-deck daemon socket, so the \
                             spawn of role {role} was NOT delivered."
                        );
                        return ExitCode::FAILURE;
                    }
                    // Unlike `delegate`'s fire-and-forget `NoReply`, `pane
                    // spawn` must not read a silent close as success — but
                    // `NoReply` folds several distinct `NoReplyCause`s (see
                    // its doc comment above), only one of which is "old
                    // daemon"; a `DeadlineExpired` here means the spawn may
                    // still be in flight, so the message below stays
                    // cause-agnostic rather than asserting "old daemon" (PR
                    // #918 review). Issue #434's broken write is a third
                    // cause, and "in time" was wrong for it — see the same
                    // arm behind `pane restart`.
                    SocketReply::NoReply => {
                        eprintln!(
                            "Error: the daemon did not answer `pane spawn {role}` — \
                             either it does not support this command (an older build; restart \
                             the daemon to pick up the new one) or the spawn is still in \
                             flight. Check the pane before retrying."
                        );
                        return ExitCode::FAILURE;
                    }
                    SocketReply::Line(line) => line,
                };
                let resp = serde_json::from_str::<dot_agent_deck::event::SpawnRoleResponse>(&line)
                    .ok()
                    .filter(|r| r.is_spawn_role_reply());
                let Some(resp) = resp else {
                    eprintln!(
                        "Error: the running daemon sent an unexpected reply to \
                         `pane spawn {role}` — it may not support this command; restart \
                         the daemon to pick up the new build."
                    );
                    return ExitCode::FAILURE;
                };
                if resp.spawned {
                    println!("Spawned role {role}");
                    ExitCode::SUCCESS
                } else {
                    eprintln!(
                        "Error: spawn of role {role} failed: {}",
                        resp.error.as_deref().unwrap_or("unknown error")
                    );
                    ExitCode::FAILURE
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
                // PRD #1258: time the pre-bind work, so a lazy-spawn that timed
                // out says in the daemon's log whether the login-shell capture or
                // the installers below took the time — the gap before the first
                // line is process start-up itself.
                let pre_bind_started = std::time::Instant::now();
                tracing::info!("daemon serve: starting pre-bind work");
                dot_agent_deck::login_shell::apply_login_shell_path();
                tracing::info!(
                    elapsed_ms = pre_bind_started.elapsed().as_millis() as u64,
                    "daemon serve: login-shell PATH step done"
                );
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
                // Every shipped agent's startup auto-install, from the same
                // registry loop `run_tui_session` runs — Claude Code's native
                // hooks, the OpenCode plugin, Codex's `hooks.json` plus its
                // SCOPED, hash-pinned trust (PRD #20 §4.2.1), and Devin's hooks.
                // Each self-guards on its agent being present and is a no-op
                // otherwise, and each runs AFTER the login-shell PATH is applied
                // so presence is detected against the daemon's real PATH.
                //
                // Codex and Devin ran here explicitly because the spawn seam can
                // only detect an agent by basename, so a launcher (`devbox run
                // codex-big`) got no hooks unless the daemon installed them
                // command-agnostically. Issue #1157 found the same gap one level
                // up for Claude Code and OpenCode, which only the TUI installed:
                // the packaged desktop app starts THIS subcommand from its
                // bundled sidecar and never runs a TUI, so a desktop-only user
                // got no Claude Code hooks and no OpenCode plugin at all, and
                // therefore no hook-driven agent status. Running the whole
                // registry here closes that, and keeps the daemon in step with
                // the TUI when an agent is added. The TUI still runs the same
                // loop; every installer is idempotent, and Codex and Devin have
                // run in both processes since they were added here.
                //
                // Issue #1637: each installer reports what it left the agent's
                // hooks pinned to, so the daemon can tell the user when that is
                // an older copy of the deck before any hook fires.
                let hook_binaries = {
                    use dot_agent_deck::agent_registry::ALL;
                    let mut pins = Vec::new();
                    for spec in ALL {
                        if let Some(install) = spec.startup_auto_install {
                            pins.extend(install());
                        }
                    }
                    // The resolution the installers' takeover check already
                    // made, not a fresh one per caller.
                    let resolution = dot_agent_deck::hook_binary::process_resolution();
                    dot_agent_deck::hook_binary::HookBinaryState::from_startup(
                        dot_agent_deck::hook_binary::DeckIdentity::current(resolution),
                        &pins,
                        dot_agent_deck::hook_binary::refused_ephemeral_location(resolution),
                    )
                };
                tracing::info!(
                    elapsed_ms = pre_bind_started.elapsed().as_millis() as u64,
                    "daemon serve: pre-bind work done, starting the server"
                );
                run_daemon_serve_cli(hook_binaries)
            }
            DaemonCmd::Hello => run_daemon_hello_cli(),
            DaemonCmd::Stop { force } => run_daemon_stop_cli(force),
            DaemonCmd::Restart { force } => run_daemon_restart_cli(force),
            DaemonCmd::Status { json } => run_daemon_status_cli(json),
            DaemonCmd::Install => run_daemon_install_cli(),
            DaemonCmd::Uninstall => run_daemon_uninstall_cli(),
            DaemonCmd::Endpoint => run_daemon_endpoint_cli(),
            DaemonCmd::Probe { json } => run_daemon_probe_cli(json),
            DaemonCmd::RestartInstalled {
                json,
                expect_version,
                confirm_hex,
                confirm_stdin,
            } => run_daemon_restart_installed_cli(json, expect_version, confirm_hex, confirm_stdin),
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
                    Ok(removed) => {
                        // Issue #1372: a Homebrew-installed deck is removed
                        // through Homebrew, not by deleting a file brew owns.
                        let uninstall = if removed.install.as_deref()
                            == Some(dot_agent_deck::remote::INSTALL_HOMEBREW)
                        {
                            "brew uninstall dot-agent-deck".to_string()
                        } else {
                            format!("rm {}", dot_agent_deck::remote::REMOTE_INSTALL_PATH)
                        };
                        println!(
                            "Removed remote '{name}' from local registry. The dot-agent-deck binary on the remote and its hooks are unaffected; if you want to clean those up, ssh in and run `dot-agent-deck hooks uninstall` and `{uninstall}`."
                        );
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("{e}");
                        ExitCode::FAILURE
                    }
                }
            }
            RemoteCmd::Doctor { name } => run_remote_doctor(&name),
            RemoteCmd::Upgrade {
                name,
                version,
                no_install,
                json,
            } => run_remote_upgrade(&name, version, no_install, json),
        },
        Some(Commands::Worktree { cmd }) => match cmd {
            WorktreeCmd::List { json, mine } => run_worktree_list_cli(json, mine),
            WorktreeCmd::Reclaim { yes } => run_worktree_reclaim_cli(yes),
            WorktreeCmd::Pin { path } => run_worktree_pin_cli(path),
            WorktreeCmd::Unpin { path } => run_worktree_unpin_cli(path),
            WorktreeCmd::Sync => run_worktree_sync_cli(),
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
                            // Issue #1077: the provenance gate refuses a pane
                            // the daemon issued a token to when none is
                            // presented, as it does for every other verb.
                            token: dot_agent_deck::hook_provenance::token_from_env(),
                        },
                    )
                }
                WaitCmd::Done { label, outcome } => dot_agent_deck::event::DaemonMessage::WaitDone(
                    dot_agent_deck::event::WaitDoneSignal {
                        pane_id,
                        label,
                        outcome: outcome.into(),
                        timestamp: chrono::Utc::now(),
                        token: dot_agent_deck::hook_provenance::token_from_env(),
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
                    // Issue #308 follow-up: the config failed to PARSE, so the
                    // `toml` error quotes the offending source line verbatim and
                    // this is an untrusted-bytes sink like the issue loop above.
                    // Neutralised at the seam, not here —
                    // `ProjectConfigError`'s `Display` escapes control, C1 and
                    // bidi characters while keeping the error frame's own
                    // newlines, exactly as `ValidationIssue`'s `Display` does
                    // for the single-line case. See both impls for why the
                    // escaping lives there rather than at each `eprintln!`.
                    eprintln!("{e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some(Commands::Wrap { agent, command }) => {
            // Issue #243: the wrapper LOGS. Until this call it did not — the
            // subcommand went straight into `run_wrap`, so the two `tracing`
            // lines the interface watch emits (which of its two facts fired, the
            // single most useful field diagnostic for a wrapped agent's boot)
            // were dropped on the floor by the no-op global subscriber, and the
            // daemon's claim that the fact is "in the wrapper's log" was false.
            // Diagnosing a Codex delegate that never got its prompt meant
            // reading the wire.
            //
            // Safe in a pane. `init_logging_from_env` installs a subscriber ONLY
            // when `DOT_AGENT_DECK_LOG` is set, and that subscriber only ever
            // writes to that file — never to stdout or stderr — so a wrapper
            // whose descriptors ARE the agent's terminal cannot paint a LOG LINE
            // into it. (The setup itself has exactly one stderr line, `Warning:
            // failed to open log file …`, on a log path it cannot open; so the
            // claim is about the log stream, not about the call being incapable
            // of writing to the terminal at all. Narrowed under issue #730's
            // rule-17 sweep, which added the same call to the `Hooks` branch
            // above.) The
            // daemon fork-execs `dot-agent-deck wrap` without clearing the
            // environment, so an operator who enabled the daemon's log gets the
            // wrapper's half of the story in the same file, correlated by
            // timestamp against the gate lines that read these events.
            init_logging_from_env();
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
    use tracing_subscriber::Layer as _;
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    // The file logger: unchanged in what it writes and at what level.
    let file = std::env::var("DOT_AGENT_DECK_LOG")
        .ok()
        .and_then(|log_val| {
            // The default path is the owner-only `state_dir()/deck.log` (this
            // fork's decision, ruling 70 of the net merge): a world-writable
            // `/tmp` log is a symlink target, and the state dir already holds
            // every other per-user daemon file (CLAUDE.md rule 12 names it).
            // `state_dir()` is resolved per platform, which is what upstream's
            // issue #1135 needed of its own default. An explicit value is still
            // used verbatim.
            let is_default = log_val.is_empty() || log_val == "1";
            let log_path = if is_default {
                let dir = state_dir();
                if let Err(e) = dot_agent_deck::platform::fsperm::ensure_owner_only_dir(&dir) {
                    warn_log_setup_failure("create owner-only state dir", &dir, e);
                    return None;
                }
                dir.join("deck.log")
            } else {
                std::path::PathBuf::from(log_val)
            };
            // Issues #1791, #1796: a test process does not open an inherited log
            // that is not its own; see `config_write_guard::log_write_allowed`.
            if !dot_agent_deck::config_write_guard::log_write_allowed(&log_path) {
                return None;
            }
            // The default `deck.log` is opened with the symlink/mode hardening
            // `daemon.log` already has (`O_NOFOLLOW` + 0o600); an explicit
            // `DOT_AGENT_DECK_LOG=<path>` is user-specified and stays on the
            // plain open.
            let open_result = if is_default {
                open_deck_log_file(&log_path)
            } else {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log_path)
            };
            match open_result {
                Ok(log_file) => Some(
                    tracing_subscriber::fmt::layer()
                        .with_writer(log_file)
                        .with_ansi(false)
                        .with_filter(dot_agent_deck::logging::env_filter_from_env()),
                ),
                Err(e) => {
                    warn_log_setup_failure("open log file", &log_path, e);
                    None
                }
            }
        });
    // Issue #1981 M8: the daemon's warnings and errors over OTLP, with a
    // filter of its own; idle until the daemon starts its telemetry.
    let otlp = dot_agent_deck::telemetry::logs::otlp_log_layer();
    if file.is_none() && otlp.is_none() {
        return;
    }
    tracing_subscriber::registry().with(file).with(otlp).init();
}

/// Prepare the endpoint directory, then lazy-spawn a daemon at `endpoint` if
/// one is not already there. Returns the message to print on failure.
///
/// **Only ever called with a primary endpoint** — see
/// [`dot_agent_deck::platform::paths::ResolvedEndpoint::is_primary`].
/// `ensure_daemon_running` refuses a non-primary one itself, so this is the
/// caller-side half of one property rather than the whole of it.
///
/// The preflight is issue #1121 round two, B1. `daemon serve` calls
/// `ensure_endpoint_dir` at its own bind sites, so the directory gets created
/// either way — but when it *cannot* be created, because a foreign uid already
/// owns `<temp dir>/dot-agent-deck-{uid}`, the refusal that names the
/// directory, both uids and the override goes into the **detached daemon's
/// log**, where nobody is looking. What the operator saw was the launcher
/// waiting out `DAEMON_START_POLL_TIMEOUT` and reporting a start timeout.
/// Running the same idempotent call here first puts the actionable message on
/// their terminal instead.
///
/// Issue #1173 then closed the wedge that preflight could only report: when
/// another uid holds the per-uid directory's name,
/// `endpoint_resolve::prepare_bind_endpoints` hands back the same socket name
/// inside a relocated owner-only directory, and `endpoint` is **rewritten to
/// it** — so the poll below waits where the daemon about to be spawned will
/// bind, which runs the same call and reuses that directory. Every other
/// preparation failure still stops here with the directory-naming message.
async fn bootstrap_primary_daemon(endpoint: &mut LocalEndpoint) -> Result<(), String> {
    let resolved = dot_agent_deck::platform::paths::ResolvedEndpoint::new(
        endpoint.path().to_path_buf(),
        endpoint.source(),
    );
    let prepared =
        dot_agent_deck::endpoint_resolve::prepare_bind_endpoints(std::slice::from_ref(&resolved))
            .await
            .map(|mut paths| paths.pop().unwrap_or_else(|| endpoint.path().to_path_buf()));
    match prepared {
        Ok(bind_at) if bind_at != endpoint.path() => {
            *endpoint = LocalEndpoint::from_resolved(
                dot_agent_deck::platform::paths::ResolvedEndpoint::new(bind_at, endpoint.source()),
            );
        }
        Ok(_) => {}
        Err(source) => {
            return Err(format!(
                "cannot prepare the endpoint directory for {}: {source}",
                endpoint.path().display()
            ));
        }
    }
    ensure_external_daemon_or_die(endpoint).await.map_err(|e| {
        format!(
            "failed to connect to daemon at {}: {e}",
            endpoint.path().display()
        )
    })
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

    // Install hooks/plugins for detected agents (best-effort), driven from the
    // agent registry: each spec's OWN startup install, in `ALL` order (PRD #20
    // M2 / R20-010), so a future agent reusing `NativeHooks`/`Plugin` runs its
    // own installer. Codex starts its scoped, hash-pinned trust write here too
    // (PRD #20 §4.2.1), on a thread of its own that this does not wait for
    // (issue #1665); Pi materializes at spawn time and has no startup install.
    //
    // PRD #91: BEFORE the daemon is bootstrapped, because a lazy-spawned
    // daemon runs the same installs before it binds and would leave nothing
    // for this one to change, so the TUI could never say that it changed the
    // hooks. Whatever it changed or could not change is the note `run_tui`
    // shows as it starts. The daemon's own pass then finds nothing to do.
    // On a blocking thread, since the installers read and write files and run
    // the agents' CLIs (Qodo on PR #1925).
    let mut startup_hooks = UnshownStartupNote(
        tokio::task::spawn_blocking(dot_agent_deck::hooks_refresh::startup_install)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("startup hook install did not finish: {e}");
                None
            }),
    );

    let state = Arc::new(RwLock::new(AppState::default()));
    // PRD #741 M2: the TUI always talks to the daemon on this machine, so it
    // names it as one. `attach_path` stays the same value it always was — the
    // endpoint's address — for the messages and the subscriber below.
    //
    // Issue #1121 round two: resolved with its provenance, because what may be
    // done to the address depends on which arm produced it. A
    // `LegacyCompat` one names a daemon from a pre-#1121 build that answered a
    // probe a moment ago: this process may talk to it, and must not unlink it,
    // lazy-spawn at it, or poll it for a daemon that would bind somewhere else.
    let mut resolved = dot_agent_deck::endpoint_resolve::client_attach_endpoint();
    // Closes the window between the resolver's own probe and this point: the
    // older daemon exits in between. Re-resolving to the primary endpoint is
    // what turns that into an ordinary cold start instead of a 15-second wait
    // for a daemon at an address nothing will ever bind.
    //
    // It does not close every such window, and an earlier version of this
    // comment claimed it was "the one" — a *later* one is still open, between
    // this probe and the handshake's own `connect`, and is recovered at the
    // handshake call below rather than here. No probe can be the last word:
    // each one only narrows the gap between the last check and the first use.
    if !resolved.is_primary()
        && !dot_agent_deck::endpoint_resolve::endpoint_is_answering(resolved.path())
    {
        resolved = dot_agent_deck::endpoint_resolve::primary_attach_endpoint();
    }
    let mut endpoint = LocalEndpoint::from_resolved(resolved);
    let mut attach_path = endpoint.path().to_path_buf();

    // If the attach socket is missing, `bootstrap_primary_daemon` fork-execs
    // `dot-agent-deck daemon serve` detached under flock-serialized contention
    // (so two simultaneous TUIs can't both win the bind — M1.3) and
    // trust-checks any existing socket (uid + 0o600 + is-socket) before the
    // TUI's DaemonClient touches it. Skipped outright for a legacy endpoint,
    // which by construction already has a daemon answering on it.
    if endpoint.is_primary() {
        if let Err(message) = bootstrap_primary_daemon(&mut endpoint).await {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
        attach_path = endpoint.path().to_path_buf();
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
    //     non-zero.
    // Issue #405: a daemon on another attach protocol is never attached to —
    // a build-id match or a declined restart exits non-zero with a refusal
    // instead (`ProtocolMismatch`); the restart arms above are unchanged.
    // Errors are already user-visible inside the helper, so we render no
    // further message here.
    let mut handshake = build_version_handshake::ensure_compatible_daemon_or_die(&endpoint).await;
    // Issue #1121: the window the probe above leaves open, one step further
    // along — a legacy daemon that exits between that probe and the
    // handshake's own `connect`. `Probe` is exactly that shape ("the socket is
    // there but nothing is answering"), and on a non-primary endpoint the
    // address can only be the pre-#1121 spelling of a daemon that has now
    // gone. Left alone it is a hard `ExitCode::FAILURE` on a host whose only
    // problem is that it has no daemon — the same class as a legacy endpoint
    // reaching an operation that cannot cope with it, so it gets the same
    // remedy the `Recovered` branch below uses: re-resolve to the endpoint the
    // daemon we are about to spawn will actually bind, and cold-start there.
    //
    // Deliberately one retry and not a loop. The retried endpoint IS primary,
    // so a second `Probe` there is a genuinely broken host rather than a
    // transition artefact, and must still be reported. The handshake is re-run
    // rather than assumed to match, for the reason the comment above gives: it
    // is the smoke test of the handshake itself, and a stale daemon of another
    // build may be sitting on the primary endpoint too — that outcome is
    // `Recovered` and composes with the branch below.
    if !endpoint.is_primary()
        && matches!(
            handshake,
            Err(build_version_handshake::HandshakeError::Probe(_))
        )
    {
        endpoint = LocalEndpoint::from_resolved(
            dot_agent_deck::endpoint_resolve::primary_attach_endpoint(),
        );
        if let Err(message) = bootstrap_primary_daemon(&mut endpoint).await {
            eprintln!("after the discovered daemon went away: {message}");
            return ExitCode::FAILURE;
        }
        attach_path = endpoint.path().to_path_buf();
        handshake = build_version_handshake::ensure_compatible_daemon_or_die(&endpoint).await;
    }
    let handshake_outcome = match handshake {
        Ok(outcome) => outcome,
        // Both already printed their message inside the helper; issue #405's
        // protocol refusal left the daemon and its agents running.
        Err(
            build_version_handshake::HandshakeError::MismatchAborted
            | build_version_handshake::HandshakeError::ProtocolMismatch { .. },
        ) => {
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
    ) {
        // Issue #1121: re-resolve before re-spawning, and to the **primary**
        // endpoint specifically. The daemon we just SIGTERM'd may have been an
        // older build reached through the compatibility read of the pre-#1121
        // fallback endpoint; nothing answers there any more, and the daemon
        // about to be spawned binds the new spelling. Polling the address of
        // the daemon we killed would time out while a perfectly healthy
        // replacement was already up.
        endpoint = LocalEndpoint::from_resolved(
            dot_agent_deck::endpoint_resolve::primary_attach_endpoint(),
        );
        if let Err(message) = bootstrap_primary_daemon(&mut endpoint).await {
            eprintln!("after version-mismatch recovery: {message}");
            return ExitCode::FAILURE;
        }
        attach_path = endpoint.path().to_path_buf();
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

    // Issue #1635: check for a newer release at start and every re-check
    // interval, and plan this machine's copies, for the footer badge and the
    // upgrade dialog.
    tokio::spawn(dot_agent_deck::upgrade_dialog::run_checker(state.clone()));

    let config = dot_agent_deck::config::DashboardConfig::load();

    // PRD #40: resolve keybindings client-side, *before* entering the
    // alternate screen, so any malformed-config / conflict / unknown-action
    // warnings land on stderr in the normal terminal (and, under a PTY, in
    // the byte stream that precedes the alt-screen switch) where they are
    // actually visible. `run_tui` (via `ratatui::init`) is what flips into
    // the alt-screen, so loading here keeps the warnings ahead of it.
    let keybindings = dot_agent_deck::keybindings::KeybindingConfig::load();

    let pane_controller: Arc<dyn PaneController> = Arc::new(
        EmbeddedPaneController::new(attach_path.clone(), tokio::runtime::Handle::current())
            .with_protocol_gate()
            // Fork issue #36: hydration waits on this gate before snapshotting
            // and releases it once every rebuilt pane is seeded.
            .with_hydration_gate(hydration_gate),
    );
    let tui_state = state.clone();
    let startup_note = startup_hooks.0.take();
    let tui_result = tokio::task::spawn_blocking(move || {
        run_tui(
            tui_state,
            pane_controller,
            config,
            keybindings,
            startup_note,
        )
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

/// PRD #91: the startup hook install's note until `run_tui` takes it. A launch
/// that returns before the TUI opens (a failed bootstrap or handshake) drops
/// it here instead, and what the user has to fix goes to stderr, as the TUI
/// would have printed it on exit (Qodo on PR #1925).
struct UnshownStartupNote(Option<dot_agent_deck::hooks_refresh::StartupNote>);

impl Drop for UnshownStartupNote {
    fn drop(&mut self) {
        for warning in self.0.take().map(|note| note.warnings).unwrap_or_default() {
            eprintln!("{warning}");
        }
    }
}

/// PRD #76 M2.17: spawn the TUI's event subscriber — see
/// [`dot_agent_deck::event_subscriber`].
///
/// Fork issue #36: spawned through `run_gated` with the controller's `gate`, so
/// the subscription is confirmed before hydration snapshots and the stream's
/// messages are held back until hydration has seeded its cards. Upstream's
/// `run` is the same loop with a gate that is already open.
fn spawn_event_subscriber(
    attach_path: std::path::PathBuf,
    state: dot_agent_deck::state::SharedState,
    gate: HydrationGate,
) {
    use dot_agent_deck::event_subscriber::{SubscriberConfig, run_gated};

    let config = SubscriberConfig {
        #[cfg(feature = "e2e")]
        drop_event: Some(e2e_subscriber_drops),
        #[cfg(feature = "e2e")]
        break_on_event: Some(e2e_subscriber_breaks),
        #[cfg(feature = "e2e")]
        applied: Some(e2e_subscriber_applied),
        ..SubscriberConfig::default()
    };
    // PRD #1666 part 2 (`lifecycle/handshake/011`): every subscription,
    // listing included, rides a connection whose own `Hello` passed.
    tokio::spawn(run_gated(
        DaemonClient::new(attach_path).with_protocol_gate(),
        state,
        config,
        gate,
    ));
}

/// Issue #621 e2e seam: make this subscriber miss a conversation's events, the
/// way it did when they arrived while it was reconnecting — it resubscribed
/// without replaying what it missed, so the daemon knew the conversation and
/// the TUI never learned it. (Since issue #1520 a reconnect re-reads the
/// daemon's state, so this seam now models a gap the subscriber does not see,
/// which the daemon's `stale` refusal still covers.) A reconnect cannot be timed against an agent's boot
/// from a PTY test, so the test names a session-id prefix in
/// `DOT_AGENT_DECK_E2E_DROP_SESSION_EVENTS` instead, and `prompt/pane-input/044`
/// asserts the prompt is still delivered.
///
/// EVERY event of that conversation is dropped, not only its `SessionStart`:
/// any later frame carrying the session id — the daemon's own `ShellIdle` for a
/// pane that went quiet, measured arriving right behind the start — would
/// establish the generation in the TUI's view on its own, so a gap that misses
/// the start alone recovers by itself. The failure needs the gap to cover the
/// boot, and this models exactly that.
///
/// Compiled only into the `e2e` build, like `effective_current_exe`'s seam in
/// `platform/paths.rs`, so a shipped binary has no switch that discards events.
#[cfg(feature = "e2e")]
fn e2e_subscriber_drops(event: &dot_agent_deck::event::AgentEvent) -> bool {
    std::env::var("DOT_AGENT_DECK_E2E_DROP_SESSION_EVENTS")
        .is_ok_and(|prefix| !prefix.is_empty() && event.session_id.starts_with(&prefix))
}

/// Issue #1520 e2e seam: tear this subscriber's stream down ONCE, losing the
/// event that triggered it, the way a `KIND_STREAM_END "lagged"` loses whatever
/// the daemon never forwarded — so a PTY test can make the deck reconnect at a
/// moment it chooses, which neither a real lag (1024 unread broadcasts) nor a
/// daemon restart (which takes the agents with it) gives it.
/// `DOT_AGENT_DECK_E2E_BREAK_STREAM_ON` names `<pane id>/<EventType>`, the
/// event type spelled as its `Debug` name (`Thinking`); `session/live/019`
/// uses it. Compiled only into the `e2e` build, like the seam above.
#[cfg(feature = "e2e")]
fn e2e_subscriber_breaks(event: &dot_agent_deck::event::AgentEvent) -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};
    static FIRED: AtomicBool = AtomicBool::new(false);
    let Ok(target) = std::env::var("DOT_AGENT_DECK_E2E_BREAK_STREAM_ON") else {
        return false;
    };
    let Some((pane, kind)) = target.split_once('/') else {
        return false;
    };
    event.pane_id.as_deref() == Some(pane)
        && format!("{:?}", event.event_type) == kind
        && !FIRED.swap(true, Ordering::SeqCst)
}

/// Reviewer R9T-05 e2e seam: when `DOT_AGENT_DECK_E2E_APPLIED_LOG` names a
/// file, append `<pane id>\t<EventType>` to it for each event this subscriber
/// has applied to the TUI's state, so a PTY test can hold a step until an
/// event is in that state rather than merely on the socket
/// (`newagent/visibility/001`'s attach race). Compiled only into the `e2e`
/// build, like the seams above.
#[cfg(feature = "e2e")]
fn e2e_subscriber_applied(event: &dot_agent_deck::event::AgentEvent) {
    let Some(path) = std::env::var_os("DOT_AGENT_DECK_E2E_APPLIED_LOG") else {
        return;
    };
    let line = format!(
        "{}\t{:?}\n",
        event.pane_id.as_deref().unwrap_or(""),
        event.event_type
    );
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = std::io::Write::write_all(&mut file, line.as_bytes());
    }
}

/// PRD #345: `remote doctor <name>`. Resolves the registry entry FIRST so an
/// unknown name costs zero ssh invocations, then runs the read-only probes and
/// prints one line per check.
///
/// **Three exit codes**, so the outcomes a script has to treat differently are
/// distinguishable:
///
/// - **0** — clear. Every check PASSed, or at most raised an advisory WARN.
/// - **1** — a check FAILed, or the command could not run at all (an unknown
///   registry name, an unreadable registry).
/// - **2** — incomplete: no FAIL, but at least one check is UNKNOWN.
///
/// Both non-zero codes keep the PRD's promise that an UNKNOWN never reads as
/// PASS. Separating them makes the single most common real-world outcome — a
/// healthy tunnel on a host where `sshd -T` needs root you do not have — a
/// stable, scriptable `2` rather than something indistinguishable from a
/// broken tunnel. See [`dot_agent_deck::remote_doctor::Verdict::exit_code`].
fn run_remote_doctor(name: &str) -> ExitCode {
    let registry_path = dot_agent_deck::remote::default_remotes_path();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    match dot_agent_deck::remote_doctor::run_doctor(name, &registry_path, &mut out) {
        Ok(verdict) => {
            let _ = out.flush();
            ExitCode::from(verdict.exit_code())
        }
        Err(e) => {
            let _ = out.flush();
            eprintln!("{e}");
            // The diagnosis never started, so there is no verdict to map. `1`
            // rather than `2`: the command itself failed, which is a different
            // thing from a diagnosis that ran and could not see everything.
            ExitCode::FAILURE
        }
    }
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

/// `dot-agent-deck daemon endpoint` — the remote half of ssh endpoint
/// discovery (issue #1174).
///
/// # What it is for
///
/// The desktop forwards a remote deck's attach socket with `ssh -L`, and to do
/// that it must first learn *where* that socket is: OpenSSH expands neither `~`
/// nor an environment variable on the remote side of `-L`, and the far host's
/// `XDG_RUNTIME_DIR` and uid are not knowable from this end. Discovery used to
/// be a shell snippet ([`dot_agent_deck::remote_tunnel::REMOTE_SOCKET_PROBE`])
/// that restated [`attach_socket_path`]'s rules in `sh` and picked a candidate
/// with filesystem tests alone. Two consequences, both of which this command
/// exists to remove:
///
/// 1. **Nothing connected**, so a stale inode left behind by a dead daemon was
///    selected exactly as a live one was, and the forward then came up against
///    a socket nobody was listening on.
/// 2. **The rule had two implementations** — this program's and a shell
///    restatement of it — under a doc-comment obligation to keep them in step
///    that nothing enforced.
///
/// Running the installed binary over there answers from *inside* the same
/// implementation instead. The snippet stays as the fallback for a host whose
/// binary is older than this subcommand or is not installed at all, so
/// discovery still works against those; that fallback is what keeps this
/// additive.
///
/// # What printing a path asserts, and what it does not
///
/// It asserts exactly three things, each checked here:
///
/// - the path is the one [`attach_socket_path`] resolves, which is the one this
///   build's daemon calls `bind(2)` on — the same function, not a copy of it;
/// - it passes [`dot_agent_deck::platform::fsperm::verify_endpoint_trusted`]:
///   not a symlink, a socket, owned by this uid, at mode `0o600`. The snippet
///   tested none of the mode and (before #1121) none of the ownership;
/// - a live listener completed an attach-protocol `Hello` within
///   [`ENDPOINT_RESOLVE_TIMEOUT`].
///
/// # Three exit codes, because the caller acts on the difference
///
/// The probe treats a refusal from *this* command differently from a host that
/// could not run it, and that distinction is the whole reason the checks above
/// bind rather than merely advise (PR #1191 review, P1):
///
/// - **`0`** — a trusted, live endpoint. The path is on stdout.
/// - **[`ENDPOINT_UNTRUSTED`] (`1`)** — something is at the path and it failed
///   a trust check. The probe **stops**: falling through would print the same
///   path unchecked and forward it, which would make every check here
///   advisory. This is the code a `0666` listener earns.
/// - **[`ENDPOINT_UNDETERMINED`] (`3`)** — nothing is at the path, or nothing
///   answered, or the handshake timed out. Nothing was *learned*, so the probe
///   falls through to its remaining candidates and then to the shell rungs.
///   Keeping this separate from `1` is what stops a dead first candidate from
///   hiding a live second one, and what leaves a version-skewed daemon to be
///   classified by the desktop rather than reported as a missing socket.
///
/// Neither collides with clap's own `2` (an older build that does not know the
/// subcommand) or a shell's `126`/`127`, which are the "this host cannot run
/// it" codes the probe must also fall through on.
///
/// **It does not authenticate the listener**, and no claim here should be read
/// as saying it does. A `Hello` proves something is listening and speaks this
/// wire; it does not prove that something is the deck's daemon. Against a
/// foreign uid the three checks bite — with ordinary permissions on the paths
/// involved, that uid cannot satisfy the ownership check, cannot write the
/// install path this rung runs from, and (under an owner-only endpoint
/// directory) cannot create the entry at all. Against an attacker already
/// running **as the remote login user** they do not: that actor can bind a
/// `0o600` socket of
/// their own at the path, and can replace the binary this very command runs
/// from. Closing *that* needs the remote end to assert an identity in-band,
/// which is issue #1189 — see `docs/develop/remote-endpoint-discovery.md`.
///
/// # Wire cost: none
///
/// The round trip is an existing [`dot_agent_deck::daemon_protocol::AttachRequest::Hello`]
/// via [`DaemonClient::capabilities`], so this command puts nothing new on the
/// wire and owes no `PROTOCOL_VERSION` bump — the same reasoning
/// `daemon status` records (issue #459). An older daemon answers a newer CLI's
/// `Hello` exactly as it always did.
#[tokio::main]
async fn run_daemon_endpoint_cli() -> ExitCode {
    // Issue #1173: the pure resolution, or a relocated endpoint a daemon of
    // this build is answering at because the per-uid directory was taken.
    let path = dot_agent_deck::endpoint_resolve::served_attach_endpoint().into_path();
    let shown = path.display();

    // The endpoint path can come from `DOT_AGENT_DECK_ATTACH_SOCKET` in the
    // remote login environment, and the caller reads our stdout line by line.
    // A control byte in the path would let that environment choose which line
    // the caller believes, so refuse to print one rather than emit a value
    // whose framing we do not control. The local `RemoteSocketPath` parser
    // refuses these too; this is the same refusal one hop earlier, where the
    // reason can still be reported.
    if shown.to_string().contains(|c: char| c.is_control()) {
        eprintln!("daemon endpoint: the endpoint path contains a control character");
        return ExitCode::from(ENDPOINT_UNTRUSTED);
    }

    // Split "there is nothing here" from "there is something here and it is
    // wrong" BEFORE running the trust check, because the two earn different
    // exit codes and `verify_endpoint_trusted` folds them into one `String`.
    // An absent endpoint is ordinary — the deck over there is simply not
    // running — and must not stop the probe trying its other candidates.
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(source) => {
            eprintln!("daemon endpoint: nothing at {shown}: {source}");
            return ExitCode::from(ENDPOINT_UNDETERMINED);
        }
    }

    if let Err(reason) = dot_agent_deck::platform::fsperm::verify_endpoint_trusted(&path) {
        eprintln!("daemon endpoint: {shown} is not a trusted endpoint: {reason}");
        return ExitCode::from(ENDPOINT_UNTRUSTED);
    }

    // A `Hello` over the existing wire. Bounded, because a listener that
    // accepts and then says nothing would otherwise hold the ssh probe open
    // until its own 20s deadline killed it, turning "the deck is not running"
    // into "the host timed out".
    let client = DaemonClient::new(path.clone());
    match tokio::time::timeout(ENDPOINT_RESOLVE_TIMEOUT, client.capabilities()).await {
        Ok(Ok(_capabilities)) => {}
        // Every handshake failure is UNDETERMINED rather than UNTRUSTED, and
        // deliberately so. A refused connect is a stale inode or a daemon that
        // is not running; a refused `Hello` is usually version skew, which the
        // desktop names far better (`HandshakeRefused`, `ProtocolRefused`) once
        // the socket is forwarded than this command could. Both are reasons to
        // keep looking, not grounds to call the endpoint hostile.
        Ok(Err(e)) => {
            eprintln!("daemon endpoint: nothing usable answered at {shown}: {e}");
            return ExitCode::from(ENDPOINT_UNDETERMINED);
        }
        Err(_elapsed) => {
            eprintln!(
                "daemon endpoint: the listener at {shown} did not complete a handshake within {}s",
                ENDPOINT_RESOLVE_TIMEOUT.as_secs()
            );
            return ExitCode::from(ENDPOINT_UNDETERMINED);
        }
    }

    println!("{shown}");
    ExitCode::SUCCESS
}

/// `dot-agent-deck remote upgrade <name> [--version V] [--no-install] [--json]`
/// (PRD #1487): install and restart through
/// [`dot_agent_deck::daemon_upgrade::upgrade_daemon`], the function every
/// client shares. The restart question is asked only when stdin and stdout are
/// both terminals; otherwise no one can answer, and live work keeps the
/// running daemon. With `--json`, stdout carries only the outcome. Exits
/// non-zero only for a failed stage.
fn run_remote_upgrade(name: &str, version: String, no_install: bool, json: bool) -> ExitCode {
    use dot_agent_deck::daemon_upgrade::{
        NoDecider, RestartDecider, SshInstaller, TtyDecider, UpgradePlan, upgrade_daemon,
        upgrade_ssh_executor,
    };
    use dot_agent_deck::remote::{RemoteUpgradeError, RemotesFile};
    use std::io::{IsTerminal, Write};

    let path = dot_agent_deck::remote::default_remotes_path();
    // Unknown names and an unreadable registry fail before any ssh, as before.
    let entry = match RemotesFile::load(&path) {
        Ok(file) => file.remotes.into_iter().find(|r| r.name == name),
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(entry) = entry else {
        eprintln!(
            "{}",
            RemoteUpgradeError::UnknownName {
                name: name.to_string()
            }
        );
        return ExitCode::FAILURE;
    };

    // What is said along the way goes to stdout, or to stderr under `--json`
    // so stdout holds the outcome alone.
    let report: fn() -> Box<dyn Write> = if json {
        || Box::new(std::io::stderr())
    } else {
        || Box::new(std::io::stdout())
    };
    // The row read above, for the install as for the port, so both reach
    // the same machine even if the row changes meanwhile.
    let mut installer = SshInstaller::for_entry(&entry, path.clone(), report());
    installer.no_install = no_install;
    let port =
        dot_agent_deck::remote_daemon::SshDaemonPort::for_entry(upgrade_ssh_executor(), &entry);
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let tty;
    let decider: &dyn RestartDecider = if interactive {
        tty = TtyDecider::new(std::io::stdin().lock(), report());
        &tty
    } else {
        &NoDecider
    };
    let plan = UpgradePlan {
        version,
        successor: dot_agent_deck::daemon_protocol::RestartSuccessor::Installed,
    };
    let mut progress_out = report();
    let outcome = upgrade_daemon(name, &plan, &installer, &port, decider, &mut |p| {
        dot_agent_deck::connect::print_upgrade_progress(name, &p, &mut *progress_out)
    });

    if json {
        match serde_json::to_string(&outcome) {
            Ok(line) => println!("{line}"),
            Err(e) => {
                eprintln!("remote upgrade: could not encode the outcome: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else if outcome.is_failure() {
        eprintln!("{}", outcome.summary(name));
    } else {
        println!("{}", outcome.summary(name));
    }
    if outcome.is_failure() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// `dot-agent-deck daemon probe [--json]` (PRD #1487, hidden plumbing). One
/// bounded `Hello` against the running daemon at this host's endpoint, through
/// [`DaemonClient::probe_running`], which never lazy-spawns. "Nothing running"
/// is an answer (exit 0, `running: false`); a probe that learned nothing — a
/// transport error or no reply within [`ENDPOINT_RESOLVE_TIMEOUT`] — is a
/// failure on stderr. Puts nothing new on the wire.
#[tokio::main]
async fn run_daemon_probe_cli(json: bool) -> ExitCode {
    use dot_agent_deck::daemon_restart::DaemonProbe;
    let client = DaemonClient::new(client_attach_socket_path());
    let hello = match tokio::time::timeout(ENDPOINT_RESOLVE_TIMEOUT, client.probe_running()).await {
        Ok(Ok(hello)) => hello,
        Ok(Err(e)) => {
            eprintln!("daemon probe: {e}");
            return ExitCode::FAILURE;
        }
        Err(_elapsed) => {
            eprintln!(
                "daemon probe: no handshake within {}s",
                ENDPOINT_RESOLVE_TIMEOUT.as_secs()
            );
            return ExitCode::FAILURE;
        }
    };
    let probe = DaemonProbe {
        running: hello.is_some(),
        hello,
        confirm_stdin: true,
    };
    if json {
        match serde_json::to_string(&probe) {
            Ok(line) => println!("{line}"),
            Err(e) => {
                eprintln!("daemon probe: could not encode the result: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        match probe
            .hello
            .as_ref()
            .and_then(|h| h.daemon_version.as_deref())
        {
            Some(v) => println!("running: {v}"),
            None if probe.running => println!("running"),
            None => println!("not running"),
        }
    }
    ExitCode::SUCCESS
}

/// `dot-agent-deck daemon restart-installed [--json] [--expect-version V]
/// [--confirm-hex H | --confirm-stdin]` (PRD #1487, hidden plumbing). Asks the
/// running daemon at this host's endpoint to restart onto the build installed
/// at its own path, through [`DaemonClient::restart_daemon`] — the verb's only
/// sender, which withholds it from a daemon that does not advertise it.
/// Prints one [`dot_agent_deck::daemon_restart::RemoteRestartReport`]: no daemon running,
/// the daemon too old for the verb, or the daemon's reply (accepted, needs
/// confirmation, refused) — all exit 0, because each is an answer. A failure
/// is on stderr, and its exit code says whether the request had been sent:
/// [`dot_agent_deck::daemon_restart::RESTART_NOT_SENT_EXIT`] before it,
/// [`dot_agent_deck::daemon_restart::RESTART_UNANSWERED_EXIT`] after it.
#[tokio::main]
async fn run_daemon_restart_installed_cli(
    json: bool,
    expect_version: Option<String>,
    confirm_hex: Option<String>,
    confirm_stdin: bool,
) -> ExitCode {
    use dot_agent_deck::daemon_client::{ClientError, GatedQuery, RestartDaemonRequest};
    use dot_agent_deck::daemon_protocol::{RestartDaemonReply, RestartSuccessor};
    use dot_agent_deck::daemon_restart::{
        CONFIRM_STDIN_TIMEOUT, RESTART_NOT_SENT_EXIT, RESTART_UNANSWERED_EXIT, RemoteRestartReport,
        decode_stop_set_hex, read_stop_set,
    };

    // Read before the daemon is asked anything, so a malformed set is refused
    // with nothing sent. The read blocks until stdin closes, so it runs on a
    // plain thread, not a runtime worker, under a bound of its own; a thread
    // still blocked when the bound passes ends with the process (Qodo
    // 4236427318).
    let confirm = if confirm_stdin {
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let _ = tx.send(read_stop_set(std::io::stdin().lock()));
        });
        match tokio::time::timeout(CONFIRM_STDIN_TIMEOUT, rx).await {
            Ok(Ok(read)) => read.map(Some),
            Ok(Err(_)) => Err("the stdin reader stopped without an answer".to_string()),
            Err(_) => Err(format!(
                "stdin did not close within {}s",
                CONFIRM_STDIN_TIMEOUT.as_secs()
            )),
        }
        .map_err(|e| format!("--confirm-stdin: {e}"))
    } else {
        confirm_hex
            .as_deref()
            .map(decode_stop_set_hex)
            .transpose()
            .map_err(|e| format!("--confirm-hex: {e}"))
    };
    let confirm = match confirm {
        Ok(confirm) => confirm,
        Err(e) => {
            eprintln!("daemon restart-installed: {e}");
            return ExitCode::from(RESTART_NOT_SENT_EXIT);
        }
    };
    let client = DaemonClient::new(client_attach_socket_path());
    match tokio::time::timeout(ENDPOINT_RESOLVE_TIMEOUT, client.probe_running()).await {
        Ok(Ok(Some(_))) => {}
        Ok(Ok(None)) => {
            return print_restart_report(
                json,
                &RemoteRestartReport {
                    running: false,
                    reply: None,
                    unsupported: false,
                },
            );
        }
        Ok(Err(e)) => {
            eprintln!("daemon restart-installed: {e}");
            return ExitCode::from(RESTART_NOT_SENT_EXIT);
        }
        Err(_elapsed) => {
            eprintln!(
                "daemon restart-installed: no handshake within {}s",
                ENDPOINT_RESOLVE_TIMEOUT.as_secs()
            );
            return ExitCode::from(RESTART_NOT_SENT_EXIT);
        }
    }
    let request = RestartDaemonRequest {
        confirm,
        expected_version: expect_version,
        successor: RestartSuccessor::Installed,
    };
    let report = match client.restart_daemon(request).await {
        Ok(GatedQuery::Answered(reply)) => RemoteRestartReport {
            running: true,
            reply: Some(reply),
            unsupported: false,
        },
        Ok(GatedQuery::Unsupported) => RemoteRestartReport {
            running: true,
            reply: None,
            unsupported: true,
        },
        // Sent, then no answer: the daemon may be restarting, which the
        // caller has to be able to tell from a request that never went out.
        Err(e @ ClientError::Unanswered(_)) => {
            eprintln!("daemon restart-installed: {e}");
            return ExitCode::from(RESTART_UNANSWERED_EXIT);
        }
        Err(e) => {
            eprintln!("daemon restart-installed: {e}");
            return ExitCode::from(RESTART_NOT_SENT_EXIT);
        }
    };
    if !json {
        // A short human line; the JSON form is what callers parse.
        let line = match &report.reply {
            Some(RestartDaemonReply::Accepted { to_version, .. }) => format!(
                "restart accepted{}",
                to_version
                    .as_deref()
                    .map(|v| format!(" (onto {v})"))
                    .unwrap_or_default()
            ),
            Some(RestartDaemonReply::NeedsConfirmation { at_stake, .. }) => format!(
                "needs confirmation: {} agent(s), {} orchestration role(s) would be stopped",
                at_stake.agents.len(),
                at_stake.roles.len()
            ),
            Some(RestartDaemonReply::Refused { message, .. }) => format!("refused: {message}"),
            None => "the running daemon is too old to restart this way".to_string(),
        };
        println!("{line}");
        return ExitCode::SUCCESS;
    }
    // The daemon has answered: a report that cannot be printed is a lost
    // reply, not a request that was never sent.
    match print_restart_report(json, &report) {
        code if code == ExitCode::SUCCESS => code,
        _ => ExitCode::from(RESTART_UNANSWERED_EXIT),
    }
}

/// Print a [`dot_agent_deck::daemon_restart::RemoteRestartReport`] as one JSON
/// line (or a short human line without `--json`).
fn print_restart_report(
    json: bool,
    report: &dot_agent_deck::daemon_restart::RemoteRestartReport,
) -> ExitCode {
    if !json {
        println!("no daemon running");
        return ExitCode::SUCCESS;
    }
    match serde_json::to_string(report) {
        Ok(line) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("daemon restart-installed: could not encode the result: {e}");
            ExitCode::FAILURE
        }
    }
}

/// [`run_daemon_endpoint_cli`]: something is at the endpoint path and it failed
/// a trust check, so the caller must **not** keep looking and must not forward
/// it. Distinct from clap's `2` so an older build is never mistaken for one.
const ENDPOINT_UNTRUSTED: u8 = 1;

/// [`run_daemon_endpoint_cli`]: nothing was learned — no inode, nothing
/// answering, or a handshake that timed out — so the caller should try its
/// remaining candidates. Deliberately not `2` (clap's) and not `126`/`127` (a
/// shell's), which mean the same thing for a different reason.
const ENDPOINT_UNDETERMINED: u8 = 3;

/// How long [`run_daemon_endpoint_cli`] waits for a listener to complete the
/// `Hello`.
///
/// Comfortably inside `remote_tunnel`'s own 20s probe deadline, so a wedged
/// listener on the far host is reported by *this* command as "no daemon here"
/// and the snippet's fallback still gets its turn, rather than the whole ssh
/// probe being killed from the near side with nothing learned. Matches
/// `daemon_status::STATUS_REQUEST_TIMEOUT`, which bounds the same kind of
/// single round trip against the same daemon.
const ENDPOINT_RESOLVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

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
/// Never spawns a daemon, never sends a mutation request, and never retries: a
/// timeout abandons the query rather than looping. "Otherwise perturbs the
/// daemon" is what this used to claim and is now too strong (issue #1121 round
/// two, N11) — resolving the endpoint through `client_attach_socket_path` makes
/// a successful connection of its own before the status connection, so on a
/// host in the fallback case the daemon sees one extra accept and one extra
/// client. No protocol byte is written on it, but it is observable in timings
/// such as idle observation, and a comment that says otherwise is read as a
/// guarantee.
#[tokio::main]
async fn run_daemon_status_cli(json: bool) -> ExitCode {
    use dot_agent_deck::daemon_status::{
        STATUS_REQUEST_TIMEOUT, StatusDocument, build_status_agents_with_delegations, format_human,
    };

    let client = DaemonClient::new(client_attach_socket_path());
    let (records, delegations) =
        match tokio::time::timeout(STATUS_REQUEST_TIMEOUT, client.list_agents_full()).await {
            Ok(Ok((listing, delegations))) => (listing.records, delegations),
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

    if delegations.malformed {
        // Not on stdout (`--json` must stay clean); `daemon status` sets up no
        // logging, so a `warn!` would go nowhere.
        eprintln!(
            "daemon status: the daemon's outstanding-delegation list was malformed and ignored; \
             an orchestrator with a delegation on a pane without a live agent may read Idle"
        );
    }
    let agents = build_status_agents_with_delegations(records, &delegations.entries);
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

/// `dot-agent-deck close` — PRD #1589. Connect-only: it never starts a daemon,
/// because closing agents on a daemon it just started would close nothing. The
/// request building and rendering live in [`dot_agent_deck::close_cli`].
#[tokio::main]
async fn run_close_cli(args: dot_agent_deck::close_cli::CloseArgs) -> ExitCode {
    let identity = match dot_agent_deck::close_cli::ambient_identity() {
        Ok(identity) => identity,
        Err(message) => {
            let output = dot_agent_deck::close_cli::identity_error_output(args.json, message);
            print!("{}", output.stdout);
            eprint!("{}", output.stderr);
            return ExitCode::from(output.code);
        }
    };
    let client = DaemonClient::new(client_attach_socket_path());
    let output = dot_agent_deck::close_cli::run_close(&client, &args, &identity).await;
    print!("{}", output.stdout);
    eprint!("{}", output.stderr);
    ExitCode::from(output.code)
}

/// `--folder`, made absolute against this shell's cwd and left otherwise as
/// spelled: the daemon canonicalises it.
fn absolute_address(
    folder: Option<String>,
    orchestration: Option<String>,
    name: Option<String>,
    pane: Option<String>,
) -> dot_agent_deck::orchestrator_address::MessageAddress {
    let cwd = std::env::current_dir().unwrap_or_default();
    dot_agent_deck::orchestrator_address::MessageAddress {
        folder: folder.map(|f| dot_agent_deck::roles_cli::absolute_folder(&f, &cwd)),
        orchestration,
        name,
        pane,
    }
}

fn print_cli_output(output: dot_agent_deck::roles_cli::CliOutput) -> ExitCode {
    print!("{}", output.stdout);
    eprint!("{}", output.stderr);
    ExitCode::from(output.code)
}

/// `dot-agent-deck roles` — issue #1947. See [`dot_agent_deck::roles_cli`].
#[tokio::main]
async fn run_roles_cli(
    json: bool,
    address: dot_agent_deck::orchestrator_address::MessageAddress,
) -> ExitCode {
    let client = DaemonClient::new(client_attach_socket_path());
    print_cli_output(dot_agent_deck::roles_cli::run_roles(&client, json, &address).await)
}

/// `dot-agent-deck inbox send` — issue #1947. Connect-only, like `close`:
/// messaging an orchestrator on a daemon this call just started would reach
/// nobody.
#[tokio::main]
async fn run_inbox_send_cli(
    address: dot_agent_deck::orchestrator_address::MessageAddress,
    body: InboxBody,
) -> ExitCode {
    let identity = match dot_agent_deck::close_cli::ambient_identity() {
        Ok(identity) => identity,
        Err(message) => {
            let message = message
                .replace("run `close`", "run `inbox send`")
                .replace("Nothing was closed.", "Nothing was sent.");
            eprintln!("{message}");
            return ExitCode::from(dot_agent_deck::close_cli::EXIT_REFUSED);
        }
    };
    let text = match dot_agent_deck::roles_cli::read_body(body.text, body.file, std::io::stdin()) {
        Ok(text) => text,
        Err(why) => {
            eprintln!("refused: {why}; nothing was sent.");
            return ExitCode::from(dot_agent_deck::close_cli::EXIT_REFUSED);
        }
    };
    let client = DaemonClient::new(client_attach_socket_path());
    print_cli_output(dot_agent_deck::roles_cli::run_send(&client, &identity, address, text).await)
}

#[tokio::main]
async fn run_daemon_install_cli() -> ExitCode {
    match dot_agent_deck::daemon_service::run_install().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("daemon install: {e}");
            ExitCode::FAILURE
        }
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
        is_mine, owner_disagreements, resolve_human_owner, sanitize_marker_creator,
        shallow_repo_warning,
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
            // Issue #300 blocker 1: an absent env var must not immediately
            // refuse -- a human caller at an interactive terminal has no
            // reason to export `DOT_AGENT_DECK_WORKTREE_OWNER` at all, and
            // `resolve_worktree_owner` already treats every unmarked row's
            // owner this same way (this is the seam it uses). Fall back to
            // that resolution -- but ONLY when this process is not itself
            // running inside an orchestration pane. `docs/orchestration.md`
            // documents cases where a legitimate orchestration pane
            // legitimately carries no `DOT_AGENT_DECK_WORKTREE_OWNER`
            // (notably a tab reattached to a warm daemon, whose metadata is
            // rebuilt without the saved identity) and promises `--mine`
            // fails loudly there rather than guessing. `DOT_AGENT_DECK_PANE_ID`
            // is set in every orchestration pane regardless (see its own doc
            // comment), so its absence is what actually distinguishes "a
            // human at an interactive terminal" (issue #300's reported case)
            // from "an orchestration pane that lost its identity" (the case
            // the fail-loud contract exists for). Only refuse once BOTH the
            // env var and human-identity resolution have failed.
            //
            // `var_os(...).is_none()`, not `var(...).is_err()`: the latter
            // is also true for `VarError::NotUnicode`, which would treat a
            // non-UTF-8 `DOT_AGENT_DECK_PANE_ID` as "absent" and fall
            // through to the human-identity fallback -- the fail-open
            // direction this gate exists to prevent -- and is also false
            // for `Ok("")`, which `var_os` correctly still reports present.
            Err(std::env::VarError::NotPresent)
                if std::env::var_os(DOT_AGENT_DECK_PANE_ID).is_none() =>
            {
                match resolve_human_owner().identity_string() {
                    Some(identity) => Some(identity),
                    None => {
                        eprintln!(
                            "worktree list --mine: {DOT_AGENT_DECK_WORKTREE_OWNER} is not set, \
                             and this caller's identity could not be resolved via `gh` either -- \
                             cannot determine which worktrees belong to this orchestration or \
                             this caller; supply {DOT_AGENT_DECK_WORKTREE_OWNER}, authenticate \
                             `gh`, or drop --mine"
                        );
                        return ExitCode::FAILURE;
                    }
                }
            }
            // Issue #300 fix round (auditor M1): a `DOT_AGENT_DECK_PANE_ID`
            // present alongside an absent owner var means this IS an
            // orchestration pane that simply lost its saved identity -- the
            // exact legitimate case `docs/orchestration.md` promises fails
            // loudly rather than guessing. Refuse exactly as before this
            // fix; do not attempt the human-identity fallback here, since
            // doing so would silently answer for the wrong scope.
            Err(std::env::VarError::NotPresent) => {
                eprintln!(
                    "worktree list --mine: {DOT_AGENT_DECK_WORKTREE_OWNER} is not set -- cannot \
                     determine which worktrees belong to this orchestration; supply it or drop \
                     --mine"
                );
                return ExitCode::FAILURE;
            }
            // Issue #300 fix round (reviewer F3): a SET-but-invalid value is
            // a different, more suspicious case than a genuinely absent one
            // -- matching the empty-value guard above's own reasoning, an
            // exported-but-meaningless value deserves its own explicit
            // refusal, not silent fallthrough into either the pane-gated
            // refusal or the human-identity fallback.
            Err(std::env::VarError::NotUnicode(_)) => {
                eprintln!(
                    "worktree list --mine: {DOT_AGENT_DECK_WORKTREE_OWNER} is set to a value \
                     that is not valid Unicode -- refusing rather than falling back to this \
                     caller's own identity, which would answer for a different scope than the \
                     one that was asked for"
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
        // precondition that comment relies on -- for `owner_kind == "agent"`
        // rows. Issue #300 relaxed `is_mine` so an `owner_kind == "human"`
        // row (a human's own `resolve_worktree_owner` fallback, never a
        // marker read) matches on `owner` alone, since a human's unmarked
        // worktree has no `owned_git_dir` marker to disagree over in the
        // first place. `is_mine` (issue #221 review round; issue #300 round)
        // is the same predicate `worktree/reclaim/030`/`083` assert against,
        // so this retain and those tests cannot silently drift apart.
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
            // construction via the `Ok(v) => Some(sanitize_marker_creator(&v))`
            // arm above, which guarantees this is the exact string the
            // filter compared against -- not a second, possibly-divergent
            // normalization of it. Issue #232 round 2 (gap 3) corrects M1's
            // premise, though: `sanitize_marker_creator` strips Unicode
            // category `Cc` but deliberately preserves `Cf` (bidi/format)
            // chars, so "already sanitized" does not mean "safe to print to
            // a terminal" -- this is the same terminal-display sink
            // `format_disagreement_warning` has, so it goes through the same
            // display sanitizer immediately before printing.
            // Issue #300 fix round: there is now a SECOND construction arm
            // (the pane-gated `Err(VarError::NotPresent) => resolve_human_owner()`
            // fallback above), which applies no sanitizer at all -- and
            // deliberately so, since `src/worktree_reclaim.rs`'s matching
            // `owner` field (built from the same `resolve_human_owner`
            // call) is unsanitized too. The two must stay symmetric: if
            // either side started sanitizing without the other, `--mine`
            // would stop matching a human's own worktree.
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

/// `dot-agent-deck worktree sync` (fork issue #744) — no path argument,
/// batch operation over every sibling isolated clone, same shape as
/// [`run_worktree_reclaim_cli`] above. Wires PRD fork#544 M7's
/// `sync_merged_workspace_to_main`/`fetch_other_live_workspace` to a real
/// call site for the first time — see
/// [`dot_agent_deck::worktree_reclaim::run_sync`]'s own doc comment for the
/// full per-clone decision. Deliberately no `--yes`/confirmation flag the
/// way `reclaim` has: `sync_merged_workspace_to_main` already refuses on its
/// own (no discard, no data loss) whenever the workspace carries anything
/// beyond the merge, so there is nothing here for a confirmation flag to
/// gate.
fn run_worktree_sync_cli() -> ExitCode {
    use dot_agent_deck::terminal_sanitize::sanitize_for_terminal_display;
    use dot_agent_deck::worktree_reclaim::{format_sync_human, run_sync};

    // Same rationale as `run_worktree_reclaim_cli`'s own `init_logging_from_env`
    // call: `sync_merged_workspace_to_main`'s failed-merge restore path logs
    // via `tracing::warn!`, which is silently dropped without a subscriber
    // installed, and this is a distinct top-level `Commands::Worktree` arm
    // that can never double-install one.
    init_logging_from_env();

    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!(
                "worktree sync: failed to resolve current directory: {}",
                sanitize_for_terminal_display(&e.to_string())
            );
            return ExitCode::FAILURE;
        }
    };

    match run_sync(&cwd) {
        Ok(report) => {
            print!("{}", format_sync_human(&report));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("worktree sync: {}", sanitize_for_terminal_display(&e));
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

#[tokio::main]
async fn run_daemon_uninstall_cli() -> ExitCode {
    match dot_agent_deck::daemon_service::run_uninstall().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("daemon uninstall: {e}");
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
/// and safe-by-default (refuses when managed agents are alive, or when the
/// daemon holds live orchestration roles — issue #770 — unless `--force` is
/// passed). The recovery flow is in
/// [`dot_agent_deck::daemon_stop::run_daemon_stop`]; this function
/// only translates outcomes into stdout/stderr text and exit codes.
#[tokio::main]
async fn run_daemon_stop_cli(force: bool) -> ExitCode {
    let endpoint = LocalEndpoint::from_config();
    match dot_agent_deck::daemon_stop::run_daemon_stop(&endpoint, force).await {
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
        Err(dot_agent_deck::daemon_stop::StopError::LiveOrchestrations { roles }) => {
            eprint!(
                "{}",
                dot_agent_deck::daemon_stop::format_live_orchestrations_refusal(&roles)
            );
            ExitCode::FAILURE
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
    let endpoint = LocalEndpoint::from_config();
    match dot_agent_deck::daemon_stop::run_daemon_restart(&endpoint, force).await {
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
        Err(dot_agent_deck::daemon_stop::StopError::LiveOrchestrations { roles }) => {
            eprint!(
                "{}",
                dot_agent_deck::daemon_stop::format_live_orchestrations_refusal(&roles)
            );
            ExitCode::FAILURE
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
/// Every agent's hook auto-install runs before this, in the `DaemonCmd::Serve`
/// arm (issue #1157), so an agent this daemon starts with no TUI attached —
/// a scheduled task, a dispatched unit, a restored pane — finds hooks naming
/// a live deck. The OpenCode plugin's install also runs again before each
/// OpenCode agent starts (PRD #1666 §11), so a plugin missing, or pinned to a
/// deleted copy, because OpenCode was set up or a copy removed after this
/// daemon started is repaired before the agent loads it (PRD #190,
/// `hooks/install/023`). A pin to another live, durable install is kept by the
/// same rules as at startup.
#[tokio::main]
async fn run_daemon_serve_cli(
    hook_binaries: dot_agent_deck::hook_binary::HookBinaryState,
) -> ExitCode {
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
    let mut initial = AppState::default();
    initial.hook_binaries = hook_binaries;
    let state = Arc::new(RwLock::new(initial));
    // Issue #1121: the BIND side, so these are deliberately the pure
    // resolvers and not `endpoint_resolve`'s client ones — the primary
    // endpoint is the new spelling, never one the compatibility read chose.
    //
    // Issue #1173: …except that when another uid holds the per-uid fallback
    // directory's name, both sockets go into a relocated owner-only sibling
    // instead of the daemon refusing to start. On every other host this hands
    // back the resolved addresses unchanged. Both in ONE call, so the two
    // sockets can never be split across directories by a squatter removing
    // their entry between two decisions. The launcher runs the same rule
    // before it spawns us, so it polls where we are about to bind.
    let prepared = dot_agent_deck::endpoint_resolve::prepare_bind_endpoints(&[
        dot_agent_deck::platform::paths::resolve_socket_path(),
        dot_agent_deck::platform::paths::resolve_attach_socket_path(),
    ])
    .await;
    let (path, attach_path) = match prepared.as_deref() {
        Ok([path, attach_path]) => (path.clone(), attach_path.clone()),
        Ok(other) => {
            eprintln!("Daemon error: expected two prepared endpoints, got {other:?}");
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("Daemon error: cannot prepare the endpoints: {e}");
            return ExitCode::FAILURE;
        }
    };

    // PRD #1487: record this daemon's own binary now, at startup, so a later
    // `restart-daemon` resolves "the build installed at my own path" from where
    // it started rather than from whatever `current_exe()` reports after an
    // upgrade replaced the file.
    let restart_control = Arc::new(dot_agent_deck::daemon_restart::RestartControl::new(
        dot_agent_deck::daemon_restart::InstallRecord::capture(),
    ));
    // PRD #2050 (auditor M2): likewise this binary's file identity, which the
    // OpenCode plugin install compares with the file at its pin to tell an
    // older daemon still running after an upgrade from a rolled-back deck.
    dot_agent_deck::opencode_manage::note_own_executable();
    let refresh_state = state.clone();
    let daemon = Daemon::with_attach(state, attach_path.clone())
        .with_restart_control(restart_control.clone());
    // Issue #1637: re-read the agents' hook configs periodically, so a notice
    // about an older copy the hooks are still set up to call stays, and one
    // the user has since fixed clears, without a restart. Here rather than in
    // `run_daemon_with`, which in-process test daemons also run: only this
    // daemon was started with the hook pins it reads.
    let pin_refresh = tokio::spawn(dot_agent_deck::daemon::run_hook_pin_refresh(
        refresh_state,
        daemon.event_tx.clone(),
    ));
    let result = run_daemon_with(&path, daemon).await;
    pin_refresh.abort();
    if let Err(e) = result {
        eprintln!("Daemon error: {e}");
        return ExitCode::FAILURE;
    }
    // PRD #1487: a supervised daemon that accepted a restart started no
    // successor; this status is what makes its service manager start one.
    if restart_control.handed_to_supervisor() {
        return ExitCode::from(dot_agent_deck::daemon_restart::SUPERVISED_RESTART_EXIT);
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
            let client = DaemonClient::new(client_attach_socket_path());
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
            let client = DaemonClient::new(client_attach_socket_path());
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
            shape,
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
                // Issue #171: no CLI flag for either yet — a hand-edit of the
                // resulting TOML is how `repo_allowlist`/`dry_run` get set
                // today, matching this fork's existing pattern for advanced
                // knobs added without a matching `schedule add` flag.
                repo_allowlist: None,
                dry_run: false,
                claim: Default::default(),
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
                    shape,
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
            shape,
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
                shape,
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
    let client = DaemonClient::new(client_attach_socket_path());
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
    use dot_agent_deck::bounded_read::MAX_TASK_BYTES;

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

    /// Issue #530: `dispatch --help` states what the exit status asserts, and
    /// quotes the two openings the caller is told to read the outcome from.
    ///
    /// The exit status is the provenance gate's acknowledgement — pinned by
    /// `daemon::hook_ingestion_tests::a_dispatch_its_handler_rejects_is_still_acknowledged_as_accepted`
    /// — so the help is where a caller learns that exit 0 does not mean a unit
    /// started. Both quotes are checked against the values the daemon builds its
    /// messages from, so rewording either message fails here rather than leaving
    /// the help describing a reply nothing sends.
    #[test]
    fn dispatch_help_says_what_its_exit_status_asserts() {
        let cli = Cli::command();
        let dispatch = cli
            .find_subcommand("dispatch")
            .expect("the dispatch subcommand");
        let help = dispatch
            .get_long_about()
            .expect("dispatch carries a long help")
            .to_string();
        // clap re-flows doc-comment text, so every quote is matched with its
        // whitespace collapsed rather than byte for byte.
        let flat: String = help.split_whitespace().collect::<Vec<_>>().join(" ");
        let completed_opening = "dispatch: a unit you dispatched has completed";
        for (quote, why) in [
            (
                "Exit status when starting a unit: 0 means the daemon admitted the request",
                "what exit 0 asserts",
            ),
            (
                "does not mean the worktree was created",
                "what exit 0 does NOT assert",
            ),
            (
                dot_agent_deck::dispatch::SPAWNED_OPENING,
                "the success reply's real opening",
            ),
            (completed_opening, "the completion report's opening"),
        ] {
            assert!(flat.contains(quote), "the help must state {why}:\n{help}");
        }
        let completed =
            dot_agent_deck::dispatch_return::compose_completion_report("probe", "report", None);
        assert!(
            completed.starts_with(completed_opening),
            "the completion report no longer opens the way the help quotes it: {completed}"
        );
    }

    /// `--orchestration` REQUIRES its value, so it can never consume the unit name.
    ///
    /// With `num_args = 0..=1` clap consumed the next bare token, so
    /// `dispatch --orchestration my-unit --task "…"` bound the UNIT NAME as the
    /// orchestration and aborted for a missing positional. A required value makes
    /// both orderings unambiguous.
    /// Issue #2053 (Qodo and Greptile on PR #2055): an agent's name may start
    /// with `-`, so `--name` and `--pane` take such a value whether it is
    /// typed as the next word or as `--name=…`, the form the reply command the
    /// deck prints uses.
    #[test]
    fn name_and_pane_take_a_value_that_starts_with_a_hyphen() {
        for argv in [
            &[
                "dot-agent-deck",
                "inbox",
                "send",
                "--name",
                "-docs",
                "--text",
                "t",
            ][..],
            &[
                "dot-agent-deck",
                "inbox",
                "send",
                "--folder=/w",
                "--name=--builder",
                "--text",
                "t",
            ],
            &[
                "dot-agent-deck",
                "inbox",
                "send",
                "--pane",
                "-p",
                "--text",
                "t",
            ],
            &["dot-agent-deck", "roles", "--name", "-docs", "--pane", "-p"],
        ] {
            let parsed = Cli::try_parse_from(argv.iter().copied());
            let (name, pane) = match parsed.map(|cli| cli.command) {
                Ok(Some(Commands::Inbox {
                    action: InboxAction::Send { name, pane, .. },
                })) => (name, pane),
                Ok(Some(Commands::Roles { name, pane, .. })) => (name, pane),
                Ok(_) => panic!("{argv:?}: parsed as another command"),
                Err(error) => panic!("{argv:?}: {error}"),
            };
            let given = |flag: &str| {
                argv.iter()
                    .position(|a| *a == flag)
                    .map(|i| argv[i + 1].to_string())
                    .or_else(|| {
                        argv.iter()
                            .find_map(|a| a.strip_prefix(&format!("{flag}=")).map(str::to_string))
                    })
            };
            assert_eq!(name, given("--name"), "{argv:?}");
            assert_eq!(pane, given("--pane"), "{argv:?}");
        }
    }

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

    // ---- Issue #328: both reads are bounded, and a non-regular path is
    // refused rather than opened. The per-shape refusals (FIFO, symlink to a
    // FIFO, character device, endless stream) are unit-tested against the
    // helper in `dot_agent_deck::bounded_read`; what these pin is that
    // `resolve_task` — the seam every `delegate` / `work-done` call goes
    // through — actually routes into it.

    #[test]
    fn task_file_over_the_size_limit_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("huge-task.md");
        std::fs::write(&path, "x".repeat(MAX_TASK_BYTES as usize + 1)).expect("write");

        let err = resolve_task(
            None,
            Some(path.to_str().unwrap().to_string()),
            std::io::empty(),
        )
        .expect_err("a task file over the cap must be refused");
        assert!(
            err.contains("exceeds the") && err.contains("limit"),
            "over-limit error should state the cap: {err}"
        );
    }

    #[test]
    fn task_file_at_the_size_limit_is_still_accepted() {
        // The cap refuses only what is genuinely past it — an input sitting
        // exactly on the boundary is a legitimate task, not a pathological one.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("big-task.md");
        let text = "x".repeat(MAX_TASK_BYTES as usize);
        std::fs::write(&path, &text).expect("write");

        let got = resolve_task(
            None,
            Some(path.to_str().unwrap().to_string()),
            std::io::empty(),
        )
        .expect("a task file exactly at the cap must be accepted");
        assert_eq!(got.len(), MAX_TASK_BYTES as usize);
    }

    #[test]
    fn stdin_over_the_size_limit_is_refused() {
        // `-` keeps working (see `task_file_dash_reads_task_verbatim_from_stdin`),
        // but the same cap applies to it.
        let oversized = "x".repeat(MAX_TASK_BYTES as usize + 1);
        let err = resolve_task(None, Some("-".to_string()), oversized.as_bytes())
            .expect_err("oversized stdin must be refused");
        assert!(
            err.contains("task from stdin") && err.contains("exceeds the"),
            "over-limit stdin error should name stdin and the cap: {err}"
        );
    }

    #[test]
    fn task_file_pointing_at_a_non_regular_file_is_refused() {
        // A directory is the portable stand-in for the whole class; the FIFO
        // and character-device cases live in the `bounded_read` unit tests.
        let dir = tempfile::tempdir().expect("tempdir");
        let err = resolve_task(
            None,
            Some(dir.path().to_str().unwrap().to_string()),
            std::io::empty(),
        )
        .expect_err("a non-regular --task-file target must be refused");
        assert!(
            err.contains("--task-file needs a regular file") && err.contains("--task-file -"),
            "refusal should say what is required and point at the stdin alternative: {err}"
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
                supersede,
            }) => {
                assert_eq!(task, None);
                assert_eq!(task_file.as_deref(), Some("/tmp/t.txt"));
                assert_eq!(to, vec!["coder".to_string()]);
                assert!(!supersede, "--supersede is opt-in (issue #580)");
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
                task_id,
            }) => {
                assert_eq!(task, None);
                assert_eq!(task_file.as_deref(), Some("-"));
                assert!(!done);
                assert_eq!(task_id, None);
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

    fn busy(role: &str, outstanding: u32) -> dot_agent_deck::event::BusyWorker {
        dot_agent_deck::event::BusyWorker {
            role: role.to_string(),
            outstanding,
            oldest_age_secs: 12 * 60,
        }
    }

    use spec::spec;

    fn blocked(
        role: &str,
        kind: dot_agent_deck::state::BlockedKind,
    ) -> dot_agent_deck::event::BlockedWorker {
        dot_agent_deck::event::BlockedWorker {
            role: role.to_string(),
            kind,
            blocked_for_secs: 5 * 60,
            resets_in_secs: None,
        }
    }

    /// Scenario: Feed `delegate`'s verdict a daemon reply naming a blocked
    /// worker that was delivered to, then one naming a blocked worker that was
    /// refused as busy. The first warns that the task was delivered but will
    /// likely not be worked on and still exits 0; the second names the busy
    /// role as blocked so `--supersede` is not mistaken for a fix, with the
    /// reset the provider gave. Neither carries any agent text.
    #[spec("orchestration/delegate/038")]
    #[test]
    fn orchestration_delegate_038_verdict_reports_blocked_for_delivered_and_busy() {
        use dot_agent_deck::state::BlockedKind;
        let mut resp = reply(&["coder"], &[], None);
        resp.blocked = vec![blocked("coder", BlockedKind::CreditsDepleted)];
        let v = delegate_verdict("pane-1", &[], &resp);
        assert!(!v.failed, "the task WAS delivered: exit 0");
        let msg = v.message.expect("a blocked worker must never be silent");
        assert!(
            msg.contains("appear BLOCKED by a provider usage limit or credit pool")
                && msg.contains("[UNTRUSTED-ROLE-LABEL: coder :END-UNTRUSTED-ROLE-LABEL]")
                && msg.contains("reported 5 minutes ago by the agent")
                && msg.contains("credits do not reset on their own")
                && msg.contains("The task WAS delivered"),
            "{msg}"
        );

        // Busy AND blocked, nothing delivered: the routing failure, annotated.
        let mut resp = reply(&[], &[], Some("this delegate was NOT sent"));
        resp.busy = vec![busy("coder", 1)];
        resp.blocked = vec![dot_agent_deck::event::BlockedWorker {
            resets_in_secs: Some(90 * 60),
            ..blocked("coder", BlockedKind::UsageLimit)
        }];
        let v = delegate_verdict("pane-1", &[], &resp);
        assert!(v.failed, "nothing was dispatched: non-zero");
        let msg = v.message.expect("reported");
        assert!(
            msg.contains("NOT sent")
                && msg.contains("busy worker(s)")
                && msg.contains("also appear BLOCKED")
                && msg.contains("--supersede will likely not get the task worked on")
                && msg.contains("; resets in 1h 30m")
                && !msg.contains("credits do not reset"),
            "{msg}"
        );

        // Busy-and-blocked beside a delivered, healthy role: exit 0, both said.
        let mut resp = reply(&["tester"], &[], None);
        resp.busy = vec![busy("coder", 1)];
        resp.blocked = vec![blocked("coder", BlockedKind::UsageLimit)];
        let v = delegate_verdict("pane-1", &[], &resp);
        assert!(!v.failed);
        let msg = v.message.expect("reported");
        assert!(
            msg.contains("also appear BLOCKED") && msg.contains("NOT sent"),
            "{msg}"
        );
        assert!(!msg.contains("The task WAS delivered but"), "{msg}");

        // The reply carries no pane text to leak, and an older daemon's reply
        // (no `blocked` key) decodes to an empty list and prints nothing extra.
        let old: DelegateResponse =
            serde_json::from_str(r#"{"kind":"delegate","delivered":["coder"]}"#).unwrap();
        assert!(old.blocked.is_empty());
        assert!(delegate_verdict("pane-1", &[], &old).message.is_none());
    }

    /// Issue #580: a busy worker beside a delivered one is a partial outcome.
    /// Exit 0 — the delivered role really has the task, and a failure would
    /// invite a retry that dispatches it twice — with a warning naming the busy
    /// role, what it owes, and the remedy.
    #[test]
    fn delegate_verdict_warns_about_a_busy_worker_on_a_partial_delivery() {
        let mut resp = reply(&["tester"], &[], None);
        resp.busy = vec![busy("coder", 1)];
        let v = delegate_verdict("pane-1", &[], &resp);
        assert!(!v.failed, "the tester was dispatched: exit 0");
        let msg = v.message.expect("a busy worker must never be silent");
        assert!(
            msg.contains("NOT sent")
                && msg.contains("[UNTRUSTED-ROLE-LABEL: coder :END-UNTRUSTED-ROLE-LABEL]")
                && msg.contains("1 unanswered delegation,")
                && msg.contains("12 minutes ago")
                && msg.contains("It WAS delivered to: tester")
                && msg.contains("--supersede"),
            "the warning must name the busy role, its debt, the delivered roles and the \
             remedy: {msg}"
        );
    }

    /// Issue #580: when every worker was busy the daemon sets `error`, and the
    /// verdict is the ordinary routing failure — non-zero, because nothing landed.
    #[test]
    fn delegate_verdict_fails_when_every_worker_was_busy() {
        let mut resp = reply(&[], &[], Some("this delegate was NOT sent"));
        resp.busy = vec![busy("coder", 2)];
        let v = delegate_verdict("pane-1", &[], &resp);
        assert!(v.failed, "nothing was dispatched: non-zero");
        assert!(v.message.expect("reported").contains("NOT sent"));

        // Defense in depth: the same outcome without `error` is still a failure.
        let mut resp = reply(&[], &[], None);
        resp.busy = vec![busy("coder", 2)];
        assert!(delegate_verdict("pane-1", &[], &resp).failed);
    }

    /// Issue #580: a `--supersede` that replaced an unanswered delegation is
    /// reported rather than silent — "at minimum reported as superseding" — but
    /// it is a success: the task was dispatched as asked.
    #[test]
    fn delegate_verdict_reports_a_supersession_without_failing() {
        let mut resp = reply(&["coder"], &[], None);
        resp.superseded = vec![busy("coder", 1)];
        let v = delegate_verdict("pane-1", &[], &resp);
        assert!(!v.failed);
        let msg = v.message.expect("a supersession must be reported");
        assert!(
            msg.contains("still owed a work-done") && msg.contains("UNTRUSTED-ROLE-LABEL: coder"),
            "the note must name the superseded worker: {msg}"
        );
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
        // The five causes, not the one that happens to be most common: the
        // old message told the user to go check role names in the toml even
        // when the role was sitting there correctly and was simply the
        // orchestrator itself, or had had its worker pane closed — or, issue
        // #554, had been renamed in the toml after the orchestration started,
        // which leaves the file looking right while the daemon still routes by
        // the old name — or, issue #524, had a worker that exited on its own.
        assert!(
            msg.contains(".dot-agent-deck.toml")
                && msg.contains("orchestrator cannot delegate to itself")
                && msg.contains("worker pane has been closed")
                && msg.contains("worker has exited")
                && msg.contains("pane restart <role>")
                && msg.contains(
                    "renamed or added in .dot-agent-deck.toml after this orchestration started"
                ),
            "the message must state all five causes, not assert one: {msg}"
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

    /// Issue #1383: `ack` exits 0 whatever happens, and says on stderr — never
    /// as a failure — when receipt could not be confirmed. Only an accepted ack
    /// whose id matched a message the deck holds prints "Acknowledged"; an unanswered
    /// one (an older daemon) and a bare admission (PR #1414 review) do not.
    #[test]
    fn ack_report_is_never_a_failure_and_says_what_happened() {
        use dot_agent_deck::event::{AckDelivery, SignalAck};
        use dot_agent_deck::hook::SocketReply;
        let line = |ack: SignalAck| {
            AckReply::Socket(SocketReply::Line(serde_json::to_string(&ack).unwrap()))
        };
        let refused =
            serde_json::to_string(&SignalAck::refused("missing_token", "no token")).unwrap();
        let cases = [
            (
                line(SignalAck::acknowledged(AckDelivery::Acknowledged)),
                AckStream::Stdout,
            ),
            (
                line(SignalAck::acknowledged(AckDelivery::AlreadyAcknowledged)),
                AckStream::Stdout,
            ),
            // Admitted, but the daemon did not say the id matched: an admission
            // is not a match.
            (line(SignalAck::accepted()), AckStream::Stderr),
            // A delivery outcome a later daemon added.
            (
                AckReply::Socket(SocketReply::Line(
                    r#"{"kind":"signal_ack","accepted":true,"delivery":"something_new"}"#
                        .to_string(),
                )),
                AckStream::Stderr,
            ),
            // An older daemon that does not know `ack` answers nothing: that
            // must not read as "Acknowledged".
            (AckReply::Socket(SocketReply::NoReply), AckStream::Stderr),
            (
                AckReply::Socket(SocketReply::Line("not json".to_string())),
                AckStream::Stderr,
            ),
            (
                AckReply::Socket(SocketReply::Line(refused)),
                AckStream::Stderr,
            ),
            (
                AckReply::Socket(SocketReply::Unreachable),
                AckStream::Stderr,
            ),
            (AckReply::Malformed, AckStream::Stderr),
            (AckReply::NoPane, AckStream::Stderr),
        ];
        for (reply, expected) in cases {
            let (stream, message) = ack_report(&reply);
            assert_eq!(stream, expected, "{message}");
            match stream {
                AckStream::Stdout => {
                    assert_eq!(message, "Acknowledged. Carry on with your task.")
                }
                AckStream::Stderr => {
                    assert!(message.starts_with("Could not confirm receipt with the deck ("));
                    assert!(message.ends_with("this is harmless — carry on with your task."));
                }
            }
        }
        // A mistyped or stale id is said so, still harmlessly and never as
        // "Acknowledged".
        let (stream, unknown) = ack_report(&line(SignalAck::acknowledged(AckDelivery::Unknown)));
        assert_eq!(stream, AckStream::Stderr, "{unknown}");
        assert!(!unknown.contains("Acknowledged"), "{unknown}");
        assert!(unknown.contains("no delivery under that id"), "{unknown}");
        assert!(
            unknown.ends_with("this is harmless — carry on with your task."),
            "{unknown}"
        );
        let (_, refused_message) = ack_report(&AckReply::Socket(SocketReply::Line(
            serde_json::to_string(&dot_agent_deck::event::SignalAck::refused(
                "missing_token",
                "no token",
            ))
            .unwrap(),
        )));
        assert!(
            refused_message.contains("missing_token"),
            "{refused_message}"
        );
    }

    /// PR #1735 second review: `ack --json` says whether the deck recorded the
    /// acknowledgement, and whether a refusal is final, for the components
    /// that confirm theirs.
    #[test]
    fn ack_json_says_whether_the_deck_recorded_it() {
        use dot_agent_deck::event::{AckDelivery, SignalAck};
        use dot_agent_deck::hook::SocketReply;
        let line = |ack: SignalAck| {
            AckReply::Socket(SocketReply::Line(serde_json::to_string(&ack).unwrap()))
        };
        for delivery in [AckDelivery::Acknowledged, AckDelivery::AlreadyAcknowledged] {
            assert_eq!(
                ack_json(&line(SignalAck::acknowledged(delivery))),
                serde_json::json!({"acknowledged": true})
            );
        }
        for (delivery, reason) in [
            (AckDelivery::Unknown, "unknown"),
            (AckDelivery::WrongAgent, "wrong_agent"),
            (AckDelivery::Undeliverable, "undeliverable"),
        ] {
            let reply = ack_json(&line(SignalAck::acknowledged(delivery)));
            assert_eq!(reply["acknowledged"], false, "{reply}");
            assert_eq!(reply["final"], true, "{reply}");
            assert_eq!(reply["reason"], reason, "{reply}");
        }
        for reply in [
            line(SignalAck::accepted()),
            line(SignalAck::refused("inbox_not_agent_process", "outside")),
            AckReply::Socket(SocketReply::NoReply),
            AckReply::Socket(SocketReply::Unreachable),
            AckReply::NoPane,
        ] {
            let reply = ack_json(&reply);
            assert_eq!(reply["acknowledged"], false, "{reply}");
            assert_eq!(reply["final"], false, "{reply}");
            assert!(reply["reason"].is_string(), "{reply}");
        }
    }
}
