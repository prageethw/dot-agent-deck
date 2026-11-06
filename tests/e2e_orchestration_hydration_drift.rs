#![cfg(feature = "e2e")]

//! L2 coverage for fork issue #314 / upstream #554: renaming or removing an
//! orchestration from a project's `.dot-agent-deck.toml` (or breaking the file)
//! while one of its tabs is live must surface on screen when the TUI reattaches
//! to the still-running daemon. Before the fix the reconnect-hydration loop in
//! `run_tui` (`src/ui.rs`) silently rebuilt the tab from a synthesised
//! `OrchestrationConfig` and logged only a `tracing::info!` line the user never
//! sees. Upstream's design (`orchestration_config_drift_warning` and
//! `surface_orchestration_config_drift`) surfaces it in three places: the tab's
//! strip label gains a `!` prefix and a `[config drift]` suffix, the status line
//! reads `Config drift: ...`, and the full message is printed when the deck
//! exits. These tests pin the two on-screen ones; the exit text is pinned by
//! `session/restore/020` (`tests/e2e_orchestration_config_drift.rs`), which also
//! covers the `Absent` branch (a legitimate remote reconnect, PRD #111) that
//! must stay silent and is deliberately not exercised here.

mod common;

use std::time::Duration;

use common::{TuiDeck, commit_fixture};
use spec::spec;

/// Leading text of the status line set when a rebuilt orchestration tab no
/// longer matches the local config (`Config drift: an orchestration tab marked
/// ! … [config drift] no longer matches .dot-agent-deck.toml — details are
/// printed on exit`). Only the head is matched, so a narrow terminal that
/// truncates the tail still passes, and it is absent unless drift was detected,
/// so it is a real RED before the fix exists.
const DRIFT_STATUS_NEEDLE: &str = "Config drift:";

/// The suffix the tab strip paints on a drifted orchestration tab. It outlives
/// the status line (which expires), and it must sit on the same strip row as the
/// orchestration's name so the right tab is the one marked.
const DRIFT_TAB_MARKER: &str = "[config drift]";

/// Assert that `deck` shows the drift the way upstream surfaces it: the status
/// line first (it is the transient one, so it is waited for before the
/// persistent marker), then a strip row naming `demo-orch` and carrying the
/// marker. `what` describes the scenario for the panic message.
fn assert_drift_surfaced(deck: &TuiDeck, what: &str) {
    assert!(
        deck.wait_for_grid_string_within(DRIFT_STATUS_NEEDLE, Duration::from_secs(10)),
        "{what} must render the config-drift status line -- expected the \
         substring {DRIFT_STATUS_NEEDLE:?} on the rendered grid within 10s, got:\n{}",
        deck.snapshot_grid()
    );
    deck.wait_until_grid(
        "the rebuilt demo-orch tab carries the [config drift] marker",
        |grid| {
            grid.lines()
                .any(|row| row.contains("demo-orch") && row.contains(DRIFT_TAB_MARKER))
        },
    );
}

/// PRD fork#544 M2b: isolation is now unconditional, so the orchestration
/// opened by the tests below does not run in `deck.workdir()` itself — it
/// runs in a freshly `git clone`d SIBLING directory
/// (`<workdir-basename>-<segment>`, `resolve_workspace_path` in
/// `src/ui.rs`). The daemon's hydration/drift check reads
/// `.dot-agent-deck.toml` from THAT role pane's own cwd, not from
/// `deck.workdir()` — so a test simulating config drift by rewriting the
/// file must rewrite the isolated clone's copy, not the source fixture's.
/// Rather than reimplement the exact sanitize/name-suggestion logic here,
/// find the one new sibling directory `open_orchestration`'s clone created:
/// `deck.workdir()`'s parent holds only per-test tempdirs, each with a
/// high-entropy random suffix, so a `<basename>-` prefix match is safe from
/// collision with a concurrently-running test's own directory.
fn find_isolated_clone_dir(deck: &TuiDeck) -> std::path::PathBuf {
    let workdir = deck.workdir();
    let basename = workdir
        .file_name()
        .expect("workdir has a basename")
        .to_string_lossy()
        .into_owned();
    let parent = workdir.parent().expect("workdir has a parent");
    let prefix = format!("{basename}-");
    std::fs::read_dir(parent)
        .expect("read workdir's parent directory")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .find(|path| {
            path != workdir
                && path
                    .file_name()
                    .map(|n| n.to_string_lossy().starts_with(prefix.as_str()))
                    .unwrap_or(false)
        })
        .unwrap_or_else(|| {
            panic!(
                "no isolated-clone sibling of {workdir:?} found under {parent:?} \
                 (prefix {prefix:?}) -- did the orchestration actually open?"
            )
        })
}

/// Scenario: Open a live Orchestration tab against the `orch-deck` fixture's
/// `demo-orch` orchestration in one TUI client, then rewrite that project's
/// `.dot-agent-deck.toml` on disk to rename the orchestration — the file
/// still parses, it just no longer lists `demo-orch` (config drift, not a
/// missing file). A second, fresh TUI client then attaches to the SAME
/// still-running daemon and, once hydration rebuilds the tab from the daemon's
/// synthesised config, must mark the `demo-orch` tab `[config drift]` and
/// explain it on the status line (`Config drift: ...`).
#[spec("orchestration/hydration/001")]
#[test]
fn hydration_001_renamed_orchestration_warns_on_reattach() {
    let deck = TuiDeck::builder().launch_with_fixture("orch-deck");
    deck.wait_for_string("No active agents");

    // The orchestration opened below needs isolated-clone provisioning,
    // which needs a ref to branch from — an unborn HEAD (the harness's own
    // bare `git init`) does not provide one.
    commit_fixture(deck.workdir());

    // Open the fixture's single orchestration live (mirrors
    // `e2e_orchestration_focus.rs::open_orchestration` / `dashboard_002`):
    // Ctrl+n -> directory picker -> confirm current dir -> new-pane form ->
    // Right selects the (only) orchestration chip -> Enter twice (Mode ->
    // Name -> submit; Command is hidden for an orchestration).
    deck.send_bytes(b"\x0e");
    deck.send_bytes(b" ");
    deck.wait_for_string("No mode");
    deck.send_bytes(b"\x1b[C");
    deck.send_bytes(b"\r");
    deck.send_bytes(b"\r");
    deck.wait_for_absence("New Agent");

    let attach_socket = deck.attach_socket_path().to_string_lossy().into_owned();
    let hook_socket = deck.hook_socket_path().to_string_lossy().into_owned();

    // Rewrite the config on disk while the daemon still has a live pane
    // recorded under the old orchestration name — the drift condition. The
    // file still parses (`cfg.is_some()`), it simply no longer lists
    // `demo-orch`, which is what distinguishes this from the legitimate
    // `cfg.is_none()` remote-reconnect case (PRD #111). This must land in
    // the ISOLATED CLONE the orchestration actually runs in, not
    // `deck.workdir()` — see `find_isolated_clone_dir`'s doc comment.
    let renamed_toml = r#"# Rewritten by orchestration/hydration/001 to rename the orchestration out
# from under the still-live daemon pane -- the drift condition under test.
[[orchestrations]]
name = "demo-orch-renamed"

[[orchestrations.roles]]
name = "orchestrator"
command = "cat"
start = true

[[orchestrations.roles]]
name = "worker"
command = "cat"
"#;
    let orchestration_dir = find_isolated_clone_dir(&deck);
    std::fs::write(orchestration_dir.join(".dot-agent-deck.toml"), renamed_toml)
        .expect("rewrite .dot-agent-deck.toml to rename the orchestration");

    // Reattach a FRESH TUI client to the SAME daemon -- the still-running
    // daemon a detached-and-reconnected user would land on. `deck` itself
    // stays live throughout (mirrors `e2e_card_layout.rs`'s two-client
    // pattern); what matters is that a fresh process's own hydration loop
    // runs against the daemon's current `ListAgents` state, independent of
    // whether another client is also attached.
    let reattached = TuiDeck::builder()
        .with_env("DOT_AGENT_DECK_ATTACH_SOCKET", attach_socket)
        .with_env("DOT_AGENT_DECK_SOCKET", hook_socket)
        .without_success_recording()
        .launch_with_fixture("minimal");

    assert_drift_surfaced(
        &reattached,
        "reattaching to a daemon whose live orchestration pane no longer \
         matches the local .dot-agent-deck.toml (renamed out from under it)",
    );
}

/// Scenario: Open a live Orchestration tab against the `orch-deck` fixture's
/// `demo-orch` orchestration in one TUI client, then CORRUPT that project's
/// `.dot-agent-deck.toml` on disk with a syntax error (an unterminated
/// string) so it no longer parses at all — distinct from `hydration_001`'s
/// rename, which keeps the file valid. The file is not deleted, so the
/// legitimate silent `Absent` / PRD #111 remote-reconnect case stays
/// untouched. A second, fresh TUI client then attaches to the SAME
/// still-running daemon and must mark the `demo-orch` tab `[config drift]` and
/// explain it on the status line, exactly as the rename does: a file that
/// exists but fails to load falls back to the daemon's roles like an absent one,
/// and must not be quiet like one (fork issue #320, upstream #554's `Unreadable`
/// case; its exit-time text, `could not be loaded`, is pinned by
/// `session/restore/020`).
#[spec("orchestration/hydration/002")]
#[test]
fn hydration_002_unparseable_config_warns_on_reattach() {
    let deck = TuiDeck::builder().launch_with_fixture("orch-deck");
    deck.wait_for_string("No active agents");

    // The orchestration opened below needs isolated-clone provisioning,
    // which needs a ref to branch from — an unborn HEAD (the harness's own
    // bare `git init`) does not provide one.
    commit_fixture(deck.workdir());

    // Open the fixture's single orchestration live -- identical setup to
    // `hydration_001`.
    deck.send_bytes(b"\x0e");
    deck.send_bytes(b" ");
    deck.wait_for_string("No mode");
    deck.send_bytes(b"\x1b[C");
    deck.send_bytes(b"\r");
    deck.send_bytes(b"\r");
    deck.wait_for_absence("New Agent");

    let attach_socket = deck.attach_socket_path().to_string_lossy().into_owned();
    let hook_socket = deck.hook_socket_path().to_string_lossy().into_owned();

    // Corrupt the fixture's config on disk while the daemon still has a live
    // pane recorded under `demo-orch` -- an unterminated string makes this
    // invalid TOML, so `toml::from_str` fails and `load_project_config`
    // returns `Err(ProjectConfigError::Parse)`. This is NOT the `Absent`
    // case (the file still exists) and NOT `hydration_001`'s drift case (the
    // file doesn't parse at all, so there's no `orchestrations` list to miss
    // from).
    let corrupted_toml = r#"# Rewritten by orchestration/hydration/002 to corrupt the file's syntax
# out from under the still-live daemon pane -- the Unparseable condition
# under test. The unterminated string below is what breaks the parse.
[[orchestrations]]
name = "demo-orch

[[orchestrations.roles]]
name = "orchestrator"
command = "cat"
start = true
"#;
    // Must land in the ISOLATED CLONE the orchestration actually runs in,
    // not `deck.workdir()` — see `find_isolated_clone_dir`'s doc comment.
    let orchestration_dir = find_isolated_clone_dir(&deck);
    std::fs::write(
        orchestration_dir.join(".dot-agent-deck.toml"),
        corrupted_toml,
    )
    .expect("corrupt .dot-agent-deck.toml with a syntax error");

    // Reattach a FRESH TUI client to the SAME daemon, exactly as
    // `hydration_001` does.
    let reattached = TuiDeck::builder()
        .with_env("DOT_AGENT_DECK_ATTACH_SOCKET", attach_socket)
        .with_env("DOT_AGENT_DECK_SOCKET", hook_socket)
        .without_success_recording()
        .launch_with_fixture("minimal");

    assert_drift_surfaced(
        &reattached,
        "reattaching to a daemon whose live orchestration pane's local \
         .dot-agent-deck.toml has been corrupted into invalid TOML (still \
         present, just unparseable)",
    );
}
