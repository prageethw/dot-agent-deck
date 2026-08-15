//! Experimental feature flag (PRD #139).
//!
//! A single boolean — `experimental` — gates **only user-visible surfaces**
//! introduced by in-flight work. Off by default. Opt-in via the
//! `[features]` table in the project `.dot-agent-deck.toml` or the
//! `DOT_AGENT_DECK_EXPERIMENTAL` env var (env wins, OQ3). Both the TUI and
//! the daemon call [`init_and_watch`] and each resolves the flag from the
//! same on-disk source, but as of fork issue #303's Phase 1 investigation
//! this is NOT a live cross-process guarantee: every consumer
//! (`show_experimental_footer`, `show_issue_dispatch_authoring`) lives in
//! `src/ui.rs`, so the daemon's call
//! installs a value into the daemon process's own `SHARED` that nothing
//! ever reads. Treat "both processes resolve the same algorithm" as true
//! and "the two processes therefore agree on anything" as not yet
//! meaningful — it would need a real daemon-side (or pane-scoped) consumer
//! to become one. If such a consumer is ever added, this premise breaks and
//! per-surface resolution needs reconsidering (see the PRD/issue #303
//! findings for the fuller argument).
//!
//! ## Gating convention (CLAUDE.md #9 / PRD #139 M3.2)
//!
//! Each gated surface declares ONE wrapper function here — e.g.
//! [`show_experimental_footer`] — and every call site reads
//! `if features::show_<name>() { … }`. The flag is a *presentation* switch:
//! gate at the user-visible seam (render / input-binding) only, never branch
//! business logic on it. When a feature graduates, `grep show_<name>` finds
//! every site for a mechanical removal.

use std::sync::{Arc, Once, OnceLock, RwLock};

use serde::Deserialize;

/// The `[features]` table from `.dot-agent-deck.toml`. A single boolean,
/// off by default. `Copy` is required by the reload test, which snapshots
/// the shared value with `*shared.read().unwrap()`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Features {
    /// Master gate for in-flight experimental surfaces. Off by default.
    pub experimental: bool,
}

impl Features {
    /// Per-test constructor (PRD #139 M4.2): build a `Features` with the
    /// experimental flag forced to `experimental`.
    pub fn test_with(experimental: bool) -> Self {
        Self { experimental }
    }
}

/// The process-global shared `Features` (OQ2: a single
/// `Arc<RwLock<Features>>` per process). Lazily initialized to the default
/// (experimental = OFF) so a read before startup wiring is well-defined.
static SHARED: OnceLock<Arc<RwLock<Features>>> = OnceLock::new();

fn shared_cell() -> &'static Arc<RwLock<Features>> {
    SHARED.get_or_init(|| Arc::new(RwLock::new(Features::default())))
}

/// A clone of the process-global shared handle (OQ2). The watcher thread
/// holds one of these to read the current value; reloaded values are applied
/// through [`install`] so there is a single write path.
pub fn shared() -> Arc<RwLock<Features>> {
    shared_cell().clone()
}

/// Snapshot of the current process-global `Features`.
pub fn current() -> Features {
    *shared_cell().read().unwrap_or_else(|e| e.into_inner())
}

/// The single apply path used by BOTH [`set_for_test`] (tests / synthetic
/// reload events) and the real file watcher ([`init_and_watch`]), so
/// production and test reloads exercise the exact same code (PRD #139 M2.1).
fn install(features: Features) {
    *shared_cell().write().unwrap_or_else(|e| e.into_inner()) = features;
}

/// Test / reload injection seam (PRD #139 M4.2): force the process-global
/// shared `Features` to `features`. The real watcher updates via this same
/// apply path ([`install`]).
pub fn set_for_test(features: Features) {
    install(features);
}

/// Read the master experimental flag from the process-global shared value.
pub fn experimental_enabled() -> bool {
    current().experimental
}

/// Production wrapper for the throwaway gated dashboard footer (PRD #139
/// M4.1). One wrapper per feature (CLAUDE.md #9): gate at the user-visible
/// seam, read the shared flag here, and keep `experimental_enabled()` checks
/// out of implementation code.
pub fn show_experimental_footer() -> bool {
    experimental_enabled()
}

/// Production wrapper for the scheduled GitHub issue-dispatch CREATION UX
/// (PRD #120, flag redesign 2026-06-24). One wrapper per feature (CLAUDE.md #9)
/// so `grep show_issue_dispatch_authoring` finds the single gate at graduation.
/// This is a *presentation* switch (rule-9-proper): it gates ONLY the new-pane
/// Mode-cycler `schedule: issues` authoring option (a render/input seam in
/// `src/ui.rs`). It does NOT gate the dispatch behavior — a configured
/// `issue_dispatch` task runs unconditionally — nor config parsing nor the
/// `schedule add --repo` CLI; those are flag-free.
pub fn show_issue_dispatch_authoring() -> bool {
    experimental_enabled()
}

/// Guards [`init_and_watch`] so the periodic watcher thread is spawned at
/// most once per process (reviewer #4 / audit INFO-3): a second call is a
/// no-op rather than leaking a duplicate poll thread.
static INIT: Once = Once::new();

/// Build the diagnosability message for fork issue #303: when the ancestor
/// walk in `features_config_path()` found no `.dot-agent-deck.toml` anywhere
/// above `project_dir`, the flag silently defaults to OFF with no other
/// signal that anything went looking. Only diagnose this when no explicit
/// override is in play — an operator-supplied `DOT_AGENT_DECK_FEATURES_CONFIG`
/// that points at a missing file is a different problem. Returns `None` when
/// a config was found (or an override is set), so callers can stay silent in
/// the common case.
fn missing_config_warning(path: &std::path::Path, project_dir: &std::path::Path) -> Option<String> {
    if std::env::var("DOT_AGENT_DECK_FEATURES_CONFIG").is_ok() || path.is_file() {
        return None;
    }
    Some(format!(
        "no {} found in {} or any ancestor directory; experimental flags default to OFF",
        crate::project_config::CONFIG_FILE_NAME,
        // SECURITY (issue #303 M1): `project_dir` is attacker-influenceable —
        // any directory the process was launched from — and this warning is
        // printed raw to the terminal ahead of the alt-screen switch
        // (`main.rs::run_tui_session`). Sanitize it the same way
        // `worktree_reclaim.rs`/`daemon_client.rs`/`keybindings.rs` sanitize
        // other operator-facing paths, or an embedded CR+ESC[2K can erase
        // this very warning and forge a reassuring line in its place.
        crate::terminal_sanitize::sanitize_path_for_terminal_display(project_dir)
    ))
}

/// Initialize the process-global `Features` from the `[features]` table of
/// the `.dot-agent-deck.toml` in `project_dir` (env override wins), log the
/// startup state and the file it came from (PRD #139 M1.3 / OQ4), and spawn
/// the periodic re-read watcher (PRD #139 M2.1). Called once at startup by
/// BOTH the TUI and the daemon so each process evaluates the flag from the
/// same file — see the module doc for why that is not the same thing as the
/// two processes agreeing on anything today. Idempotent: the body runs at
/// most once per process, so a second call cannot spawn a duplicate watcher
/// thread.
///
/// `project_dir` is passed in rather than derived here (issue #577): the
/// caller decides which project the flag belongs to, exactly as every other
/// config read takes the directory it was handed
/// ([`crate::project_config::load_project_config`]).
///
/// Returns the fork issue #303 diagnosability message (see
/// [`missing_config_warning`]) the FIRST time this is called, so a caller
/// with a user-visible seam (the TUI's startup path, before it enters the
/// alternate screen) can surface it without needing `DOT_AGENT_DECK_LOG` or a
/// restart. Returns `None` on a config found (or an override set), and also
/// on any call after the first — `init_and_watch` is only ever called once
/// per process in production, so this is not a real limitation.
pub fn init_and_watch(project_dir: &std::path::Path) -> Option<String> {
    let mut warning = None;
    INIT.call_once(|| {
        let path = crate::config::features_config_path(project_dir);
        let resolved = crate::config::resolve_features(crate::config::load_features_file(
            &path,
            Features::default(),
        ));
        install(resolved);
        // Name the file the value came from (issue #577): when the flag
        // resolves OFF because the deck is reading a `.dot-agent-deck.toml`
        // that isn't the one the operator edited — or one that does not exist
        // — the symptom is otherwise indistinguishable from the gated feature
        // having been removed, with nothing in the log to tell them apart.
        tracing::info!(
            "experimental flag: {} (from {})",
            if resolved.experimental { "ON" } else { "OFF" },
            path.display()
        );
        if let Some(message) = missing_config_warning(&path, project_dir) {
            tracing::warn!("{message}");
            warning = Some(message);
        }
        spawn_watcher(path);
    });
    warning
}

/// Periodic re-read watcher (OQ1). The deck has no existing config-reload
/// file watcher and `notify` is not a dependency, so — per the PRD's
/// "no new third-party crate dependencies" success criterion — this is a
/// ~2s polling fallback rather than an event-driven watcher. On each tick it
/// re-resolves the flag (file value, then env override wins) and, on a
/// change, applies it through the same [`install`] path `set_for_test` uses,
/// with no process restart. Partial-write tolerance: the loader keeps the
/// previous value on a parse error, and a ~200ms re-read settles a save
/// caught mid-write before applying.
fn spawn_watcher(path: std::path::PathBuf) {
    let handle = shared();
    // Greptile P2: surface a spawn failure instead of silently discarding it.
    // If the OS refuses the thread (e.g. thread-limit), live config reload is
    // disabled for the process lifetime; without this log the operator would
    // have no signal that the documented ~2s reload simply isn't running. The
    // loop below is infinite and poison-safe (lock errors are recovered via
    // `into_inner`), so a started thread never exits or panics on its own —
    // the spawn call is the only failure point worth reporting.
    let spawn_result = std::thread::Builder::new()
        .name("dad-features-watch".to_string())
        .spawn(move || {
            let poll = std::time::Duration::from_secs(2);
            let debounce = std::time::Duration::from_millis(200);
            let mut last = *handle.read().unwrap_or_else(|e| e.into_inner());
            loop {
                std::thread::sleep(poll);
                let candidate =
                    crate::config::resolve_features(crate::config::load_features_file(&path, last));
                if candidate == last {
                    continue;
                }
                // A config save can be a multi-write (truncate then write), so
                // the first read may catch a half-written file. Wait ~200ms and
                // re-read; apply the settled value. The loader already keeps
                // `last` on a parse error, so a mid-write read never clobbers
                // the live flag.
                std::thread::sleep(debounce);
                let settled =
                    crate::config::resolve_features(crate::config::load_features_file(&path, last));
                if settled != last {
                    // Reviewer #1: apply through the single write path so the
                    // "single apply path" contract on `install`/`set_for_test`
                    // is literally true (the watcher no longer writes the lock
                    // directly).
                    install(settled);
                    tracing::info!(
                        "experimental flag: {} (reloaded)",
                        if settled.experimental { "ON" } else { "OFF" }
                    );
                    last = settled;
                }
            }
        });
    if let Err(err) = spawn_result {
        tracing::warn!(
            "failed to spawn experimental-flag watcher thread: {err}; \
             live config reload is disabled for this process"
        );
    }
}
