#![cfg(feature = "e2e")]

//! L2 end-to-end coverage for the `Ctrl+n` (New Pane) global chord on an
//! Orchestration tab with the orchestrator's own pane focused (issue #521).
//!
//! `Ctrl+n` is a global chord — `global_action_for_mode` (`src/ui.rs`) keeps
//! it resolving from every `UiMode`, and its dispatch arm
//! (`Action::NewPane`, `src/ui.rs`) sets `ui.mode = UiMode::DirPicker`
//! unconditionally. On paper that means the same chord that opens the
//! directory picker from the Dashboard (used by every other e2e test in this
//! suite via its own `open_orchestration` helper) should behave identically
//! once an Orchestration tab is already open and its own orchestrator pane is
//! focused. Issue #521 reports that in that one specific state it does
//! nothing at all — no picker, no status message, no forwarded keystroke.
//!
//! `001` pins the freshly-opened-tab shape of the repro (passes against
//! unfixed `main` — the resolution logic really is mode/tab-independent on
//! paper, so that scenario alone does not reproduce the bug). `002` and `003`
//! each isolate one of the two concrete repro angles still open: the wire
//! encoding a real kitty-capable terminal uses (CSI-u vs. the legacy control
//! byte), and reaching the orchestrator's pane via a tab switch rather than a
//! fresh tab open.
//!
//! `004` isolates a THIRD angle found while none of `001`-`003` reproduced:
//! the `run_tui` event-read loop's `non_live_input_feedback` swallow gate
//! (`src/ui.rs`), which intercepts every key/paste in `PaneInput` mode
//! *before* `global_action_for_mode` ever sees it whenever the focused pane's
//! session has declared a non-`Live` `Writable` (the concrete example named
//! in `AppState::pane_writable`'s doc comment is a wrapped Codex pane). The
//! `orch-deck` fixture's `cat` stub roles never declare a `live_target` at
//! all, so `pane_writable` defaults to `Live` for them regardless of tab
//! state or focus history — which is exactly why `001`-`003` could not have
//! exercised this path no matter which angle they varied.
//!
//! Gated behind the `e2e` feature so `cargo test-fast` never compiles it.

mod common;

use std::time::Duration;

use common::TuiDeck;
use dot_agent_deck::event::Writable;
use spec::spec;

#[cfg(unix)]
fn write_executable(path: &std::path::Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::write(path, contents).expect("write history-only self-declaring script");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod history-only self-declaring script");
}

/// Drive the new-pane dialog to open the (single) orchestration in the
/// `orch-deck` fixture. Duplicated per-file by this suite's own convention
/// (see `e2e_orchestration_lock.rs`, `e2e_orchestration_focus.rs`, etc. — each
/// carries its own copy rather than sharing one across `tests/*.rs` binaries).
/// Lands with the orchestrator (start) role focused in `PaneInput` mode.
fn open_orchestration(deck: &TuiDeck) {
    deck.send_keys(b"\x0e"); // Ctrl+n -> directory picker
    deck.send_keys(b" "); // Space -> confirm current dir -> new-pane form
    deck.wait_for_string("No mode"); // form up, Mode field focused at "No mode"
    deck.send_keys(b"\x1b[C"); // Right -> [Orch: …]
    deck.send_keys(b"\r"); // Mode -> Name
    deck.send_keys(b"\r"); // submit (Command hidden for an orchestration)
}

/// Scenario: issue #521. Open a real Orchestration tab (`orch-deck` fixture,
/// two `cat` stub roles) and leave the orchestrator's own pane focused — the
/// default focus right after the tab opens, with no jump away and back.
/// Press `Ctrl+n` and confirm the directory picker (the ` Select Directory `
/// popup `Action::NewPane` opens) appears on the rendered grid, exactly as it
/// does from the Dashboard.
#[spec("orchestration/newpane/001")]
#[test]
fn newpane_001_ctrl_n_opens_picker_with_orchestrator_pane_focused() {
    let deck = TuiDeck::builder()
        .with_env("DOT_AGENT_DECK_EXPERIMENTAL", "1")
        .with_pty_size(120, 40)
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("No active sessions");

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent"); // form closed -> tab up, orchestrator focused
    deck.wait_for_string("[Command Mode Ctrl+D]"); // live PTY, PaneInput mode, orchestrator focused

    // Ctrl+n with the orchestrator's own pane focused on an Orchestration tab.
    deck.send_bytes(b"\x0e");

    assert!(
        deck.wait_for_grid_string_within("Select Directory", Duration::from_secs(3)),
        "Ctrl+n with the orchestrator's own pane focused on an Orchestration \
         tab did not open the directory picker (issue #521) — expected the \
         ` Select Directory ` popup to appear, the same Action::NewPane \
         result Ctrl+n produces from the Dashboard.\nGrid:\n{}",
        deck.snapshot_grid()
    );
}

/// Ctrl+n as a kitty CSI-u keypress: `CSI 110 ; 5 u` (codepoint 110 = `'n'`,
/// modifier param `1 + ctrl(4)` = 5) — what a kitty-capable terminal sends
/// once the enhanced keyboard protocol is active (the deck pushes
/// `DISAMBIGUATE_ESCAPE_CODES` at startup unconditionally; see
/// `tests/e2e_pane_shift_enter.rs`'s `PUSH_ENHANCEMENT`/`key_forwarding_002`
/// for the push itself). Confirmed against the real crossterm decoder by
/// `keyevent_ctrl_c0_matches_crossterm_decoder` (`src/ui.rs`) to decode as
/// `Char('n') + CONTROL` — the identical `(KeyCode, KeyModifiers)` pair the
/// legacy `\x0e` byte decodes to — so `matches_binding` (`src/keybindings.rs`,
/// compares only code + modifiers, never `KeyEventKind`) cannot distinguish
/// the two wire forms.
const CTRL_N_CSI_U: &[u8] = b"\x1b[110;5u";

/// Scenario: issue #521, CSI-u encoding hypothesis. Identical to `newpane_001`
/// except the injected bytes are the kitty-protocol CSI-u encoding of Ctrl+n
/// (`CTRL_N_CSI_U`) rather than the legacy single control byte `\x0e` — the
/// wire form a kitty-capable real terminal sends once the deck's own startup
/// push of the enhanced keyboard protocol is in effect. Confirms or refutes
/// the theory that the encoding, not the resolution logic, is where issue
/// #521 lives.
#[spec("orchestration/newpane/002")]
#[test]
fn newpane_002_ctrl_n_csi_u_encoding_with_orchestrator_pane_focused() {
    let deck = TuiDeck::builder()
        .with_env("DOT_AGENT_DECK_EXPERIMENTAL", "1")
        .with_pty_size(120, 40)
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("No active sessions");

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent"); // form closed -> tab up, orchestrator focused
    deck.wait_for_string("[Command Mode Ctrl+D]"); // live PTY, PaneInput mode, orchestrator focused

    // Ctrl+n, CSI-u encoded, with the orchestrator's own pane focused.
    deck.send_bytes(CTRL_N_CSI_U);

    assert!(
        deck.wait_for_grid_string_within("Select Directory", Duration::from_secs(3)),
        "Ctrl+n sent as the CSI-u kitty encoding ({CTRL_N_CSI_U:?}) with the \
         orchestrator's own pane focused on an Orchestration tab did not open \
         the directory picker (issue #521 CSI-u hypothesis) — expected the \
         ` Select Directory ` popup to appear.\nGrid:\n{}",
        deck.snapshot_grid()
    );
}

/// Ctrl+PageDown / Ctrl+PageUp — the non-configurable global tab-cycling
/// chords (`global_action`, `src/ui.rs`; see
/// `tests/e2e_orchestration_route_isolation.rs`'s identical constants for the
/// established precedent).
const TAB_NEXT: &[u8] = b"\x1b[6;5~";
const TAB_PREV: &[u8] = b"\x1b[5;5~";

/// Scenario: issue #521, tab-switch-back hypothesis. Open the Orchestration
/// tab as `newpane_001` does, then leave it for the Dashboard (`Ctrl+PageUp`)
/// and switch back (`Ctrl+PageDown`) so the orchestrator's own pane is
/// reached by `switch_tab_with_focus`'s restore path rather than by the tab
/// having just been created — the one repro angle from issue #521 explicitly
/// flagged as not yet confirmed. Press `Ctrl+n` and confirm the directory
/// picker opens.
#[spec("orchestration/newpane/003")]
#[test]
fn newpane_003_ctrl_n_after_tab_switch_away_and_back_to_orchestrator_pane() {
    let deck = TuiDeck::builder()
        .with_env("DOT_AGENT_DECK_EXPERIMENTAL", "1")
        .with_pty_size(120, 40)
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("No active sessions");

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent"); // form closed -> tab up, orchestrator focused
    deck.wait_for_string("[Command Mode Ctrl+D]"); // live PTY, PaneInput mode, orchestrator focused

    // Leave for the Dashboard, then return — landing back on the
    // orchestrator's own pane via the tab-switch restore path rather than a
    // fresh tab open. Settled on quiescence (not a specific string) between
    // the two switches: `capture_focus_on_switch_out`/`restore_focus_on_switch_in`
    // (`src/tab.rs`) are no-ops for the Dashboard, so which UiMode the deck
    // lands in while the Dashboard is up is deliberately left unasserted here
    // — the CSI-u sibling test above already covers mode-independence of
    // Ctrl+n's resolution; this test isolates the tab-switch-back angle alone.
    deck.send_bytes(TAB_PREV);
    deck.wait_until_quiescent();
    deck.send_bytes(TAB_NEXT);
    deck.wait_until_quiescent();

    // Ctrl+n with the orchestrator's own pane focused, reached via a tab
    // switch rather than a fresh tab open.
    deck.send_bytes(b"\x0e");

    assert!(
        deck.wait_for_grid_string_within("Select Directory", Duration::from_secs(3)),
        "Ctrl+n with the orchestrator's own pane focused on an Orchestration \
         tab, reached via Ctrl+PageUp/Ctrl+PageDown rather than a fresh tab \
         open, did not open the directory picker (issue #521 tab-switch-back \
         hypothesis).\nGrid:\n{}",
        deck.snapshot_grid()
    );
}

/// Scenario: issue #521, non-live-focused-pane hypothesis. Open an
/// Orchestration tab (`newpane-nonlive` fixture) whose orchestrator role, on
/// startup, declares its OWN session `HistoryOnly` via a synthetic
/// `session_start` hook event carrying `live_target: {kind: process,
/// writable: history-only}` — the same technique
/// `e2e_pane_send_result.rs`'s `pane_input_007` already uses to put an
/// orchestrator role pane in that state, and the same declaration
/// `AppState::pane_writable`'s doc comment names as the concrete real-world
/// example (a wrapped Codex pane). The role then `exec`s `cat` so its PTY
/// stays open and interactive, exactly like `orch-deck`'s stub roles. Press
/// `Ctrl+n` with that pane focused and confirm which of two outcomes
/// happens: the picker opens (this hypothesis does NOT explain issue #521),
/// or the `non_live_input_feedback` swallow gate (`src/ui.rs`, the `run_tui`
/// event-read loop) fires first — no picker, and the "History-only session
/// cannot accept live input" status message appears instead.
#[spec("orchestration/newpane/004")]
#[test]
#[cfg(unix)]
fn newpane_004_ctrl_n_swallowed_when_focused_pane_reports_history_only() {
    let deck = TuiDeck::builder()
        .with_env("DOT_AGENT_DECK_EXPERIMENTAL", "1")
        .with_pty_size(120, 40)
        .launch_with_fixture("newpane-nonlive");
    deck.wait_for_string("No active sessions");

    let script = deck.workdir().join("newpane-nonlive.sh");
    write_executable(
        &script,
        r#"#!/bin/sh
python3 - <<'PY'
import datetime
import json
import os
import socket

payload = {
    "session_id": "newpane-nonlive-session",
    "agent_type": "codex",
    "event_type": "session_start",
    "timestamp": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "pane_id": os.environ["DOT_AGENT_DECK_PANE_ID"],
    "agent_id": os.environ.get("DOT_AGENT_DECK_AGENT_ID"),
    "live_target": {"kind": "process", "writable": "history-only"},
}
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(os.environ["DOT_AGENT_DECK_SOCKET"])
s.sendall((json.dumps(payload) + "\n").encode())
s.close()
PY
exec cat
"#,
    );

    let events = deck.subscribe_events();

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent"); // form closed -> tab up, orchestrator focused
    deck.wait_for_string("[Command Mode Ctrl+D]"); // live PTY, PaneInput mode, orchestrator focused

    // Wait for the daemon to have actually APPLIED the synthetic
    // history-only declaration before pressing Ctrl+n — otherwise the test
    // races the event against the keypress instead of deterministically
    // exercising the swallow gate.
    events.wait_for(
        |event| event.live_target.map(|lt| lt.writable) == Some(Writable::HistoryOnly),
        Duration::from_secs(5),
    );

    // Ctrl+n with the orchestrator's own (now HistoryOnly) pane focused.
    deck.send_bytes(b"\x0e");

    let picker_opened =
        deck.wait_for_grid_string_within("Select Directory", Duration::from_secs(3));
    let feedback_shown = deck.wait_for_grid_string_within(
        "History-only session cannot accept live input",
        Duration::from_secs(2),
    );
    let grid = deck.snapshot_grid();

    assert!(
        !picker_opened,
        "expected the non_live_input_feedback swallow gate to intercept \
         Ctrl+n before global_action_for_mode ever saw it (issue #521 \
         non-live-pane hypothesis), but the directory picker opened anyway \
         — this hypothesis does NOT explain issue #521.\nGrid:\n{grid}"
    );
    assert!(
        feedback_shown,
        "the directory picker did not open, but the \
         non_live_input_feedback status message never appeared either — \
         Ctrl+n was swallowed by something other than the expected gate.\n\
         Grid:\n{grid}"
    );
}
