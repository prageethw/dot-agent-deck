#![cfg(feature = "e2e")]

//! L2 end-to-end coverage for PRD #373 M2: the 30-second inactivity
//! snap-back on Orchestration tabs. Closes the CLAUDE.md rule 4 gap —
//! #373 shipped M1/M2 with only L1/unit coverage
//! (`tabs/orchestration/012`-`018` in `src/tab.rs`/`src/ui.rs`) and zero
//! PTY-attached e2e coverage.
//!
//! `orchestration_focus_001` reuses the `orch-deck` fixture (two stub
//! `cat` roles, no LLM tokens spent) `orchestration_lock_004` already
//! established. It observes "which pane is focused" purely on the
//! RENDERED GRID, the same surface a user sees: the currently-focused
//! role's pane box top border fuses its title in as `┌<role>` (Plain,
//! PaneInput mode — see `e2e_dashboard_pane_column.rs::pane_box_left_edge`
//! for the precedent), while every other role collapses to a small
//! numbered card with no live PTY content at all
//! (`orchestration/layout/004`). So a keystroke can only ever echo inside
//! the ONE expanded box on screen, and that box's header names which role
//! it belongs to — no daemon-side per-pane scrollback lookup is needed
//! (an earlier version of this test tried `common::wait_for_pane_text_on`
//! against each role's registered agent id and found it always empty for
//! these plain `cat` stubs; that path is for daemon/stream-backed
//! real-agent panes, not these).
//!
//! The production interval is `TabManager::INACTIVITY_TIMEOUT`
//! (`src/tab.rs`), hardcoded to 30 real seconds at its sole per-frame call
//! site (`src/ui.rs`, the `auto_focus_after_inactivity` branch of the
//! auto-focus chain) with no test-only override today. This test sets
//! `DOT_AGENT_DECK_INACTIVITY_TIMEOUT_SECS=2` on the spawned binary,
//! mirroring the existing `DOT_AGENT_DECK_IDLE_SHUTDOWN_SECS` seam
//! (`src/agent_pty.rs`, `src/daemon.rs`) — but nothing in production reads
//! that variable yet, so this is expected to be RED for that reason (in
//! addition to pinning the behavior itself).
//!
//! Decision 6: gated behind the `e2e` feature so `cargo test-fast` never
//! compiles it.

mod common;

use std::time::Duration;

use common::TuiDeck;
use spec::spec;

/// Test-only override for `TabManager::INACTIVITY_TIMEOUT` this test
/// expects the production per-frame call site to read (see module doc).
/// Kept short so the test suite stays fast once the seam exists.
const SHORT_INACTIVITY_TIMEOUT_SECS: &str = "2";
const SHORT_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(2);

/// Drive the new-pane dialog to open the (single) orchestration in the
/// `orch-deck` fixture. Mirrors `e2e_orchestration_lock.rs::open_orchestration`
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
/// mechanism `e2e_orchestration_lock.rs::focus_worker_role` uses.
/// `focus_deck` re-enters `PaneInput` mode on success, so no separate
/// Enter is needed. Reusable across both role-jumps this test needs
/// (initial focus, and the re-focus at the start of the control half).
fn focus_worker_role(deck: &TuiDeck) {
    deck.send_bytes(b"\x04"); // Ctrl+D -> Normal mode
    deck.send_keys(b"2"); // Jump2 -> focus role index 1 ("worker")
}

/// The rendered grid's expanded-pane top border fuses the pane's title
/// directly into the box-drawing corner as `┌<role>` in `PaneInput` mode
/// (`TerminalWidget`, `src/terminal_widget.rs`; the precedent needle is
/// `e2e_dashboard_pane_column.rs::pane_box_left_edge`'s `plain_needle`).
/// Only the currently-focused role ever renders this way — every other
/// role collapses to a small numbered card with no live PTY body
/// (`orchestration/layout/004`) — so this string's presence names which
/// role currently holds focus with live content on screen.
fn expanded_header(role: &str) -> String {
    format!("┌{role}")
}

/// Scenario: Open a real orchestration tab, unlock it (so this test is
/// isolated from PRD #361/#374's separate lock behavior), and focus the
/// non-orchestrator "worker" role. With ZERO further input, once the
/// (shortened) inactivity interval elapses, confirm a subsequent keystroke
/// sent with NO manual refocus shows up inside the ORCHESTRATOR's own
/// expanded pane box on the rendered grid rather than the worker's — i.e.
/// focus visibly snapped back on its own. Then, as the control half,
/// re-focus the worker role and keep sending it small keystrokes
/// throughout an interval LONGER than the configured timeout; confirm a
/// final keystroke still lands in the worker's own expanded box, proving
/// ongoing activity suppresses the snap-back. RED today: there is no
/// test-only override for the production 30-second interval, so the real
/// interval never elapses within this test's short waits.
#[spec("orchestration/focus/001")]
#[test]
fn orchestration_focus_001_inactivity_snap_back_visible_on_real_binary() {
    const WORKER_INITIAL: &str = "FOCUS001_WORKER_INITIAL_3d18";
    const ORCH_AFTER_IDLE: &str = "FOCUS001_ORCH_IDLE_9b62";
    const WORKER_REFOCUS: &str = "FOCUS001_WORKER_REFOCUS_4e07";
    const WORKER_FINAL: &str = "FOCUS001_WORKER_FINAL_c581";

    let deck = TuiDeck::builder()
        .with_pty_size(120, 40)
        .with_env(
            "DOT_AGENT_DECK_INACTIVITY_TIMEOUT_SECS",
            SHORT_INACTIVITY_TIMEOUT_SECS,
        )
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("No active sessions");

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent"); // new-pane form closed -> tab up, orchestrator focused

    // Settle point mirroring `e2e_orchestration_lock.rs::orchestration_lock_004`:
    // confirm the orchestrator pane (still focused, never gated) is fully up
    // and echoing before doing anything else. Without this, the worker
    // role's `cat` process may not have finished spawning yet (process
    // spawn is ~380ms on this machine) when the focus switch + first
    // keystroke below race ahead of it.
    const ORCH_SETTLE: &str = "FOCUS001_ORCH_SETTLE_1a90";
    deck.send_keys(format!("{ORCH_SETTLE}\r").as_bytes());
    deck.wait_for_string(ORCH_SETTLE);

    // Unlock the tab (PRD #361/#374's command-entry lock is out of scope
    // here — already covered by `orchestration/lock/004`-`006`) so every
    // keystroke below forwards to whichever pane is focused, regardless of
    // lock state.
    deck.send_bytes(b"\x05"); // Ctrl+e == 0x05

    let orch_expanded = expanded_header("orchestrator");
    let worker_expanded = expanded_header("worker");

    // --- Part 1: manually focus the worker role, then send NOTHING for
    // the configured inactivity interval -> expect an automatic snap-back.
    focus_worker_role(&deck);

    // Confirm the forwarding target really is the worker before relying
    // on the timer: the worker's box must be the expanded one, and the
    // sentinel must land inside it.
    deck.send_keys(format!("{WORKER_INITIAL}\r").as_bytes());
    let initial_ok = deck.wait_for_grid_predicate_within(Duration::from_secs(5), |grid| {
        grid.contains(&worker_expanded) && grid.contains(WORKER_INITIAL)
    });
    assert!(
        initial_ok,
        "a sentinel typed right after focusing the worker role never showed \
         up inside the worker's own expanded pane box (needle {worker_expanded:?}) \
         — cannot proceed with the inactivity assertion below without \
         confirming the initial focus target first.\n\
         === rendered grid ===\n{}",
        deck.snapshot_grid(),
    );

    // Let the (shortened) inactivity interval elapse with ZERO further
    // input at all. `wait_for_grid_predicate_within`'s internal poll loop
    // (common/mod.rs) is the sanctioned wait primitive (Decision 21 bans a
    // raw `std::thread::sleep` in an `e2e_*.rs` body) — the predicate
    // never matches, so this simply blocks for the full duration.
    deck.wait_for_grid_predicate_within(SHORT_INACTIVITY_TIMEOUT * 2, |_| false);

    // No manual refocus here on purpose: if the snap-back fired on its
    // own, this keystroke lands in the orchestrator's expanded box without
    // the test doing anything to move focus itself.
    deck.send_keys(format!("{ORCH_AFTER_IDLE}\r").as_bytes());
    let snapped_back = deck.wait_for_grid_predicate_within(Duration::from_secs(5), |grid| {
        grid.contains(&orch_expanded) && grid.contains(ORCH_AFTER_IDLE)
    });
    assert!(
        snapped_back,
        "after {:?} with no activity on a non-orchestrator role pane in an \
         Orchestration tab, a subsequent keystroke never showed up inside \
         the ORCHESTRATOR's own expanded pane box (needle {orch_expanded:?}) \
         — expected `TabManager::auto_focus_after_inactivity` to have \
         snapped focus back to the orchestrator role once the (shortened) \
         inactivity interval elapsed.\n\
         === rendered grid ===\n{}",
        SHORT_INACTIVITY_TIMEOUT,
        deck.snapshot_grid(),
    );

    // --- Part 2 (control): re-focus the worker role, then keep it ALIVE
    // with small keystrokes spanning MORE wall-clock time than the
    // configured timeout -> expect NO snap-back this time.
    focus_worker_role(&deck);
    deck.send_keys(format!("{WORKER_REFOCUS}\r").as_bytes());
    let refocus_ok = deck.wait_for_grid_predicate_within(Duration::from_secs(5), |grid| {
        grid.contains(&worker_expanded) && grid.contains(WORKER_REFOCUS)
    });
    assert!(
        refocus_ok,
        "re-focusing the worker role for the control half didn't show a \
         confirmation sentinel inside its own expanded pane box.\n\
         === rendered grid ===\n{}",
        deck.snapshot_grid(),
    );

    // Keep the worker role ALIVE for longer than the configured timeout:
    // the predicate below never matches, so `wait_for_grid_predicate_within`
    // just blocks for the full duration, but on each ~50ms poll (its
    // internal cadence, common/mod.rs) it also forwards a keystroke —
    // continuous activity spanning more wall-clock time than
    // `SHORT_INACTIVITY_TIMEOUT`, entirely through the sanctioned poll
    // primitive rather than a raw sleep loop (Decision 21).
    deck.wait_for_grid_predicate_within(SHORT_INACTIVITY_TIMEOUT * 2, |_| {
        deck.send_keys(b"x"); // any forwarded keystroke resets the timer
        false
    });

    deck.send_keys(format!("{WORKER_FINAL}\r").as_bytes());
    let stayed_on_worker = deck.wait_for_grid_predicate_within(Duration::from_secs(5), |grid| {
        grid.contains(&worker_expanded) && grid.contains(WORKER_FINAL)
    });
    assert!(
        stayed_on_worker,
        "with continuous keystroke activity spanning longer than the \
         configured inactivity interval, a final keystroke never showed up \
         inside the WORKER's own expanded pane box — expected the ongoing \
         activity to keep resetting the timer and suppress any snap-back, \
         but focus appears to have moved off the worker role anyway.\n\
         === rendered grid ===\n{}",
        deck.snapshot_grid(),
    );
}
