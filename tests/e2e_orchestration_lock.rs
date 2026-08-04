#![cfg(feature = "e2e")]

//! L2 end-to-end coverage for PRD #361 Item 3: the command-entry lock on
//! Orchestration tabs. By default, a keystroke typed while a non-
//! orchestrator role pane is focused must not reach that pane's PTY; the
//! orchestrator pane's own input is never gated; `Ctrl+e` toggles the lock;
//! and the small set of always-available `Ctrl+`-chords (resolved before the
//! PTY-forward fallback the lock gates) keep working regardless of lock
//! state or which pane is focused.
//!
//! Uses the `orch-deck` fixture (two stub `cat` roles, no LLM tokens spent)
//! shared with the PRD #336/#361 Item 4 pane-column tests.
//!
//! Decision 6: gated behind the `e2e` feature so `cargo test-fast` never
//! compiles it.

mod common;

use std::time::Duration;

use common::TuiDeck;
use spec::spec;

/// Drive the new-pane dialog to open the (single) orchestration in the
/// `orch-deck` fixture. Mirrors `e2e_orchestration_pane_column.rs::open_orchestration`
/// — with no `[[modes]]` defined the Mode chip row is `[No mode] [Orch: …]
/// [schedule]`, so ONE Right selects the orchestration; selecting an
/// orchestration hides the Command field, so a second Enter submits the
/// form. Lands with the orchestrator (start) role focused in `PaneInput`
/// mode.
fn open_orchestration(deck: &TuiDeck) {
    deck.send_keys(b"\x0e"); // Ctrl+n -> directory picker
    deck.send_keys(b" "); // Space -> confirm current dir -> new-pane form
    deck.wait_for_string("No mode"); // form up, Mode field focused at "No mode"
    deck.send_keys(b"\x1b[C"); // Right -> [Orch: demo-orch]
    deck.send_keys(b"\r"); // Mode -> Name
    deck.send_keys(b"\r"); // submit (Command hidden for an orchestration)
}

/// Switch focus from the orchestrator role to the `orch-deck` fixture's
/// second role ("worker", `role_pane_ids` index 1): Ctrl+D back to Normal
/// mode, then digit `2` (`Jump2` -> `Action::FocusCard(1)`) — the same
/// mechanism `focus/orchestration/001` pins for "1-9 on an orchestration tab
/// jumps to role pane N and focuses it". `focus_deck` re-enters `PaneInput`
/// mode on success, so no separate Enter is needed.
fn focus_worker_role(deck: &TuiDeck) {
    deck.send_bytes(b"\x04"); // Ctrl+D -> Normal mode
    deck.send_keys(b"2"); // Jump2 -> focus role index 1 ("worker")
}

/// Scenario: Open a real orchestration tab (default LOCKED). First confirm
/// the orchestrator pane's own input is never gated: type a sentinel into it
/// and see it echoed by its `cat` process. Then focus the non-orchestrator
/// "worker" role and, while still locked, type a second sentinel — it must
/// NOT reach the worker's PTY (no terminal echo, since `write_raw_bytes` is
/// never called for a dropped keystroke), so it never appears on the
/// rendered grid. Finally send `Ctrl+e` to unlock and type a third sentinel
/// into the still-focused worker pane — it must now forward and appear
/// (echoed by that pane's own `cat`). RED today: there is no lock at all, so
/// the "locked" sentinel reaches the worker's PTY same as the others and the
/// negative assertion fails (this test also can't compile today as part of
/// the crate, since `src/ui.rs`'s new `orchestration/lock/00[1-3]` tests
/// reference the not-yet-implemented lock field/action).
#[spec("orchestration/lock/004")]
#[test]
fn orchestration_lock_004_forwarding_gated_by_lock_state() {
    const ORCH_SENTINEL: &str = "LOCK004_ORCH_9f21";
    const WORKER_LOCKED_SENTINEL: &str = "LOCK004_WORKER_LOCKED_7ac4";
    const WORKER_UNLOCKED_SENTINEL: &str = "LOCK004_WORKER_UNLOCKED_c83e";

    let deck = TuiDeck::builder()
        .with_pty_size(120, 40)
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("No active sessions");

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent"); // new-pane form closed -> tab up, orchestrator focused

    // The orchestrator pane is NEVER gated: even though the tab starts
    // LOCKED by default, typing into the currently-focused orchestrator
    // role must still reach its PTY.
    deck.send_keys(format!("{ORCH_SENTINEL}\r").as_bytes());
    deck.wait_for_string(ORCH_SENTINEL);

    // Focus the non-orchestrator "worker" role. The tab is still locked —
    // nothing has toggled it yet.
    focus_worker_role(&deck);

    // Locked: a keystroke aimed at the worker pane must NOT reach its PTY.
    deck.send_keys(format!("{WORKER_LOCKED_SENTINEL}\r").as_bytes());
    let leaked = deck.wait_for_grid_string_within(WORKER_LOCKED_SENTINEL, Duration::from_secs(2));
    assert!(
        !leaked,
        "a keystroke typed into the non-orchestrator worker pane reached \
         its PTY while the orchestration tab's command-entry lock was \
         engaged (the default state) — expected it to be dropped before \
         Action::ForwardToPane.\nGrid:\n{}",
        deck.snapshot_grid()
    );

    // Ctrl+e unlocks the tab.
    deck.send_bytes(b"\x05"); // Ctrl+e == 0x05

    // Unlocked: typing into the still-focused worker pane must now forward
    // normally.
    deck.send_keys(format!("{WORKER_UNLOCKED_SENTINEL}\r").as_bytes());
    deck.wait_for_string(WORKER_UNLOCKED_SENTINEL);
}

/// Scenario: Open a real orchestration tab, focus the non-orchestrator
/// "worker" role (leaving the tab in its default LOCKED state), then press
/// `Ctrl+t` (`toggle_layout`) — one of the small always-available global
/// chords `global_action_for_mode` resolves BEFORE the PTY-forward fallback
/// the lock gates (per the PRD's investigation, item 3's gate sits entirely
/// inside that fallback). The toggle must fire identically regardless of the
/// lock or which pane is focused, surfacing its `Layout: …` status message
/// (the same observable `e2e_keybindings.rs::remap_001` uses). Regression
/// guard mirroring `tabs/orchestration/007`'s role for Item 4 — this chord
/// already works today with no lock gate present, so it is not itself a
/// runtime-RED assertion; its RED status today comes from the same crate-
/// wide compile failure as `orchestration/lock/001`-`004` (the lock field/
/// action referenced by the other new tests don't exist yet). Its value is
/// guarding against an overly-broad gate implementation once the lock ships.
#[spec("orchestration/lock/005")]
#[test]
fn orchestration_lock_005_global_chord_unaffected_by_lock_state() {
    let deck = TuiDeck::builder()
        .with_pty_size(120, 40)
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("No active sessions");

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent");

    // Focus the non-orchestrator worker role — still LOCKED, the tab's
    // default state.
    focus_worker_role(&deck);

    deck.send_bytes(b"\x14"); // Ctrl+t (toggle_layout)
    deck.wait_for_string("Layout:");
}
