#![cfg(feature = "e2e")]

//! L2 end-to-end coverage for PRD #336 (toggle orchestration pane-column
//! split ratio).
//!
//! Spawns the real `dot-agent-deck` binary against the `orch-deck` fixture
//! (two stub `cat` roles, no LLM tokens spent) and drives the Ctrl+l chord
//! through the PTY, asserting on the rendered vt100 grid's column geometry.
//!
//! Decision 6: gated behind the `e2e` feature so `cargo test-fast` never
//! compiles it.

mod common;

use std::time::Duration;

use common::TuiDeck;
use spec::spec;

/// Drive the new-pane dialog to open the (single) orchestration in the
/// `orch-deck` fixture. Mirrors `e2e_dashboard_selection.rs::open_orchestration`
/// — with no `[[modes]]` defined the Mode chip row is `[No mode] [Orch: …]
/// [schedule]`, so ONE Right selects the orchestration; selecting an
/// orchestration hides the Command field, so a second Enter submits the form.
fn open_orchestration(deck: &TuiDeck) {
    deck.send_keys(b"\x0e"); // Ctrl+n -> directory picker
    deck.send_keys(b" "); // Space -> confirm current dir -> new-pane form
    deck.wait_for_string("No mode"); // form up, Mode field focused at "No mode"
    deck.send_keys(b"\x1b[C"); // Right -> [Orch: demo-orch]
    deck.send_keys(b"\r"); // Mode -> Name
    deck.send_keys(b"\r"); // submit (Command hidden for an orchestration)
}

/// Column index of the orchestration tab's role-pane column's LEFT edge: the
/// role-pane box drawn for the fixture's `start = true` role ("orchestrator")
/// renders its title fused into the top border as `┌orchestrator───…`, so the
/// column of that `┌` is exactly `panes_area.x` — the boundary between the
/// sidebar (role list) and the pane column that `ORCHESTRATION_LEFT_PERCENT`
/// / `ORCHESTRATION_PANES_PERCENT` (src/ui.rs:1951-1952) control. Distinct
/// from the sidebar's own truncated `orchestrat…` card label, so there is no
/// collision risk.
fn pane_column_left_edge(grid: &str) -> u16 {
    for line in grid.lines() {
        if let Some(byte_idx) = line.find("┌orchestrator") {
            return line[..byte_idx].chars().count() as u16;
        }
    }
    panic!("orchestrator role-pane box top border not found in grid:\n{grid}");
}

/// Scenario: open a real orchestration tab (120-col PTY) at the default 34/66
/// split, confirm the pane column's left edge sits at the expected ~34%-width
/// boundary, then send Ctrl+l and wait for that boundary to move to the
/// narrower-sidebar 25%-width position (sidebar visibly narrows, pane column
/// visibly widens). Send Ctrl+l again and confirm the boundary returns to the
/// original 34% position. RED today: Ctrl+l is unbound (PRD #336 verified
/// against the `ACTIONS` default table), so the boundary never moves and the
/// wait for the narrow geometry times out.
#[spec("tabs/orchestration/006")]
#[test]
fn orchestration_006_ctrl_l_toggles_pane_column_split() {
    let deck = TuiDeck::builder()
        .with_pty_size(120, 40)
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("No active sessions");

    open_orchestration(&deck);
    deck.wait_for_string("worker"); // 2nd role card -> orchestration tab is up

    // Baseline: the default 34/66 split puts the pane column's left edge at
    // 34% of the 120-col frame (col 40 or 41, depending on Percentage
    // rounding) — well clear of both the 25%-split boundary (col 30) this
    // test pins after the toggle.
    let default_edge = pane_column_left_edge(&deck.snapshot_grid());
    assert!(
        (40..=41).contains(&default_edge),
        "expected the default 34/66 split's pane-column edge near col 40/41, \
         got {default_edge}\nGrid:\n{}",
        deck.snapshot_grid()
    );

    // Ctrl+l: toggle to the narrower-sidebar 25/75 split. The sidebar
    // narrows and the pane column widens, so the boundary column DECREASES
    // (25% of 120 = col 30) — a `contains` window tolerates rounding without
    // caring about the exact boundary constant.
    deck.send_bytes(b"\x0c"); // Ctrl+l == 0x0c
    let narrowed = deck.wait_for_grid_predicate_within(Duration::from_secs(3), |grid| {
        let edge = pane_column_left_edge(grid);
        (29..=30).contains(&edge)
    });
    assert!(
        narrowed,
        "Ctrl+l did not narrow the sidebar to the 25/75 split within 3s — \
         pane-column edge stayed at {}\nGrid:\n{}",
        pane_column_left_edge(&deck.snapshot_grid()),
        deck.snapshot_grid()
    );

    // Second Ctrl+l: toggle back to the 34/66 default.
    deck.send_bytes(b"\x0c");
    let restored = deck.wait_for_grid_predicate_within(Duration::from_secs(3), |grid| {
        let edge = pane_column_left_edge(grid);
        (40..=41).contains(&edge)
    });
    assert!(
        restored,
        "a second Ctrl+l did not restore the 34/66 default split within 3s — \
         pane-column edge stayed at {}\nGrid:\n{}",
        pane_column_left_edge(&deck.snapshot_grid()),
        deck.snapshot_grid()
    );
}
