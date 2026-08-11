#![cfg(feature = "e2e")]

//! L2 coverage for fork issue #224: opening a new Orchestration tab (Ctrl+n)
//! while a Dashboard pane is already live and attached must switch to and
//! STAY on the new tab, and must not reset the deck-global split stage.
//! Isolates the precondition `tabs/dashboard/001` incidentally exercises
//! while doing three other jobs at once (PRD #387 M1/decision 2).

mod common;

use std::time::Duration;

use common::TuiDeck;
use spec::spec;

/// Left edge (in columns) of a Tiled pane's box whose title fuses into the
/// top border as `┌<pane_title>` (Plain, unfocused/PaneInput) or
/// `┏<pane_title>` (Thick, focused command-mode — `TerminalWidget` in
/// `src/terminal_widget.rs`) — the boundary between the sidebar (deck cards /
/// role list) and the pane column that the split-stage percentages control.
/// Duplicated from `tests/e2e_dashboard_pane_column.rs` (each integration
/// test file compiles as its own crate, so helpers cannot be shared across
/// them).
fn find_pane_box_left_edge(grid: &str, pane_title: &str) -> Option<u16> {
    let plain_needle = format!("┌{pane_title}");
    let thick_needle = format!("┏{pane_title}");
    for line in grid.lines() {
        if let Some(byte_idx) = line
            .find(&plain_needle)
            .or_else(|| line.find(&thick_needle))
        {
            return Some(line[..byte_idx].chars().count() as u16);
        }
    }
    None
}

fn pane_box_left_edge(grid: &str, pane_title: &str) -> u16 {
    find_pane_box_left_edge(grid, pane_title)
        .unwrap_or_else(|| panic!("{pane_title:?} pane box top border not found in grid:\n{grid}"))
}

/// Scenario: launch the deck with a Dashboard pane already live and
/// attached, then run the SAME four-press `Ctrl+l` pre-open cycle
/// `tabs/dashboard/001` uses (Default -> Narrow -> Hidden -> Default ->
/// Narrow) so this test arrives at the orchestration-open step in the same
/// state and after comparable wall-clock, before opening a new Orchestration
/// tab via Ctrl+n and completing the form. The new tab's role pane must
/// render and STAY the active view (checked on an early read AND held across
/// a 3s follow-up window, not just the first read that finds it — the
/// reported defect is a revert back to the Dashboard tab that happens some
/// time after the switch), and the split stage must still read Narrow rather
/// than having reset to Default. Round 3: mirrors `dashboard_001`'s post-open
/// `Ctrl+D` + `Ctrl+l` by sending that same input to the freshly-opened tab,
/// then repeats both checks — still the active view, held again, and the
/// split now advanced to Hidden rather than reset to Default — to test
/// whether sending further input to the new tab (not just watching it
/// passively) is what triggers the reported revert.
#[spec("tabs/dashboard/002")]
#[test]
fn dashboard_002_orchestration_tab_opened_over_live_dashboard_pane_stays_active() {
    const DASH_PANE: &str = "dash-orch-open";

    let deck = TuiDeck::builder()
        .with_pty_size(100, 40)
        .with_continue_session(DASH_PANE, "cat")
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("[Command Mode Ctrl+D]"); // live PTY, PaneInput mode
    deck.wait_for_string(DASH_PANE);

    deck.send_bytes(b"\x04"); // Ctrl+D -> Normal mode, still on the Dashboard tab

    // Mirror `dashboard_001`'s exact pre-open sequence (round 1 of fork
    // issue #224's investigation reproduced the defect only through
    // `dashboard_001`, not through a single Ctrl+l press here — either the
    // extra split-stage transitions or the extra wall-clock they burn
    // matters, and this test does not need to know which). Four presses:
    // Default(33) -> Narrow(25) -> Hidden(0) -> Default(33) -> Narrow(25).
    // This is NOT re-asserting the cycle itself — `tabs/dashboard/001` owns
    // that job — these waits exist only so each press lands before the next
    // one is sent.
    for expected_edge in [25u16, 0, 33, 25] {
        deck.send_bytes(b"\x0c"); // Ctrl+l == 0x0c
        let reached = deck.wait_for_grid_predicate_within(Duration::from_secs(3), |grid| {
            pane_box_left_edge(grid, DASH_PANE) == expected_edge
        });
        assert!(
            reached,
            "Ctrl+l did not bring the Dashboard sidebar to the {expected_edge}\
             -column split-stage edge within 3s during the pre-open cycle — \
             pane-column edge stayed at {}\nGrid:\n{}",
            pane_box_left_edge(&deck.snapshot_grid(), DASH_PANE),
            deck.snapshot_grid()
        );
    }

    // Open a real Orchestration tab (Ctrl+n new-pane flow) in the same
    // directory, WHILE the Dashboard pane above is still live and attached
    // — this precondition is the whole point; an empty Dashboard reproduces
    // nothing (`orchestration_006` / `identity_004` cover that case and both
    // pass).
    deck.send_keys(b"\x0e"); // Ctrl+n -> directory picker
    deck.send_keys(b" "); // Space -> confirm current dir -> new-pane form
    deck.wait_for_string("No mode"); // form up, Mode field focused
    deck.send_keys(b"\x1b[C"); // Right -> [Orch: demo-orch]
    deck.send_keys(b"\r"); // Mode -> Name
    deck.send_keys(b"\r"); // submit (Command hidden for an orchestration)
    deck.wait_for_absence("New Agent"); // new-pane form closed

    // The reported defect: the switch to the new Orchestration tab happens
    // and then REVERTS back to the Dashboard tab some time later — a single
    // bounded probe that returns as soon as the role pane is found would
    // pass and hide the bug (this is exactly what happened in `dashboard_001`
    // before this focused test existed: PR #223's 3s wait finds the
    // `orchestrator` pane box, and the very next grid read no longer does).
    // `wait_until_grid_then_hold` waits for the role pane to first appear,
    // then keeps re-reading the grid and re-asserting the predicate for the
    // hold window, so a revert anywhere inside that window fails the
    // assertion instead of being missed. Round 1 held for 750ms and did not
    // reproduce; widened to 3s here in case the revert lands later than that
    // (a periodic background reconcile, rather than something tied to the
    // open path itself, would explain a longer delay).
    deck.wait_until_grid_then_hold(
        "the new Orchestration tab's role pane staying the active view \
         (not reverting back to the Dashboard tab)",
        Duration::from_secs(3),
        |grid| find_pane_box_left_edge(grid, "orchestrator").is_some(),
    );

    // The second reported symptom: the deck-global split stage must still
    // read Narrow (pane-column edge at column 25), not have reset to
    // Default (which would put the edge at column 34 for an Orchestration
    // tab's own untoggled default).
    let orch_edge_after_open = pane_box_left_edge(&deck.snapshot_grid(), "orchestrator");
    assert_eq!(
        orch_edge_after_open,
        25,
        "the deck-global split stage must stay Narrow (25/75) after opening \
         an Orchestration tab over a live Dashboard pane, not reset to \
         Default — got pane-column edge {orch_edge_after_open}\nGrid:\n{}",
        deck.snapshot_grid()
    );

    // Send further input to the freshly-opened Orchestration tab, mirroring
    // `dashboard_001`'s post-open `Ctrl+D` + `Ctrl+l` (`e2e_dashboard_pane_column.rs:180-181`).
    // Round 2's own analysis identified this as the one thing `dashboard_001`
    // does that this test didn't: `dashboard_001` sends further input to the
    // new tab afterward, `dashboard_002` only watched it passively. PRD #387
    // M1 scopes `Ctrl+l` to command mode on every tab type, and opening the
    // tab re-lands the deck in PaneInput mode on its start-role pane, so
    // `Ctrl+D` must enter Normal mode before the toggle.
    deck.send_bytes(b"\x04"); // Ctrl+D -> Normal mode
    deck.send_bytes(b"\x0c"); // Ctrl+l -> Narrow -> Hidden

    // Both symptoms again, now with input directed at the new tab: it must
    // still be the active view (not reverted to the Dashboard tab), held
    // across the same 3s window as the pre-input check above rather than
    // trusting a single read.
    deck.wait_until_grid_then_hold(
        "the Orchestration tab's role pane staying the active view after \
         Ctrl+D/Ctrl+l input was sent to it (not reverting back to the \
         Dashboard tab)",
        Duration::from_secs(3),
        |grid| find_pane_box_left_edge(grid, "orchestrator").is_some(),
    );

    // And the split stage must have advanced from Narrow to Hidden (the
    // correct effect of the Ctrl+l just sent), not have reset to Default.
    let orch_edge_after_input = pane_box_left_edge(&deck.snapshot_grid(), "orchestrator");
    assert_eq!(
        orch_edge_after_input,
        0,
        "after Ctrl+D/Ctrl+l was sent to the freshly-opened Orchestration \
         tab, the split stage must advance to Hidden (0/100) from the \
         Narrow stage it opened at, not reset to Default — got pane-column \
         edge {orch_edge_after_input}\nGrid:\n{}",
        deck.snapshot_grid()
    );
}
