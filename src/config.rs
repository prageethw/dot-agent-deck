use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::state::SessionStatus;

pub const CONFIG_KEYS: &[(&str, &str)] = &[
    ("default_command", "Default shell command for new panes"),
    (
        "auto_config_prompt",
        "Enable/disable the config generation prompt (default: true)",
    ),
    (
        "bell.enabled",
        "Enable/disable terminal bell (default: true)",
    ),
    (
        "bell.on_waiting_for_input",
        "Bell when agent waits for input (default: true)",
    ),
    (
        "bell.on_idle",
        "Bell when session goes idle (default: false)",
    ),
    ("bell.on_error", "Bell on agent error (default: true)"),
];

pub fn config_keys_help() -> String {
    let mut help = String::from("Available keys:\n");
    for (key, desc) in CONFIG_KEYS {
        help.push_str(&format!("  {key:<30} {desc}\n"));
    }
    help
}

/// Hook-ingestion endpoint path. Delegates to [`crate::platform::paths`] (PRD
/// #42 M1): Unix resolves the `$XDG_RUNTIME_DIR`/per-uid-`/tmp` socket path,
/// Windows resolves the named-pipe name. `DOT_AGENT_DECK_SOCKET` overrides.
pub fn socket_path() -> PathBuf {
    crate::platform::paths::socket_path()
}

/// Path of the M1.2 streaming-attach endpoint. Separate from the existing
/// hook-ingestion socket (PRD #76 line 219) so the two protocols have
/// disjoint, clearly-typed wire formats: hook ingestion is line-delimited
/// JSON, attach is a binary frame protocol (see `daemon_protocol`). Delegates
/// to [`crate::platform::paths`]; `DOT_AGENT_DECK_ATTACH_SOCKET` overrides.
pub fn attach_socket_path() -> PathBuf {
    crate::platform::paths::attach_socket_path()
}

/// Per-user state directory. Used by lazy-spawn (PRD #76 M4.3) for the
/// detached daemon log and the spawn mutex (`spawn.lock`). Delegates to
/// [`crate::platform::paths`] (PRD #42 M1); `DOT_AGENT_DECK_STATE_DIR`
/// overrides, then `$XDG_STATE_HOME`/`$HOME` on Unix or `%LOCALAPPDATA%` on
/// Windows.
pub fn state_dir() -> PathBuf {
    crate::platform::paths::state_dir()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BellConfig {
    pub enabled: bool,
    pub on_waiting_for_input: bool,
    pub on_idle: bool,
    pub on_error: bool,
}

impl Default for BellConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            on_waiting_for_input: true,
            on_idle: false,
            on_error: true,
        }
    }
}

impl BellConfig {
    pub fn should_bell(&self, status: &SessionStatus) -> bool {
        if !self.enabled {
            return false;
        }
        match status {
            SessionStatus::WaitingForInput => self.on_waiting_for_input,
            SessionStatus::Idle => self.on_idle,
            SessionStatus::Error => self.on_error,
            _ => false,
        }
    }
}

/// Issue #519: the removed `[idle_art]` section is deliberately NOT declared
/// here as an accepted-and-ignored field. `DashboardConfig` sets no
/// `#[serde(deny_unknown_fields)]`, so serde drops unknown tables silently and
/// a `config.toml` still carrying `[idle_art]` keeps loading unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DashboardConfig {
    pub default_command: String,
    pub bell: BellConfig,
    pub auto_config_prompt: bool,
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            default_command: String::new(),
            bell: BellConfig::default(),
            auto_config_prompt: true,
        }
    }
}

impl DashboardConfig {
    pub fn load() -> Self {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(contents) => match toml::from_str(&contents) {
                Ok(config) => config,
                Err(err) => {
                    eprintln!("Invalid config at {}: {err}", path.display());
                    Self::default()
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => {
                eprintln!("Failed to read config at {}: {err}", path.display());
                Self::default()
            }
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create config directory: {e}"))?;
        }
        let contents =
            toml::to_string_pretty(self).map_err(|e| format!("Failed to serialize config: {e}"))?;
        std::fs::write(&path, contents)
            .map_err(|e| format!("Failed to write config at {}: {e}", path.display()))
    }

    pub fn get_field(&self, key: &str) -> Result<String, String> {
        match key {
            "default_command" => Ok(self.default_command.clone()),
            "bell.enabled" => Ok(self.bell.enabled.to_string()),
            "bell.on_waiting_for_input" => Ok(self.bell.on_waiting_for_input.to_string()),
            "bell.on_idle" => Ok(self.bell.on_idle.to_string()),
            "bell.on_error" => Ok(self.bell.on_error.to_string()),
            "auto_config_prompt" => Ok(self.auto_config_prompt.to_string()),
            _ => Err(format!("Unknown config key: {key}\n{}", config_keys_help())),
        }
    }

    pub fn set_field(&mut self, key: &str, value: &str) -> Result<(), String> {
        let parse_bool = |v: &str| -> Result<bool, String> {
            v.parse().map_err(|_| format!("Invalid boolean: {v}"))
        };
        match key {
            "default_command" => {
                self.default_command = value.to_string();
                Ok(())
            }
            "bell.enabled" => {
                self.bell.enabled = parse_bool(value)?;
                Ok(())
            }
            "bell.on_waiting_for_input" => {
                self.bell.on_waiting_for_input = parse_bool(value)?;
                Ok(())
            }
            "bell.on_idle" => {
                self.bell.on_idle = parse_bool(value)?;
                Ok(())
            }
            "bell.on_error" => {
                self.bell.on_error = parse_bool(value)?;
                Ok(())
            }
            "auto_config_prompt" => {
                self.auto_config_prompt = value
                    .parse()
                    .map_err(|_| "Expected 'true' or 'false'".to_string())?;
                Ok(())
            }
            _ => Err(format!("Unknown config key: {key}\n{}", config_keys_help())),
        }
    }
}

fn config_path() -> PathBuf {
    if let Ok(dir) = std::env::var("DOT_AGENT_DECK_CONFIG") {
        return PathBuf::from(dir);
    }
    config_dir().join("config.toml")
}

fn session_path() -> PathBuf {
    if let Ok(dir) = std::env::var("DOT_AGENT_DECK_SESSION") {
        return PathBuf::from(dir);
    }
    config_dir().join("session.toml")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedPane {
    pub dir: String,
    pub name: String,
    pub command: String,
    /// When set, this pane was the agent pane of a mode tab.
    /// The value is the mode name from the project config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// When set, this pane was the orchestrator pane of an orchestration
    /// tab; the snapshot carries enough metadata to rebuild the whole tab
    /// (orchestrator + role panes, prompt, role order, start cursor) on the
    /// daemon-empty restore path. `Option` + `#[serde(default)]` so older
    /// `session.toml` files (no `orchestration` key) still parse with
    /// `orchestration == None`. See [`OrchestrationSnapshot`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestration: Option<OrchestrationSnapshot>,
}

/// PRD #89 M2b.2 — orchestration metadata captured on a saved pane so the
/// daemon-empty restore path (fresh machine / crash recovery, when there is
/// no warm daemon to hydrate from) can rebuild the orchestration tab. Schema
/// ported from the closed PRD #74 design.
///
/// Carried as `SavedPane::orchestration: Option<OrchestrationSnapshot>` with
/// `#[serde(default)]`, so a `session.toml` written before this field existed
/// (no `[panes.orchestration]` table) still parses, yielding
/// `orchestration == None`. A `version` field is present from day one so a
/// future schema change can be migrated rather than silently dropped. No
/// `#[serde(deny_unknown_fields)]` — forward-compat with snapshots a newer
/// binary may write with extra keys.
///
/// This struct ONLY captures the metadata + its (de)serialization; the
/// restore branch that rebuilds the tab from it is M2b.3 (a separate step).
///
/// PRD #89 review-fix F12: every sub-field carries `#[serde(default)]` so a
/// MALFORMED partial `[panes.orchestration]` block degrades to a defaulted
/// snapshot (which the restore path then drift-checks and falls back from)
/// rather than failing the WHOLE-file TOML parse and dropping ALL panes. A
/// zero-default `version`/empty `roles` is harmless — the restore path treats a
/// role-less / out-of-range snapshot as drift (F2) and restores a plain pane.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrchestrationSnapshot {
    /// Schema version, for future migration. `1` is the initial format.
    #[serde(default)]
    pub version: u32,
    /// Role names in DISPLAY order — the same order as the tab's
    /// `role_pane_ids`, so the restore branch can recreate the role panes
    /// in the order the user saw them.
    #[serde(default)]
    pub roles: Vec<String>,
    /// Index into `roles` of the start (orchestrator) role — restores the
    /// "next role to start" cursor.
    #[serde(default)]
    pub start_role_index: usize,
    /// Pre-built prompt injected into the start (orchestrator) role on
    /// restore. Empty string when the orchestration had no prompt.
    #[serde(default)]
    pub orchestrator_prompt: String,
    /// Resolved orchestration config NAME — half of the reference used to
    /// re-resolve the `OrchestrationConfig` from disk on restore.
    #[serde(default)]
    pub config_name: String,
    /// Project PATH the orchestration was resolved from (the directory that
    /// holds `.dot-agent-deck.toml`) — the other half of the re-resolution
    /// reference.
    #[serde(default)]
    pub project_path: String,
    /// Which roles had been started, by index into `roles`. Optional —
    /// snapshots that predate this field load with an empty list.
    ///
    /// PRD #89 review-fix F3: FORWARD-COMPAT ONLY — captured (as
    /// `vec![start_role_index]`) but not yet consumed on restore. Kept so a
    /// later "restore which roles were started" feature has the data already
    /// in old snapshots; do not assume a reader exists today.
    #[serde(default)]
    pub started_role_indices: Vec<usize>,
    /// PRD #89 review-fix F4: the user-typed orchestration tab TITLE
    /// (`Tab::Orchestration.name`), captured so the daemon-empty restore path
    /// rebuilds the tab under the user's title rather than the canonical
    /// config/cwd name. `None` when the user didn't name the orchestration
    /// (the title then falls back to the resolved canonical name on restore).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_title: Option<String>,
    /// Fork #166 M3.0 / PR #215 fixup: the exact creator string this
    /// orchestration stamped into its worktree marker and every role pane's
    /// `DOT_AGENT_DECK_WORKTREE_OWNER` when it created a worktree
    /// (`orchestration_creator_string`'s output) — captured here so restore
    /// can carry the identity forward instead of fabricating or losing it.
    /// `None` when this orchestration owned no worktree (started directly
    /// in `main`), matching `AgentSpawnOptions::owner`'s own `None` case.
    /// `#[serde(default)]` like every other field here: a snapshot written
    /// before this field existed loads with `None`, so a tab reopened after
    /// upgrading mid-session restores with no identity and `--mine` refuses
    /// loudly rather than guessing — see `docs/orchestration.md`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SavedSession {
    #[serde(default)]
    pub panes: Vec<SavedPane>,
    /// PRD #196: the global "last command" — the most recent command the user
    /// spawned an INTERACTIVE agent with from the new-pane flow. Read back as the
    /// new-pane Command-field seed when `default_command` is empty (the fallback
    /// chain default → last → blank). `Option` + `#[serde(default)]` so an old
    /// `session.toml` written before this field existed loads as `None` (PRD:
    /// treat missing/unreadable as empty, never a hard failure). Authoring-mode
    /// fallback spawns (schedule / issue-dispatch) are excluded from recording.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_command: Option<String>,
}

impl SavedSession {
    pub fn load() -> Self {
        let path = session_path();
        match std::fs::read_to_string(&path) {
            Ok(contents) => match toml::from_str(&contents) {
                Ok(session) => session,
                Err(err) => {
                    eprintln!("Invalid session at {}: {err}", path.display());
                    Self::default()
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => {
                eprintln!("Failed to read session at {}: {err}", path.display());
                Self::default()
            }
        }
    }

    pub fn save(&self) -> Result<(), String> {
        use std::io::Write;

        let path = session_path();
        let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
        // PRD #163 auditor: through the fsperm seam rather than plain
        // `create_dir_all`, so the directory holding this snapshot is owner-only
        // too — parity with `schedules.toml`/`remotes.toml`. The file's own
        // mode/DACL protects the command lines and prompts inside; this protects
        // the directory metadata when the config dir is redirected somewhere
        // shared. Create-only: an existing directory keeps its mode (PRD #127 S2).
        crate::platform::fsperm::create_owner_only_dir(parent)
            .map_err(|e| format!("Failed to create session directory: {e}"))?;

        let contents = toml::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize session: {e}"))?;

        // PRD #89 review-fix F6/F7: write owner-only (0o600) AND atomically. The
        // snapshot now carries command lines, project paths, and prompts and is
        // written continuously, so (a) it must not be world-readable regardless
        // of the user's umask, and (b) a crash mid-write must never leave a
        // half-written `session.toml` for the next launch to choke on. Mirror
        // remote.rs: write a sibling temp file opened with mode 0o600, then
        // `rename(2)` it into place (atomic on a POSIX same-filesystem rename).
        // The pid suffix avoids collisions between concurrently-saving decks.
        let file_name = path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| "session.toml".to_string());
        let tmp_path = parent.join(format!("{file_name}.{}.tmp", std::process::id()));

        // PRD #42 M1: owner-only (0o600) creation mode comes from the platform
        // seam — `.mode(0o600)` on Unix; on Windows (#163) the DACL cannot be
        // supplied at create time, so the seam instead puts `WRITE_DAC` on the
        // handle, which is what lets the `set_file_owner_only` call below apply it.
        let mut open_opts = std::fs::OpenOptions::new();
        open_opts.create(true).write(true).truncate(true);
        crate::platform::fsperm::set_create_mode_owner_only(&mut open_opts);
        let mut tmp_file = open_opts
            .open(&tmp_path)
            .map_err(|e| format!("Failed to open temp session at {}: {e}", tmp_path.display()))?;

        // Owner-only permissions BEFORE the first content byte (PRD #163 M4).
        // Defense in depth on Unix (a stale temp file from a crashed previous save
        // would keep its old mode, since the create-mode only applies on create),
        // and on Windows where the protected current-user-only DACL is applied at
        // all — `std::fs::OpenOptions` has no `SECURITY_ATTRIBUTES` hook, so the
        // create-mode seam can only pre-authorize this call (`WRITE_DAC`), not
        // stand in for it. Running it before the write means the
        // snapshot's command lines, project paths and prompts are never exposed
        // under a loose inherited ACL; the end state on Unix is identical.
        if let Err(e) = crate::platform::fsperm::set_file_owner_only(&tmp_file) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(format!(
                "Failed to set permissions on temp session at {}: {e}",
                tmp_path.display()
            ));
        }
        if let Err(e) = tmp_file.write_all(contents.as_bytes()) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(format!(
                "Failed to write temp session at {}: {e}",
                tmp_path.display()
            ));
        }
        drop(tmp_file);

        std::fs::rename(&tmp_path, &path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp_path);
            format!("Failed to write session at {}: {e}", path.display())
        })
    }

    pub fn clear() -> Result<(), std::io::Error> {
        let path = session_path();
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Build a `SavedSession` snapshot from the live UI state.
    ///
    /// Must be called *before* tearing down mode/orchestration tabs — i.e., while
    /// `live_panes` (the authoritative `state.managed_pane_ids`) still contains
    /// every pane, including mode-tab agent panes that carry `mode = Some(...)`.
    /// `retain` here only prunes panes the user externally closed before exit;
    /// running it after teardown would also drop the mode-tab agent pane and lose
    /// the mode field, breaking auto-restore of the mode tab (PRD #69).
    pub fn snapshot(
        pane_metadata: &mut HashMap<String, SavedPane>,
        pane_display_names: &HashMap<String, String>,
        live_panes: &HashSet<String>,
    ) -> Self {
        pane_metadata.retain(|id, _| live_panes.contains(id));
        for (id, meta) in pane_metadata.iter_mut() {
            if let Some(name) = pane_display_names.get(id) {
                meta.name = name.clone();
            }
        }
        let mut ids: Vec<&String> = pane_metadata.keys().collect();
        ids.sort_by_key(|id| id.parse::<u64>().unwrap_or(0));
        Self {
            panes: ids
                .into_iter()
                .filter_map(|id| pane_metadata.get(id).cloned())
                .collect(),
            // PRD #196: the snapshot is built from live panes only; the caller
            // overlays the runtime `last_command` before persisting (it is global
            // state, not derived from the pane list).
            last_command: None,
        }
    }
}

/// PRD #89 M1.2 — leading-edge throttle that coalesces saved-session snapshot
/// writes so a burst of meaningful state changes (e.g. orchestration setup
/// spawning many panes) produces one or two disk writes, not one per change.
///
/// Behaviour: the first pending change writes immediately (leading edge), then
/// writes are throttled to at most one per `interval`; a single trailing write
/// flushes whatever accumulated while the throttle was closed. So a tight burst
/// collapses to ≤2 writes (one leading + one trailing), and sustained activity
/// is bounded to ~one write per `interval` regardless of how many changes occur.
///
/// Pure data + logic: the caller supplies the clock as a monotonic [`Duration`]
/// from an arbitrary epoch (in production, `epoch.elapsed()`; in tests, any
/// value), so it is fully unit-testable without wall-clock sleeps.
#[derive(Debug, Clone)]
pub struct SnapshotCoalescer {
    /// Minimum spacing between disk writes.
    interval: Duration,
    /// A change is pending (recorded but not yet flushed to disk).
    dirty: bool,
    /// Clock value of the last write; `None` until the first write happens.
    last_write: Option<Duration>,
}

impl SnapshotCoalescer {
    /// Create a coalescer that allows at most one write per `interval`.
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            dirty: false,
            last_write: None,
        }
    }

    /// Record that a meaningful state change occurred. Does not write — the
    /// actual coalesced write happens when the caller next sees [`Self::is_due`]
    /// return `true`.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Whether a write is due at `now`: a change is pending AND either nothing
    /// has been written yet (leading edge) or at least `interval` has elapsed
    /// since the last write (trailing edge / throttle release).
    pub fn is_due(&self, now: Duration) -> bool {
        if !self.dirty {
            return false;
        }
        match self.last_write {
            None => true,
            Some(last) => now.saturating_sub(last) >= self.interval,
        }
    }

    /// Mark a write as completed at `now`, clearing the pending flag and arming
    /// the throttle so the next write waits a full `interval`.
    pub fn record_write(&mut self, now: Duration) {
        self.dirty = false;
        self.last_write = Some(now);
    }
}

// ---------------------------------------------------------------------------
// Scheduled tasks — global, daemon-owned config (PRD #127, M1.2)
// ---------------------------------------------------------------------------

/// PRD #120 GitHub issue-dispatch configuration, carried by a
/// `[[scheduled_tasks]]` entry whose `[scheduled_tasks.issue_dispatch]` table is
/// present. The table's **presence is the task-type discriminator**: a
/// [`ScheduledTask`] with `issue_dispatch == Some(_)` enumerates open issues for
/// `repo` and dispatches one agent per issue on fire, instead of the single
/// spawn behaviour of PRD #127. The shared scheduler fields — `name`, `cron`,
/// `working_dir` (the workspace root the repo clones under), `prompt` (the
/// per-issue template, see [`crate::issue_dispatch`]), and `enabled` — still come
/// from the enclosing [`ScheduledTask`]; only the GitHub-specific knobs live
/// here. One repo per task is a locked PRD decision (several repos → several
/// schedules), which is why this is a single `repo`, not a list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IssueDispatchConfig {
    /// Target repo in `owner/name` form (e.g. `vfarcic/dot-ai`).
    pub repo: String,
    /// Maximum number of issues dispatched per fire (the per-run cap). Omitting
    /// it defaults to 3 — the value documented in the changelog — so a
    /// hand-written `[scheduled_tasks.issue_dispatch]` table that leaves it out
    /// still deserializes (and the task still fires) instead of being rejected.
    #[serde(default = "default_max_per_run")]
    pub max_per_run: usize,
    /// Optional label filter (e.g. `agent-eligible`). `None` = no label gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Optional raw `gh` search-query override for advanced users. `None` = the
    /// default "all open issues up to `max_per_run`" listing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// PRD #421 M2.0/M2.1: opt-in triage. When true, the dispatch flow ensures
    /// the triage label vocabulary exists and appends the triage instruction to
    /// each dispatched issue's prompt, so the spawned agent applies its own
    /// priority/size labels. Off by default — this is additive behaviour, not
    /// a required part of dispatch.
    #[serde(default)]
    pub triage: bool,
}

/// One `[[scheduled_tasks]]` entry from the global
/// `~/.config/dot-agent-deck/schedules.toml`. The daemon's job list. See PRD
/// #127 "Configuration: global, daemon-owned".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScheduledTask {
    /// Reuse-registry key; unique per daemon. Renaming is forbidden via the
    /// edit path (it orphans the reused tab) — treat as remove + add.
    pub name: String,
    /// Cron expression (5-field POSIX or 6/7-field). Validated by the
    /// scheduler / CLI before write. Evaluated in local time.
    pub cron: String,
    /// Spawn target directory. `~` and `$VAR` are expanded at load time
    /// (see [`expand_path`]); relative paths resolve against `$HOME`.
    pub working_dir: String,
    /// Single-agent command (mirrors the new-deck dialog); ignored when the
    /// target dir defines `[[orchestrations]]`. Required: a missing or blank
    /// value is rejected at load time (see [`validate_task`]) — there is no
    /// `$SHELL` fallback for scheduled tasks. Kept `Option` only so the file
    /// shape round-trips and the absence can be reported as a load error rather
    /// than a parse failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// The prompt delivered to the spawned agent / orchestrator role.
    pub prompt: String,
    /// Open a fresh tab on every fire instead of reusing one. Default false
    /// (reuse — the dominant access pattern; see PRD "Tab lifecycle").
    #[serde(default)]
    pub new_tab_per_fire: bool,
    /// Whether the daemon registers and fires this task. Default true.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// PRD #120: when present, this is an `issue_dispatch` task — on fire it
    /// enumerates open issues for `issue_dispatch.repo` and dispatches an agent
    /// per issue, reusing `prompt` as the per-issue template (the
    /// `{{issue_number}}` placeholder substituted at fire time) and
    /// `working_dir` as the workspace root. Absent (`None`) → the original
    /// single-spawn task. The table's presence is the task-type discriminator.
    ///
    /// MUST stay the LAST field: TOML serializes it as a
    /// `[scheduled_tasks.issue_dispatch]` sub-table, which has to follow all the
    /// scalar fields of the entry to round-trip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_dispatch: Option<IssueDispatchConfig>,
}

fn default_enabled() -> bool {
    true
}

/// Default `issue_dispatch.max_per_run` when the field is omitted: 3, matching
/// the changelog's documented "enumeration cap, default 3". `pub` so the CLI
/// `schedule add` path can reuse it for `--max-per-run`'s default and not drift
/// from this serde default.
pub fn default_max_per_run() -> usize {
    3
}

/// Internal mirror of the file shape so a well-formed file deserializes in one
/// shot; the robust loader below falls back to per-entry parsing when the
/// strict parse fails, so one bad entry can't block the rest.
#[derive(Debug, Default, Deserialize)]
struct SchedulesFile {
    #[serde(default)]
    scheduled_tasks: Vec<ScheduledTask>,
}

/// A per-entry (or file-level) load failure. `entry` is the array index when
/// the failure is attributable to a single `[[scheduled_tasks]]` block, `None`
/// for a file-level error. The caller surfaces these via the scheduler's
/// notification seam (PRD #126) — a malformed entry never crashes the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleLoadError {
    pub entry: Option<usize>,
    pub message: String,
}

/// Result of loading the global schedules config: the entries that parsed
/// (with paths expanded), plus any per-entry / file-level errors.
#[derive(Debug, Default, Clone)]
pub struct LoadedSchedules {
    pub tasks: Vec<ScheduledTask>,
    pub errors: Vec<ScheduleLoadError>,
}

/// Global schedules path: `$XDG_CONFIG_HOME/dot-agent-deck/schedules.toml`,
/// falling back to `~/.config/...` (on Windows: `%APPDATA%\dot-agent-deck`,
/// which has no XDG stage — see
/// [`crate::platform::paths::xdg_config_home`]). `DOT_AGENT_DECK_SCHEDULES`
/// overrides it so tests never touch the real home dir.
pub fn schedules_path() -> PathBuf {
    if let Ok(p) = std::env::var("DOT_AGENT_DECK_SCHEDULES") {
        return PathBuf::from(p);
    }
    match crate::platform::paths::xdg_config_home() {
        Some(dir) => dir.join("dot-agent-deck/schedules.toml"),
        None => config_dir().join("schedules.toml"),
    }
}

impl LoadedSchedules {
    /// Load from the global [`schedules_path`].
    pub fn load() -> Self {
        Self::load_from(&schedules_path())
    }

    /// Load from an explicit path (tests, and any future supervised-mode
    /// override). A missing file is not an error — it yields an empty set.
    pub fn load_from(path: &std::path::Path) -> Self {
        let contents = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Self::default();
            }
            Err(err) => {
                return Self {
                    tasks: Vec::new(),
                    errors: vec![ScheduleLoadError {
                        entry: None,
                        message: format!("failed to read {}: {err}", path.display()),
                    }],
                };
            }
        };
        Self::parse(&contents)
    }

    /// Parse schedules from a TOML string with robust per-entry handling: a
    /// single malformed `[[scheduled_tasks]]` entry is reported as an error and
    /// skipped without blocking the valid entries.
    pub fn parse(contents: &str) -> Self {
        // Fast path: the whole file is well-formed TOML. Each entry still has to
        // clear the semantic check below (a command-less entry is rejected), so
        // we validate per-entry even on this path rather than blindly collecting.
        if let Ok(file) = toml::from_str::<SchedulesFile>(contents) {
            let mut out = Self::default();
            for (i, task) in file.scheduled_tasks.into_iter().enumerate() {
                match validate_task(task, i) {
                    Ok(task) => out.tasks.push(task),
                    Err(err) => out.errors.push(err),
                }
            }
            return out;
        }

        // Slow path: parse to a generic table, then deserialize each
        // `[[scheduled_tasks]]` entry individually so one bad entry doesn't
        // take the others down with it.
        let table: toml::Table = match contents.parse() {
            Ok(t) => t,
            Err(err) => {
                return Self {
                    tasks: Vec::new(),
                    errors: vec![ScheduleLoadError {
                        entry: None,
                        message: format!("malformed TOML: {err}"),
                    }],
                };
            }
        };

        let mut out = Self::default();
        let Some(value) = table.get("scheduled_tasks") else {
            return out;
        };
        let Some(entries) = value.as_array() else {
            out.errors.push(ScheduleLoadError {
                entry: None,
                message: "`scheduled_tasks` must be an array of tables".to_string(),
            });
            return out;
        };

        for (i, entry) in entries.iter().enumerate() {
            match entry.clone().try_into::<ScheduledTask>() {
                Ok(task) => match validate_task(task, i) {
                    Ok(task) => out.tasks.push(task),
                    Err(err) => out.errors.push(err),
                },
                Err(err) => out.errors.push(ScheduleLoadError {
                    entry: Some(i),
                    message: err.to_string(),
                }),
            }
        }
        out
    }
}

/// Validate a freshly-parsed task and apply load-time path expansion. A
/// hand-edited entry with no (or blank) `command` is REJECTED here (PRD #127
/// follow-up, USER DECISION): a scheduled task needs an agent command to act on
/// its prompt, and there is no silent `$SHELL` fallback. Rejection mirrors the
/// malformed-entry path — the error is surfaced via the daemon's notification
/// seam (PRD #126) and the entry is skipped, without blocking valid siblings or
/// crashing the daemon.
fn validate_task(task: ScheduledTask, index: usize) -> Result<ScheduledTask, ScheduleLoadError> {
    // fork #222: an unbounded `task.name` lets two scheduled tasks' marker-creator
    // strings (`issue-dispatch:{name}#{issue}`) collide once
    // `sanitize_marker_creator` truncates at `MARKER_CREATOR_MAX_CHARS`
    // (src/worktree_reclaim.rs). Reject an overlong name outright at the
    // producer, borrowing the same numeric VALUE the daemon already enforces
    // on a live orchestration's display name (`DISPLAY_NAME_MAX_LEN`) — but
    // deliberately on a different UNIT. `DISPLAY_NAME_MAX_LEN` is a byte cap
    // everywhere else it's used; this counts chars because
    // `sanitize_marker_creator` truncates by chars, and chars is the unit
    // that actually determines whether two names' truncated creator strings
    // collide.
    if task.name.chars().count() > crate::agent_pty::DISPLAY_NAME_MAX_LEN {
        return Err(ScheduleLoadError {
            entry: Some(index),
            message: format!(
                "scheduled task name is {} characters, exceeding the {}-character limit",
                task.name.chars().count(),
                crate::agent_pty::DISPLAY_NAME_MAX_LEN
            ),
        });
    }
    // fork #222 edge 1 follow-up: the length check alone doesn't close the
    // collision — `sanitize_marker_creator` also drops control characters,
    // maps `\n`/`\r` to a space, and trims, all BEFORE truncating. Two
    // distinct, both-short names (e.g. `"deploy prod"` and `"deploy\nprod"`)
    // can still collapse to the identical marker-creator string. Reject any
    // name normalization would change at all, so the only remaining
    // collision surface is the length bound just above. Deliberately does
    // not echo the offending name back, matching the length check above.
    if crate::worktree_reclaim::marker_creator_normalizes(&task.name) {
        return Err(ScheduleLoadError {
            entry: Some(index),
            message: "scheduled task name contains control characters, a newline/carriage \
                       return, or leading/trailing whitespace that would be stripped before \
                       comparison, which can make it collide with another task's \
                       worktree-ownership identity"
                .to_string(),
        });
    }
    // PRD #120: an issue-dispatch task has no top-level `command` — the per-issue
    // spawn derives its command from each cloned repo's `.dot-agent-deck.toml`
    // (orchestration roles, or the single-agent default). Only the #127
    // single-spawn task type requires `command`.
    if let Some(disp) = &task.issue_dispatch {
        // M1: `repo`/`label`/`query` come from hand-edited TOML and flow into
        // `gh`/`git` argv (an argument-injection vector run unattended by the
        // daemon). Reject a malformed slug or a leading-`-` filter here rather
        // than at fire time.
        if let Err(message) = crate::issue_dispatch::validate_issue_dispatch_config(
            &disp.repo,
            disp.max_per_run,
            disp.label.as_deref(),
            disp.query.as_deref(),
        ) {
            return Err(ScheduleLoadError {
                entry: Some(index),
                message: format!("scheduled task {:?}: {message}", task.name),
            });
        }
        return Ok(expand_task(task));
    }
    match &task.command {
        Some(cmd) if !cmd.trim().is_empty() => Ok(expand_task(task)),
        _ => Err(ScheduleLoadError {
            entry: Some(index),
            message: format!(
                "scheduled task {:?} has no `command`; a command is required \
                 (a scheduled task needs an agent command to act on its prompt — \
                 there is no $SHELL fallback)",
                task.name
            ),
        }),
    }
}

/// Apply load-time path expansion to a task's `working_dir`.
fn expand_task(mut task: ScheduledTask) -> ScheduledTask {
    task.working_dir = expand_path(&task.working_dir);
    task
}

/// Expand `~` and `$VAR` / `${VAR}` in a path, then resolve a relative result
/// against `$HOME` (NOT any agent cwd — the authoring agent's cwd is
/// irrelevant for a global daemon). PRD #127 Open Q7.
pub fn expand_path(input: &str) -> String {
    let home = dirs_home();

    // `~` / `~/...` → home.
    let after_tilde = if input == "~" {
        return home.to_string_lossy().into_owned();
    } else if let Some(rest) = input.strip_prefix("~/") {
        format!("{}/{}", home.to_string_lossy(), rest)
    } else {
        input.to_string()
    };

    let expanded = expand_env_vars(&after_tilde);

    // Resolve a still-relative path against $HOME.
    if expanded.starts_with('/') {
        expanded
    } else {
        home.join(&expanded).to_string_lossy().into_owned()
    }
}

/// Substitute `$VAR` and `${VAR}` with their environment values. An undefined
/// variable expands to the empty string (matching common shell-ish behavior
/// without failing the whole load). A `$` that does not begin a valid variable
/// reference is left untouched.
fn expand_env_vars(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            // ${VAR}
            Some('{') => {
                chars.next(); // consume '{'
                let mut name = String::new();
                let mut closed = false;
                for nc in chars.by_ref() {
                    if nc == '}' {
                        closed = true;
                        break;
                    }
                    name.push(nc);
                }
                if closed && !name.is_empty() {
                    out.push_str(&std::env::var(&name).unwrap_or_default());
                } else {
                    // Not a well-formed reference — emit verbatim.
                    out.push('$');
                    out.push('{');
                    out.push_str(&name);
                }
            }
            // $VAR — name is [A-Za-z_][A-Za-z0-9_]*
            Some(&first) if first == '_' || first.is_ascii_alphabetic() => {
                let mut name = String::new();
                while let Some(&nc) = chars.peek() {
                    if nc == '_' || nc.is_ascii_alphanumeric() {
                        name.push(nc);
                        chars.next();
                    } else {
                        break;
                    }
                }
                out.push_str(&std::env::var(&name).unwrap_or_default());
            }
            // Lone `$` — leave it.
            _ => out.push('$'),
        }
    }
    out
}

const STAR_PROMPT_INTERVAL: u64 = 10;

fn star_prompt_path() -> PathBuf {
    if let Ok(p) = std::env::var("DOT_AGENT_DECK_STAR_PROMPT") {
        return PathBuf::from(p);
    }
    config_dir().join("star-prompt-state.json")
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StarPromptState {
    pub launch_count: u64,
    pub permanently_dismissed: bool,
    pub last_prompt_at_launch: u64,
}

impl StarPromptState {
    pub fn load() -> Self {
        let path = star_prompt_path();
        match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str(&contents) {
                Ok(state) => state,
                Err(err) => {
                    eprintln!("Invalid star prompt state at {}: {err}", path.display());
                    Self::default()
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => {
                eprintln!(
                    "Failed to read star prompt state at {}: {err}",
                    path.display()
                );
                Self::default()
            }
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = star_prompt_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create star prompt directory: {e}"))?;
        }
        let contents = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize star prompt state: {e}"))?;
        std::fs::write(&path, contents).map_err(|e| {
            format!(
                "Failed to write star prompt state at {}: {e}",
                path.display()
            )
        })
    }

    pub fn increment_and_check(&mut self) -> bool {
        self.launch_count += 1;
        let _ = self.save();
        !self.permanently_dismissed
            && self.launch_count - self.last_prompt_at_launch >= STAR_PROMPT_INTERVAL
    }

    pub fn snooze(&mut self) {
        self.last_prompt_at_launch = self.launch_count;
        let _ = self.save();
    }

    pub fn dismiss_permanently(&mut self) {
        self.permanently_dismissed = true;
        let _ = self.save();
    }
}

// ---------------------------------------------------------------------------
// Config generation state — tracks directories where the user chose "Never"
// for the auto-config-prompt modal.
// ---------------------------------------------------------------------------

fn config_gen_state_path() -> PathBuf {
    if let Ok(p) = std::env::var("DOT_AGENT_DECK_CONFIG_GEN_STATE") {
        return PathBuf::from(p);
    }
    config_dir().join("config-gen-state.json")
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfigGenState {
    pub suppressed_dirs: Vec<String>,
}

impl ConfigGenState {
    pub fn load() -> Self {
        let path = config_gen_state_path();
        match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str(&contents) {
                Ok(state) => state,
                Err(err) => {
                    eprintln!("Invalid config gen state at {}: {err}", path.display());
                    Self::default()
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => {
                eprintln!(
                    "Failed to read config gen state at {}: {err}",
                    path.display()
                );
                Self::default()
            }
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = config_gen_state_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create config gen state directory: {e}"))?;
        }
        let contents = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize config gen state: {e}"))?;
        std::fs::write(&path, contents).map_err(|e| {
            format!(
                "Failed to write config gen state at {}: {e}",
                path.display()
            )
        })
    }

    pub fn is_suppressed(&self, dir: &str) -> bool {
        self.suppressed_dirs.iter().any(|d| d == dir)
    }

    pub fn suppress_dir(&mut self, dir: &str) {
        if !self.is_suppressed(dir) {
            self.suppressed_dirs.push(dir.to_string());
            let _ = self.save();
        }
    }
}

/// Serializes tests that mutate `DOT_AGENT_DECK_STATE_DIR` /
/// `XDG_STATE_HOME` / `HOME`. Rust runs unit tests in parallel and these are
/// process-global, so any test that wants to observe a specific value of
/// `state_dir()` must hold this lock for the duration of its env-var fiddling.
#[cfg(test)]
pub static STATE_DIR_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Serializes tests that mutate `DOT_AGENT_DECK_CONFIG_GEN_STATE` or call
/// `ConfigGenState::save()` / `load()` (directly or through handlers like
/// `handle_config_gen_prompt_key`). Rust runs unit tests in parallel, so
/// without this lock those tests race on the shared env var and on whatever
/// state file each one points it at.
#[cfg(test)]
pub(crate) static CONFIG_GEN_STATE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Test-only RAII guard that sets `DOT_AGENT_DECK_CONFIG_GEN_STATE` and
/// restores its prior value on drop, even if the test panics. Callers must
/// hold `CONFIG_GEN_STATE_ENV_LOCK` for the guard's lifetime.
#[cfg(test)]
pub(crate) struct ConfigGenStateEnvGuard {
    prev: Option<String>,
}

#[cfg(test)]
impl ConfigGenStateEnvGuard {
    pub(crate) fn set(value: &str) -> Self {
        let prev = std::env::var("DOT_AGENT_DECK_CONFIG_GEN_STATE").ok();
        // SAFETY: callers must hold CONFIG_GEN_STATE_ENV_LOCK for the
        // duration of this guard, which serializes env-var access.
        unsafe {
            std::env::set_var("DOT_AGENT_DECK_CONFIG_GEN_STATE", value);
        }
        Self { prev }
    }
}

#[cfg(test)]
impl Drop for ConfigGenStateEnvGuard {
    fn drop(&mut self) {
        // SAFETY: see ConfigGenStateEnvGuard::set.
        unsafe {
            match self.prev.take() {
                Some(v) => std::env::set_var("DOT_AGENT_DECK_CONFIG_GEN_STATE", v),
                None => std::env::remove_var("DOT_AGENT_DECK_CONFIG_GEN_STATE"),
            }
        }
    }
}

/// Serializes cwd/env mutation only among the tests IN THIS MODULE that take
/// this specific lock — process cwd and/or `DOT_AGENT_DECK_FEATURES_CONFIG`
/// are both process-global (fork issue #303). It does NOT serialize against
/// other tests elsewhere in the crate that separately mutate the process
/// cwd (e.g. `schedule_cli.rs`'s own function-local cwd mutex) — two
/// independent mutexes do not exclude each other. That gap is harmless
/// today only because `cargo nextest` runs each test in its own process, so
/// no two tests' cwd mutations can interleave regardless of which lock (if
/// any) they hold; it would not be safe under a same-process test runner.
/// Any test that wants to observe a specific resolved value from
/// `features_config_path_for_display()` must hold this lock for the duration
/// of its cwd/env fiddling.
#[cfg(test)]
static FEATURES_CONFIG_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Test-only RAII guard: snapshots the process cwd and
/// `DOT_AGENT_DECK_FEATURES_CONFIG`, clears the env var so a value leaking in
/// from the outer test-runner environment can't mask the defect under test,
/// and restores BOTH on drop — even if the test panics. Callers must hold
/// `FEATURES_CONFIG_TEST_LOCK` for the guard's lifetime.
#[cfg(test)]
struct FeaturesConfigCwdEnvGuard {
    prev_cwd: PathBuf,
    prev_override: Option<String>,
}

#[cfg(test)]
impl FeaturesConfigCwdEnvGuard {
    fn new() -> Self {
        let prev_cwd = std::env::current_dir().expect("read process cwd");
        let prev_override = std::env::var("DOT_AGENT_DECK_FEATURES_CONFIG").ok();
        // SAFETY: callers hold FEATURES_CONFIG_TEST_LOCK for the duration of
        // this guard, which serializes env-var access.
        unsafe {
            std::env::remove_var("DOT_AGENT_DECK_FEATURES_CONFIG");
        }
        Self {
            prev_cwd,
            prev_override,
        }
    }
}

#[cfg(test)]
impl Drop for FeaturesConfigCwdEnvGuard {
    fn drop(&mut self) {
        // Best-effort: if this fails the process cwd is left wherever the
        // test last set it, but there is nothing more corrective to do here.
        let _ = std::env::set_current_dir(&self.prev_cwd);
        // SAFETY: see FeaturesConfigCwdEnvGuard::new.
        unsafe {
            match self.prev_override.take() {
                Some(v) => std::env::set_var("DOT_AGENT_DECK_FEATURES_CONFIG", v),
                None => std::env::remove_var("DOT_AGENT_DECK_FEATURES_CONFIG"),
            }
        }
    }
}

/// Home directory used to anchor config/state/cache paths. Delegates to
/// [`crate::platform::paths::home_dir`] (PRD #42 M1): `$HOME` (fallback `/`) on
/// Unix, `%USERPROFILE%` on Windows.
pub(crate) fn dirs_home() -> PathBuf {
    crate::platform::paths::home_dir()
}

/// Directory holding the user's dot-agent-deck config files. Delegates to
/// [`crate::platform::paths::config_dir`] (PRD #163 M1): `~/.config/dot-agent-deck`
/// on Unix — byte-for-byte what every caller previously spelled inline —
/// `%APPDATA%\dot-agent-deck` on Windows.
///
/// Each per-file `DOT_AGENT_DECK_*` override is checked by its own resolver
/// *before* this is called, so overrides stay authoritative.
pub(crate) fn config_dir() -> PathBuf {
    crate::platform::paths::config_dir()
}

// ---------------------------------------------------------------------------
// Experimental feature flag — `[features]` table in `.dot-agent-deck.toml`
// (PRD #139). The flag plumbing lives in `crate::features`; this module owns
// only the parse + env-merge + file-load helpers it builds on.
// ---------------------------------------------------------------------------

/// Env var that overrides the `[features] experimental` value. A
/// case-insensitive `1`/`true` forces the flag ON; any other set value
/// forces it OFF. Env WINS over the file (PRD #139 OQ3), so once it is set,
/// file edits to that field are ignored on reload.
pub const EXPERIMENTAL_ENV: &str = "DOT_AGENT_DECK_EXPERIMENTAL";

/// Internal mirror of the `.dot-agent-deck.toml` shape for the `[features]`
/// table only. Every other key (`[[modes]]`, `[[orchestrations]]`, …) is
/// ignored, so this loader is decoupled from `ProjectConfig`'s schema and an
/// absent `[features]` table deserializes to the default (experimental =
/// false).
#[derive(Debug, Default, Deserialize)]
struct FeaturesFile {
    #[serde(default)]
    features: crate::features::Features,
}

/// Parse the `[features]` table out of `.dot-agent-deck.toml` contents. An
/// absent table (or empty file) yields the default (`experimental = false`).
/// Returns `Err` on malformed TOML so the hot-reload path can keep the
/// previous value (PRD #139 M2.1).
pub fn parse_features(contents: &str) -> Result<crate::features::Features, toml::de::Error> {
    Ok(toml::from_str::<FeaturesFile>(contents)?.features)
}

/// Apply the env override to a file-derived value. `DOT_AGENT_DECK_EXPERIMENTAL`
/// WINS over the file when set (OQ3): a case-insensitive `1`/`true` forces ON,
/// any other set value forces OFF, and an unset var defers to `file`.
pub fn resolve_features(file: crate::features::Features) -> crate::features::Features {
    let experimental = match std::env::var(EXPERIMENTAL_ENV) {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true"
        }
        Err(_) => file.experimental,
    };
    crate::features::Features { experimental }
}

/// Path of the `.dot-agent-deck.toml` whose `[features]` table backs the
/// flag: the file in `project_dir`, the directory the CALLER has decided is
/// the project.
///
/// Issue #577: this used to join [`std::env::current_dir`] unconditionally,
/// making it the only config read in the deck keyed to the process's own
/// working directory rather than to a directory it was handed — every other
/// one goes through [`crate::project_config::load_project_config`] with an
/// explicit dir. A deck launched anywhere but its project therefore read the
/// `[features]` table from the launch directory's file (usually one that does
/// not exist), so every experimental surface silently resolved OFF and the
/// symptom was indistinguishable from the feature having been removed. The
/// cwd read now happens once at the entry point (`launch_project_dir` in
/// `main.rs`), where it is a deliberate choice rather than a hidden default.
///
/// `DOT_AGENT_DECK_FEATURES_CONFIG` names the file outright and still wins
/// over `project_dir`, so tests — and an operator pointing the flag at one
/// specific file — depend on no directory at all.
pub fn features_config_path(project_dir: &Path) -> PathBuf {
    if let Ok(p) = std::env::var("DOT_AGENT_DECK_FEATURES_CONFIG") {
        return PathBuf::from(p);
    }
    project_dir.join(crate::project_config::CONFIG_FILE_NAME)
}

/// Display-only convenience wrapper over
/// [`features_config_path_with_diagnostics`] for callers (currently only
/// tests) that just want *a* path to show, not a trust decision: when the
/// walk finds nowhere trustworthy at all, that function returns `None` and
/// this wrapper substitutes `cwd.join(crate::project_config::CONFIG_FILE_NAME)`
/// purely so there is something non-empty to print. See that function's own
/// doc for the actual ancestor walk, its `DOT_AGENT_DECK_FEATURES_CONFIG`
/// override, and its world-writable/indeterminate-ancestor trust rules
/// (fork issue #309) — this wrapper does not reimplement any of it.
///
/// Reads the process cwd itself, unlike [`features_config_path_with_diagnostics`]
/// (issue #577: production callers pass an explicit `project_dir` instead) —
/// acceptable here because this wrapper is display/test-only and never feeds
/// a value back into loading or watching a file.
pub fn features_config_path_for_display() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let (path, _) = features_config_path_with_diagnostics(&cwd);
    path.unwrap_or_else(|| cwd.join(crate::project_config::CONFIG_FILE_NAME))
}

/// [`features_config_path_for_display`]'s real logic, returning one diagnostic message
/// per declined ancestor whose candidate file actually existed there —
/// nothing is emitted for an ancestor that was merely untrusted but held no
/// config (reviewer minor 1: warning on every `/tmp`-launched deck when
/// nothing was ever declined is noise, not diagnostics). Each message is
/// also `tracing::warn!`-logged as it is found, so `DOT_AGENT_DECK_LOG`
/// still captures it even if the caller discards the returned `Vec`.
///
/// `project_dir` is passed in rather than derived here (issue #577): the
/// caller decides which directory the walk starts from, exactly as
/// [`features_config_path`] does — production callers pass
/// [`resolve_project_dir`]'s result via `main.rs`'s `launch_project_dir`, so
/// this walk runs a SECOND, independent hardening pass starting from a
/// directory the ownership-based walk already vetted.
///
/// Returns `None` for the path when NO ancestor could be trusted — every
/// one was either world-writable or its safety was indeterminate (fork
/// issue #309 auditor finding M-1r). `None` is a deliberate refusal to name
/// a location at all, not a "safest guess": every candidate directory
/// remaining at that point is one the walk has already declined to trust,
/// so there is no path left to hand back that is not itself a live
/// instance of the thing this function exists to reject. Callers that load
/// or watch a file (`init_and_watch`) must treat `None` as "install the
/// default and start no watcher"; only [`features_config_path_for_display`]'s
/// display-only wrapper substitutes a path for it, and only for printing.
///
/// The walk classifies each ancestor with [`classify_dir_safety`], which
/// returns one of three states rather than a bare bool, and the two
/// non-`Safe` states are handled identically here (both decline a present
/// candidate and are never a fallback candidate) — see that function's doc
/// for why `Unknown` cannot be folded into `Safe` here even though it can
/// be for the search itself. The walk tracks the nearest ancestor it has
/// confirmed `Safe` as it goes, and falls back to *that* directory's joined
/// path when no ancestor holds a trusted config — never unconditionally to
/// `project_dir`, and never to an ancestor whose safety is merely unknown.
/// Before the M-1 fix, a `cd /tmp && deck` launched directly inside a
/// world-writable directory declined that directory's own attacker-planted
/// config, logged the decline, and then handed the identical attacker path
/// back anyway via an unconditional `cwd.join(...)` fallback — #309's own
/// headline scenario, still reachable despite the warning claiming
/// otherwise. That headline scenario is closed. Two narrower residuals
/// remained until this round (auditor M-1r / reviewer L-1) — both now
/// closed by returning `None` instead of ever falling back to an untrusted
/// path:
///
/// - `current_dir()` failing (e.g. `ENAMETOOLONG` behind a symlink into a
///   deep attacker-controlled tree) makes `cwd` the placeholder `"."`, whose
///   ancestors are `["." , ""]`. `std::fs::metadata("")` errors, so the
///   empty ancestor used to be treated as `Safe` by the old fail-open bool
///   — and `Path::new("").join(CONFIG_FILE_NAME)` is a *relative* path,
///   which resolves against the real (attacker-controlled) process cwd, not
///   against any directory the walk actually vetted. That let the very file
///   just declined one iteration earlier be handed straight back. Under the
///   three-state split, `""`'s `Unknown` result is never a fallback
///   candidate and never trusted when a candidate is present, so this path
///   can no longer be reached.
/// - When every ancestor up to and including `/` is world-writable,
///   `safe_fallback` stays `None` throughout; the old code substituted
///   `cwd` there regardless, which is byte-identical to the declined
///   attacker config if cwd itself was declined. Returning `None` all the
///   way out removes that special case rather than papering over it — there
///   genuinely is nowhere left to trust.
pub fn features_config_path_with_diagnostics(project_dir: &Path) -> (Option<PathBuf>, Vec<String>) {
    if let Ok(p) = std::env::var("DOT_AGENT_DECK_FEATURES_CONFIG") {
        return (Some(PathBuf::from(p)), Vec::new());
    }
    resolve_ancestor_walk(project_dir.ancestors())
}

/// Decide whether a candidate `.dot-agent-deck.toml` found by the ancestor
/// walk may be trusted, given the uid that owns it and the uid we are running
/// as (issue #577).
///
/// The pure-data core of the walk's trust check, split out so its rules are
/// unit-testable on every platform — the same shape as
/// [`crate::platform::fsperm::endpoint_owner_is_trusted`], and for the same
/// reason: a decision this small should not need a foreign-owned file (or
/// root) to exercise.
///
/// Walking upward means considering directories the operator did not name and
/// may not own — `/tmp/project` sits under a world-writable `/tmp`, where any
/// local user can create `/tmp/.dot-agent-deck.toml`. Ownership is the check
/// that matters: an attacker cannot create a file owned by *us*. It fails
/// closed, so an ancestor we cannot vouch for is skipped and the walk
/// continues past it rather than adopting it.
/// `cfg_attr` rather than a `#[cfg(unix)]` on the function: the rule is
/// platform-independent and its test runs everywhere, but the only production
/// caller is inside `config_candidate_is_trusted`'s `#[cfg(unix)]` arm, so on
/// Windows this is dead code to a build that does not compile the tests —
/// which `build-windows` is, since it runs bare `cargo clippy -- -D warnings`
/// without `--all-targets` (CLAUDE.md rule 2). Same shape, mirror-image
/// platform, as `fsperm::endpoint_owner_is_trusted`'s
/// `#[cfg_attr(not(windows), allow(dead_code))]`.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn config_owner_is_trusted(file_uid: u32, our_uid: u32) -> bool {
    file_uid == our_uid
}

/// The project directory the deck's process-global config reads key off:
/// `start` if it holds a trusted `.dot-agent-deck.toml`, else the nearest
/// ancestor that does, else `start` unchanged.
///
/// Issue #577: the deck used to key the `[features]` table to its own working
/// directory, so running it from anywhere below the project root — `repo/src`,
/// `repo/docs`, a nested crate — read a `.dot-agent-deck.toml` that is not
/// there and silently resolved every experimental surface OFF. Walking up is
/// how a project-scoped config is normally found, and it makes the flag depend
/// on which PROJECT you are in rather than on which of its directories you
/// happened to be standing in.
///
/// Two deliberate limits:
///
/// - A candidate must be a regular file owned by the current uid
///   ([`config_owner_is_trusted`]); anything else is skipped and the walk
///   continues. `metadata` follows symlinks, so the ownership answer is about
///   the resolved target, matching [`load_features_file`]'s own `is_file()`
///   check.
/// - When nothing qualifies the result is `start` itself, so the resulting
///   path is byte-identical to the pre-#577 behaviour and a deck launched
///   outside any project reads exactly what it read before.
///
/// This does NOT make the flag per-project — it stays one process-global
/// toggle (CLAUDE.md rule 9). A deck launched entirely outside its project,
/// with panes pointed into it, still reads the launch directory's file;
/// `DOT_AGENT_DECK_FEATURES_CONFIG` is the escape hatch there.
pub fn resolve_project_dir(start: &Path) -> PathBuf {
    // `ancestors()` yields `start` first, so a project whose config sits in
    // the launch directory itself resolves without walking anywhere and keeps
    // exactly its pre-#577 behaviour. It is lexical rather than symlink-
    // resolving, which is what we want: `current_dir()` already hands us the
    // physical path on Unix, and re-resolving would report a project root the
    // operator never typed.
    for dir in start.ancestors() {
        if config_candidate_is_trusted(&dir.join(crate::project_config::CONFIG_FILE_NAME)) {
            return dir.to_path_buf();
        }
    }
    start.to_path_buf()
}

/// Whether `path` is a `.dot-agent-deck.toml` the ancestor walk may stop at:
/// a regular file owned by the current user. Anything else — absent, a
/// directory, a foreign-owned file — is skipped so the walk continues past it.
///
/// `metadata` follows symlinks, so a symlink to a foreign-owned file is judged
/// by its target, and a dangling one is simply absent. This is the same
/// stat-then-read shape [`load_features_file`] already uses; it is not a
/// TOCTOU guarantee and does not need to be, because the loader re-validates
/// (regular file, size cap, parse) before it trusts a byte of the content.
/// This check decides only WHICH directory is the project.
fn config_candidate_is_trusted(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        config_owner_is_trusted(metadata.uid(), crate::platform::paths::current_uid())
    }
    // Windows has no uid, and the file analogue of the SID check in
    // `platform::fsperm` takes a kernel HANDLE rather than a path — it is
    // built for the named pipe and the spawn mutex, not for stat-ing a config.
    // The exposure it would close is also structurally smaller here: the walk
    // climbs through the per-user profile (`C:\Users\<me>\…`), which is
    // ACL'd to that user, before reaching `C:\`, which is admin-writable by
    // default — there is no world-writable `/tmp` equivalent on the way up.
    // So Windows accepts any regular file, and the property that differs is
    // recorded here rather than left to be inferred from the `cfg`.
    #[cfg(not(unix))]
    {
        true
    }
}

/// The walk itself, factored out of [`features_config_path_with_diagnostics`]
/// over an explicit ancestor sequence rather than calling `cwd.ancestors()`
/// internally. This exists for testability, not for any production caller:
/// the real walk always terminates at the real filesystem root, which is
/// normally `Safe` on any machine these tests run on, so the "nowhere
/// trustworthy" (`None`) outcome and the degenerate `current_dir()`-failed
/// route (auditor M-1r) can't be produced by chdir'ing into a fixture alone
/// without chmod-ing real system directories — not something a test may do.
/// Taking the ancestor sequence as a parameter lets a test hand it a short,
/// self-contained, entirely-fixture-owned list instead.
fn resolve_ancestor_walk<'a, I>(ancestors: I) -> (Option<PathBuf>, Vec<String>)
where
    I: IntoIterator<Item = &'a std::path::Path>,
{
    let mut safe_fallback: Option<&std::path::Path> = None;
    let mut declined = Vec::new();
    for ancestor in ancestors {
        let safety = classify_dir_safety(ancestor);
        let candidate = ancestor.join(crate::project_config::CONFIG_FILE_NAME);
        if candidate.is_file() {
            if safety == DirSafety::Safe {
                return (Some(candidate), declined);
            }
            let reason = match safety {
                DirSafety::WorldWritable => {
                    "its directory is world-writable, so the file may have \
                     been planted by another user"
                }
                DirSafety::Unknown => {
                    "its directory's write permissions could not be \
                     determined, so it cannot be trusted"
                }
                DirSafety::Safe => unreachable!("handled above"),
            };
            let message = format!(
                "declining {} in features-config search: {reason} (fork \
                 issue #309); continuing search upward",
                crate::terminal_sanitize::sanitize_path_for_terminal_display(&candidate)
            );
            tracing::warn!("{message}");
            declined.push(message);
            continue;
        }
        if safety == DirSafety::Safe {
            safe_fallback.get_or_insert(ancestor);
        }
    }
    let path = safe_fallback.map(|dir| dir.join(crate::project_config::CONFIG_FILE_NAME));
    (path, declined)
}

/// The trust classification of a directory for the features-config ancestor
/// walk (fork issue #309). Three states rather than a bare bool because the
/// walk has two different consumers of this result with two different
/// soundness requirements (round-2 auditor finding M-1r):
///
/// - **Search** (a candidate file exists in this ancestor): `Unknown` and
///   `WorldWritable` are handled identically — the candidate is declined,
///   not trusted, and the search continues upward. Declining on `Unknown`
///   here is a deliberate change from the original fail-open bool: that
///   bool's doc argued a `stat` failure on the directory implies
///   `candidate.is_file()` on a file inside it fails the same way, so
///   nothing gets through — an argument that is sound for a genuine
///   absolute-path ancestor but does not hold for the degenerate `""`
///   ancestor produced by a failed `current_dir()` (see
///   [`features_config_path_with_diagnostics`]'s doc), where the "candidate
///   inside this ancestor" is actually a relative path that resolves
///   against the real process cwd instead. Declining on `Unknown`
///   unconditionally closes that gap without needing to special-case which
///   ancestor triggered it.
/// - **Fallback selection** (no candidate here, but this ancestor might
///   still serve as the safe directory the resolver falls back to):
///   `Unknown` is never recorded as the fallback. An ancestor whose
///   writability could not be determined is not a place the resolver has
///   any basis to trust, so it must not become the directory the watcher
///   polls for the rest of the process's life (auditor M-1r, scenario B).
// `WorldWritable` and `Unknown` are only constructed on Unix (see
// `classify_dir_safety` below); the non-Unix arm always returns `Safe`, so
// `-D dead-code` flags both variants there. Remove this once #383 lands a
// Windows ACL check that can construct them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(unix), allow(dead_code))]
enum DirSafety {
    /// Confirmed not world-writable. May supply a candidate or serve as a
    /// fallback.
    Safe,
    /// Confirmed world-writable (Unix "other" write bit set). A candidate
    /// here is declined; never a fallback.
    WorldWritable,
    /// `stat` on the directory itself failed, so its writability could not
    /// be determined. A candidate here is declined (never trusted); never a
    /// fallback. On Unix this is a genuine "don't know"; the non-Unix arm
    /// below never produces it today (see its own doc for why).
    Unknown,
}

/// Classify `dir`'s trust for the features-config ancestor walk. See
/// [`DirSafety`] for how the two call sites in
/// [`features_config_path_with_diagnostics`] use the three states
/// differently.
///
/// Deliberately narrower than #309's own suggested predicate —
/// group-writable and attacker-*owned* ancestors are not covered (tracked
/// separately as fork issue #384; not folded in here).
#[cfg(unix)]
fn classify_dir_safety(dir: &std::path::Path) -> DirSafety {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(dir) {
        Ok(meta) => {
            if meta.permissions().mode() & 0o002 != 0 {
                DirSafety::WorldWritable
            } else {
                DirSafety::Safe
            }
        }
        Err(_) => DirSafety::Unknown,
    }
}

/// On non-Unix targets there is no equivalent POSIX mode bit to check —
/// Windows ACLs don't map onto it cheaply — so this always reports `Safe`,
/// unchanged from the pre-round-2 behavior (Windows follow-up tracked as
/// fork issue #383). Deliberately `Safe`, not `Unknown`: `Unknown` declines
/// every present candidate (see [`DirSafety`]'s doc), so returning it here
/// unconditionally would decline every `.dot-agent-deck.toml` on Windows —
/// there is no ACL check happening at all yet to justify that. `Unknown` on
/// this crate's non-Unix build is reserved for a genuine "couldn't
/// determine" outcome once #383 adds one; there isn't one today. Same split
/// as `platform::paths::is_executable_file`'s, for the same reason.
#[cfg(not(unix))]
fn classify_dir_safety(_dir: &std::path::Path) -> DirSafety {
    DirSafety::Safe
}

/// Upper bound on the `.dot-agent-deck.toml` the feature-flag loader will
/// read. A `[features]` table is a handful of bytes; this cap stops a
/// pathological `DOT_AGENT_DECK_FEATURES_CONFIG` target (a huge regular file)
/// from exhausting memory on the detached ~2s watcher thread (audit LOW-1).
const MAX_FEATURES_CONFIG_BYTES: u64 = 64 * 1024;

/// Open `path` for the features-config loaders, opening before checking so
/// there is no window between "confirm what this is" and "use it" (see
/// [`load_features_file`]'s doc comment for the full TOCTOU rationale). On
/// Unix this sets `O_NONBLOCK` so that if the target has been swapped for a
/// FIFO between resolution and this call, `open` returns immediately instead
/// of blocking until a writer appears (fork issue #310) — a `is_file()`
/// check on the resulting handle then rejects it below. `O_NONBLOCK` does
/// not affect reads from a regular file, only special files like FIFOs and
/// sockets, so it is safe to leave set for the read that follows. Also sets
/// `O_NOCTTY` (auditor m-1): without it, a symlinked TTY device at the
/// resolved path would be acquired as the controlling terminal of the
/// `setsid`-detached daemon, which otherwise has none — a real open-time
/// side effect `O_NONBLOCK` alone does not touch. Every other open-time
/// device side effect (e.g. `/dev/watchdog` arming on open) has no portable
/// guard and is accepted as a residual: reaching it requires an ancestor the
/// walk already trusts, i.e. an attacker who can already plant a config
/// there.
#[cfg(unix)]
fn open_features_config_file(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(path)
}

/// Windows has no FIFO-swap hazard to guard against (no equivalent
/// non-blocking open exists), but a plain `File::open` cannot obtain a
/// handle to a *directory* at all without `FILE_FLAG_BACKUP_SEMANTICS` — so
/// without it, a directory target fails at `open` with an opaque error and
/// collapses into [`FeaturesFileOutcome::Unreadable`] downstream instead of
/// [`FeaturesFileOutcome::NotRegular`], regressing the label the pre-#310
/// stat-first order produced (the pre-existing
/// `describe_features_file_reports_not_regular_for_a_directory` test pins
/// this). Requesting the flag lets a directory handle open successfully
/// here too, so the same downstream `is_file()` check on the handle rejects
/// it exactly as it does on Unix.
///
/// Privilege assumption (auditor i-2): `FILE_FLAG_BACKUP_SEMANTICS` has a
/// second, unused effect here. Besides being the documented way to obtain a
/// directory handle via `CreateFile` (the only effect this code relies on),
/// it also *requests* backup/restore semantics, which bypass ACL checks —
/// but only when `SeBackupPrivilege`/`SeRestorePrivilege` are both present
/// AND enabled in the process token. Windows does not enable those by
/// default even for Administrators (present-but-disabled, requiring an
/// explicit `AdjustTokenPrivileges` call neither Rust's `std` nor this
/// codebase makes), so for a normal deck process this flag's only reachable
/// effect is the directory-handle one. If the deck is ever run under a
/// token with `SeBackupPrivilege` enabled (a backup-operator service
/// context), this open could read a features config whose ACL denies the
/// invoking user — narrow and unlikely, but a real difference from
/// `File::open` worth knowing about if that context ever applies.
#[cfg(windows)]
fn open_features_config_file(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_features_config_file(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

/// Load the `[features]` table from `path`. A missing file is the default
/// (OFF). A non-regular target (FIFO, device, …), an oversized file, or
/// malformed/partial TOML keeps `previous` — the partial-write tolerance the
/// watcher relies on (PRD #139 M2.1) plus the runaway-target guard from audit
/// LOW-1. Warnings never echo file content (audit INFO-2): only the path is
/// logged, so pointing the override at a sensitive file can't leak its bytes.
///
/// Opens `path` first and stats the resulting *handle* rather than the path
/// (fork issue #310): stat-then-open leaves a window in which the target can
/// be replaced, and if the replacement is a FIFO, opening it blocks
/// indefinitely on the startup path. Checking and using the same handle
/// closes that window; `is_file()` on the handle still rejects a FIFO (or a
/// symlink resolved to one) exactly as the old path-based check did. One
/// consequence of the reorder: every target, including a non-regular one
/// that the old stat-first order would have rejected before ever touching
/// it, is now genuinely opened before it is rejected — see
/// [`open_features_config_file`]'s doc comment for what that costs and why
/// `O_NOCTTY` is there.
pub fn load_features_file(
    path: &std::path::Path,
    previous: crate::features::Features,
) -> crate::features::Features {
    use std::io::Read;

    let file = match open_features_config_file(path) {
        Ok(f) => f,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return crate::features::Features::default();
        }
        Err(_) => {
            tracing::warn!(
                "failed to open {}; keeping previous experimental={}",
                path.display(),
                previous.experimental
            );
            return previous;
        }
    };
    let metadata = match file.metadata() {
        Ok(m) => m,
        Err(_) => {
            tracing::warn!(
                "failed to stat {}; keeping previous experimental={}",
                path.display(),
                previous.experimental
            );
            return previous;
        }
    };
    if !metadata.is_file() {
        tracing::warn!(
            "features config {} is not a regular file; keeping previous experimental={}",
            path.display(),
            previous.experimental
        );
        return previous;
    }
    if metadata.len() > MAX_FEATURES_CONFIG_BYTES {
        tracing::warn!(
            "features config {} exceeds {MAX_FEATURES_CONFIG_BYTES} bytes; keeping previous experimental={}",
            path.display(),
            previous.experimental
        );
        return previous;
    }

    // Read with a hard cap as defense-in-depth against growth after the
    // fstat above.
    let mut contents = String::new();
    if file
        .take(MAX_FEATURES_CONFIG_BYTES)
        .read_to_string(&mut contents)
        .is_err()
    {
        tracing::warn!(
            "failed to read {}; keeping previous experimental={}",
            path.display(),
            previous.experimental
        );
        return previous;
    }

    match parse_features(&contents) {
        Ok(features) => features,
        // audit INFO-2: never include the toml error's Display — it embeds a
        // snippet of the offending input, which could leak a sensitive file's
        // contents if the override path is pointed at one.
        Err(_) => {
            tracing::warn!(
                "invalid [features] table in {}: malformed TOML; keeping previous experimental={}",
                path.display(),
                previous.experimental
            );
            previous
        }
    }
}

/// Diagnostic-only outcome of reading `path`, distinguishing "the file
/// genuinely supplied the resolved value" from every reason
/// [`load_features_file`] falls back instead — used only by
/// `dot-agent-deck features status` (fork issue #303 Phase 2 review), which
/// needs to *explain* a load failure that `load_features_file` itself only
/// needs to survive. The applied `Features` value is still produced by
/// [`load_features_file`] alone; this mirrors its branching to label the
/// outcome, never to recompute the value, so the two can disagree about the
/// wording but never about the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeaturesFileOutcome {
    /// No file at the resolved path; the default applies.
    NotFound,
    /// The target exists, opened successfully, but is not a regular file
    /// (FIFO, device, directory, symlink to one of those, …) once fstat'd
    /// on the open handle. A target of the same shape that *fails to open*
    /// (a Unix-domain socket returns `ENXIO`; an unreadable directory on
    /// some platforms) is [`Unreadable`](Self::Unreadable) instead, since
    /// this variant can only be produced by successfully opening first
    /// (fork issue #310's open-then-fstat order).
    NotRegular,
    /// The target exists but exceeds `MAX_FEATURES_CONFIG_BYTES`.
    Oversized,
    /// The target exists but could not be opened, or could not be stat'd or
    /// read once opened (permissions, a Unix-domain socket, or any other
    /// `open`-time failure that isn't `NotFound`).
    Unreadable,
    /// The target was read but its TOML is malformed.
    Malformed,
    /// The target was read and parsed successfully (an absent `[features]`
    /// table still counts as parsed — `#[serde(default)]` makes that a valid,
    /// genuinely-read outcome rather than a fallback).
    Parsed,
}

/// Diagnostic-only mirror of [`load_features_file`]'s branching over `path`,
/// reporting which branch was taken instead of the resulting `Features`. See
/// [`FeaturesFileOutcome`]. Uses the same open-first-then-fstat-the-handle
/// sequence as `load_features_file` (fork issue #310), so the two functions
/// cannot drift on which targets they open or reject.
pub fn describe_features_file(path: &std::path::Path) -> FeaturesFileOutcome {
    use std::io::Read;

    let file = match open_features_config_file(path) {
        Ok(f) => f,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return FeaturesFileOutcome::NotFound;
        }
        Err(_) => return FeaturesFileOutcome::Unreadable,
    };
    let metadata = match file.metadata() {
        Ok(m) => m,
        Err(_) => return FeaturesFileOutcome::Unreadable,
    };
    if !metadata.is_file() {
        return FeaturesFileOutcome::NotRegular;
    }
    if metadata.len() > MAX_FEATURES_CONFIG_BYTES {
        return FeaturesFileOutcome::Oversized;
    }
    let mut contents = String::new();
    if file
        .take(MAX_FEATURES_CONFIG_BYTES)
        .read_to_string(&mut contents)
        .is_err()
    {
        return FeaturesFileOutcome::Unreadable;
    }
    match parse_features(&contents) {
        Ok(_) => FeaturesFileOutcome::Parsed,
        Err(_) => FeaturesFileOutcome::Malformed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spec::spec;

    /// The pure trust rule the ancestor walk stops on (issue #577). Split out
    /// of the filesystem check so the deletion-unsafe direction — adopting an
    /// ancestor config we do not own — is exercised without needing a
    /// foreign-owned file or root, mirroring how
    /// `platform::fsperm::endpoint_owner_is_trusted` is tested. Ownership is
    /// the check that matters when walking upward: `/tmp/project` sits under a
    /// world-writable `/tmp` where any local user can drop a
    /// `/tmp/.dot-agent-deck.toml`, and an attacker cannot create a file owned
    /// by us.
    #[test]
    fn ancestor_config_is_trusted_only_when_we_own_it() {
        assert!(
            config_owner_is_trusted(1000, 1000),
            "our own file is trusted"
        );
        assert!(
            !config_owner_is_trusted(0, 1000),
            "a root-owned config planted above us is not adopted"
        );
        assert!(
            !config_owner_is_trusted(1001, 1000),
            "another local user's config planted above us is not adopted"
        );
    }

    /// Scenario: Drive a `SnapshotCoalescer` (interval 500ms) synchronously with
    /// a 50-change burst all observed at the same instant — after each
    /// `mark_dirty` the simulated event loop checks `is_due(now)` and writes if
    /// due — then perform one trailing check after the interval has elapsed.
    /// The leading-edge write fires on the first change; the remaining 49 are
    /// throttled; the trailing check flushes the accumulated burst — so the
    /// burst collapses to at most two writes (here exactly two), never the 50 a
    /// naive write-per-change would produce.
    #[spec("session/save/003")]
    #[test]
    fn save_003_coalesces_burst_to_at_most_two_writes() {
        let interval = Duration::from_millis(500);
        let mut coalescer = SnapshotCoalescer::new(interval);
        let mut writes = 0usize;

        // A tight burst: 50 rapid changes, all at the same instant (now = 0),
        // each followed by the event loop's `is_due` check — exactly how the
        // main loop drives it. Only the leading-edge write should fire here.
        let now = Duration::ZERO;
        for _ in 0..50 {
            coalescer.mark_dirty();
            if coalescer.is_due(now) {
                writes += 1;
                coalescer.record_write(now);
            }
        }

        // The loop keeps ticking; once `interval` has elapsed with a change
        // still pending, the single trailing write flushes the coalesced burst.
        let after = interval;
        if coalescer.is_due(after) {
            writes += 1;
            coalescer.record_write(after);
        }

        // PRD #89 review-fix G2: assert the EXACT write count, not `<= 2` / `>= 1`.
        // For this scenario the count is fully determined — one leading-edge write
        // on the first change plus one trailing write after the interval elapses =
        // 2 — so any off-by-one in the coalescer (an extra leading write, a missed
        // trailing flush) flips this count and fails the test.
        assert_eq!(
            writes, 2,
            "a 50-change burst must coalesce to exactly two writes \
             (leading edge + one trailing flush), got {writes}"
        );

        // After the trailing flush nothing is pending, so no further write is
        // due no matter how far the clock advances.
        assert!(
            !coalescer.is_due(after + interval + interval),
            "no write is due once the burst has been flushed and nothing new is dirty"
        );
    }

    #[test]
    fn bell_config_defaults() {
        let bc = BellConfig::default();
        assert!(bc.enabled);
        assert!(bc.on_waiting_for_input);
        assert!(!bc.on_idle);
        assert!(bc.on_error);
    }

    #[test]
    fn bell_config_deserialize_empty() {
        let bc: BellConfig = toml::from_str("").unwrap();
        assert!(bc.enabled);
        assert!(bc.on_waiting_for_input);
        assert!(!bc.on_idle);
        assert!(bc.on_error);
    }

    #[test]
    fn bell_config_deserialize_partial() {
        let bc: BellConfig = toml::from_str("on_idle = true").unwrap();
        assert!(bc.enabled);
        assert!(bc.on_idle);
    }

    #[test]
    fn dashboard_config_without_bell_section() {
        let dc: DashboardConfig = toml::from_str(r#"default_command = "echo hi""#).unwrap();
        assert_eq!(dc.default_command, "echo hi");
        assert!(dc.bell.enabled);
    }

    #[test]
    fn dashboard_config_with_bell_section() {
        let toml_str = r#"
default_command = "test"

[bell]
enabled = false
on_idle = true
"#;
        let dc: DashboardConfig = toml::from_str(toml_str).unwrap();
        assert!(!dc.bell.enabled);
        assert!(dc.bell.on_idle);
        assert!(dc.bell.on_waiting_for_input);
    }

    #[test]
    fn should_bell_respects_enabled() {
        let bc = BellConfig {
            enabled: false,
            ..Default::default()
        };
        assert!(!bc.should_bell(&SessionStatus::WaitingForInput));
        assert!(!bc.should_bell(&SessionStatus::Error));
    }

    #[test]
    fn saved_session_round_trip() {
        let session = SavedSession {
            panes: vec![
                SavedPane {
                    dir: "/repo/api".to_string(),
                    name: "api".to_string(),
                    command: "claude".to_string(),
                    mode: None,
                    orchestration: None,
                },
                SavedPane {
                    dir: "/repo/ui".to_string(),
                    name: "ui".to_string(),
                    command: "".to_string(),
                    mode: None,
                    orchestration: None,
                },
            ],
            last_command: None,
        };
        let toml_str = toml::to_string_pretty(&session).unwrap();
        let loaded: SavedSession = toml::from_str(&toml_str).unwrap();
        assert_eq!(loaded.panes.len(), 2);
        assert_eq!(loaded.panes[0].dir, "/repo/api");
        assert_eq!(loaded.panes[0].name, "api");
        assert_eq!(loaded.panes[0].command, "claude");
        assert_eq!(loaded.panes[1].command, "");
    }

    /// PRD #196: the global `last_command` round-trips through serialize →
    /// deserialize, AND a session TOML written before the field existed (no
    /// `last_command` key) loads as `None` rather than failing — the
    /// `#[serde(default)]` forward-compat guarantee the form-seed fallback relies
    /// on (missing/unreadable → empty, never a hard failure).
    #[test]
    fn saved_session_last_command_round_trip_and_missing_key() {
        // (a) A set last_command round-trips intact.
        let session = SavedSession {
            panes: Vec::new(),
            last_command: Some("claude".to_string()),
        };
        let toml_str = toml::to_string_pretty(&session).unwrap();
        let loaded: SavedSession = toml::from_str(&toml_str).unwrap();
        assert_eq!(loaded.last_command.as_deref(), Some("claude"));

        // (b) A legacy session.toml predating the field still parses, with
        // last_command == None (the #[serde(default)] guarantee).
        let legacy = r#"
[[panes]]
dir = "/repo/legacy"
name = "old-pane"
command = "vim"
"#;
        let legacy_loaded: SavedSession = toml::from_str(legacy).unwrap();
        assert_eq!(legacy_loaded.panes.len(), 1);
        assert!(
            legacy_loaded.last_command.is_none(),
            "a session.toml with no last_command key must load as None"
        );
    }

    /// Scenario: Build a `SavedSession` whose single pane carries an
    /// `OrchestrationSnapshot` (3 roles in display order, a start-role cursor,
    /// an orchestrator prompt, the resolved config name + project path, a
    /// started-roles list, and — fork #166 M3.0 — a persisted `owner`
    /// identity string), serialize it to TOML and deserialize it back —
    /// asserting every orchestration field round-trips intact, `owner`
    /// included. Then deserialize two backward-compat `session.toml`
    /// strings: one with NO `orchestration` key at all, and one WITH an
    /// `[panes.orchestration]` block but no `owner` key (a snapshot written
    /// before this field existed) — asserting both still parse, the first
    /// with `orchestration == None` and the second with `owner == None`,
    /// proving the `#[serde(default)]` forward-compat guarantee for both the
    /// whole block and the new field individually.
    #[spec("config/saved-session/001")]
    #[test]
    fn saved_session_001_orchestration_serde_round_trip_and_legacy_parse() {
        // (a) Round-trip a pane carrying an OrchestrationSnapshot.
        let session = SavedSession {
            panes: vec![SavedPane {
                dir: "/repo/app".to_string(),
                name: "orchestrator".to_string(),
                command: "claude".to_string(),
                mode: None,
                orchestration: Some(OrchestrationSnapshot {
                    version: 1,
                    roles: vec![
                        "orchestrator".to_string(),
                        "coder".to_string(),
                        "reviewer".to_string(),
                    ],
                    start_role_index: 0,
                    orchestrator_prompt: "Build the feature end to end".to_string(),
                    config_name: "tdd-cycle".to_string(),
                    project_path: "/repo/app".to_string(),
                    started_role_indices: vec![0, 1],
                    display_title: Some("My TDD Run".to_string()),
                    owner: Some("orchestration:tdd-cycle".to_string()),
                }),
            }],
            last_command: None,
        };

        let toml_str = toml::to_string_pretty(&session).unwrap();
        let loaded: SavedSession = toml::from_str(&toml_str).unwrap();

        assert_eq!(loaded.panes.len(), 1);
        let pane = &loaded.panes[0];
        assert_eq!(pane.dir, "/repo/app");
        assert_eq!(pane.name, "orchestrator");
        assert_eq!(pane.command, "claude");
        assert_eq!(pane.mode, None);

        let orch = pane
            .orchestration
            .as_ref()
            .expect("orchestration must round-trip as Some");
        assert_eq!(orch.version, 1);
        assert_eq!(orch.roles, vec!["orchestrator", "coder", "reviewer"]);
        assert_eq!(orch.start_role_index, 0);
        assert_eq!(orch.orchestrator_prompt, "Build the feature end to end");
        assert_eq!(orch.config_name, "tdd-cycle");
        assert_eq!(orch.project_path, "/repo/app");
        assert_eq!(orch.started_role_indices, vec![0, 1]);
        assert_eq!(orch.display_title.as_deref(), Some("My TDD Run"));
        assert_eq!(orch.owner.as_deref(), Some("orchestration:tdd-cycle"));

        // (b) A legacy session.toml predating the orchestration field still
        // parses, with orchestration == None (the #[serde(default)] guarantee).
        let legacy = r#"
[[panes]]
dir = "/repo/legacy"
name = "old-pane"
command = "vim"
"#;
        let legacy_loaded: SavedSession = toml::from_str(legacy).unwrap();
        assert_eq!(legacy_loaded.panes.len(), 1);
        assert_eq!(legacy_loaded.panes[0].dir, "/repo/legacy");
        assert!(
            legacy_loaded.panes[0].orchestration.is_none(),
            "a legacy snapshot with no orchestration key must parse with orchestration == None"
        );

        // (c) Fork #166 M3.0: an orchestration snapshot written BEFORE the
        // `owner` field existed (has `[panes.orchestration]` but no `owner`
        // key) must still parse, with `owner == None` — the honest
        // "no identity captured" outcome for a pre-upgrade snapshot, not a
        // parse failure.
        let pre_owner_field = r#"
[[panes]]
dir = "/repo/app"
name = "orchestrator"
command = "claude"

[panes.orchestration]
version = 1
roles = ["orchestrator", "coder"]
start_role_index = 0
orchestrator_prompt = ""
config_name = "tdd-cycle"
project_path = "/repo/app"
"#;
        let pre_owner_loaded: SavedSession = toml::from_str(pre_owner_field).unwrap();
        let pre_owner_orch = pre_owner_loaded.panes[0]
            .orchestration
            .as_ref()
            .expect("orchestration block must still parse without an owner key");
        assert!(
            pre_owner_orch.owner.is_none(),
            "a pre-M3.0 snapshot with no owner key must restore with owner == None, \
             not a fabricated or defaulted identity"
        );
    }

    #[test]
    fn saved_session_empty_default() {
        let session = SavedSession::default();
        assert!(session.panes.is_empty());
    }

    #[test]
    fn saved_session_deserialize_empty() {
        let session: SavedSession = toml::from_str("").unwrap();
        assert!(session.panes.is_empty());
    }

    #[test]
    fn saved_session_load_save_clear() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.toml");
        let prev = std::env::var("DOT_AGENT_DECK_SESSION").ok();
        // SAFETY: test is single-threaded; no other code reads this var concurrently.
        unsafe {
            std::env::set_var("DOT_AGENT_DECK_SESSION", path.to_str().unwrap());
        }

        // Load returns default when file missing
        let session = SavedSession::load();
        assert!(session.panes.is_empty());

        // Save then load round-trips
        let session = SavedSession {
            panes: vec![SavedPane {
                dir: "/tmp/test".to_string(),
                name: "test".to_string(),
                command: "echo hi".to_string(),
                mode: None,
                orchestration: None,
            }],
            last_command: None,
        };
        session.save().unwrap();
        let loaded = SavedSession::load();
        assert_eq!(loaded.panes.len(), 1);
        assert_eq!(loaded.panes[0].dir, "/tmp/test");

        // Clear removes the file
        SavedSession::clear().unwrap();
        assert!(!path.exists());

        // SAFETY: test cleanup — restore original env var.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("DOT_AGENT_DECK_SESSION", v),
                None => std::env::remove_var("DOT_AGENT_DECK_SESSION"),
            }
        }
    }

    #[test]
    fn should_bell_per_status() {
        let bc = BellConfig::default();
        assert!(bc.should_bell(&SessionStatus::WaitingForInput));
        assert!(!bc.should_bell(&SessionStatus::Idle));
        assert!(bc.should_bell(&SessionStatus::Error));
        assert!(!bc.should_bell(&SessionStatus::Thinking));
        assert!(!bc.should_bell(&SessionStatus::Working));
        assert!(!bc.should_bell(&SessionStatus::Compacting));
    }

    #[test]
    fn star_prompt_default_values() {
        let state = StarPromptState::default();
        assert_eq!(state.launch_count, 0);
        assert!(!state.permanently_dismissed);
        assert_eq!(state.last_prompt_at_launch, 0);
    }

    #[test]
    fn star_prompt_serde_round_trip() {
        let state = StarPromptState {
            launch_count: 42,
            permanently_dismissed: true,
            last_prompt_at_launch: 30,
        };
        let json = serde_json::to_string(&state).unwrap();
        let loaded: StarPromptState = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.launch_count, 42);
        assert!(loaded.permanently_dismissed);
        assert_eq!(loaded.last_prompt_at_launch, 30);
    }

    #[test]
    fn star_prompt_serde_missing_fields() {
        let loaded: StarPromptState = serde_json::from_str("{}").unwrap();
        assert_eq!(loaded.launch_count, 0);
        assert!(!loaded.permanently_dismissed);
        assert_eq!(loaded.last_prompt_at_launch, 0);
    }

    #[test]
    fn star_prompt_increment_and_check_triggers_at_10() {
        // Test pure logic without file I/O — manually track state
        let mut state = StarPromptState::default();
        for i in 1..=9 {
            state.launch_count = i;
            let should_show = !state.permanently_dismissed
                && state.launch_count - state.last_prompt_at_launch >= STAR_PROMPT_INTERVAL;
            assert!(!should_show, "should not trigger at launch {i}");
        }
        state.launch_count = 10;
        let should_show = !state.permanently_dismissed
            && state.launch_count - state.last_prompt_at_launch >= STAR_PROMPT_INTERVAL;
        assert!(should_show, "should trigger at launch 10");
    }

    #[test]
    fn star_prompt_snooze_resets_window() {
        let mut state = StarPromptState::default();
        state.launch_count = 10;
        state.last_prompt_at_launch = state.launch_count; // snooze
        for i in 11..=19 {
            state.launch_count = i;
            let should_show = !state.permanently_dismissed
                && state.launch_count - state.last_prompt_at_launch >= STAR_PROMPT_INTERVAL;
            assert!(!should_show, "should not trigger at launch {i}");
        }
        state.launch_count = 20;
        let should_show = !state.permanently_dismissed
            && state.launch_count - state.last_prompt_at_launch >= STAR_PROMPT_INTERVAL;
        assert!(should_show, "should trigger at launch 20");
    }

    #[test]
    fn star_prompt_dismiss_permanently() {
        let mut state = StarPromptState {
            permanently_dismissed: true,
            ..StarPromptState::default()
        };
        for i in 1..=20 {
            state.launch_count = i;
            let should_show = !state.permanently_dismissed
                && state.launch_count - state.last_prompt_at_launch >= STAR_PROMPT_INTERVAL;
            assert!(!should_show, "dismissed state should never trigger");
        }
    }

    #[test]
    fn star_prompt_load_save_cycle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("star.json");
        let prev = std::env::var("DOT_AGENT_DECK_STAR_PROMPT").ok();
        // SAFETY: test is single-threaded; no other code reads this var concurrently.
        unsafe {
            std::env::set_var("DOT_AGENT_DECK_STAR_PROMPT", path.to_str().unwrap());
        }

        let state = StarPromptState {
            launch_count: 15,
            permanently_dismissed: false,
            last_prompt_at_launch: 10,
        };
        state.save().unwrap();

        let loaded = StarPromptState::load();
        assert_eq!(loaded.launch_count, 15);
        assert!(!loaded.permanently_dismissed);
        assert_eq!(loaded.last_prompt_at_launch, 10);

        // Load from corrupted file returns default
        std::fs::write(&path, "not valid json!!!").unwrap();
        let loaded = StarPromptState::load();
        assert_eq!(loaded.launch_count, 0);

        // Load from missing file returns default
        std::fs::remove_file(&path).unwrap();
        let loaded = StarPromptState::load();
        assert_eq!(loaded.launch_count, 0);
        assert!(!loaded.permanently_dismissed);

        // SAFETY: test cleanup — restore original env var.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("DOT_AGENT_DECK_STAR_PROMPT", v),
                None => std::env::remove_var("DOT_AGENT_DECK_STAR_PROMPT"),
            }
        }
    }

    /// Issue #519: a `config.toml` written before the idle-art removal still
    /// carries an `[idle_art]` section. `DashboardConfig` sets no
    /// `#[serde(deny_unknown_fields)]`, so the stale table must be dropped
    /// silently — a hard parse failure here would print "Invalid config" and
    /// reset every OTHER key to its default on startup.
    #[test]
    fn dashboard_config_ignores_removed_idle_art_section() {
        let toml_str = r#"
default_command = "claude"
auto_config_prompt = false

[idle_art]
enabled = true
provider = "openai"
model = "gpt-4o-mini"
timeout_secs = 600
"#;
        let dc: DashboardConfig =
            toml::from_str(toml_str).expect("a stale [idle_art] section must not fail the load");
        assert_eq!(dc.default_command, "claude");
        assert!(!dc.auto_config_prompt);
    }

    /// Issue #519: the four `idle_art.*` keys are gone from [`CONFIG_KEYS`], so
    /// `config get` / `config set` now report them as unknown rather than
    /// accepting a setting nothing reads.
    #[test]
    fn idle_art_config_keys_are_unknown() {
        let mut dc = DashboardConfig::default();
        for key in [
            "idle_art.enabled",
            "idle_art.provider",
            "idle_art.model",
            "idle_art.timeout_secs",
        ] {
            assert!(dc.get_field(key).is_err(), "get_field({key}) should fail");
            assert!(
                dc.set_field(key, "true").is_err(),
                "set_field({key}) should fail"
            );
        }
        assert!(
            !CONFIG_KEYS.iter().any(|(k, _)| k.starts_with("idle_art.")),
            "no idle_art.* key should remain in the `config set --help` listing"
        );
    }

    #[test]
    fn auto_config_prompt_defaults_to_true() {
        let dc = DashboardConfig::default();
        assert!(dc.auto_config_prompt);
    }

    #[test]
    fn auto_config_prompt_deserialize_missing() {
        let dc: DashboardConfig = toml::from_str("").unwrap();
        assert!(dc.auto_config_prompt);
    }

    #[test]
    fn auto_config_prompt_deserialize_false() {
        let dc: DashboardConfig = toml::from_str("auto_config_prompt = false").unwrap();
        assert!(!dc.auto_config_prompt);
    }

    // PRD #42 M2: asserts the Unix `/tmp` + per-uid fallback specifically
    // (`current_uid()` is Unix-only and the path shape is POSIX), so it is
    // gated to Unix. The Windows endpoint is a per-user named pipe with no
    // `/tmp`/uid analogue — covered separately under PRD #163/#164.
    #[cfg(unix)]
    #[test]
    fn attach_socket_fallback_is_per_user() {
        // PRD #93 round-2 reviewer REV-2: when XDG_RUNTIME_DIR is unset
        // *and* DOT_AGENT_DECK_ATTACH_SOCKET is unset, the fallback under
        // /tmp must include the uid so two users on the same host don't
        // collide. The old `/tmp/dot-agent-deck-attach.sock` would
        // sandwich two daemons onto one path and let the first binder
        // arbitrarily lock the rest of the host out.
        let _g = STATE_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_attach = std::env::var("DOT_AGENT_DECK_ATTACH_SOCKET").ok();
        let prev_sock = std::env::var("DOT_AGENT_DECK_SOCKET").ok();
        let prev_xdg = std::env::var("XDG_RUNTIME_DIR").ok();
        // SAFETY: state-dir lock held, restored on the way out.
        unsafe {
            std::env::remove_var("DOT_AGENT_DECK_ATTACH_SOCKET");
            std::env::remove_var("DOT_AGENT_DECK_SOCKET");
            std::env::remove_var("XDG_RUNTIME_DIR");
        }

        let uid = crate::platform::paths::current_uid();
        let attach = attach_socket_path();
        let hook = socket_path();
        let attach_str = attach.to_string_lossy();
        let hook_str = hook.to_string_lossy();
        assert!(
            attach_str.contains(&format!("-{uid}.sock")),
            "attach fallback must embed uid: got {attach_str}"
        );
        assert!(
            hook_str.contains(&format!("-{uid}.sock")),
            "hook fallback must embed uid: got {hook_str}"
        );
        assert!(
            attach_str.starts_with("/tmp/"),
            "attach fallback should live under /tmp when XDG is unset: got {attach_str}"
        );

        // SAFETY: same lock; restoring previous values.
        unsafe {
            match prev_attach {
                Some(v) => std::env::set_var("DOT_AGENT_DECK_ATTACH_SOCKET", v),
                None => std::env::remove_var("DOT_AGENT_DECK_ATTACH_SOCKET"),
            }
            match prev_sock {
                Some(v) => std::env::set_var("DOT_AGENT_DECK_SOCKET", v),
                None => std::env::remove_var("DOT_AGENT_DECK_SOCKET"),
            }
            match prev_xdg {
                Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }
    }

    #[test]
    fn state_dir_uses_explicit_override_first() {
        let _guard = STATE_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_state = std::env::var("DOT_AGENT_DECK_STATE_DIR").ok();
        let prev_xdg = std::env::var("XDG_STATE_HOME").ok();
        // SAFETY: env-var lock held; restored on the way out.
        unsafe {
            std::env::set_var("DOT_AGENT_DECK_STATE_DIR", "/tmp/explicit-state");
            std::env::set_var("XDG_STATE_HOME", "/should/be/ignored");
        }

        assert_eq!(state_dir(), PathBuf::from("/tmp/explicit-state"));

        // SAFETY: same lock held; restoring previous values.
        unsafe {
            match prev_state {
                Some(v) => std::env::set_var("DOT_AGENT_DECK_STATE_DIR", v),
                None => std::env::remove_var("DOT_AGENT_DECK_STATE_DIR"),
            }
            match prev_xdg {
                Some(v) => std::env::set_var("XDG_STATE_HOME", v),
                None => std::env::remove_var("XDG_STATE_HOME"),
            }
        }
    }

    #[test]
    fn state_dir_uses_xdg_state_home_when_set() {
        let _guard = STATE_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_state = std::env::var("DOT_AGENT_DECK_STATE_DIR").ok();
        let prev_xdg = std::env::var("XDG_STATE_HOME").ok();
        // SAFETY: env-var lock held; restored on the way out.
        unsafe {
            std::env::remove_var("DOT_AGENT_DECK_STATE_DIR");
            std::env::set_var("XDG_STATE_HOME", "/var/lib/state");
        }

        // Unix honors `$XDG_STATE_HOME`. Windows has no XDG concept, so the
        // resolver ignores it and returns `%LOCALAPPDATA%\dot-agent-deck`
        // (derived here from the same `dirs` crate the resolver uses, so no
        // machine-specific username is hardcoded).
        #[cfg(unix)]
        assert_eq!(state_dir(), PathBuf::from("/var/lib/state/dot-agent-deck"));
        #[cfg(windows)]
        {
            let expected = dirs::data_local_dir()
                .map(|p| p.join("dot-agent-deck"))
                .unwrap_or_else(|| {
                    crate::platform::paths::home_dir().join("AppData/Local/dot-agent-deck")
                });
            assert_eq!(state_dir(), expected);
        }

        // SAFETY: same lock held; restoring previous values.
        unsafe {
            match prev_state {
                Some(v) => std::env::set_var("DOT_AGENT_DECK_STATE_DIR", v),
                None => std::env::remove_var("DOT_AGENT_DECK_STATE_DIR"),
            }
            match prev_xdg {
                Some(v) => std::env::set_var("XDG_STATE_HOME", v),
                None => std::env::remove_var("XDG_STATE_HOME"),
            }
        }
    }

    #[test]
    fn state_dir_falls_back_to_home_when_xdg_unset() {
        let _guard = STATE_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_state = std::env::var("DOT_AGENT_DECK_STATE_DIR").ok();
        let prev_xdg = std::env::var("XDG_STATE_HOME").ok();
        let prev_home = std::env::var("HOME").ok();
        // SAFETY: env-var lock held; restored on the way out.
        unsafe {
            std::env::remove_var("DOT_AGENT_DECK_STATE_DIR");
            std::env::remove_var("XDG_STATE_HOME");
            std::env::set_var("HOME", "/home/test-user");
        }

        // Unix falls back to `$HOME/.local/state`. Windows ignores `$HOME`
        // entirely and returns `%LOCALAPPDATA%\dot-agent-deck` (derived from the
        // same `dirs` crate the resolver uses — no hardcoded username/prefix).
        #[cfg(unix)]
        assert_eq!(
            state_dir(),
            PathBuf::from("/home/test-user/.local/state/dot-agent-deck")
        );
        #[cfg(windows)]
        {
            let expected = dirs::data_local_dir()
                .map(|p| p.join("dot-agent-deck"))
                .unwrap_or_else(|| {
                    crate::platform::paths::home_dir().join("AppData/Local/dot-agent-deck")
                });
            assert_eq!(state_dir(), expected);
        }

        // SAFETY: same lock held; restoring previous values.
        unsafe {
            match prev_state {
                Some(v) => std::env::set_var("DOT_AGENT_DECK_STATE_DIR", v),
                None => std::env::remove_var("DOT_AGENT_DECK_STATE_DIR"),
            }
            match prev_xdg {
                Some(v) => std::env::set_var("XDG_STATE_HOME", v),
                None => std::env::remove_var("XDG_STATE_HOME"),
            }
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[test]
    fn auto_config_prompt_get_set_field() {
        let mut dc = DashboardConfig::default();
        assert_eq!(dc.get_field("auto_config_prompt").unwrap(), "true");
        dc.set_field("auto_config_prompt", "false").unwrap();
        assert!(!dc.auto_config_prompt);
        assert_eq!(dc.get_field("auto_config_prompt").unwrap(), "false");
        assert!(dc.set_field("auto_config_prompt", "notbool").is_err());
    }

    #[test]
    fn config_gen_state_default_empty() {
        let state = ConfigGenState::default();
        assert!(state.suppressed_dirs.is_empty());
    }

    #[test]
    fn config_gen_state_suppress_and_check() {
        let mut state = ConfigGenState::default();
        assert!(!state.is_suppressed("/some/dir"));
        state.suppressed_dirs.push("/some/dir".to_string());
        assert!(state.is_suppressed("/some/dir"));
        assert!(!state.is_suppressed("/other/dir"));
    }

    #[test]
    fn config_gen_state_suppress_dir_deduplicates() {
        // suppress_dir() calls save(), which reads DOT_AGENT_DECK_CONFIG_GEN_STATE.
        // Hold the env-var lock and point at a temp path so we neither race
        // against load_save_cycle nor pollute the real home dir.
        let _guard = CONFIG_GEN_STATE_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config-gen-state.json");
        // Drop guard restores the env var even if an assertion below panics.
        let _env_restore = ConfigGenStateEnvGuard::set(path.to_str().unwrap());

        let mut state = ConfigGenState::default();
        state.suppressed_dirs.push("/dup".to_string());
        state.suppressed_dirs.push("/dup".to_string()); // manual dup
        // suppress_dir should not add again
        assert_eq!(state.suppressed_dirs.len(), 2);
        // But the method itself checks before adding
        let mut state2 = ConfigGenState::default();
        state2.suppressed_dirs.push("/dup".to_string());
        state2.suppress_dir("/dup");
        assert_eq!(state2.suppressed_dirs.len(), 1);
    }

    #[test]
    fn config_gen_state_serde_round_trip() {
        let state = ConfigGenState {
            suppressed_dirs: vec!["/a".to_string(), "/b".to_string()],
        };
        let json = serde_json::to_string(&state).unwrap();
        let loaded: ConfigGenState = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.suppressed_dirs.len(), 2);
        assert!(loaded.is_suppressed("/a"));
        assert!(loaded.is_suppressed("/b"));
    }

    // scheduler/config/001 — one valid + one malformed `[[scheduled_tasks]]`:
    // the valid entry loads, the malformed one is reported as an error, and
    // there is no panic.
    #[test]
    fn schedules_load_one_valid_one_malformed() {
        let toml_str = r#"
[[scheduled_tasks]]
name = "good"
cron = "0 9 * * *"
working_dir = "/tmp/good"
command = "claude"
prompt = "do the thing"

[[scheduled_tasks]]
name = "bad"
# `cron` is required but missing, and prompt is missing too → entry fails
working_dir = "/tmp/bad"
"#;
        let loaded = LoadedSchedules::parse(toml_str);
        assert_eq!(loaded.tasks.len(), 1, "valid entry still loads");
        assert_eq!(loaded.tasks[0].name, "good");
        assert_eq!(loaded.errors.len(), 1, "malformed entry reported");
        assert_eq!(loaded.errors[0].entry, Some(1));
    }

    // PRD #127 follow-up — a hand-edited entry with no `command` is REJECTED on
    // load (no silent $SHELL fallback): it is reported as an error and skipped,
    // while a sibling entry that DOES carry a command still loads. Mirrors the
    // malformed-entry handling so the daemon never crashes on a bad entry.
    #[test]
    fn schedules_reject_command_less_entry_keep_valid() {
        let toml_str = r#"
[[scheduled_tasks]]
name = "no-cmd"
cron = "0 9 * * *"
working_dir = "/tmp/no-cmd"
prompt = "do the thing"

[[scheduled_tasks]]
name = "has-cmd"
cron = "0 9 * * *"
working_dir = "/tmp/has-cmd"
command = "claude"
prompt = "do the thing"
"#;
        let loaded = LoadedSchedules::parse(toml_str);
        assert_eq!(
            loaded.tasks.len(),
            1,
            "only the command-bearing entry loads"
        );
        assert_eq!(loaded.tasks[0].name, "has-cmd");
        assert_eq!(loaded.errors.len(), 1, "the command-less entry is reported");
        assert_eq!(loaded.errors[0].entry, Some(0));
        assert!(
            loaded.errors[0].message.to_lowercase().contains("command"),
            "error must name the missing command, got: {}",
            loaded.errors[0].message
        );

        // A blank (whitespace-only) command is rejected the same way.
        let blank = r#"
[[scheduled_tasks]]
name = "blank-cmd"
cron = "0 9 * * *"
working_dir = "/tmp/blank-cmd"
command = "   "
prompt = "do the thing"
"#;
        let loaded = LoadedSchedules::parse(blank);
        assert!(loaded.tasks.is_empty(), "a blank command is not a command");
        assert_eq!(loaded.errors.len(), 1);
    }

    /// Scenario: Issue #222 edge 1 — `validate_task` never bounds
    /// `ScheduledTask.name`, so two names sharing a 185-char prefix collide
    /// once `sanitize_marker_creator` (worktree_reclaim.rs,
    /// `MARKER_CREATOR_MAX_CHARS` = 200) truncates the formatted
    /// `issue-dispatch:{task_name}#{issue}` creator string at 200 chars: the
    /// 15-char `"issue-dispatch:"` prefix plus a 185-char shared name prefix
    /// is exactly the 200-char cutoff, so both entries' markers become
    /// byte-identical and match each other's worktrees under `--mine`. The
    /// issue's preferred fix is a load-time rejection at the producer
    /// (here), not a truncation-collision-avoidance trick at the sink.
    #[spec("scheduler/config/003")]
    #[test]
    fn config_003_schedules_reject_overlong_task_name_that_would_collide() {
        let shared_prefix = "a".repeat(185);
        let name_one = format!("{shared_prefix}{}", "b".repeat(65));
        let name_two = format!("{shared_prefix}{}", "c".repeat(65));
        assert_ne!(name_one, name_two, "sanity: the two names must differ");

        let creator_one = crate::worktree_reclaim::sanitize_marker_creator(&format!(
            "issue-dispatch:{name_one}#7"
        ));
        let creator_two = crate::worktree_reclaim::sanitize_marker_creator(&format!(
            "issue-dispatch:{name_two}#7"
        ));
        assert_eq!(
            creator_one, creator_two,
            "sanity: today's 200-char truncation genuinely collides these two names"
        );

        let toml_str = format!(
            r#"
[[scheduled_tasks]]
name = "{name_one}"
cron = "0 9 * * *"
working_dir = "/tmp/one"
command = "claude"
prompt = "do the thing"

[[scheduled_tasks]]
name = "{name_two}"
cron = "0 9 * * *"
working_dir = "/tmp/two"
command = "claude"
prompt = "do the thing"
"#
        );
        let loaded = LoadedSchedules::parse(&toml_str);
        assert!(
            loaded.tasks.is_empty(),
            "an overlong task name must be rejected at load time — today's gap: \
             `validate_task` never checks `task.name` length, so both of these \
             load successfully and later collide under `sanitize_marker_creator`'s \
             200-char truncation — got tasks: {:?}",
            loaded.tasks.iter().map(|t| &t.name).collect::<Vec<_>>()
        );
        assert_eq!(
            loaded.errors.len(),
            2,
            "both overlong entries should be reported as load errors, got: {:?}",
            loaded.errors
        );
    }

    #[test]
    fn schedules_missing_file_is_empty_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.toml");
        let loaded = LoadedSchedules::load_from(&path);
        assert!(loaded.tasks.is_empty());
        assert!(loaded.errors.is_empty());
    }

    // scheduler/config/002 — a minimal entry applies the documented defaults
    // (`new_tab_per_fire=false`, `enabled=true`) and `~`/`$VAR` in `working_dir`
    // are expanded at load time. `command` is required (PRD #127 follow-up) so
    // each entry carries one.
    #[test]
    fn schedules_defaults_and_path_expansion() {
        let _guard = STATE_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_home = std::env::var("HOME").ok();
        let prev_var = std::env::var("DAD_TEST_DIR").ok();
        // SAFETY: env-var lock held; restored on the way out.
        unsafe {
            std::env::set_var("HOME", "/home/tester");
            std::env::set_var("DAD_TEST_DIR", "projects/digest");
        }

        let toml_str = r#"
[[scheduled_tasks]]
name = "minimal"
cron = "0 9 * * *"
working_dir = "~/scheduled/morning"
command = "claude"
prompt = "hi"

[[scheduled_tasks]]
name = "with-var"
cron = "0 9 * * *"
working_dir = "$DAD_TEST_DIR"
command = "claude"
prompt = "hi"
"#;
        let loaded = LoadedSchedules::parse(toml_str);
        assert!(loaded.errors.is_empty());
        assert_eq!(loaded.tasks.len(), 2);

        let minimal = &loaded.tasks[0];
        assert!(!minimal.new_tab_per_fire, "new_tab_per_fire defaults false");
        assert!(minimal.enabled, "enabled defaults true");
        assert_eq!(minimal.command.as_deref(), Some("claude"));
        // `~/...` is expanded at load time. On Unix the home is the `HOME` we
        // set above; on Windows `~` resolves via the path resolver (e.g.
        // `%USERPROFILE%`), so assert the expansion *logic* (anchored at the
        // resolver's home, tilde gone, expected suffix) rather than a hardcoded
        // path — the username varies per machine.
        #[cfg(unix)]
        assert_eq!(minimal.working_dir, "/home/tester/scheduled/morning");
        #[cfg(windows)]
        {
            let home = crate::platform::paths::home_dir();
            assert!(minimal.working_dir.starts_with(&*home.to_string_lossy()));
            assert!(minimal.working_dir.ends_with("scheduled/morning"));
            assert!(!minimal.working_dir.contains('~'));
        }

        // Relative result (from $VAR) resolves against home.
        let with_var = &loaded.tasks[1];
        #[cfg(unix)]
        assert_eq!(with_var.working_dir, "/home/tester/projects/digest");
        #[cfg(windows)]
        {
            let home = crate::platform::paths::home_dir();
            assert!(with_var.working_dir.starts_with(&*home.to_string_lossy()));
            assert!(with_var.working_dir.ends_with("projects/digest"));
        }

        // SAFETY: same lock held; restore previous values.
        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match prev_var {
                Some(v) => std::env::set_var("DAD_TEST_DIR", v),
                None => std::env::remove_var("DAD_TEST_DIR"),
            }
        }
    }

    #[test]
    fn schedules_round_trip_explicit_fields() {
        let toml_str = r#"
[[scheduled_tasks]]
name = "full"
cron = "0 9 * * MON-FRI"
working_dir = "/abs/path"
command = "claude"
prompt = "multi\nline"
new_tab_per_fire = true
enabled = false
"#;
        let loaded = LoadedSchedules::parse(toml_str);
        assert!(loaded.errors.is_empty());
        let t = &loaded.tasks[0];
        assert_eq!(t.command.as_deref(), Some("claude"));
        assert!(t.new_tab_per_fire);
        assert!(!t.enabled);
        assert_eq!(t.working_dir, "/abs/path");
    }

    // PRD #120 (U1) — a fully-specified `issue_dispatch` task round-trips every
    // field from `schedules.toml`: the `[scheduled_tasks.issue_dispatch]` table
    // is the task-type discriminator, and a command-less entry is accepted
    // (unlike a #127 single-spawn task) because the per-issue command comes from
    // the cloned repo's config.
    #[test]
    fn issue_dispatch_round_trips_all_fields() {
        let toml_str = r#"
[[scheduled_tasks]]
name = "Issues vfarcic/dot-ai"
cron = "0 9 * * MON-FRI"
working_dir = "/work/space"
prompt = "Work on issue {{issue_number}}"
enabled = true

[scheduled_tasks.issue_dispatch]
repo = "vfarcic/dot-ai"
max_per_run = 3
label = "agent-eligible"
query = "is:open sort:created-asc"
"#;
        let loaded = LoadedSchedules::parse(toml_str);
        assert!(loaded.errors.is_empty(), "errors: {:?}", loaded.errors);
        assert_eq!(loaded.tasks.len(), 1);
        let t = &loaded.tasks[0];
        // Shared scheduler fields still carry the task's identity / schedule.
        assert_eq!(t.name, "Issues vfarcic/dot-ai");
        assert_eq!(t.cron, "0 9 * * MON-FRI");
        assert_eq!(t.working_dir, "/work/space");
        assert_eq!(t.prompt, "Work on issue {{issue_number}}");
        assert!(
            t.command.is_none(),
            "issue-dispatch needs no top-level command"
        );
        // The discriminator table carries every GitHub-specific field.
        let disp = t
            .issue_dispatch
            .as_ref()
            .expect("issue_dispatch table present → issue-dispatch task");
        assert_eq!(disp.repo, "vfarcic/dot-ai");
        assert_eq!(disp.max_per_run, 3);
        assert_eq!(disp.label.as_deref(), Some("agent-eligible"));
        assert_eq!(disp.query.as_deref(), Some("is:open sort:created-asc"));
    }

    // PRD #120 (U1) — the optional filters (`label`, `query`) default to `None`
    // when omitted, and a plain `[[scheduled_tasks]]` entry with no
    // `issue_dispatch` table is still a (command-required) single-spawn task.
    #[test]
    fn issue_dispatch_optional_filters_default_to_none() {
        let toml_str = r#"
[[scheduled_tasks]]
name = "Issues vfarcic/dot-ai"
cron = "0 9 * * *"
working_dir = "/work/space"
prompt = "Work on issue {{issue_number}}"

[scheduled_tasks.issue_dispatch]
repo = "vfarcic/dot-ai"
max_per_run = 5
"#;
        let loaded = LoadedSchedules::parse(toml_str);
        assert!(loaded.errors.is_empty(), "errors: {:?}", loaded.errors);
        let disp = loaded.tasks[0]
            .issue_dispatch
            .as_ref()
            .expect("issue-dispatch task");
        assert_eq!(disp.repo, "vfarcic/dot-ai");
        assert_eq!(disp.max_per_run, 5);
        assert!(disp.label.is_none(), "omitted label defaults to None");
        assert!(disp.query.is_none(), "omitted query defaults to None");
        // `enabled` still defaults true via the shared machinery.
        assert!(loaded.tasks[0].enabled);

        // A non-issue-dispatch entry has no table and is the original task type.
        let plain = r#"
[[scheduled_tasks]]
name = "plain"
cron = "0 9 * * *"
working_dir = "/tmp"
command = "claude"
prompt = "hi"
"#;
        let loaded = LoadedSchedules::parse(plain);
        assert!(loaded.errors.is_empty());
        assert!(loaded.tasks[0].issue_dispatch.is_none());
    }

    // PRD #120 — `max_per_run` is optional: a hand-written issue-dispatch table
    // that omits it deserializes (and validates/loads) with the changelog's
    // documented default of 3, rather than being rejected for a missing field.
    #[test]
    fn issue_dispatch_max_per_run_defaults_to_three_when_omitted() {
        let toml_str = r#"
[[scheduled_tasks]]
name = "Issues vfarcic/dot-ai"
cron = "0 9 * * *"
working_dir = "/work/space"
prompt = "Work on issue {{issue_number}}"

[scheduled_tasks.issue_dispatch]
repo = "vfarcic/dot-ai"
"#;
        let loaded = LoadedSchedules::parse(toml_str);
        // The entry must validate and load (not land in `errors`).
        assert!(loaded.errors.is_empty(), "errors: {:?}", loaded.errors);
        assert_eq!(loaded.tasks.len(), 1);
        let disp = loaded.tasks[0]
            .issue_dispatch
            .as_ref()
            .expect("issue-dispatch task");
        assert_eq!(disp.repo, "vfarcic/dot-ai");
        assert_eq!(
            disp.max_per_run, 3,
            "omitted max_per_run must default to 3 (matches changelog)"
        );
    }

    // PRD #120 (M1) — a hand-edited issue-dispatch task with a malformed `repo`
    // is rejected at load time (the value flows into `gh`/`git` argv), surfaced
    // as a per-entry error and skipped, while valid siblings still load.
    #[test]
    fn issue_dispatch_rejects_invalid_repo() {
        let toml_str = r#"
[[scheduled_tasks]]
name = "bad repo"
cron = "0 9 * * *"
working_dir = "/work/space"
prompt = "Work on issue {{issue_number}}"

[scheduled_tasks.issue_dispatch]
repo = "ext::sh -c id"
max_per_run = 3
"#;
        let loaded = LoadedSchedules::parse(toml_str);
        assert!(loaded.tasks.is_empty(), "malformed repo must be rejected");
        assert_eq!(loaded.errors.len(), 1);
        assert!(
            loaded.errors[0].message.contains("owner/name"),
            "error should explain the slug requirement: {:?}",
            loaded.errors[0].message
        );
    }

    #[test]
    fn issue_dispatch_rejects_leading_dash_label() {
        let toml_str = r#"
[[scheduled_tasks]]
name = "bad label"
cron = "0 9 * * *"
working_dir = "/work/space"
prompt = "Work on issue {{issue_number}}"

[scheduled_tasks.issue_dispatch]
repo = "acme/widgets"
max_per_run = 3
label = "-rf"
"#;
        let loaded = LoadedSchedules::parse(toml_str);
        assert!(
            loaded.tasks.is_empty(),
            "a leading-`-` label must be rejected"
        );
        assert_eq!(loaded.errors.len(), 1);
    }

    #[test]
    fn expand_path_handles_braced_and_lone_dollar() {
        let _guard = STATE_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_home = std::env::var("HOME").ok();
        let prev_var = std::env::var("DAD_BRACE").ok();
        // SAFETY: env-var lock held; restored below.
        unsafe {
            std::env::set_var("HOME", "/home/tester");
            std::env::set_var("DAD_BRACE", "braced");
        }

        assert_eq!(expand_path("/a/${DAD_BRACE}/b"), "/a/braced/b");
        // `~` expands to the home directory. On Unix that is the `HOME` we set
        // above; on Windows it resolves via the path resolver (`%USERPROFILE%`),
        // so derive the expected value from the resolver instead of hardcoding a
        // machine-specific path.
        #[cfg(unix)]
        assert_eq!(expand_path("~"), "/home/tester");
        #[cfg(windows)]
        assert_eq!(
            expand_path("~"),
            crate::platform::paths::home_dir()
                .to_string_lossy()
                .into_owned()
        );
        // A lone `$` and an undefined var don't panic.
        assert_eq!(expand_path("/lit/$"), "/lit/$");
        assert_eq!(expand_path("/x/$DAD_UNDEFINED/y"), "/x//y");

        // SAFETY: same lock held; restore previous values.
        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match prev_var {
                Some(v) => std::env::set_var("DAD_BRACE", v),
                None => std::env::remove_var("DAD_BRACE"),
            }
        }
    }

    #[test]
    fn config_gen_state_load_save_cycle() {
        // Serialize against any other test that touches this env var or calls
        // save()/load() — Rust runs unit tests in parallel.
        let _guard = CONFIG_GEN_STATE_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config-gen-state.json");
        let prev = std::env::var("DOT_AGENT_DECK_CONFIG_GEN_STATE").ok();
        // SAFETY: env-var lock held for the duration of this test.
        unsafe {
            std::env::set_var("DOT_AGENT_DECK_CONFIG_GEN_STATE", path.to_str().unwrap());
        }

        // Load returns default when file missing
        let state = ConfigGenState::load();
        assert!(state.suppressed_dirs.is_empty());

        // Save then load round-trips
        let mut state = ConfigGenState::default();
        state.suppressed_dirs.push("/test/dir".to_string());
        state.save().unwrap();
        let loaded = ConfigGenState::load();
        assert_eq!(loaded.suppressed_dirs.len(), 1);
        assert!(loaded.is_suppressed("/test/dir"));

        // Load from corrupted file returns default
        std::fs::write(&path, "not valid json!!!").unwrap();
        let loaded = ConfigGenState::load();
        assert!(loaded.suppressed_dirs.is_empty());

        // SAFETY: test cleanup — restore original env var.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("DOT_AGENT_DECK_CONFIG_GEN_STATE", v),
                None => std::env::remove_var("DOT_AGENT_DECK_CONFIG_GEN_STATE"),
            }
        }
    }

    // Fork issue #303: `features_config_path()` resolved the feature-flag
    // config against the process's OWN cwd, while every other config read in
    // the deck resolves against the explicit project directory. This pins
    // the corrected contract: launched from a process cwd nested several
    // levels below the project root, the resolver must still find the
    // PROJECT's `.dot-agent-deck.toml` — not join the process cwd directly —
    // and `DOT_AGENT_DECK_FEATURES_CONFIG` must keep overriding both.
    #[test]
    fn features_config_path_resolves_against_project_dir_not_process_cwd() {
        let _lock = FEATURES_CONFIG_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _guard = FeaturesConfigCwdEnvGuard::new();

        // Isolated fixture: this repo's own `.dot-agent-deck.toml` sets
        // `[features] experimental = true` too, so a bug that accidentally
        // read the real repo config here would pass for the wrong reason.
        let project = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        let project_config = project_root.join(crate::project_config::CONFIG_FILE_NAME);
        std::fs::write(&project_config, "[features]\nexperimental = true\n").unwrap();

        // A process cwd nested three levels below the project root —
        // deliberately not the project directory itself.
        let nested_cwd = project_root.join("a").join("b").join("c");
        std::fs::create_dir_all(&nested_cwd).unwrap();

        std::env::set_current_dir(&nested_cwd).expect("chdir into nested fixture dir");
        let resolved = features_config_path_for_display();

        assert_eq!(
            resolved,
            project_config,
            "features_config_path_for_display() must resolve to the project directory's \
             .dot-agent-deck.toml ({}) even when the process cwd is nested \
             elsewhere under the project ({}); got {}",
            project_config.display(),
            nested_cwd.display(),
            resolved.display()
        );

        // The resolved value for a project whose config carries
        // `experimental = true` must be `true` from the nested cwd too. This
        // now passes: the ancestor walk above already resolved `resolved` to
        // the PROJECT's config, not `<nested_cwd>/.dot-agent-deck.toml` (which
        // does not exist), so loading it and resolving the flag yields `true`.
        let loaded = load_features_file(&resolved, crate::features::Features::default());
        let resolved_flag = resolve_features(loaded);
        assert!(
            resolved_flag.experimental,
            "expected experimental=true from the project's \
             .dot-agent-deck.toml when the process cwd is nested elsewhere \
             in the project; got false (resolved path: {})",
            resolved.display()
        );

        // DOT_AGENT_DECK_FEATURES_CONFIG must still win over both the
        // process cwd and the project-directory resolution — existing
        // behavior this fix must not regress.
        let override_target = project_root.join("override-features.toml");
        std::fs::write(&override_target, "[features]\nexperimental = false\n").unwrap();
        // SAFETY: FEATURES_CONFIG_TEST_LOCK is held for the guard's (and
        // hence this test's) lifetime.
        unsafe {
            std::env::set_var(
                "DOT_AGENT_DECK_FEATURES_CONFIG",
                override_target.to_str().unwrap(),
            );
        }
        let resolved_with_override = features_config_path_for_display();
        assert_eq!(
            resolved_with_override, override_target,
            "DOT_AGENT_DECK_FEATURES_CONFIG must still win over the \
             project-directory resolution"
        );
    }

    // fork #303/#349 review (auditor M2 / reviewer F3): `describe_features_file`
    // backs `dot-agent-deck features status`'s ability to distinguish "the
    // file genuinely supplied the value" from "the file exists but the value
    // fell back to a default", so its branches get direct unit coverage
    // rather than resting solely on the e2e `features/status/00N` tests,
    // which only exercise the NotFound and Parsed outcomes end to end.

    #[test]
    fn describe_features_file_reports_not_found_for_a_missing_path() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist.toml");
        assert_eq!(
            describe_features_file(&missing),
            FeaturesFileOutcome::NotFound
        );
    }

    #[test]
    fn describe_features_file_reports_parsed_for_valid_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".dot-agent-deck.toml");
        std::fs::write(&path, "[features]\nexperimental = true\n").unwrap();
        assert_eq!(describe_features_file(&path), FeaturesFileOutcome::Parsed);
    }

    #[test]
    fn describe_features_file_reports_parsed_when_features_table_absent() {
        // An absent `[features]` table is still a successful parse
        // (`#[serde(default)]`) — a genuinely-read outcome, not a fallback.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".dot-agent-deck.toml");
        std::fs::write(&path, "[[modes]]\nname = \"x\"\ncommand = \"echo\"\n").unwrap();
        assert_eq!(describe_features_file(&path), FeaturesFileOutcome::Parsed);
    }

    #[test]
    fn describe_features_file_reports_malformed_for_invalid_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".dot-agent-deck.toml");
        std::fs::write(&path, "[features\nexperimental = true\n").unwrap();
        assert_eq!(
            describe_features_file(&path),
            FeaturesFileOutcome::Malformed
        );
    }

    #[test]
    fn describe_features_file_reports_not_regular_for_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let subdir = dir.path().join("a-directory.toml");
        std::fs::create_dir(&subdir).unwrap();
        assert_eq!(
            describe_features_file(&subdir),
            FeaturesFileOutcome::NotRegular
        );
    }

    // Fork issue #309: an ancestor directory that is world-writable must be
    // declined rather than trusted — a config planted there by another user
    // must not be adopted — while the walk must keep going up and still find
    // a legitimate config in a normal ancestor further above it. This pins
    // both halves of the required behavior in one fixture, since the second
    // half only means something in the presence of the first. It also pins
    // auditor finding M-1: when cwd itself is the unsafe directory and no
    // ancestor above it holds a config, the fallback must land on the
    // nearest SAFE ancestor, never back on cwd's own declined config.
    #[test]
    #[cfg(unix)]
    fn features_config_path_skips_world_writable_ancestor_but_finds_a_normal_one_above_it() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = FEATURES_CONFIG_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _guard = FeaturesConfigCwdEnvGuard::new();

        let project = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();

        // A legitimate config in a normal (non-world-writable) ancestor.
        let legit_config = project_root.join(crate::project_config::CONFIG_FILE_NAME);
        std::fs::write(&legit_config, "[features]\nexperimental = true\n").unwrap();

        // A world-writable directory nested under it, carrying an
        // attacker-plantable config that must never be adopted.
        let world_writable = project_root.join("world-writable");
        std::fs::create_dir_all(&world_writable).unwrap();
        std::fs::set_permissions(&world_writable, std::fs::Permissions::from_mode(0o777)).unwrap();
        let attacker_config = world_writable.join(crate::project_config::CONFIG_FILE_NAME);
        std::fs::write(&attacker_config, "[features]\nexperimental = false\n").unwrap();

        // The process cwd, nested below the world-writable directory — the
        // fallback-lands-under-a-safe-directory case, which already worked
        // before the M-1 fix below.
        let cwd = world_writable.join("cwd");
        std::fs::create_dir_all(&cwd).unwrap();

        std::env::set_current_dir(&cwd).expect("chdir into fixture dir");
        let (resolved, declined) = features_config_path_with_diagnostics(&cwd);

        assert_eq!(
            resolved,
            Some(legit_config.clone()),
            "features_config_path() must skip the world-writable ancestor's \
             attacker-plantable config ({}) and continue upward to the \
             legitimate one ({}); got {:?}",
            attacker_config.display(),
            legit_config.display(),
            resolved
        );
        assert_eq!(
            declined.len(),
            1,
            "expected exactly one declined-ancestor diagnostic for the \
             world-writable directory; got {declined:?}"
        );

        // Auditor finding M-1: cwd IS the world-writable, attacker-plantable
        // directory this time (`cd /tmp && deck`, #309's own headline
        // scenario), with no legitimate config in any ancestor above it — so
        // the unguarded fallback the fix originally shipped with would
        // return straight back into the attacker's own directory, undoing
        // the decline above.
        let bare_root = tempfile::tempdir().unwrap();
        let safe_parent = bare_root.path().canonicalize().unwrap();
        let unsafe_cwd = safe_parent.join("world-writable-cwd");
        std::fs::create_dir_all(&unsafe_cwd).unwrap();
        std::fs::set_permissions(&unsafe_cwd, std::fs::Permissions::from_mode(0o777)).unwrap();
        let unsafe_cwd_config = unsafe_cwd.join(crate::project_config::CONFIG_FILE_NAME);
        std::fs::write(&unsafe_cwd_config, "[features]\nexperimental = true\n").unwrap();

        std::env::set_current_dir(&unsafe_cwd).expect("chdir into unsafe-cwd fixture dir");
        let (resolved2, declined2) = features_config_path_with_diagnostics(&unsafe_cwd);

        assert_ne!(
            resolved2,
            Some(unsafe_cwd_config.clone()),
            "features_config_path() must not fall back into the \
             world-writable cwd's own attacker-plantable config after \
             declining it; got {resolved2:?}"
        );
        assert_eq!(
            resolved2,
            Some(safe_parent.join(crate::project_config::CONFIG_FILE_NAME)),
            "the guarded fallback must land on the nearest SAFE ancestor \
             ({}), not cwd itself; got {:?}",
            safe_parent.display(),
            resolved2
        );
        assert_eq!(
            declined2.len(),
            1,
            "expected exactly one declined-ancestor diagnostic for the \
             world-writable cwd; got {declined2:?}"
        );
    }

    // Round 2 (auditor M-1r, Route A): `current_dir()` failing makes `cwd`
    // the placeholder `PathBuf::from(".")`, whose `ancestors()` are exactly
    // `[".", ""]`. The `""` ancestor's `metadata("")` fails
    // (`classify_dir_safety` reports `Unknown`), and
    // `Path::new("").join(CONFIG_FILE_NAME)` is a *relative* path that
    // resolves against the real process cwd rather than against any
    // directory the walk actually vetted — which, before this round, let
    // the old fail-open bool treat `""` as trusted and hand back the exact
    // file `"."` just declined one iteration earlier. This drives
    // `resolve_ancestor_walk` directly over that literal `[".", ""]`
    // sequence (see its doc for why the real `cwd.ancestors()` walk can't
    // reproduce this in a test) with the real process cwd chdir'd into a
    // world-writable fixture holding the attacker config, so `"."` and `""`
    // both resolve to the same real, attacker-controlled file.
    #[test]
    #[cfg(unix)]
    fn resolve_ancestor_walk_never_trusts_the_degenerate_empty_ancestor() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = FEATURES_CONFIG_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _guard = FeaturesConfigCwdEnvGuard::new();

        let fixture = tempfile::tempdir().unwrap();
        let dir = fixture.path().canonicalize().unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        let attacker_config = dir.join(crate::project_config::CONFIG_FILE_NAME);
        std::fs::write(&attacker_config, "[features]\nexperimental = true\n").unwrap();

        std::env::set_current_dir(&dir).expect("chdir into fixture dir");

        let degenerate_ancestors = [std::path::Path::new("."), std::path::Path::new("")];
        let (resolved, declined) = resolve_ancestor_walk(degenerate_ancestors);

        assert_eq!(
            resolved, None,
            "the degenerate current_dir()-failed ancestor sequence must \
             resolve to nowhere trustworthy rather than handing back the \
             attacker's own declined config via the empty ancestor; got \
             {resolved:?}"
        );
        assert_eq!(
            declined.len(),
            2,
            "expected both the \".\" and \"\" ancestors to be declined \
             (the same real attacker-controlled file, reached two \
             different ways); got {declined:?}"
        );
    }

    // Round 2 (auditor M-1r, scenario B / reviewer L-1): when NO ancestor in
    // the (synthetic) sequence is trustworthy at all, the resolver must
    // report that plainly rather than falling back to any of them —
    // including the innermost one, which is exactly what the pre-round-2
    // `unwrap_or(&cwd)` did. `resolve_ancestor_walk` is exercised directly
    // (see its doc) because the real `cwd.ancestors()` walk always reaches
    // the real filesystem root, which this test cannot make untrustworthy.
    #[test]
    #[cfg(unix)]
    fn resolve_ancestor_walk_reports_none_when_nothing_is_trustworthy() {
        use std::os::unix::fs::PermissionsExt;

        let outer = tempfile::tempdir().unwrap();
        let outer_dir = outer.path().canonicalize().unwrap();
        std::fs::set_permissions(&outer_dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        let inner_dir = outer_dir.join("inner");
        std::fs::create_dir_all(&inner_dir).unwrap();
        std::fs::set_permissions(&inner_dir, std::fs::Permissions::from_mode(0o777)).unwrap();

        // No candidate config anywhere in this synthetic chain, so the walk
        // reaches the end with `safe_fallback` still `None` — no ancestor
        // was ever confirmed `Safe`.
        let ancestors = [inner_dir.as_path(), outer_dir.as_path()];
        let (resolved, declined) = resolve_ancestor_walk(ancestors);

        assert_eq!(
            resolved, None,
            "a synthetic ancestor sequence with no Safe directory anywhere \
             in it must resolve to nowhere trustworthy, not to the \
             innermost world-writable one; got {resolved:?}"
        );
        assert!(
            declined.is_empty(),
            "no candidate file existed anywhere in this fixture, so no \
             decline message should have been produced; got {declined:?}"
        );
    }

    // This does NOT reproduce fork issue #310's race (reviewer M1): the
    // pre-#310 code stats first and returns at `is_file()`, so `open` is
    // never reached for a FIFO that already exists when the function is
    // called — that pre-fix code passes this exact test too. #310's real
    // race needs the target to be a regular file at `metadata()` time and a
    // FIFO at `open()` time, a window that isn't deterministically
    // reproducible from a unit test. What this DOES pin, and pins for real,
    // is the post-fix shape's load-bearing half: `open_features_config_file`
    // must keep `O_NONBLOCK` set, or a pre-existing FIFO target hangs
    // `load_features_file` at `open` instead of returning promptly and
    // rejecting it as non-regular. Driven off a spawned thread with a
    // bounded `recv_timeout` on the test thread, so a regression fails
    // loudly with a clear panic message instead of hanging the whole suite
    // waiting on a blocked `open` that never returns.
    #[test]
    #[cfg(unix)]
    fn load_features_file_does_not_hang_on_a_fifo_and_keeps_previous() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".dot-agent-deck.toml");

        let c_path = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        // SAFETY: `c_path` is a valid NUL-terminated string for the
        // lifetime of this call, and 0o600 is a standard owner-only mode —
        // the mkfifo(2) FFI contract is satisfied.
        let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        assert_eq!(rc, 0, "mkfifo failed: {}", std::io::Error::last_os_error());

        let previous = crate::features::Features::test_with(true);
        let (tx, rx) = std::sync::mpsc::channel();
        let path_for_thread = path.clone();
        std::thread::spawn(move || {
            let result = load_features_file(&path_for_thread, previous);
            // The receiver may already be gone if this test timed out and
            // panicked first; a dropped result is fine either way.
            let _ = tx.send(result);
        });

        let result = rx.recv_timeout(Duration::from_secs(5)).expect(
            "load_features_file blocked on a FIFO target instead of \
             returning promptly — O_NONBLOCK was removed from \
             open_features_config_file",
        );
        assert_eq!(
            result, previous,
            "a FIFO target must be rejected as non-regular and keep `previous`"
        );
    }
}
