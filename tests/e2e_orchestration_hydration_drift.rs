#![cfg(feature = "e2e")]

//! L2 coverage for fork issue #314 / upstream #554: renaming or removing an
//! orchestration from a project's `.dot-agent-deck.toml` while one of its
//! tabs is live must surface on screen when the TUI reattaches to the
//! still-running daemon. Today the reconnect-hydration loop in `run_tui`
//! (`src/ui.rs`, the `else` arm beside the string `config drift or stale`)
//! silently rebuilds the tab from a synthesised `OrchestrationConfig` and
//! logs only a `tracing::info!` line the user never sees — unlike the
//! snapshot-restore path in the same file, which warns and names the
//! orchestration for the identical drift class. The sibling `cfg.is_none()`
//! branch (a legitimate remote reconnect, PRD #111) must stay silent and is
//! deliberately not exercised here.

mod common;

use std::time::Duration;

use common::TuiDeck;
use spec::spec;

/// Substring the drift warning must carry once the fix lands: names the
/// `orch-deck` fixture's orchestration and states it is no longer found in
/// the local config. Deliberately more than the bare orchestration name —
/// the tab title already renders `demo-orch` on screen today (the
/// synthesised config keeps the daemon-known name), so a needle of just
/// `"demo-orch"` would pass before the fix exists and not be a real RED.
const DRIFT_WARNING_NEEDLE: &str = "orchestration 'demo-orch' not found in local config";

/// Substring the fix for fork issue #320 must carry: names the local config
/// file and states it failed to parse — the `LocalConfigState::Unparseable`
/// branch, distinct from both `Absent` (silent, PRD #111) and the
/// `DRIFT_WARNING_NEEDLE` "found == false" case above. Deliberately includes
/// the filename so a needle of just "parse" or "invalid" could not pass by
/// coincidentally matching some unrelated error string.
const PARSE_WARNING_NEEDLE: &str = "local .dot-agent-deck.toml could not be parsed";

/// Scenario: Open a live Orchestration tab against the `orch-deck` fixture's
/// `demo-orch` orchestration in one TUI client, then rewrite that project's
/// `.dot-agent-deck.toml` on disk to rename the orchestration — the file
/// still parses, it just no longer lists `demo-orch` (config drift, not a
/// missing file). A second, fresh TUI client then attaches to the SAME
/// still-running daemon and must render an in-session status-line warning
/// naming `demo-orch` once hydration rebuilds the tab from the daemon's
/// synthesised config.
#[spec("orchestration/hydration/001")]
#[test]
fn hydration_001_renamed_orchestration_warns_on_reattach() {
    let deck = TuiDeck::builder().launch_with_fixture("orch-deck");
    deck.wait_for_string("No active sessions");

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

    // Rewrite the fixture's config on disk while the daemon still has a
    // live pane recorded under the old orchestration name — the drift
    // condition. The file still parses (`cfg.is_some()`), it simply no
    // longer lists `demo-orch`, which is what distinguishes this from the
    // legitimate `cfg.is_none()` remote-reconnect case (PRD #111).
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
    std::fs::write(deck.workdir().join(".dot-agent-deck.toml"), renamed_toml)
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

    assert!(
        reattached.wait_for_grid_string_within(DRIFT_WARNING_NEEDLE, Duration::from_secs(10)),
        "reattaching to a daemon whose live orchestration pane no longer \
         matches the local .dot-agent-deck.toml (renamed out from under it) \
         must render an on-screen drift warning naming the orchestration -- \
         expected the substring {DRIFT_WARNING_NEEDLE:?} on the rendered \
         grid within 10s, got:\n{}",
        reattached.snapshot_grid()
    );
}

/// Scenario: Open a live Orchestration tab against the `orch-deck` fixture's
/// `demo-orch` orchestration in one TUI client, then CORRUPT that project's
/// `.dot-agent-deck.toml` on disk with a syntax error (an unterminated
/// string) so it no longer parses at all — distinct from `hydration_001`'s
/// rename, which keeps the file valid. The file is not deleted, so the
/// legitimate silent `Absent` / PRD #111 remote-reconnect case stays
/// untouched. A second, fresh TUI client then attaches to the SAME
/// still-running daemon and must render an in-session status-line warning
/// naming the local config file as unparseable once hydration's
/// `lookup_config` closure runs against it (fork issue #320: today it maps
/// `Err(ProjectConfigError::Parse)` to `None`, taking the branch reserved for
/// an absent file, so the broken edit is currently silent).
#[spec("orchestration/hydration/002")]
#[test]
fn hydration_002_unparseable_config_warns_on_reattach() {
    let deck = TuiDeck::builder().launch_with_fixture("orch-deck");
    deck.wait_for_string("No active sessions");

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
    std::fs::write(deck.workdir().join(".dot-agent-deck.toml"), corrupted_toml)
        .expect("corrupt .dot-agent-deck.toml with a syntax error");

    // Reattach a FRESH TUI client to the SAME daemon, exactly as
    // `hydration_001` does.
    let reattached = TuiDeck::builder()
        .with_env("DOT_AGENT_DECK_ATTACH_SOCKET", attach_socket)
        .with_env("DOT_AGENT_DECK_SOCKET", hook_socket)
        .without_success_recording()
        .launch_with_fixture("minimal");

    assert!(
        reattached.wait_for_grid_string_within(PARSE_WARNING_NEEDLE, Duration::from_secs(10)),
        "reattaching to a daemon whose live orchestration pane's local \
         .dot-agent-deck.toml has been corrupted into invalid TOML (still \
         present, just unparseable) must render an on-screen warning naming \
         the local config file -- expected the substring \
         {PARSE_WARNING_NEEDLE:?} on the rendered grid within 10s, got:\n{}",
        reattached.snapshot_grid()
    );
}
