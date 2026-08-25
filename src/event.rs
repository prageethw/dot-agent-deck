use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    ToolStart,
    ToolEnd,
    Thinking,
    Compacting,
    SubagentStart,
    SubagentStop,
    WaitingForInput,
    PermissionRequest,
    Idle,
    Error,
    SessionStart,
    SessionEnd,
    /// PRD #370 M2: synthesized daemon-side (never sent by an agent's own
    /// hooks/wrapper) when a pane's agent process has a transitive descendant
    /// running in a POSIX session of its own — i.e. a shelled-out command is
    /// actively running with no agent-emitted event to say so. See
    /// [`crate::state::AppState::apply_event`]'s `ShellBusy`/`ShellIdle` arms
    /// for the precedence rules against real, agent-emitted status.
    ShellBusy,
    /// PRD #370 M2: the paired synthesized event — the detached descendant is
    /// gone, i.e. the previously-running foreground command has finished. See
    /// [`ShellBusy`](Self::ShellBusy).
    ShellIdle,
    /// PRD #370 / precedent PRD #201 (`AgentType`'s identical retrofit):
    /// forward-compat catch-all for a future/unknown `event_type` string on
    /// the wire, so a build newer than THIS one can add further variants
    /// without another `PROTOCOL_VERSION` bump. Deserialize-only — never
    /// produced by this build. Treated as a no-op wherever `EventType` is
    /// matched (never proof of agent activity, never changes `SessionStatus`).
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentType {
    ClaudeCode,
    OpenCode,
    Pi,
    /// OpenAI Codex CLI (PRD #20 M7) — the first wrapper-strategy agent. Wired
    /// in via `dot-agent-deck wrap -- codex …`; its events are synthesized from
    /// stdout by [`crate::wrap`] and ride the existing raw-`AgentEvent` socket.
    /// Serializes to `"codex"` (snake_case); resolved from the `codex` binary
    /// basename through the registry ([`crate::agent_registry`]).
    Codex,
    /// Devin CLI — Cognition's local terminal agent. Like Codex, it ships a
    /// Claude-Code-compatible hooks engine, so its native command hooks post the
    /// SAME stdin JSON shape Claude does and are ingested by the existing
    /// [`crate::hook`] `"devin"` arm. Unlike Codex it needs no wrapper and no
    /// hook-trust ceremony, so it is a pure `NativeHooks` agent
    /// ([`crate::devin_hooks_manage`]). Serializes to `"devin"` (snake_case);
    /// resolved from the `devin` binary basename through the registry
    /// ([`crate::agent_registry`]).
    Devin,
    /// "No recognized agent type." Produced by [`AgentType::from_command`] for
    /// any unrecognized binary, mapped to `Option::None` by
    /// [`crate::state::SessionState::live_snapshot`], and rendered as the "No
    /// agent" dashboard placeholder.
    ///
    /// PRD #201 forward-compat catch-all: this variant also carries
    /// `#[serde(other)]`, so any UNRECOGNIZED wire value (e.g. a `pi` record
    /// reaching a pre-Pi reader, or a future agent type reaching today's
    /// build) deserializes here instead of failing the whole `AgentEvent` /
    /// `AgentRecord` decode. That mirrors [`crate::state::SessionStatus::Unknown`]
    /// — but reuses the pre-existing neutral variant rather than adding a new
    /// one: `None` already means "type not known", already maps to the "No
    /// agent" placeholder, and is exactly what an unrecognized binary yields
    /// via `from_command`, so an unknown wire value landing on `None` is the
    /// consistent, non-active outcome (it never masquerades as a real agent).
    /// `#[serde(other)]` is deserialize-only; `None` still serializes to
    /// `"none"` (and `"none"` still deserializes back to `None` via this same
    /// catch-all), so round-trips are unchanged.
    #[serde(other)]
    None,
}

impl AgentType {
    /// PRD #76 M2.13: best-effort inference of agent type from the binary
    /// name in a spawn command. Used by TUI spawn sites to populate
    /// `StartAgentOptions.agent_type` so the daemon's registry can echo it
    /// back via `list_agents` and a remote reconnect can build placeholder
    /// sessions with the correct type instead of "No agent".
    ///
    /// Returns `Some(AgentType)` only for recognized agent binaries
    /// (`claude` → `ClaudeCode`, `opencode` → `OpenCode`, `pi` → `Pi`,
    /// `codex` → `Codex`);
    /// unknown commands and `None` input return `None` so the daemon stores
    /// "type not known yet" rather than misclassifying. Whitespace
    /// before the binary name is ignored to match shell-style invocations.
    ///
    /// PRD #20 M2: the per-agent basename→type mapping now lives in the agent
    /// registry ([`crate::agent_registry`]); this fn keeps the command-parsing
    /// (basename extraction, arg stripping) and delegates the lookup. The
    /// recognized set and the "unknown → `None`" behaviour are unchanged.
    ///
    /// PRD #20 finding #19: the parser is no longer limited to the first
    /// whitespace token. It tokenizes the command with quote awareness and
    /// conservatively looks through common launch forms so a Wrapper-strategy
    /// agent behind a launcher is still detected and wrapped:
    /// - leading `VAR=VALUE` assignments and an `env`/`sudo` prefix (with their
    ///   own option flags/assignments) are skipped to reach the real binary;
    /// - a quoted executable path (`"/opt/OpenAI Codex/codex"`) resolves by its
    ///   basename;
    /// - a shell launcher (`sh -c '<script>'`, `bash -lc "<script>"`) is
    ///   recursed into via its `-c` script argument.
    ///
    /// Everything still degrades to `None` for an unrecognized binary, so a
    /// non-agent command is never misclassified.
    pub fn from_command(cmd: Option<&str>) -> Option<Self> {
        let tokens = tokenize_command(cmd?);
        detect_from_tokens(&tokens, DETECT_RECURSION_BUDGET)
    }

    /// Like [`from_command`], but ALSO recognizes `devbox run <script>` via
    /// [`crate::agent_registry::detect_from_devbox_script`]'s hyphen-segment
    /// heuristic. Deliberately SEPARATE from `from_command`: that function's
    /// result also feeds `wrap_launch_command`'s wrap-vs-bare decision (see
    /// the documented invariant at `agent_pty.rs:5871-5911`, which names
    /// `devbox run codex-big` as its own "resolves to no agent type"
    /// exemplar) — widening devbox recognition there silently auto-wraps a
    /// devbox-launched agent, which broke
    /// `spawn_007_hook_learned_badge_does_not_change_respawn_launch`. This
    /// function is presentation-only (badge / "expects a report" display)
    /// and must NEVER be used anywhere that decides whether to spawn,
    /// respawn, or wrap a launch command.
    pub fn from_command_including_devbox(cmd: Option<&str>) -> Option<Self> {
        if let Some(t) = Self::from_command(cmd) {
            return Some(t);
        }
        let tokens = tokenize_command(cmd?);
        if tokens.first().map(String::as_str) == Some("devbox")
            && tokens.get(1).map(String::as_str) == Some("run")
            && let Some(script) = tokens.get(2)
        {
            return crate::agent_registry::detect_from_devbox_script(script);
        }
        None
    }
}

/// PRD #20 finding R20-016: hard cap on shell-launcher recursion, decremented
/// across [`detect_from_tokens`] calls (NOT reset per call). A deeply nested
/// `sh -c "sh -c \"sh -c …\""` can otherwise recurse until stack exhaustion;
/// with an explicit budget each `-c` level costs one unit and the chain
/// terminates safely at `None`.
const DETECT_RECURSION_BUDGET: usize = 8;

/// Short options (matched WITH their leading dash) that consume the FOLLOWING
/// token as their argument for a given launcher, so command detection skips both
/// the option and its argument to reach the real binary (`sudo -u root codex`,
/// `env -u FOO codex`). Conservative allow-list: only these consume the next
/// token; every other flag is treated as self-contained.
fn launcher_option_takes_arg(launcher: &str, opt: &str) -> bool {
    match launcher {
        "sudo" => matches!(
            opt,
            "-u" | "-g" | "-h" | "-p" | "-C" | "-D" | "-R" | "-T" | "-U" | "-r" | "-t"
        ),
        "env" => matches!(opt, "-u" | "-C" | "-S"),
        _ => false,
    }
}

/// Whether `arg` is a shell SHORT-option cluster that selects command mode
/// (`-c`) — e.g. `-c`, `-lc`, `-ic`. Only a single-dash cluster of ASCII-letter
/// options containing `c` counts; a LONG option like `--rcfile` (which merely
/// happens to contain the letter `c`) is deliberately NOT command mode, so a
/// startup-file path is never mistaken for a `-c` script.
fn is_shell_command_flag(arg: &str) -> bool {
    match arg.strip_prefix('-') {
        Some(rest) if !rest.is_empty() && !rest.starts_with('-') => {
            rest.chars().all(|c| c.is_ascii_alphabetic()) && rest.contains('c')
        }
        _ => false,
    }
}

/// Whether `token` looks like a leading `NAME=VALUE` environment assignment
/// (e.g. `FOO=1`) rather than a program or path. Conservative: `NAME` must be a
/// shell-identifier-shaped run before the first `=`, so a path that merely
/// contains `=` isn't misread as an assignment.
fn is_env_assignment(token: &str) -> bool {
    match token.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && name.chars().next().is_some_and(|c| !c.is_ascii_digit())
        }
        None => false,
    }
}

/// Resolve the agent type from an already-tokenized command, looking through
/// `env`/`sudo`/shell-launcher prefixes. See [`AgentType::from_command`].
fn detect_from_tokens(tokens: &[String], budget: usize) -> Option<AgentType> {
    // R20-016: the recursion budget is decremented across calls (a `-c` script
    // recurse below spends one unit), so a deeply nested `sh -c "sh -c …"` can no
    // longer recurse until stack exhaustion — it terminates at `None`.
    if budget == 0 {
        return None;
    }
    let mut idx = 0;
    // Bound the number of prefix hops (`env`/`sudo` chains) within one frame.
    for _ in 0..8 {
        // Skip a run of leading environment assignments (`FOO=1 codex`).
        while tokens.get(idx).is_some_and(|t| is_env_assignment(t)) {
            idx += 1;
        }
        let token = tokens.get(idx)?;
        let basename = std::path::Path::new(token)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(token.as_str());

        // `env` / `sudo` prefix: skip the launcher and any of its own flags,
        // `VAR=VALUE` assignments, and — R20-016 — the ARGUMENT of an
        // option that consumes one (`sudo -u root`, `env -u FOO`), then
        // re-resolve from the next real token. `--` ends option parsing.
        if basename == "env" || basename == "sudo" {
            idx += 1;
            while let Some(next) = tokens.get(idx) {
                if next == "--" {
                    idx += 1;
                    break;
                } else if is_env_assignment(next) {
                    idx += 1;
                } else if next.starts_with('-') {
                    // A single-dash short option may consume the following
                    // token as its argument (e.g. `-u root`); a `--long` option
                    // either bundles its value (`--unset=FOO`) or is a flag, so
                    // never over-consume for those.
                    let consumes =
                        !next.starts_with("--") && launcher_option_takes_arg(basename, next);
                    idx += 1;
                    if consumes && tokens.get(idx).is_some_and(|t| !t.starts_with('-')) {
                        idx += 1;
                    }
                } else {
                    break;
                }
            }
            continue;
        }

        // Shell launcher: `sh -c '<script>'`, `bash -lc "<script>"`. Recurse into
        // the script argument that follows a valid command-mode short-option
        // cluster ([`is_shell_command_flag`]); a shell with no `-c` (or one given
        // only a `--rcfile`-style long option) stays an unrecognized binary.
        if matches!(basename, "sh" | "bash" | "zsh" | "dash" | "ksh" | "ash") {
            let mut j = idx + 1;
            while let Some(arg) = tokens.get(j) {
                if arg.starts_with('-') {
                    if is_shell_command_flag(arg)
                        && let Some(script) = tokens.get(j + 1)
                    {
                        let inner = tokenize_command(script);
                        return detect_from_tokens(&inner, budget - 1);
                    }
                    j += 1;
                } else {
                    break;
                }
            }
            return crate::agent_registry::detect_from_basename(basename);
        }

        // An ordinary binary token — resolve it directly.
        return crate::agent_registry::detect_from_basename(basename);
    }
    None
}

/// Split a command string into whitespace-separated tokens, honoring single and
/// double quotes (the quote characters are stripped, and whitespace inside a
/// quoted run is preserved so a quoted executable path stays one token). This is
/// a deliberately small shell-word splitter — enough for agent detection
/// ([`AgentType::from_command`]), not a full POSIX parser.
fn tokenize_command(cmd: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut started = false;
    let mut in_single = false;
    let mut in_double = false;
    for c in cmd.chars() {
        match c {
            '\'' if !in_double => {
                in_single = !in_single;
                started = true;
            }
            '"' if !in_single => {
                in_double = !in_double;
                started = true;
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if started {
                    tokens.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            c => {
                cur.push(c);
                started = true;
            }
        }
    }
    if started {
        tokens.push(cur);
    }
    tokens
}

/// PRD #20 M3: the concrete handle, if any, through which a session's input is
/// delivered — the `kind` half of a [`LiveTarget`] descriptor. Serializes
/// kebab-case (`process`, `pty`, `tmux`, `sdk`, `none`). Purely descriptive: it
/// tells the UI what *kind* of thing (if any) backs the session; whether it can
/// actually be written to *now* is the separate [`Writable`] axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TargetKind {
    /// A child process the producer owns (e.g. a `dot-agent-deck wrap` child).
    Process,
    /// A pseudo-terminal the daemon controls — the live, writable Claude
    /// Code / OpenCode / Pi pane case.
    Pty,
    /// A `tmux` pane/window.
    Tmux,
    /// An in-process SDK/agent handle.
    Sdk,
    /// No concrete handle — the session is known only from history/logs.
    ///
    /// Forward-compat catch-all (matches [`AgentType::None`]): a future/unknown
    /// `kind` on the wire deserializes here via `#[serde(other)]` instead of
    /// failing the whole `LiveTarget`/`AgentEvent` decode. Deserialize-only —
    /// `None` still serializes to `"none"`.
    #[serde(other)]
    None,
}

/// PRD #20 M3: whether the dashboard can deliver input to a session right now —
/// the `writable` half of a [`LiveTarget`] descriptor. Serializes kebab-case
/// (`live`, `history-only`, `none`).
///
/// A dashboard-visible session is not necessarily a live, writable target:
/// today's Claude/OpenCode/Pi panes are `Live` (a PTY the daemon drives), but a
/// wrapped Codex session surfaced via [`crate::wrap`] is `HistoryOnly` — the
/// user's keystrokes reach the child through the inherited terminal, not a
/// daemon-controlled handle, so the *dashboard* cannot inject live input. The UI
/// reads this to render non-`Live` sessions distinctly and to refuse (with
/// honest feedback) an attempt to type into a card that can't accept input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Writable {
    /// Input can be delivered to the running session now.
    Live,
    /// The session can only be resumed/replayed from history — no live write.
    HistoryOnly,
    /// Neither live write nor history resume — view-only.
    ///
    /// Forward-compat catch-all (matches [`AgentType::None`]): a future/unknown
    /// `writable` on the wire deserializes here via `#[serde(other)]` — the
    /// safe, non-writable outcome — instead of failing the decode.
    /// Deserialize-only; `None` still serializes to `"none"`.
    #[serde(other)]
    None,
}

/// PRD #20 M3: a per-session descriptor of whether/how a session can receive
/// input. Carried on [`AgentEvent::live_target`] (optional + additive) so an
/// adapter can declare that the session it surfaces is a live PTY target, a
/// history-only wrapper session, or view-only — and the UI never invites users
/// to type into a card that can't accept input.
///
/// See the "Liveness & Write Semantics" section of PRD #20: the `kind`
/// ([`TargetKind`]) names the concrete handle and `writable` ([`Writable`])
/// names what can be done with it now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveTarget {
    pub kind: TargetKind,
    pub writable: Writable,
}

/// PRD #20 M3: the honest outcome of delivering input to a session, returned
/// instead of a fire-and-forget `Result<(), _>`. Serializes kebab-case; every
/// variant keeps a distinct public wire value so a caller can tell accepted
/// input apart from a stale, wrong, or unwritable target.
///
/// Rides the daemon wire on [`crate::daemon_protocol::AttachResponse::send_result`]
/// as an additive, optional field (a missing value decodes to `None`), so it is
/// forward-compatible and needs no `PROTOCOL_VERSION` bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SendResult {
    /// Delivered to the live target.
    Applied,
    /// Accepted, not yet confirmed applied.
    Queued,
    /// Target moved on / our view was behind.
    Stale,
    /// The handle no longer maps to the session we meant.
    WrongSession,
    /// No live target — only history resume is possible.
    HistoryOnly,
    /// Nothing to write to.
    NoLiveTarget,
    /// PRD #20 R20-004 (finding #3): a write STARTED reaching the target but the
    /// full payload+submit sequence did not complete (a partial write followed
    /// by a writer error). Some bytes MAY already have been delivered, so this is
    /// neither a clean success nor a safely-retryable failure: the daemon caches
    /// it against the `delivery_id` so a retry REPLAYS this (does not blind-submit
    /// again, which could duplicate the partial input), and the UI surfaces it as
    /// a terminal non-delivery rather than looping a retry. Older clients decode
    /// the unknown `"ambiguous"` value to [`SendResult::Unknown`] (also treated as
    /// a non-delivered, conservative outcome), so it stays forward-compatible.
    Ambiguous,
    /// PRD #20 R20-011: forward-compat catch-all. A future daemon may report an
    /// honest outcome this build does not know; a pre-`serde(other)` reader would
    /// reject the whole [`crate::daemon_protocol::AttachResponse`] as malformed
    /// (`unknown variant`) rather than degrade gracefully. Deserializing an
    /// unknown value here — the SAFE, non-delivered outcome — keeps the response
    /// decodable and forces every UI match to treat it conservatively (never as a
    /// delivered success). Deserialize-only in practice: this build never
    /// constructs `Unknown`, so it is never sent on the wire.
    #[serde(other)]
    Unknown,
}

/// PRD #201 M1.2 (test-plan row 3): map a lifecycle **state** string an agent's
/// extension reports via `dot-agent-deck agent-event --type <state>` to the
/// [`EventType`] that drives the target pane's card status. This is the single
/// production seam the CLI subcommand and the fast-tier status tests share.
///
/// The canonical `--type` vocabulary is exactly three states — `running`,
/// `waiting`, `finished`. Anything else returns `None` so the subcommand can
/// reject an unknown `--type` with a clear non-zero error instead of silently
/// emitting a wrong (or default) status. The Phase 2 extension and the docs
/// MUST use the same three strings.
pub fn agent_event_type_from_state(state: &str) -> Option<EventType> {
    match state {
        "running" => Some(EventType::Thinking),
        "waiting" => Some(EventType::WaitingForInput),
        "finished" => Some(EventType::Idle),
        _ => None,
    }
}

/// `AgentEvent.metadata` key carrying a human-friendly card title (PRD #127
/// finding #2). The daemon's live-surface path (`surface_spawned_pane`) sets
/// this to the schedule's task name so an ALREADY-ATTACHED TUI titles the
/// live card with the friendly name — matching what a disconnect/reconnect
/// already renders from the daemon registry's `display_name`. Real agent hooks
/// don't emit it; consumers treat its absence as "no friendly name known".
pub const DISPLAY_NAME_METADATA_KEY: &str = "display_name";

/// `AgentEvent.metadata` key carrying a DAEMON-AUTHORED report that an
/// automatic prompt delivery failed on this pane (issue #424).
///
/// Only [`crate::daemon`] sets it, on one synthetic [`EventType::Error`] event
/// bound to the pane's existing card; no agent ever emits it. Consumers that
/// don't know the key ignore it (the documented `metadata` contract), which is
/// why reporting this way needs no protocol change: the value rides the same
/// free-form map `DISPLAY_NAME_METADATA_KEY` and
/// [`SESSION_START_ORIGIN_METADATA_KEY`] already use.
///
/// The value is FIXED daemon text. Nothing a repository, a prompt or a role
/// controls is interpolated into it — the same rule that governed the in-pane
/// notice this replaced, kept because the text still reaches a human.
pub const DELIVERY_NOTICE_METADATA_KEY: &str = "delivery_notice";

/// `AgentEvent.metadata` key declaring WHERE a `SessionStart` came from (PRD
/// #225 M3). The wrapper adapter is the only INTENDED producer, with one of the
/// three values [`WRAPPER_FORK_SESSION_START_ORIGIN`] /
/// [`WRAPPER_INTERFACE_READY_SESSION_START_ORIGIN`] /
/// [`WRAPPER_INTERFACE_SETTLED_SESSION_START_ORIGIN`]; every other producer omits
/// it, and consumers read an absent key as "this `SessionStart` came from an
/// initialized session".
///
/// "Intended" is not "enforced". `metadata` is a free-form, unvalidated map on an
/// unauthenticated socket, so any same-uid process can write any of these values
/// (issue #243 audit F1, reproduced). Anything a consumer GRANTS on the strength
/// of a value here has to establish provenance for itself — see
/// [`WRAPPER_INTERFACE_READY_SESSION_START_ORIGIN`].
///
/// Additive on the wire in both directions: an OLD wrapper emits no key (or only
/// the fork one) and a new daemon treats its events exactly as it does today; a
/// NEW wrapper's key is ignored by an old daemon. The KEY is therefore not a
/// [`crate::daemon_protocol::PROTOCOL_VERSION`] bump — but issue #243's second
/// value is a semantic change behind that stable wire, because a wrapper now
/// emits TWO `SessionStart` events where it emitted one. See
/// `changelog.d/243.breaking.md`.
pub const SESSION_START_ORIGIN_METADATA_KEY: &str = "session_start_origin";

/// The [`SESSION_START_ORIGIN_METADATA_KEY`] value meaning "this `SessionStart`
/// was emitted by `dot-agent-deck wrap` right after `fork`/`exec`, purely to
/// surface the dashboard card for a slow-booting agent" (PRD #225 M3). It does
/// NOT mean the wrapped agent can accept input yet — for Codex the real TUI is
/// still seconds away — so readiness gates
/// ([`crate::state::wait_for_session_start`]) must not treat it as proof of
/// interactivity for an agent that will emit a genuine native `SessionStart`
/// later.
pub const WRAPPER_FORK_SESSION_START_ORIGIN: &str = "wrapper_fork";

/// The [`SESSION_START_ORIGIN_METADATA_KEY`] value meaning "`dot-agent-deck wrap`
/// watched the wrapped child take the inner PTY OUT OF COOKED MODE" (issue #243).
///
/// This is the pre-prompt readiness signal the delegate and scheduler gates were
/// missing. Codex posts its own native `SessionStart` when the first *turn*
/// starts — i.e. as a consequence of the very prompt the gate is withholding —
/// so before this the gate had nothing to release on and paid
/// [`crate::state::SESSION_START_WAIT_TIMEOUT`] in full on every `clear = true`
/// delegate.
///
/// **One fact, not two.** The wrapper observes the child two ways
/// (`InterfaceWatch` in [`crate::wrap`]) and they carry DIFFERENT values, because
/// they are not equally strong. This value is fact 1 — the child cleared
/// `ICANON`/`ECHO`, a genuine observation that the child consumes keystrokes
/// rather than echoing them. Fact 2 — output went quiet for a while — carries
/// [`WRAPPER_INTERFACE_SETTLED_SESSION_START_ORIGIN`] instead and buys strictly
/// less; see that constant for why.
///
/// **What this fact does NOT establish: that the child will accept a submit.**
/// It was read that way for two rounds of this issue — as the exact inverse of
/// the PRD #225 defect where a prompt was echoed away by a still-canonical line
/// discipline, and therefore as proof that no blind interval was left to pay.
/// Measurement retracts that. A full-screen TUI enables raw mode at INIT, before
/// it paints: real codex-cli 0.149.0 does it 85 ms after a direct exec, and
/// `orchestration/delegate/009` recorded fork + 100 ms on both the original
/// worker and its `clear = true` replacement, then lost the pointer into an
/// unsubmitted composer. Raw mode is NECESSARY for input-readiness and not
/// SUFFICIENT — it says the AGENT owns the terminal, not that the composer is
/// listening — so this value RELEASES the readiness gate (it is the best release
/// signal the deck has) and still owes a post-readiness buffer, sized against the
/// initialisation it announces the start of. See
/// [`crate::state::WRAPPER_INTERFACE_READINESS_BUFFER`].
///
/// It is still WRAPPER PROVENANCE rather than an agent conversation: the session
/// id on it is the wrapper's own, not the agent's, so it must never bind a
/// delivery's generation or move a pane's hook session. That is what
/// [`AgentEvent::is_wrapper_session_start`] separates from
/// [`AgentEvent::is_wrapper_fork_session_start`], which stays fork-only so the
/// readiness gate can still tell the wrapper's events apart.
///
/// **What carrying this value does NOT establish: that a wrapper wrote it.**
/// `dot-agent-deck hook` refuses to forward it (`crate::hook` narrows that
/// forwarding to the fork value alone), and that narrowing is worth keeping — but
/// it is not the trust boundary, because the daemon's hook socket ALSO accepts a
/// raw [`AgentEvent`] JSON line whose `metadata` map is free-form and
/// unvalidated. Any same-uid process can therefore post an event carrying this
/// value; it was reproduced during issue #243's audit from a bare `python3` with
/// no deck environment at all. Provenance is established by the DAEMON, at the
/// site that acts on it — `crate::state::dispatch_one_owned` prices this value as
/// a real TUI's initialisation only for an agent this daemon itself spawned as a
/// Wrapper-strategy agent
/// (`crate::agent_pty::AgentPtyRegistry::agent_spawned_as_wrapper_host`, read
/// from the frozen launch-shape record no hook path can write). What a forgery
/// can buy is bounded by what this value grants, and since it no longer
/// suppresses a buffer the answer is a gate release that a bare unmarked
/// `SessionStart` already bought before this issue. Do not add a new privilege
/// keyed on this value without going through that check too — and do not
/// reintroduce one that a blind interval no longer covers.
pub const WRAPPER_INTERFACE_READY_SESSION_START_ORIGIN: &str = "wrapper_interface_ready";

/// The [`SESSION_START_ORIGIN_METADATA_KEY`] value meaning "`dot-agent-deck wrap`
/// saw the wrapped child's output SETTLE" — it wrote something and then went
/// quiet for `INTERFACE_SETTLE_WINDOW` (750 ms; `crate::wrap`) (issue #243).
///
/// The weaker of the wrapper's two interface facts, and split out from
/// [`WRAPPER_INTERFACE_READY_SESSION_START_ORIGIN`] because settling is a GUESS
/// where raw-input mode is an observation. Silence means "stopped producing
/// output"; whether the thing that stopped is an interface waiting at its prompt
/// or a LAUNCHER stalled part-way through its own boot is precisely what it
/// cannot tell you. The production launch shape is `devbox run codex-big`, which
/// prints one banner line at ~0.1 s and then computes its shellenv in SILENCE for
/// a measured 2750–4132 ms before `codex` is exec'd at all — so it satisfies this
/// fact while the pty is still in cooked mode, which is PRD #225 Defect 1
/// exactly.
///
/// **It is therefore PROVISIONAL, not a release.** That distinction was learned
/// the expensive way. The two facts do not arrive in order of strength: measured
/// over 13 launcher probes and 8 wrapper spawns, this one fired 21/21 and the
/// strong observation never fired first, arriving 2005–3370 ms later. A gate that
/// released here and paid the 1000 ms buffer still wrote at +1.85 s into the
/// launcher's own line discipline, and 3/3 production runs left the pointer
/// parked unsubmitted in Codex's composer with no turn ever starting. So for a
/// Wrapper-strategy agent the gate holds this fact for
/// [`crate::state::INTERFACE_UPGRADE_WINDOW`] to see whether
/// [`WRAPPER_INTERFACE_READY_SESSION_START_ORIGIN`] is still coming, and releases
/// on it only when the window expires — with the post-readiness buffer
/// ([`crate::state::DELEGATE_READINESS_BUFFER`]) still on, since it never skips
/// that. A settled launcher and a settled REPL stay indistinguishable at this
/// seam; what the window buys is the later evidence that tells them apart, and
/// what the buffer covers is the case where none arrives.
///
/// Waiting forever is still worse than releasing on a guess, which is why the
/// window is a bound and not a condition. The bound is
/// [`crate::state::SESSION_START_WAIT_TIMEOUT`] itself, so a wrapped agent whose
/// strong fact never arrives reaches its prompt at the same instant it did
/// before this issue: the fallback costs the wait this issue opened on, and
/// never a second more.
///
/// Everything said about provenance on
/// [`WRAPPER_INTERFACE_READY_SESSION_START_ORIGIN`] applies to this value too.
pub const WRAPPER_INTERFACE_SETTLED_SESSION_START_ORIGIN: &str = "wrapper_interface_settled";

/// `AgentEvent.metadata` key carrying `codex_spawn_prep`'s (`src/wrap.rs`) REAL
/// native hook install/trust outcome for the invocation that produced this
/// `SessionStart` (PRD #254). Only the wrapper sets it, and only on a
/// Codex-identity event — every other producer and agent type omit it. Value
/// is the `bool`'s `Display` form (`"true"`/`"false"`); an absent key means
/// "no outcome reported" (an old wrapper build, a non-Codex identity, or a
/// non-wrapper producer), which consumers must read as unknown, not as
/// success — see [`AgentEvent::codex_hook_trust_outcome`].
///
/// Additive on the wire in both directions, same as
/// [`SESSION_START_ORIGIN_METADATA_KEY`]: an old wrapper omits the key and a
/// new daemon reads `None`; a new wrapper's key is ignored by an old daemon.
/// Not a [`crate::daemon_protocol::PROTOCOL_VERSION`] bump.
pub const CODEX_HOOK_TRUST_METADATA_KEY: &str = "codex_hook_trust_confirmed";

/// PRD #20 M1: current schema version of the [`AgentEvent`] JSON wire shape.
///
/// This versions the **payload shape of a single `AgentEvent` record** — the
/// stable public JSON schema documented on [`AgentEvent`] below. It is
/// DISTINCT from [`crate::daemon_protocol::PROTOCOL_VERSION`], which versions
/// the **attach-socket handshake/framing** between a TUI and the daemon. The
/// two move independently: adding an optional, serde-skipped field to
/// `AgentEvent` (as M1 does) bumps neither, because old and new peers stay
/// wire-compatible; a breaking change to the *attach handshake* bumps only
/// `PROTOCOL_VERSION`; a breaking change to the *event record shape* would
/// bump only this constant. Do not conflate them.
///
/// Producers MAY stamp [`AgentEvent::schema_version`] with this value to
/// advertise which schema they wrote. It stays at `1` for the current shape;
/// a future, non-additive change to the record's fields bumps it. Because the
/// field is optional and skipped when `None`, existing producers that leave it
/// unset emit byte-identical JSON to before, and a consumer treats a missing
/// `schema_version` as the baseline (v1) schema.
pub const AGENT_EVENT_SCHEMA_VERSION: u32 = 1;

/// Stable public JSON schema for a single agent event.
///
/// `AgentEvent` is the wire record every agent integration (Claude Code hooks,
/// the OpenCode plugin, Pi's `agent-event` CLI, and future wrapper adapters)
/// serializes to the daemon's hook socket, and that the daemon re-broadcasts to
/// attached TUIs over `KIND_EVENT` (wrapped in [`BroadcastMsg::Event`]). Third
/// parties author events against this schema, so it is a **stable public API**:
/// fields are added additively (optional + serde-skipped so old and new
/// payloads round-trip unchanged), never repurposed. The record's schema
/// version is [`AGENT_EVENT_SCHEMA_VERSION`] (distinct from the attach-wire
/// [`crate::daemon_protocol::PROTOCOL_VERSION`] — see that constant's docs).
///
/// ## JSON schema (field · type · optionality · meaning · producers)
///
/// - **`session_id`** · string · **required** · stable id that groups events
///   into a single dashboard card. Set by every producer.
/// - **`agent_type`** · enum ([`AgentType`], snake_case) · **required** · which
///   agent produced the event. Claude hooks set `claude_code`, the OpenCode
///   plugin sets `open_code`, Pi's `agent-event` CLI sets `pi`, the
///   live-surface path derives it from the spawn command via
///   [`AgentType::from_command`]. Unrecognized values decode to `none` (the
///   `#[serde(other)]` catch-all), never failing the whole-record decode.
/// - **`event_type`** · enum ([`EventType`], snake_case) · **required** · the
///   lifecycle/tool event that drives the card's status. Set by every producer.
/// - **`tool_name`** · string · optional (omitted/`null` ⇒ `None`) · the tool
///   for `tool_start` / `tool_end` events. Set by the Claude/OpenCode hook
///   builders; `None` for pure lifecycle events.
/// - **`tool_detail`** · string · optional · short human-readable detail for a
///   tool event (e.g. the file path or command). Set by the hook builders.
/// - **`cwd`** · string · optional · working directory of the session. Set by
///   hooks and the live-surface path; used for orchestration bucketing.
/// - **`timestamp`** · string (RFC 3339 / ISO 8601 UTC) · **required** · when
///   the event was produced. Set by every producer.
/// - **`user_prompt`** · string · optional · truncated text of the user prompt
///   that triggered the turn. Set by hooks when a prompt is present.
/// - **`metadata`** · object (string→string) · optional (defaults to empty) ·
///   free-form extra keys, e.g. [`DISPLAY_NAME_METADATA_KEY`], `bash_command`,
///   `permission_state`. Consumers treat unknown keys as ignorable.
/// - **`pane_id`** · string · optional · the `DOT_AGENT_DECK_PANE_ID` the event
///   routes to. Populated from the env var the daemon injects at spawn; `None`
///   for events not scoped to a known pane.
/// - **`agent_id`** · string · optional · daemon-side registry id of the
///   producing agent (from `DOT_AGENT_DECK_AGENT_ID`). Lets agent-id-scoped
///   filters (e.g. post-respawn `SessionStart` waits) target the right agent;
///   `None` payloads simply don't match those filters.
/// - **`agent_version`** · string · optional (**PRD #20 M1**, added additively)
///   · self-reported version of the agent binary/integration that produced the
///   event (e.g. a Codex/Claude CLI version), for diagnostics and
///   version-aware rendering. No current producer sets it; `None` (the default,
///   omitted from the wire) means "version not reported".
/// - **`schema_version`** · integer · optional (**PRD #20 M1**, added
///   additively) · the [`AGENT_EVENT_SCHEMA_VERSION`] the producer wrote, for
///   forward compatibility. The wrapper adapter ([`crate::wrap`]) stamps it on
///   every event it emits; native hooks currently omit it. `None` (the default,
///   omitted from the wire) is read as the baseline (v1) schema. This is the
///   **event-record** schema version, NOT the attach-wire
///   [`crate::daemon_protocol::PROTOCOL_VERSION`].
/// - **`live_target`** · object (`{ "kind": <TargetKind>, "writable":
///   <Writable> }`, both kebab-case) · optional (**PRD #20 M3**, added
///   additively; omitted/`null` ⇒ `None`) · declares whether/how the session
///   can receive dashboard input (see [`LiveTarget`]). Producers: the wrapper
///   adapter ([`crate::wrap`]) stamps it on every event — `pty`/`live` when it
///   runs inside a daemon-managed pane (its child is reachable through that
///   live PTY), else `process`/`history-only` for a standalone wrap. Native
///   PTY panes (Claude/OpenCode/Pi) omit it, which the UI reads as the
///   historical `live`/writable default. Absence never fails the decode.
/// - **`model`** · string · optional (**PRD fork#378**, added additively) ·
///   the agent's self-reported active model (e.g. `"Opus"`,
///   `"gpt-5.1-codex-mini"`), posted top-level by the hook exactly as
///   `agent_version` is. `None` (the default, omitted from the wire) means
///   "model not reported"; a later event carrying a different model
///   overwrites a previously-known one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEvent {
    pub session_id: String,
    pub agent_type: AgentType,
    pub event_type: EventType,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_detail: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    pub timestamp: DateTime<Utc>,
    #[serde(default)]
    pub user_prompt: Option<String>,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
    #[serde(default)]
    pub pane_id: Option<String>,
    /// PRD #92 F9 followup-7: daemon-side registry id of the agent
    /// that produced this hook event. Populated by the agent's hook
    /// script from the `DOT_AGENT_DECK_AGENT_ID` env var the daemon
    /// injects at spawn time (same pattern as
    /// [`crate::agent_pty::DOT_AGENT_DECK_PANE_ID`]). Lets the
    /// post-respawn dispatch task scope its `SessionStart` wait to
    /// the NEW agent's id, so a late `SessionStart` from the OLD
    /// agent — emitted in the subscribe→kill window — can't be
    /// mis-accepted as the NEW agent's readiness signal. Optional
    /// because hook payloads from external agents (or test forgers)
    /// may omit it; events with `None` simply won't match
    /// agent-id-scoped filters.
    #[serde(default)]
    pub agent_id: Option<String>,
    /// PRD #20 M1: self-reported version of the agent binary/integration that
    /// produced this event (e.g. a wrapped Codex or Claude CLI version), for
    /// diagnostics and version-aware rendering. Optional and additive:
    /// `#[serde(default)]` lets older payloads that lack the field deserialize
    /// to `None`, and `skip_serializing_if` omits it from the wire when unset —
    /// so existing producers emit byte-identical JSON and old/new peers stay
    /// compatible. No current producer sets it; `None` means "version not
    /// reported".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_version: Option<String>,
    /// PRD #20 M1: the [`AGENT_EVENT_SCHEMA_VERSION`] the producer wrote, for
    /// forward compatibility of this record's JSON shape. This is the
    /// **event-record** schema version and is DISTINCT from the attach-socket
    /// [`crate::daemon_protocol::PROTOCOL_VERSION`] (see those docs). Optional
    /// and additive for the same reasons as `agent_version`: a missing value
    /// deserializes to `None` and is read as the baseline (v1) schema, and it
    /// is omitted from the wire when unset. The wrapper adapter
    /// ([`crate::wrap`]) stamps it on every event it emits; native hooks
    /// currently leave it unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u32>,
    /// PRD #20 M3: per-session live-target descriptor declaring whether/how the
    /// session can receive input (see [`LiveTarget`]). Optional and additive:
    /// `#[serde(default)]` lets legacy payloads that predate the field
    /// deserialize to `None`, and `skip_serializing_if` omits it from the wire
    /// when unset — so existing producers emit byte-identical JSON and old/new
    /// peers stay compatible. The wrapper adapter ([`crate::wrap`]) stamps
    /// `pty`/`live` when it runs inside a daemon-managed pane and
    /// `process`/`history-only` for a standalone wrap; native PTY panes leave
    /// it `None`, which the UI reads as the historical live/writable default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_target: Option<LiveTarget>,
    /// PRD fork#378: the agent's self-reported active model (e.g. `"Opus"`,
    /// `"gpt-5.1-codex-mini"`), for the agent-type badge's model segment.
    /// Optional and additive for the same reasons as `agent_version`:
    /// `#[serde(default)]` lets older payloads that lack the field
    /// deserialize to `None`, and `skip_serializing_if` omits it from the
    /// wire when unset — so existing producers emit byte-identical JSON and
    /// old/new peers stay compatible. `None` means "model not reported".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl AgentEvent {
    /// PRD #225 M3: does this event carry the wrapper's fork-time,
    /// card-surfacing origin marker (see
    /// [`WRAPPER_FORK_SESSION_START_ORIGIN`])? `false` for every event without
    /// the marker — including everything an older wrapper or a native hook
    /// emits — so the absent-key default is "a genuine, session-derived event".
    pub fn is_wrapper_fork_session_start(&self) -> bool {
        self.metadata
            .get(SESSION_START_ORIGIN_METADATA_KEY)
            .is_some_and(|origin| origin == WRAPPER_FORK_SESSION_START_ORIGIN)
    }

    /// Issue #243: does this event carry the wrapper's INTERFACE-READY origin
    /// marker (see [`WRAPPER_INTERFACE_READY_SESSION_START_ORIGIN`])?
    ///
    /// The strongest readiness marker in the system: the wrapper watched this
    /// child clear `ICANON`/`ECHO` on the inner PTY, as opposed to "a session
    /// object exists". It is the only marker for which a readiness gate may drop
    /// the blind post-signal buffer — and even then only after the gate has
    /// established that the agent it names is one THIS daemon spawned as a
    /// wrapper, because the marker itself is producer-writable (see the
    /// constant).
    ///
    /// Narrower than the question most callers want. "Did the wrapper observe
    /// the interface at all" — either fact, which is what RELEASES the gate — is
    /// [`Self::is_wrapper_interface_session_start`].
    pub fn is_wrapper_interface_ready_session_start(&self) -> bool {
        self.metadata
            .get(SESSION_START_ORIGIN_METADATA_KEY)
            .is_some_and(|origin| origin == WRAPPER_INTERFACE_READY_SESSION_START_ORIGIN)
    }

    /// Issue #243 (review): does this event carry the wrapper's OUTPUT-SETTLED
    /// origin marker (see [`WRAPPER_INTERFACE_SETTLED_SESSION_START_ORIGIN`])?
    ///
    /// The weaker interface fact — the child wrote something and then went quiet.
    /// Enough to release a readiness gate that would otherwise wait 30 s for a
    /// signal that never comes; NOT enough to drop the post-readiness buffer,
    /// because a stalled launcher settles exactly like a REPL waiting at its
    /// prompt.
    pub fn is_wrapper_interface_settled_session_start(&self) -> bool {
        self.metadata
            .get(SESSION_START_ORIGIN_METADATA_KEY)
            .is_some_and(|origin| origin == WRAPPER_INTERFACE_SETTLED_SESSION_START_ORIGIN)
    }

    /// Issue #243: did the wrapper observe this child's interface AT ALL —
    /// either fact?
    ///
    /// This is the READINESS question, and it is the one the gate asks: both
    /// facts mean the wrapper saw something happen to the child that a bare
    /// fork-time event does not, so both release the wait. What they do not share
    /// is how much they prove, which is why the buffer keys on the narrower
    /// [`Self::is_wrapper_interface_ready_session_start`] instead.
    pub fn is_wrapper_interface_session_start(&self) -> bool {
        self.is_wrapper_interface_ready_session_start()
            || self.is_wrapper_interface_settled_session_start()
    }

    /// Issue #243: was this `SessionStart` authored by `dot-agent-deck wrap`
    /// ABOUT ITS OWN CHILD — any of its origins — rather than by an initialized
    /// agent session announcing itself?
    ///
    /// This is the "is it a conversation" question, and it is NOT the same as the
    /// readiness question. Every wrapper event carries the wrapper's own session
    /// id, so none may bind a delivery's generation, move a pane's hook session,
    /// or arm a re-submission — while the interface ones *do* satisfy the
    /// readiness gate and the fork-time one usually does not. Every
    /// site that previously asked `!is_wrapper_fork_session_start()` to mean
    /// "genuine conversation" asks this instead; the two sites that genuinely
    /// mean "fork-time boot provenance" keep asking the narrower question.
    ///
    /// `false` for every event without the key — including everything an older
    /// wrapper, a native hook or a future producer emits — so the absent-key
    /// default stays "a genuine, session-derived event".
    pub fn is_wrapper_session_start(&self) -> bool {
        self.is_wrapper_fork_session_start() || self.is_wrapper_interface_session_start()
    }

    /// PRD #254: this event's stamped Codex native-hook install/trust outcome
    /// (see [`CODEX_HOOK_TRUST_METADATA_KEY`]), if any. `None` when the key is
    /// absent — an old wrapper build, a non-Codex-identity spawn, or any
    /// non-wrapper producer, none of which report anything about hook trust,
    /// and none of which should be read as either a success or a failure.
    /// `Some(false)` (auditor I2) when the key IS present but its value is
    /// neither `"true"` nor `"false"` — a future format, a typo, a corrupted
    /// write. A present-but-garbled value is evidence something went wrong
    /// and must not be read the same as an absent key, or it silently
    /// restores the pre-fix `Reports` behaviour H1 exists to close.
    pub fn codex_hook_trust_outcome(&self) -> Option<bool> {
        match self
            .metadata
            .get(CODEX_HOOK_TRUST_METADATA_KEY)
            .map(String::as_str)
        {
            Some("true") => Some(true),
            Some("false") => Some(false),
            Some(_) => Some(false),
            None => None,
        }
    }

    /// Issue #424 D4: was this event SYNTHESIZED BY THE DAEMON rather than
    /// produced by the pane's agent?
    ///
    /// The daemon emits identified events of its own through the same pipeline
    /// real hook events take — [`EventType::ShellBusy`]/[`EventType::ShellIdle`]
    /// from the shell-activity monitor (PRD #370/#386), and the delivery-notice
    /// [`EventType::Error`] (issue #424). They carry the pane's registry
    /// `agent_id` because that is how they land on the right card, and that is
    /// exactly what made them indistinguishable from producer evidence to
    /// `crate::ui::evidence_channel_is_unidentified`: one of them arriving was
    /// enough to conclude the pane has a tagged reporting channel, when it proves
    /// only that the DAEMON can tag its own events. A pane behind a legacy
    /// untagged hook then resumed retyping through a channel that still could not
    /// confirm anything.
    ///
    /// The delivery-notice half is recognized by its metadata key, and that is
    /// safe in the only direction it can be abused: a forged event claiming the
    /// key is EXCLUDED from the evidence channel, i.e. it loses standing rather
    /// than gaining any. The key is not, and must not be treated as, an
    /// authentication marker (auditor) — a forged raw `Error` without it marks a
    /// card exactly as it did before.
    pub fn is_daemon_synthetic(&self) -> bool {
        matches!(self.event_type, EventType::ShellBusy | EventType::ShellIdle)
            || self.metadata.contains_key(DELIVERY_NOTICE_METADATA_KEY)
    }
}

/// Envelope for messages sent to the daemon over the Unix socket.
///
/// Existing hook senders transmit raw `AgentEvent` JSON (no `message_type` field).
/// New message types (e.g. `WorkDone`) include `"message_type": "work_done"` so the
/// daemon can distinguish them.  The daemon tries `DaemonMessage` first, then falls
/// back to `AgentEvent`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "message_type")]
pub enum DaemonMessage {
    /// Orchestrator delegates work to one or more worker roles.
    #[serde(rename = "delegate")]
    Delegate(DelegateSignal),
    /// Worker (or orchestrator with `done`) reports task completion.
    #[serde(rename = "work_done")]
    WorkDone(WorkDoneSignal),
    /// PRD #201 native prompt delivery: a READ-ONLY request for the seed the
    /// daemon prepared for a pane, so the pane's extension can deliver it
    /// NATIVELY (`pi.sendUserMessage`) instead of the daemon typing it into the
    /// PTY. Unlike the two fire-and-forget signals above, this one gets a
    /// reply: the daemon writes a [`GetSeedResponse`] JSON line back on the
    /// same connection, then the seed is cleared (delivered exactly once). An
    /// older daemon that doesn't know this variant fails to parse it and sends
    /// no reply — the `get-seed` CLI then reports an empty seed, the extension
    /// no-sends, and the daemon's PTY-injection safety net still delivers. So
    /// this variant degrades gracefully across versions (see the rule-12 note
    /// in `docs/develop/versioning.md`): it rides the unversioned hook socket
    /// and does NOT move the attach `PROTOCOL_VERSION`.
    #[serde(rename = "get_seed")]
    GetSeed(GetSeedRequest),
    /// PRD #220: agent-callable dispatch — creates a git worktree and spawns a
    /// fully-isolated orchestration inside it. One-step parallel line of work:
    /// the agent calls `dispatch <name>` and the daemon handles worktree
    /// lifecycle (create, spawn, cleanup on tab close).
    #[serde(rename = "dispatch")]
    Dispatch(DispatchSignal),
    /// PRD #220: read-only request for the spawn targets available to a pane's
    /// repo. Like [`Self::GetSeed`], the daemon writes a
    /// [`ListTargetsResponse`] JSON line back on the same connection; every other
    /// message on this socket is fire-and-forget.
    ///
    /// Answered by the DAEMON rather than computed in the CLI so the menu comes
    /// from the same cwd and the same config the dispatch will use — a listing
    /// that can disagree with the spawn is worse than no listing. Additive on the
    /// hook socket, so it does NOT move the attach `PROTOCOL_VERSION`; an older
    /// daemon simply never replies, which the CLI degrades on.
    #[serde(rename = "list_targets")]
    ListTargets(ListTargetsRequest),
}

/// PRD #201: payload of [`DaemonMessage::GetSeed`] — the pane whose pending
/// seed the caller wants. Sourced from `DOT_AGENT_DECK_PANE_ID` by the
/// `get-seed` CLI (same pane-scoping the delegate / work-done / agent-event
/// verbs use).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetSeedRequest {
    pub pane_id: String,
}

/// PRD #201: the daemon's reply to a [`DaemonMessage::GetSeed`], written as a
/// single JSON line back on the hook-socket connection. `seed` is `None`
/// (serialized as `null`) when no seed is pending for the pane — the pane is
/// unknown, or the seed was already delivered (pulled or injected).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetSeedResponse {
    #[serde(default)]
    pub seed: Option<String>,
}

/// PRD #220: payload of [`DaemonMessage::ListTargets`] — the pane whose repo's
/// spawn targets the caller wants listed.
///
/// Pane-scoped rather than carrying a directory, deliberately: the daemon
/// resolves the caller's cwd from the PTY registry's `AgentRecord.cwd`, which is
/// the SAME source `dispatch` itself uses. An earlier cut read the CLI process's
/// own `current_dir()` locally, which diverged from the dispatch whenever the
/// agent had `cd`'d — the listing then advertised targets the dispatch could not
/// start, or reported none where the repo defined them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListTargetsRequest {
    pub pane_id: String,
}

/// PRD #220: the daemon's reply to a [`DaemonMessage::ListTargets`], one JSON
/// line back on the hook-socket connection (the [`GetSeedResponse`] pattern).
///
/// `rendered` is the human-readable listing the dispatcher agent relays to the
/// user; `orchestrations` carries the same data structurally so a caller can act
/// on it without parsing prose. `error` is set when the repo's
/// `.dot-agent-deck.toml` exists but could not be parsed — distinguishing "no
/// orchestrations" from "your config is broken", which a bare empty list cannot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListTargetsResponse {
    pub rendered: String,
    #[serde(default)]
    pub orchestrations: Vec<ListedOrchestration>,
    #[serde(default)]
    pub error: Option<String>,
}

/// One spawnable orchestration in a [`ListTargetsResponse`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListedOrchestration {
    pub name: String,
    pub roles: usize,
    /// Issue #704: is this the one a dispatch with no `--orchestration <name>`
    /// (and a scheduled task rooted here) would open?
    ///
    /// Additive and `#[serde(default)]`, so an older daemon's reply — which omits
    /// the key entirely — still parses, with `false` for every entry. That reads
    /// as "this build cannot tell you which is the default", which is exactly
    /// true of it, so no `PROTOCOL_VERSION` bump: the wire SHAPE is unchanged for
    /// every peer that does not know the field.
    #[serde(default)]
    pub default: bool,
}

/// The daemon's reply to a [`DaemonMessage::Delegate`], one JSON line back on
/// the hook-socket connection (the [`GetSeedResponse`] / [`ListTargetsResponse`]
/// pattern).
///
/// `delegate` used to be fire-and-forget, and that is half of a reported bug: an
/// orchestration started by `dispatch --orchestration` could not delegate at all,
/// yet every `dot-agent-deck delegate` it ran printed nothing and exited 0. The
/// orchestrator therefore announced that its worker was working and waited
/// forever for a `work-done` that could not arrive. A delegation that reached
/// nobody has to be distinguishable from one that reached somebody, and the only
/// place that distinction exists is the daemon.
///
/// `error` is a routing failure that stopped the delegate outright (the sender is
/// not a pane the daemon holds a role for, or is not an orchestrator).
/// `unresolved_roles` is the partial case: the sender was fine, but one or more
/// `--to` roles matched no worker pane. They are kept separate so the message can
/// say which happened, and because they carry different verdicts: `error` means
/// nothing landed, while `unresolved_roles` alongside a non-empty `delivered` is
/// a delegate that HALF landed — see the `Delegate` arm of `main.rs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegateResponse {
    /// Affirmative discriminator, always [`DELEGATE_RESPONSE_KIND`] on a reply
    /// this daemon wrote.
    ///
    /// PR #466 review: without it, success was *residual* rather than
    /// affirmative. Every other field is `#[serde(default)]`, so
    /// `from_str::<DelegateResponse>` succeeds on ANY JSON object — `{}`, or
    /// another verb's reply such as `{"seed":null}` — and yields empty
    /// `delivered`, empty `unresolved_roles`, no `error`: indistinguishable
    /// from a clean success. The one verb whose whole purpose is answering
    /// "did this land?" answered yes whenever it could not tell. A reply that
    /// does not carry this marker is now treated exactly like
    /// [`crate::hook::SocketReply::NoReply`] — a daemon we do not understand,
    /// which is not proof of failure but is not evidence of success either.
    ///
    /// `Option<String>` and `#[serde(default)]` on purpose: the field must
    /// deserialize to `None` when absent (that is the whole point), while the
    /// hand-written [`Default`] impl below stamps it on every response the
    /// daemon builds, including the `..Default::default()` early returns.
    #[serde(default)]
    pub kind: Option<String>,
    /// Roles whose dispatch was queued to a worker pane.
    ///
    /// "Queued to a pane that resolved at delegate time" is the exact claim —
    /// the fan-out is detached (see [`crate::state::AppState::handle_delegate`]),
    /// so no synchronous reply can promise the worker read it. A role whose
    /// worker exited WITHOUT going through the `StopAgent` close path also still
    /// resolves here, because only that path calls `AppState::unregister_pane`
    /// (greptile P1 on PR #466, deferred to issue #524 — the liveness of a
    /// registered pane is not decidable here, since a `clear = true` role's dead
    /// pane is legitimately respawned by the dispatch rather than being a miss).
    #[serde(default)]
    pub delivered: Vec<String>,
    /// Roles named by `--to` that resolved to no worker pane in this
    /// orchestration.
    #[serde(default)]
    pub unresolved_roles: Vec<String>,
    /// Set when the delegate could not be routed at all.
    #[serde(default)]
    pub error: Option<String>,
}

/// The value [`DelegateResponse::kind`] carries on every reply this daemon
/// writes. Bumping or renaming it would make every older CLI stop trusting the
/// reply and fall back to "delivered, unverifiable" — a safe degradation, but a
/// deliberate one.
pub const DELEGATE_RESPONSE_KIND: &str = "delegate";

impl Default for DelegateResponse {
    fn default() -> Self {
        Self {
            kind: Some(DELEGATE_RESPONSE_KIND.to_string()),
            delivered: Vec::new(),
            unresolved_roles: Vec::new(),
            error: None,
        }
    }
}

impl DelegateResponse {
    /// Whether this parsed reply positively identifies itself as a delegate
    /// response. See [`Self::kind`].
    pub fn is_delegate_reply(&self) -> bool {
        self.kind.as_deref() == Some(DELEGATE_RESPONSE_KIND)
    }
}

/// Signal sent by the orchestrator via `dot-agent-deck delegate`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegateSignal {
    pub pane_id: String,
    pub task: String,
    /// Role names to delegate to (one or more).
    pub to: Vec<String>,
    pub timestamp: DateTime<Utc>,
    /// Issue #586 M4: an optional subject tag (issue/PR number, or a short
    /// opaque token) this delegation is for. `#[serde(default)]` so an older
    /// CLI's payload (no `subject` field) still parses to `None` — additive,
    /// never rejects, no `PROTOCOL_VERSION` bump.
    #[serde(default)]
    pub subject: Option<String>,
}

/// Daemon → attached-TUI broadcast (PRD #76 M2.17). The daemon publishes
/// one of these per ingested hook event; subscribers receive them as
/// `KIND_EVENT` frames on the attach socket.
///
/// PRD #93 round-5: the `Delegate` / `WorkDone` variants used to ride this
/// channel too, because the daemon couldn't validate or dispatch them
/// locally in external-daemon mode (the role map lived on the TUI side).
/// The daemon now owns the role map and the PTY registry, so it dispatches
/// those signals directly into the target pane's PTY — no broadcast hop,
/// no replay buffer, no salvage. Only hook events keep using this channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum BroadcastMsg {
    /// A hook event (existing M2.17 wire shape, now wrapped).
    #[serde(rename = "event")]
    Event(AgentEvent),
    /// PRD #120: a daemon-spawned ORCHESTRATION (the issue-dispatch path),
    /// pushed to already-attached TUIs so they can build the orchestration tab
    /// LIVE — mid-session, with no reconnect. The single-agent live-surface
    /// path (a synthetic [`EventType::SessionStart`] painted as a flat
    /// dashboard card by [`crate::state::AppState::apply_event`]) cannot
    /// reconstruct a multi-role tab, and orchestration tabs were previously
    /// rebuilt ONLY at TUI hydration (startup / reconnect). This variant
    /// carries the structural membership the TUI's
    /// `open_orchestration_tab_with_existing_role_panes` machinery needs to
    /// build the tab on the fly.
    ///
    /// Adding this variant changes the `KIND_EVENT` payload schema (an older
    /// peer would mis-parse the new `kind` tag), so it bumps
    /// [`crate::daemon_protocol::PROTOCOL_VERSION`].
    #[serde(rename = "orchestration_surface")]
    OrchestrationSurface(OrchestrationSurface),
    /// PRD 236: a dispatched worktree the daemon KEPT on tab close instead of
    /// removing (uncommitted work, or a status probe that itself failed) — see
    /// [`crate::issue_dispatch_run::RemoveOutcome`]. The close handler runs
    /// detached from the close response (`daemon_protocol.rs`), and the pane
    /// that triggered it may already be gone by the time the outcome is known,
    /// so this is a deck-level broadcast rather than a reply riding the close
    /// response itself — every attached TUI gets it, not just the one that
    /// closed the tab.
    ///
    /// Adding this variant changes the `KIND_EVENT` payload schema the same way
    /// [`BroadcastMsg::OrchestrationSurface`] did, so it too bumps
    /// [`crate::daemon_protocol::PROTOCOL_VERSION`].
    #[serde(rename = "worktree_kept")]
    WorktreeKept(WorktreeKeptNotice),
}

/// PRD 236: payload for [`BroadcastMsg::WorktreeKept`] — a dispatched worktree
/// the daemon left on disk instead of removing on tab close, and why.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeKeptNotice {
    /// Absolute path of the retained worktree, exactly as recorded in the
    /// daemon's [`crate::issue_dispatch_run::WorktreeRegistry`]. Named
    /// explicitly rather than only logged server-side, so the user knows where
    /// to go recover the work.
    pub path: String,
    /// Why the worktree was kept rather than removed.
    pub reason: KeptReason,
    /// The error text `git worktree remove` failed with — set exactly when
    /// `reason == KeptReason::RemovalFailed`, `None` otherwise. A separate
    /// field rather than a payload on the `RemovalFailed` variant itself
    /// (PRD 236 review): `#[serde(other)]`'s forward-compat catch-all (on
    /// [`KeptReason::ProbeError`]) is only valid on an externally-tagged enum
    /// whose variants are ALL unit — `serde_derive` rejects the combination
    /// otherwise — so keeping every `KeptReason` variant field-less is what
    /// lets the catch-all exist at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Why [`remove_worktree`](crate::issue_dispatch_run::remove_worktree) left a
/// worktree in place instead of removing it — either
/// [`RemovalPolicy::KeepIfDirty`](crate::issue_dispatch_run::RemovalPolicy::KeepIfDirty)
/// chose not to attempt removal because the tree is dirty (or its dirtiness
/// could not be checked), removal was attempted and failed, or the entry's
/// policy is [`RemovalPolicy::IsolatedClone`](crate::issue_dispatch_run::RemovalPolicy::IsolatedClone),
/// under which removal is never attempted at all, regardless of dirtiness.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KeptReason {
    /// `git status --porcelain` reported uncommitted or untracked changes.
    Dirty,
    /// `git worktree remove` itself was attempted and failed (non-zero exit
    /// or spawn error) — the error text rides [`WorktreeKeptNotice::error`],
    /// not this variant, so `KeptReason` can stay field-less (see that
    /// field's doc for why). Previously this outcome was reported as
    /// [`crate::issue_dispatch_run::RemoveOutcome::Removed`] and never
    /// reached the wire at all (PRD 236 review, reproduced against a locked
    /// worktree on git 2.55.0).
    RemovalFailed,
    /// The entry is an isolated `git clone`
    /// ([`RemovalPolicy::IsolatedClone`](crate::issue_dispatch_run::RemovalPolicy::IsolatedClone)),
    /// not a linked worktree of anything — kept unconditionally rather than
    /// attempting `git worktree remove` (which does not apply to it) or a
    /// bare `remove_dir_all` (which could discard commits that exist only
    /// on this clone's own local branch). PRD fork#325 M3 (issue #490 fix
    /// round); an actually-safe automatic removal is deferred to M4.
    IsolatedClone,
    /// The `git status --porcelain` probe itself failed (not a valid worktree,
    /// `git` missing, etc.) — kept fail-safe: unknown is treated as dirty
    /// rather than assumed clean.
    ///
    /// PRD 236 review: also the forward-compat catch-all (matches
    /// `AgentType::None` / `EventType::Unknown`'s retrofit) for any future
    /// wire variant this build doesn't recognize — the conservative,
    /// already-fail-safe outcome, so an unrecognized reason never fails the
    /// whole `WorktreeKeptNotice` decode. Deserialize-only in practice.
    /// `#[serde(other)]` must be the LAST variant (serde_derive requirement),
    /// which is also why it sits below `RemovalFailed` rather than in the
    /// declaration order the wire's `PROTOCOL_VERSION 7 → 8` bump introduced
    /// them.
    #[serde(other)]
    ProbeError,
}

/// PRD #120: the structural membership of a daemon-spawned orchestration,
/// pushed to attached TUIs (via [`BroadcastMsg::OrchestrationSurface`]) so they
/// can build the orchestration tab live. Mirrors what the hydration partition
/// (`OrchestrationHydrationBucket`) reconstructs from per-pane
/// [`crate::agent_pty::TabMembership`] records at reconnect — but for a spawn
/// that happens WHILE a TUI is attached.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrchestrationSurface {
    /// Canonical orchestration name — the tab IDENTITY and (absent a
    /// `display_title`) the tab-strip LABEL.
    pub name: String,
    /// Absolute orchestration cwd shared by every role pane — the tab's cwd and
    /// the hydration partition's bucket key.
    pub cwd: String,
    /// Optional user-facing tab title; `None` falls back to `name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_title: Option<String>,
    /// The spawned role panes, in role order.
    pub roles: Vec<OrchestrationSurfaceRole>,
}

/// One role pane of a live-surfaced orchestration (see [`OrchestrationSurface`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrchestrationSurfaceRole {
    /// The `DOT_AGENT_DECK_PANE_ID` the daemon tagged the pane with — reused as
    /// the TUI-side local pane id so hook events keep routing correctly. The TUI
    /// attaches to the live PTY by resolving THIS pane id through `list_agents`
    /// (see `EmbeddedPaneController::hydrate_pane`), not by a registry agent id —
    /// so no `agent_id` rides on the wire.
    pub pane_id: String,
    /// Position of this role in the orchestration config's `roles`.
    pub role_index: usize,
    /// Role name (e.g. `orchestrator`, `worker`).
    pub role_name: String,
    /// Whether this is the start (orchestrator) role.
    pub is_start_role: bool,
}

/// PRD #220: signal sent by an agent via `dot-agent-deck dispatch`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchSignal {
    pub pane_id: String,
    pub name: String,
    #[serde(default)]
    pub task: Option<String>,
    /// PRD #220: the shape the USER chose for this unit — `single` for one
    /// agent, `orchestration[:name]` for a team. Absent means "whatever the
    /// dispatched worktree's config implies", the pre-selector behaviour.
    ///
    /// `#[serde(default)]` keeps this additive: an older daemon that never knew
    /// the field is unaffected (it rejects the whole `dispatch` variant anyway),
    /// and an older CLI omitting it still deserializes against a newer daemon.
    /// So the hook-socket shape is unchanged and `PROTOCOL_VERSION` does not move.
    #[serde(default)]
    pub shape: Option<DispatchShape>,
    pub timestamp: DateTime<Utc>,
}

/// PRD #220: the wire form of the user's single-vs-orchestration choice.
///
/// Its own type rather than a bare string so an unrecognised value fails at
/// deserialization instead of being silently read as "use the default" — the
/// selector exists to remove exactly that class of surprise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DispatchShape {
    /// One agent, even where the dir defines `[[orchestrations]]`.
    SingleAgent,
    /// A full orchestration; `name` absent = the dir's first.
    Orchestration {
        #[serde(default)]
        name: Option<String>,
    },
}

/// Signal sent by a worker via `dot-agent-deck work-done`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkDoneSignal {
    pub pane_id: String,
    pub task: String,
    /// When true, the orchestrator signals that the entire orchestration is complete.
    #[serde(default)]
    pub done: bool,
    pub timestamp: DateTime<Utc>,
    /// Fork #358 (M2 redesign): the `AppState::pane_registration_generation`
    /// value this worker was actually SPAWNED under. The `work-done` CLI
    /// reads this straight from its own `DOT_AGENT_DECK_REGISTRATION_GENERATION`
    /// environment variable — captured and injected at spawn time, sibling
    /// to `DOT_AGENT_DECK_PANE_ID` — rather than asking the daemon what the
    /// pane's generation is right now. That distinction is the whole fix:
    /// a value re-derived from live daemon state at send time is, by
    /// construction, whatever the pane's CURRENT generation is, so it can
    /// never disagree with itself microseconds later at delivery — the
    /// original cut of this field did exactly that and the gate it fed
    /// never actually fired for the reported bug. `handle_work_done`
    /// refuses delivery when this no longer matches the pane's CURRENT
    /// generation: the pane was re-registered (worktree teardown + reuse)
    /// since this worker was spawned, so `pane_cwd_map`/`pane_role_map` now
    /// point at a different tenant. Fork #358 M4: this is only HALF of the
    /// refusal check — see [`Self::daemon_boot_id`] below for the other
    /// half, added because a bare daemon restart can reset this counter to
    /// the same value a pre-restart worker already carried, which this
    /// field alone cannot distinguish. `#[serde(default)]` so an older CLI
    /// build that doesn't send this field still parses (defaulting to `0`,
    /// which never matches a real registration's generation — those start
    /// at `1` — so an old CLI talking to a post-#358 daemon has its reports
    /// refused rather than silently misdelivered; see
    /// `changelog.d/358.breaking.md` for why this tradeoff was accepted).
    #[serde(default)]
    pub generation: u64,
    /// Fork #358 M4: the `AppState::daemon_boot_id` value in effect when
    /// this worker's registration generation (above) was reserved — read
    /// from `DOT_AGENT_DECK_DAEMON_BOOT_ID`, injected at spawn time sibling
    /// to `DOT_AGENT_DECK_REGISTRATION_GENERATION`, same recipe as that
    /// field. Needed because `generation` ALONE turned out not to close
    /// fork issue #358's actual repro (reviewer + auditor, independently,
    /// on 2026-08-17): `pane_registration_generation` is an in-memory map
    /// that resets to empty on every daemon restart, exactly like the
    /// counter it guards — so a pre-restart worker's signal and the
    /// post-restart pane that reused its pane_id can both legitimately
    /// carry generation `1`, and the generation check alone lets the stale
    /// signal through. Pairing it with the daemon's own boot id closes that:
    /// a fresh `AppState` mints a fresh `daemon_boot_id` on every
    /// construction (real restart or test), so a pre-restart value can
    /// never match a post-restart one, whatever the generation says.
    /// `#[serde(default)]` so an older CLI build that doesn't send this
    /// field still parses (defaulting to `""`, which no real
    /// `daemon_boot_id` is ever minted as — see `DaemonBootId::default` —
    /// so an old CLI's report is refused exactly like a bare `generation: 0`
    /// already was; see `changelog.d/358.breaking.md`).
    #[serde(default)]
    pub daemon_boot_id: String,
    /// Issue #586 M4: the subject tag this worker is echoing back on its own
    /// report, so the daemon can compare it against the delegation's own
    /// `DelegateSignal::subject` and flag a disagreement. `#[serde(default)]`
    /// so an older CLI's payload (no `subject` field) still parses to
    /// `None` — additive, never rejects, no `PROTOCOL_VERSION` bump.
    #[serde(default)]
    pub subject: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use spec::spec;

    #[test]
    fn parse_full_event() {
        let json = r#"{
            "session_id": "abc-123",
            "agent_type": "claude_code",
            "event_type": "tool_start",
            "tool_name": "Read",
            "tool_detail": "src/main.rs",
            "cwd": "/home/user/project",
            "timestamp": "2026-03-22T10:00:00Z",
            "metadata": {"key": "value"}
        }"#;
        let event: AgentEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.session_id, "abc-123");
        assert_eq!(event.agent_type, AgentType::ClaudeCode);
        assert_eq!(event.event_type, EventType::ToolStart);
        assert_eq!(event.tool_name.as_deref(), Some("Read"));
    }

    #[test]
    fn parse_minimal_event() {
        let json = r#"{
            "session_id": "abc-123",
            "agent_type": "claude_code",
            "event_type": "idle",
            "timestamp": "2026-03-22T10:00:00Z"
        }"#;
        let event: AgentEvent = serde_json::from_str(json).unwrap();
        assert!(event.tool_name.is_none());
        assert!(event.tool_detail.is_none());
        assert!(event.cwd.is_none());
        assert!(event.metadata.is_empty());
    }

    // PRD #20 M1: an AgentEvent JSON written by an OLDER producer — one that
    // predates the `agent_version` / `schema_version` fields — must still
    // deserialize. This pins the backward-compatibility half of the "stable
    // public JSON schema" contract: adding the two optional fields cannot break
    // decoding of any previously-emitted payload.
    #[test]
    fn parse_event_without_new_version_fields_defaults_to_none() {
        let json = r#"{
            "session_id": "abc-123",
            "agent_type": "claude_code",
            "event_type": "idle",
            "timestamp": "2026-03-22T10:00:00Z"
        }"#;
        let event: AgentEvent = serde_json::from_str(json).unwrap();
        assert!(
            event.agent_version.is_none(),
            "a payload lacking agent_version must decode it as None"
        );
        assert!(
            event.schema_version.is_none(),
            "a payload lacking schema_version must decode it as None (read as baseline v1)"
        );
    }

    // PRD #20 M1: with the new fields SET, the event round-trips through JSON
    // unchanged — the forward half of the schema contract (a newer producer's
    // richer payload survives a serialize→deserialize cycle).
    #[test]
    fn round_trip_event_with_new_version_fields() {
        let event = AgentEvent {
            session_id: "rt-1".into(),
            agent_type: AgentType::ClaudeCode,
            event_type: EventType::Thinking,
            tool_name: None,
            tool_detail: None,
            cwd: None,
            timestamp: chrono::DateTime::parse_from_rfc3339("2026-03-22T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            user_prompt: None,
            metadata: HashMap::new(),
            pane_id: None,
            agent_id: None,
            agent_version: Some("codex-1.2.3".into()),
            schema_version: Some(AGENT_EVENT_SCHEMA_VERSION),
            live_target: None,
            model: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        // Both fields must appear on the wire when set…
        assert!(json.contains("\"agent_version\":\"codex-1.2.3\""), "{json}");
        assert!(json.contains("\"schema_version\":1"), "{json}");
        // …and survive the decode.
        let back: AgentEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back.agent_version.as_deref(), Some("codex-1.2.3"));
        assert_eq!(back.schema_version, Some(AGENT_EVENT_SCHEMA_VERSION));
    }

    // PRD #20 M1: `skip_serializing_if = "Option::is_none"` means an event that
    // leaves the new fields unset emits BYTE-IDENTICAL JSON to before they
    // existed — the keys are absent, not `null`. This is what keeps existing
    // producers behaviour-preserving and old/new peers wire-compatible.
    #[test]
    fn none_version_fields_are_omitted_from_the_wire() {
        let event = AgentEvent {
            session_id: "min-1".into(),
            agent_type: AgentType::ClaudeCode,
            event_type: EventType::Idle,
            tool_name: None,
            tool_detail: None,
            cwd: None,
            timestamp: chrono::DateTime::parse_from_rfc3339("2026-03-22T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            user_prompt: None,
            metadata: HashMap::new(),
            pane_id: None,
            agent_id: None,
            agent_version: None,
            schema_version: None,
            live_target: None,
            model: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(
            !json.contains("agent_version"),
            "None agent_version must be omitted from the wire, not serialized as null: {json}"
        );
        assert!(
            !json.contains("schema_version"),
            "None schema_version must be omitted from the wire, not serialized as null: {json}"
        );
    }

    #[test]
    fn parse_event_with_user_prompt() {
        let json = r#"{
            "session_id": "abc-123",
            "agent_type": "claude_code",
            "event_type": "thinking",
            "user_prompt": "fix the login bug",
            "timestamp": "2026-03-22T10:00:00Z"
        }"#;
        let event: AgentEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.user_prompt.as_deref(), Some("fix the login bug"));
    }

    #[test]
    fn parse_event_without_user_prompt() {
        let json = r#"{
            "session_id": "abc-123",
            "agent_type": "claude_code",
            "event_type": "tool_start",
            "timestamp": "2026-03-22T10:00:00Z"
        }"#;
        let event: AgentEvent = serde_json::from_str(json).unwrap();
        assert!(event.user_prompt.is_none());
    }

    #[test]
    fn reject_invalid_event_type() {
        // PRD #370 (rule-12 wire safety, same class as PRD #201's `AgentType`
        // precedent immediately below): `EventType` gained a `#[serde(other)]`
        // catch-all alongside its new `ShellBusy`/`ShellIdle` variants, so an
        // unrecognized `event_type` no longer fails the whole `AgentEvent`
        // decode — it deserializes to `EventType::Unknown` instead. This test
        // used to assert the OPPOSITE (a hard decode error); the change is
        // deliberate forward-compat, not a regression — see
        // `unknown_event_type_deserializes_to_the_catch_all` below for the
        // dedicated coverage this rename hands off to.
        let json = r#"{
            "session_id": "abc-123",
            "agent_type": "claude_code",
            "event_type": "unknown_type",
            "timestamp": "2026-03-22T10:00:00Z"
        }"#;
        let event: AgentEvent = serde_json::from_str(json).expect("must decode, not error");
        assert_eq!(event.event_type, EventType::Unknown);
    }

    // PRD #370 (rule-12 wire safety): `EventType` gained wire-serialized
    // `ShellBusy`/`ShellIdle` variants (the daemon-synthesized "a foreground
    // shell command is running" signal). Without a `#[serde(other)]`
    // fallback, a NEWER daemon emitting one would break an OLDER reader's
    // WHOLE-frame decode (the `KIND_EVENT` broadcast), stranding its event
    // stream — the exact class of break PRD #201 fixed for `AgentType`
    // above. The catch-all makes any unrecognized value — one of these two
    // new variants at a pre-#370 reader, OR a future event type at today's
    // build — deserialize to the neutral `Unknown` placeholder instead of
    // erroring, so this class of break can never repeat for a future type.
    #[test]
    fn unknown_event_type_deserializes_to_the_catch_all() {
        let ty: EventType = serde_json::from_str("\"some_future_event_type\"").unwrap();
        assert_eq!(ty, EventType::Unknown);

        // Deserialize-only: `Unknown` is never produced by this build, so it
        // has no "own" wire name to round-trip through — unlike
        // `AgentType::None`, which legitimately serializes as `"none"`.
        // Confirm the REAL variants this build DOES produce still round-trip
        // cleanly, so the catch-all didn't disturb ordinary encode/decode.
        assert_eq!(
            serde_json::from_str::<EventType>(
                &serde_json::to_string(&EventType::ShellBusy).unwrap()
            )
            .unwrap(),
            EventType::ShellBusy
        );
    }

    // PRD #76 M2.13: pin the AgentType::from_command inference rules.
    // Spawn-site callers (orchestration roles, new-pane form, session
    // restore) feed the daemon's `StartAgent.agent_type` through this
    // helper so the hydrated dashboard card on reconnect has the right
    // type. The mapping must be stable: a regression that flips the
    // `claude` → ClaudeCode arm would silently strand every reconnected
    // pane back at "No agent".
    #[test]
    fn agent_type_from_command_recognizes_claude() {
        assert_eq!(
            AgentType::from_command(Some("claude")),
            Some(AgentType::ClaudeCode)
        );
        // Full path also resolves via file_name().
        assert_eq!(
            AgentType::from_command(Some("/usr/local/bin/claude")),
            Some(AgentType::ClaudeCode)
        );
        // Args after the binary are ignored.
        assert_eq!(
            AgentType::from_command(Some("claude --dangerously-skip-permissions")),
            Some(AgentType::ClaudeCode)
        );
    }

    #[test]
    fn agent_type_from_command_recognizes_opencode() {
        assert_eq!(
            AgentType::from_command(Some("opencode")),
            Some(AgentType::OpenCode)
        );
        assert_eq!(
            AgentType::from_command(Some("/opt/bin/opencode --foo")),
            Some(AgentType::OpenCode)
        );
    }

    // PRD #201 M1.1 (test-plan row 1): pin the `pi` → AgentType::Pi mapping
    // so a plain `pi` pane and a scheduled `pi` job are recognized as a
    // first-class agent type, and reassert claude/opencode as a regression
    // guard — the same detection path feeds all three. Mirrors the path/arg
    // shapes covered for claude/opencode above.
    #[test]
    fn agent_type_from_command_recognizes_pi() {
        assert_eq!(AgentType::from_command(Some("pi")), Some(AgentType::Pi));
        // Full path also resolves via file_name().
        assert_eq!(
            AgentType::from_command(Some("/usr/local/bin/pi")),
            Some(AgentType::Pi)
        );
        // Args after the binary are ignored.
        assert_eq!(
            AgentType::from_command(Some("pi --some-flag")),
            Some(AgentType::Pi)
        );
        // No regression: claude/opencode still map to their own types.
        assert_eq!(
            AgentType::from_command(Some("claude")),
            Some(AgentType::ClaudeCode)
        );
        assert_eq!(
            AgentType::from_command(Some("opencode")),
            Some(AgentType::OpenCode)
        );
    }

    // PRD #536 follow-up, retargeted after the regression in
    // `spawn_007_hook_learned_badge_does_not_change_respawn_launch`:
    // `devbox run <script>` is a launcher hop this fork's own committed
    // `.dot-agent-deck.toml` uses for every orchestration role (`devbox run
    // claude-sonnet-devbox`, `devbox run claude-opus-devbox --permission-mode
    // plan`, …), but the SAME `AgentType::from_command` also feeds
    // `wrap_launch_command`'s wrap-vs-bare respawn decision (see the
    // documented invariant at `agent_pty.rs:5871-5911`, which names `devbox
    // run codex-big` as its own "resolves to no agent type" exemplar) — so
    // devbox recognition must live ONLY on the separate, presentation-only
    // `AgentType::from_command_including_devbox`, never on `from_command`
    // itself. See `agent_type_from_command_never_resolves_devbox_wrap_decision`
    // just below for the regression-guard half of this split.
    #[test]
    fn from_command_including_devbox_recognizes_devbox_run() {
        assert_eq!(
            AgentType::from_command_including_devbox(Some("devbox run claude-sonnet-devbox")),
            Some(AgentType::ClaudeCode)
        );
        // Trailing args after the script name must not break detection — this
        // is the EXACT shape of this fork's reviewer/auditor role commands.
        assert_eq!(
            AgentType::from_command_including_devbox(Some(
                "devbox run claude-opus-devbox --permission-mode plan"
            )),
            Some(AgentType::ClaudeCode)
        );
        assert_eq!(
            AgentType::from_command_including_devbox(Some("devbox run codex-devbox")),
            Some(AgentType::Codex)
        );
        assert_eq!(
            AgentType::from_command_including_devbox(Some("devbox run some-random-script")),
            None
        );
        // `devbox shell` (this fork's `init_command`) is not a `run` and must
        // NOT match.
        assert_eq!(
            AgentType::from_command_including_devbox(Some("devbox shell")),
            None
        );
        // No further tokens at all — must not panic or misdetect.
        assert_eq!(
            AgentType::from_command_including_devbox(Some("devbox")),
            None
        );
        // Pin the `from_command` fallback branch itself: a plain, non-devbox,
        // already-recognized command must resolve exactly like `from_command`
        // would. Every other assertion above is devbox-shaped, so nothing
        // else in this test would catch that fallback line being deleted.
        assert_eq!(
            AgentType::from_command_including_devbox(Some("claude")),
            Some(AgentType::ClaudeCode)
        );
    }

    // Regression guard for `spawn_007_hook_learned_badge_does_not_change_respawn_launch`
    // (`tests/agent_detection.rs:347`) — `spawn_007` is the ONLY test that actually
    // catches this regression; its sibling `spawn_008` sets an explicit
    // creation-time `agent_type: Some(Codex)` in its fixture, which makes its two
    // code paths converge regardless of the badge, so it does NOT guard this
    // invariant (it instead pins the separate "respawn wrap decision follows the
    // launched command" scenario). The ORIGINAL, shared `AgentType::from_command`
    // — which `wrap_launch_command`'s callers use
    // to decide whether to auto-wrap a respawned launch command — must NEVER
    // resolve a devbox-wrapped command to an agent type, no matter how
    // agent-shaped the devbox script name looks. The documented invariant at
    // `agent_pty.rs:5871-5911` names `devbox run codex-big` as its own
    // "resolves to no agent type" exemplar specifically so a pane whose
    // creation-time identity is frozen via `spawn_agent_type` doesn't get
    // silently auto-wrapped on respawn. Devbox-script recognition belongs
    // exclusively behind `AgentType::from_command_including_devbox`, used only
    // by the badge / `expects_agent_report` call sites in `src/ui.rs`.
    #[test]
    fn agent_type_from_command_never_resolves_devbox_wrap_decision() {
        assert_eq!(AgentType::from_command(Some("devbox run codex-big")), None);
        assert_eq!(
            AgentType::from_command(Some("devbox run claude-sonnet-devbox")),
            None
        );
        assert_eq!(
            AgentType::from_command(Some("devbox run claude-opus-devbox --permission-mode plan")),
            None
        );
        assert_eq!(
            AgentType::from_command(Some("devbox run codex-devbox")),
            None
        );
    }

    #[test]
    fn agent_type_from_command_returns_none_for_unknown_or_empty() {
        // Non-agent commands must NOT misclassify — the daemon would
        // otherwise echo a wrong type via list_agents and the dashboard
        // would mislabel non-agent panes on reconnect.
        assert!(AgentType::from_command(Some("sh")).is_none());
        assert!(AgentType::from_command(Some("/bin/bash")).is_none());
        assert!(AgentType::from_command(Some("vim")).is_none());
        assert!(AgentType::from_command(None).is_none());
        // Whitespace-only / empty input also stays None.
        assert!(AgentType::from_command(Some("")).is_none());
        assert!(AgentType::from_command(Some("   ")).is_none());
    }

    // PRD #201 (rule-12 wire safety): `AgentType` gained a wire-serialized `Pi`
    // variant. Without a `#[serde(other)]` fallback, a NEWER daemon emitting
    // `agent_type = "pi"` would break an OLDER reader's WHOLE-response decode
    // (`list_agents` / the `KIND_EVENT` broadcast), stranding its agent list.
    // The `#[serde(other)]` on `AgentType::None` makes any unrecognized value —
    // a `pi` record at a pre-Pi reader, OR a future agent type at today's build
    // — deserialize to the neutral `None` ("No agent") placeholder instead of
    // erroring, so this class of break can never repeat for a future type.
    #[test]
    fn unknown_agent_type_deserializes_to_none_fallback() {
        // The enum directly: an entirely unknown future value.
        let ty: AgentType = serde_json::from_str("\"someunknownfuturetype\"").unwrap();
        assert_eq!(ty, AgentType::None);

        // The value carried in a full `AgentEvent` (the real wire shape a
        // subscriber decodes over `KIND_EVENT`): the unknown `agent_type` must
        // NOT fail the whole-event decode.
        let json = r#"{
            "session_id": "fwd-compat-1",
            "agent_type": "someunknownfuturetype",
            "event_type": "thinking",
            "timestamp": "2026-03-22T10:00:00Z"
        }"#;
        let event: AgentEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.agent_type, AgentType::None);
        assert_eq!(event.event_type, EventType::Thinking);

        // `#[serde(other)]` is deserialize-only: `None` still round-trips
        // through its own `"none"` name, so serialization is unaffected.
        assert_eq!(serde_json::to_string(&AgentType::None).unwrap(), "\"none\"");
        assert_eq!(
            serde_json::from_str::<AgentType>("\"none\"").unwrap(),
            AgentType::None
        );
        // And the recognized values still map to their own variants (regression
        // guard: the catch-all must not swallow known types).
        assert_eq!(
            serde_json::from_str::<AgentType>("\"pi\"").unwrap(),
            AgentType::Pi
        );
        assert_eq!(
            serde_json::from_str::<AgentType>("\"claude_code\"").unwrap(),
            AgentType::ClaudeCode
        );
    }

    #[test]
    fn parse_open_code_event() {
        let json = r#"{
            "session_id": "oc-456",
            "agent_type": "open_code",
            "event_type": "session_start",
            "timestamp": "2026-03-22T10:00:00Z"
        }"#;
        let event: AgentEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.agent_type, AgentType::OpenCode);
        assert_eq!(event.event_type, EventType::SessionStart);
    }

    #[test]
    fn serialize_deserialize_delegate_signal() {
        let signal = DelegateSignal {
            pane_id: "pane-1".into(),
            task: "Implement login".into(),
            to: vec!["coder".into()],
            timestamp: chrono::DateTime::parse_from_rfc3339("2026-04-17T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            subject: None,
        };
        let msg = DaemonMessage::Delegate(signal);
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: DaemonMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            DaemonMessage::Delegate(s) => {
                assert_eq!(s.pane_id, "pane-1");
                assert_eq!(s.task, "Implement login");
                assert_eq!(s.to, vec!["coder"]);
            }
            _ => panic!("expected Delegate"),
        }
    }

    #[test]
    fn serialize_deserialize_work_done_signal() {
        let signal = WorkDoneSignal {
            pane_id: "pane-2".into(),
            task: "Implemented login".into(),
            done: false,
            timestamp: chrono::DateTime::parse_from_rfc3339("2026-04-17T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            generation: 0,
            daemon_boot_id: "boot-deadbeef".into(),
            subject: None,
        };
        let msg = DaemonMessage::WorkDone(signal);
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: DaemonMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            DaemonMessage::WorkDone(s) => {
                assert_eq!(s.pane_id, "pane-2");
                assert_eq!(s.task, "Implemented login");
                assert!(!s.done);
                assert_eq!(s.daemon_boot_id, "boot-deadbeef");
            }
            _ => panic!("expected WorkDone"),
        }
    }

    #[test]
    fn serialize_deserialize_get_seed_request() {
        // PRD #201: the get-seed request carries the pane id and tags itself
        // `message_type: "get_seed"` so the daemon's hook loop can distinguish
        // it from the fire-and-forget delegate / work-done signals.
        let msg = DaemonMessage::GetSeed(GetSeedRequest {
            pane_id: "pane-7".into(),
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert!(
            json.contains("\"message_type\":\"get_seed\""),
            "get-seed must be tagged so an OLD daemon that doesn't know it fails \
             to parse and simply doesn't reply (graceful degradation): {json}"
        );
        let parsed: DaemonMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            DaemonMessage::GetSeed(r) => assert_eq!(r.pane_id, "pane-7"),
            _ => panic!("expected GetSeed"),
        }
    }

    #[test]
    fn serialize_deserialize_get_seed_response() {
        // Some(seed) round-trips…
        let resp = GetSeedResponse {
            seed: Some("Read .dot-agent-deck/worker-task-coder.md for your task.".into()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: GetSeedResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.seed.as_deref(),
            Some("Read .dot-agent-deck/worker-task-coder.md for your task.")
        );
        // …and "no seed" is a null the get-seed CLI reads as "print nothing".
        let none = GetSeedResponse { seed: None };
        let json = serde_json::to_string(&none).unwrap();
        assert_eq!(json, "{\"seed\":null}");
        let back: GetSeedResponse = serde_json::from_str(&json).unwrap();
        assert!(back.seed.is_none());
    }

    #[test]
    fn work_done_signal_defaults() {
        let json = r#"{
            "message_type": "work_done",
            "pane_id": "pane-2",
            "task": "Done",
            "timestamp": "2026-04-17T10:00:00Z"
        }"#;
        let msg: DaemonMessage = serde_json::from_str(json).unwrap();
        match msg {
            DaemonMessage::WorkDone(s) => {
                assert!(!s.done);
            }
            _ => panic!("expected WorkDone"),
        }
    }

    // PRD #120: the live-orchestration-surface broadcast must round-trip
    // through the same `BroadcastMsg` wire the daemon forwards over KIND_EVENT,
    // and tag itself `orchestration_surface` so it's distinguishable from the
    // `event` variant an older peer expects (the reason PROTOCOL_VERSION bumped).
    #[test]
    fn orchestration_surface_broadcast_round_trips() {
        let msg = BroadcastMsg::OrchestrationSurface(OrchestrationSurface {
            name: "issue-work".into(),
            cwd: "/work/github-issues/.worktrees/issue-1".into(),
            display_title: None,
            roles: vec![
                OrchestrationSurfaceRole {
                    pane_id: "sched-github-issues-0-r0".into(),
                    role_index: 0,
                    role_name: "orchestrator".into(),
                    is_start_role: true,
                },
                OrchestrationSurfaceRole {
                    pane_id: "sched-github-issues-0-r1".into(),
                    role_index: 1,
                    role_name: "worker".into(),
                    is_start_role: false,
                },
            ],
        });
        let json = serde_json::to_string(&msg).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["kind"], "orchestration_surface");
        // `display_title: None` is omitted from the wire (skip_serializing_if).
        assert!(
            v.as_object().unwrap().get("display_title").is_none(),
            "None display_title must be omitted from the wire payload"
        );

        let back: BroadcastMsg = serde_json::from_str(&json).unwrap();
        let BroadcastMsg::OrchestrationSurface(s) = back else {
            panic!("expected a BroadcastMsg::OrchestrationSurface");
        };
        assert_eq!(s.name, "issue-work");
        assert_eq!(s.roles.len(), 2);
        assert_eq!(s.roles[0].role_name, "orchestrator");
        assert!(s.roles[0].is_start_role);
        assert_eq!(s.roles[1].pane_id, "sched-github-issues-0-r1");
        assert_eq!(s.roles[1].role_index, 1);
    }

    /// Scenario: Build a `BroadcastMsg::WorktreeKept` carrying a `Dirty`
    /// reason and no error text, serialize it to JSON, check the wire tag
    /// and shape (including that the absent `error` is omitted, not sent as
    /// `null`), then deserialize it back and confirm every field survives.
    /// Repeats the check for a `RemovalFailed` reason carrying the `error`
    /// field, since that's the one case where it's populated.
    #[spec("worktree/reclaim/047")]
    #[test]
    fn reclaim_047_worktree_kept_broadcast_round_trips() {
        let msg = BroadcastMsg::WorktreeKept(WorktreeKeptNotice {
            path: "/work/github-issues/.worktrees/issue-7".into(),
            reason: KeptReason::Dirty,
            error: None,
        });
        let json = serde_json::to_string(&msg).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["kind"], "worktree_kept");
        assert_eq!(v["path"], "/work/github-issues/.worktrees/issue-7");
        assert_eq!(v["reason"], "dirty");
        assert!(
            v.as_object().unwrap().get("error").is_none(),
            "a None error must be omitted from the wire payload, not sent as null"
        );

        let back: BroadcastMsg = serde_json::from_str(&json).unwrap();
        let BroadcastMsg::WorktreeKept(notice) = back else {
            panic!("expected a BroadcastMsg::WorktreeKept");
        };
        assert_eq!(notice.path, "/work/github-issues/.worktrees/issue-7");
        assert_eq!(notice.reason, KeptReason::Dirty);
        assert_eq!(notice.error, None);

        // `RemovalFailed` is the one reason that populates `error` -- round-trip it too.
        let msg = BroadcastMsg::WorktreeKept(WorktreeKeptNotice {
            path: "/work/github-issues/.worktrees/issue-9".into(),
            reason: KeptReason::RemovalFailed,
            error: Some("git worktree remove failed (exit 128)".into()),
        });
        let json = serde_json::to_string(&msg).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["reason"], "removal_failed");
        assert_eq!(v["error"], "git worktree remove failed (exit 128)");

        let back: BroadcastMsg = serde_json::from_str(&json).unwrap();
        let BroadcastMsg::WorktreeKept(notice) = back else {
            panic!("expected a BroadcastMsg::WorktreeKept");
        };
        assert_eq!(notice.reason, KeptReason::RemovalFailed);
        assert_eq!(
            notice.error,
            Some("git worktree remove failed (exit 128)".to_string())
        );
    }

    // PRD #201 M1.2 (test-plan row 3): pin the lifecycle-state → EventType
    // mapping the `dot-agent-deck agent-event --type <state>` subcommand and
    // the fast-tier status tests both consume. The three canonical states must
    // map to the exact EventTypes that drive Thinking / WaitingForInput / Idle
    // card statuses, and any other string must return None so the CLI rejects
    // an unknown `--type` non-zero rather than emitting a wrong status.
    #[test]
    fn agent_event_type_from_state_maps_canonical_lifecycle_states() {
        assert_eq!(
            agent_event_type_from_state("running"),
            Some(EventType::Thinking)
        );
        assert_eq!(
            agent_event_type_from_state("waiting"),
            Some(EventType::WaitingForInput)
        );
        assert_eq!(
            agent_event_type_from_state("finished"),
            Some(EventType::Idle)
        );
        // Unknown / malformed states map to None (the CLI turns this into a
        // clear non-zero error). Includes casing and near-miss variants.
        assert_eq!(agent_event_type_from_state("idle"), None);
        assert_eq!(agent_event_type_from_state("Running"), None);
        assert_eq!(agent_event_type_from_state("done"), None);
        assert_eq!(agent_event_type_from_state(""), None);
    }

    #[test]
    fn agent_event_not_parseable_as_daemon_message() {
        let json = r#"{
            "session_id": "abc-123",
            "agent_type": "claude_code",
            "event_type": "idle",
            "timestamp": "2026-03-22T10:00:00Z"
        }"#;
        assert!(serde_json::from_str::<DaemonMessage>(json).is_err());
    }

    /// Builds a Codex `SessionStart` `AgentEvent` with `CODEX_HOOK_TRUST_METADATA_KEY`
    /// stamped to `value` in its metadata (or omitted entirely when `value` is
    /// `None`), for exercising `codex_hook_trust_outcome()`'s parse in isolation.
    fn codex_session_start_with_trust_metadata(value: Option<&str>) -> AgentEvent {
        let mut metadata = HashMap::new();
        if let Some(value) = value {
            metadata.insert(CODEX_HOOK_TRUST_METADATA_KEY.to_string(), value.to_string());
        }
        AgentEvent {
            session_id: "codex-hook-outcome".into(),
            agent_type: AgentType::Codex,
            event_type: EventType::SessionStart,
            tool_name: None,
            tool_detail: None,
            cwd: None,
            timestamp: Utc::now(),
            user_prompt: None,
            metadata,
            pane_id: Some("codex-hook-outcome-pane".into()),
            agent_id: None,
            agent_version: None,
            schema_version: None,
            live_target: None,
            model: None,
        }
    }

    /// Scenario: PRD #254's `codex_hook_trust_outcome()` parses the wrapper's
    /// two real stamped values -- `"true"` reports a known-successful hook
    /// install/trust, `"false"` reports a known failure. Neither passes
    /// through any resolution logic beyond the literal string match, so this
    /// pins the parse in isolation from the rest of the plumbing chain
    /// (`codex_spawn_prep`, `apply_event`) that consumes it.
    #[test]
    fn codex_hook_trust_outcome_parses_true_and_false() {
        assert_eq!(
            codex_session_start_with_trust_metadata(Some("true")).codex_hook_trust_outcome(),
            Some(true)
        );
        assert_eq!(
            codex_session_start_with_trust_metadata(Some("false")).codex_hook_trust_outcome(),
            Some(false)
        );
    }

    /// Scenario: an event carrying no `CODEX_HOOK_TRUST_METADATA_KEY` at all
    /// -- an old wrapper build, a non-Codex-identity spawn, or any
    /// non-wrapper producer -- must resolve `None` ("no outcome reported"),
    /// never read as either a success or a failure. This is the deliberate
    /// backward-compatibility default the function's own doc comment
    /// describes, and it stays `None` even though I2 (below) changed how a
    /// PRESENT-but-garbled value resolves -- an absent key and a garbled
    /// value are different facts and must not collapse to the same outcome.
    #[test]
    fn codex_hook_trust_outcome_absent_key_is_none() {
        assert_eq!(
            codex_session_start_with_trust_metadata(None).codex_hook_trust_outcome(),
            None
        );
    }

    /// Scenario: PRD #254 auditor I2. A stamped-but-UNRECOGNISED value --
    /// neither the literal `"true"` nor `"false"` any current wrapper writes
    /// (a future format, a typo, a corrupted write) -- must NOT fall into the
    /// same arm as an ABSENT key, which would read as "no outcome reported"
    /// and resolve the pane back to `Reports` -- the exact
    /// silently-healthy-looking failure shape H1 is about (see
    /// `codex_spawn_prep_ok_zero_hooks_is_not_confirmed` in `wrap.rs`), just
    /// reached through a garbled value instead of a missing one. A key that
    /// IS present but unparseable is evidence something went wrong and must
    /// not be indistinguishable from "nothing was reported": it resolves to
    /// `Some(false)` (not confirmed), the same fail-safe direction as every
    /// other unresolvable outcome in this chain. This pins that fix: it was
    /// proven RED against the pre-fix code, where the `Some(_)` and `None`
    /// arms both collapsed into one wildcard.
    #[test]
    fn codex_hook_trust_outcome_unrecognized_present_value_is_not_confirmed() {
        assert_eq!(
            codex_session_start_with_trust_metadata(Some("maybe")).codex_hook_trust_outcome(),
            Some(false),
            "PRD #254 I2: a stamped-but-unrecognised trust value must resolve \
             as a known failure (Some(false)), not fall through to None as if \
             the key were absent -- that silently restores the pre-fix \
             Reports behaviour"
        );
    }
}
