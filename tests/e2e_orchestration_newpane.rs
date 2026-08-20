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
//! Gated behind the `e2e` feature so `cargo test-fast` never compiles it.

mod common;

use std::time::Duration;

use common::TuiDeck;
use spec::spec;

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
