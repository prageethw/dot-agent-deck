#![cfg(feature = "e2e")]

//! L2 end-to-end coverage for the Dashboard tab's pane column split toggle
//! (PRD #361 Item 4 — extends PRD #336's orchestration-only Ctrl+l toggle,
//! covered by `tests/e2e_orchestration_pane_column.rs`, to Dashboard tabs).
//!
//! Decision 6: gated behind the `e2e` feature so `cargo test-fast` never
//! compiles it.

mod common;

use std::time::Duration;

use common::TuiDeck;
use spec::spec;

/// Left edge (in columns) of a Tiled pane's box whose title fuses into the
/// top border as `┌<pane_title>` (Plain, unfocused/PaneInput) or
/// `┏<pane_title>` (Thick, focused command-mode — `TerminalWidget` in
/// `src/terminal_widget.rs`) — the boundary between the sidebar (deck cards /
/// role list) and the pane column that the split-stage percentages control.
/// Generalizes `e2e_orchestration_pane_column.rs`'s `pane_column_left_edge`
/// (hardcoded to the "orchestrator" role name) to any pane title, so it
/// covers both a Dashboard pane's session name and an orchestration role name
/// from the same helper.
fn pane_box_left_edge(grid: &str, pane_title: &str) -> u16 {
    let plain_needle = format!("┌{pane_title}");
    let thick_needle = format!("┏{pane_title}");
    for line in grid.lines() {
        if let Some(byte_idx) = line
            .find(&plain_needle)
            .or_else(|| line.find(&thick_needle))
        {
            return line[..byte_idx].chars().count() as u16;
        }
    }
    panic!("{pane_title:?} pane box top border not found in grid:\n{grid}");
}

/// Scenario: Extends the Ctrl+l split-toggle to Dashboard tabs — launch with
/// a live Dashboard pane, Ctrl+l cycle through Default (33%) -> Narrow (25%)
/// -> Hidden (0%) -> Default, asserting the pane column's left edge at each
/// stage. Open a second Orchestration tab and confirm its own 34% default and
/// toggle are unaffected by the Dashboard tab's stage, proving cross-tab-type
/// isolation. PRD #387 M1 scopes Ctrl+l to command mode on every tab type;
/// opening the Orchestration tab lands the deck back in PaneInput mode on
/// its start-role pane, so a Ctrl+D precedes that tab's own toggle press.
#[spec("tabs/dashboard/001")]
#[test]
fn dashboard_001_ctrl_l_cycles_dashboard_split_stage_isolated_from_orchestration() {
    const DASH_PANE: &str = "dash-layout-cycle";

    let deck = TuiDeck::builder()
        .with_pty_size(100, 40)
        .with_continue_session(DASH_PANE, "cat")
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("[Command Mode Ctrl+D]"); // live PTY, PaneInput mode
    deck.wait_for_string(DASH_PANE);

    deck.send_bytes(b"\x04"); // Ctrl+D -> Normal mode, still on the Dashboard tab

    // Baseline: the Dashboard tab's own default 33/67 split (distinct from
    // Orchestration's 34/66) puts the pane column's left edge at col 33 of
    // the 100-col frame (no rounding ambiguity — 33% of 100 is exact).
    let default_edge = pane_box_left_edge(&deck.snapshot_grid(), DASH_PANE);
    assert_eq!(
        default_edge,
        33,
        "expected the Dashboard tab's default 33/67 split's pane-column \
         edge at col 33, got {default_edge}\nGrid:\n{}",
        deck.snapshot_grid()
    );

    // Ctrl+l: Default -> Narrow (25/75).
    deck.send_bytes(b"\x0c"); // Ctrl+l == 0x0c
    let narrowed = deck.wait_for_grid_predicate_within(Duration::from_secs(3), |grid| {
        pane_box_left_edge(grid, DASH_PANE) == 25
    });
    assert!(
        narrowed,
        "Ctrl+l did not narrow the Dashboard sidebar to the 25/75 split \
         within 3s — pane-column edge stayed at {}\nGrid:\n{}",
        pane_box_left_edge(&deck.snapshot_grid(), DASH_PANE),
        deck.snapshot_grid()
    );

    // Ctrl+l: Narrow -> Hidden (0/100, sidebar collapsed).
    deck.send_bytes(b"\x0c");
    let hidden = deck.wait_for_grid_predicate_within(Duration::from_secs(3), |grid| {
        pane_box_left_edge(grid, DASH_PANE) == 0
    });
    assert!(
        hidden,
        "a second Ctrl+l did not collapse the Dashboard sidebar to the \
         Hidden stage within 3s — pane-column edge stayed at {}\nGrid:\n{}",
        pane_box_left_edge(&deck.snapshot_grid(), DASH_PANE),
        deck.snapshot_grid()
    );

    // Ctrl+l: Hidden -> Default, completing the loop.
    deck.send_bytes(b"\x0c");
    let restored = deck.wait_for_grid_predicate_within(Duration::from_secs(3), |grid| {
        pane_box_left_edge(grid, DASH_PANE) == 33
    });
    assert!(
        restored,
        "a third Ctrl+l did not restore the Dashboard tab's 33/67 default \
         split within 3s — pane-column edge stayed at {}\nGrid:\n{}",
        pane_box_left_edge(&deck.snapshot_grid(), DASH_PANE),
        deck.snapshot_grid()
    );

    // Cross-tab-type isolation: open a SECOND, real Orchestration tab in the
    // same directory (Ctrl+n new-pane flow) and confirm it starts at ITS OWN
    // default 34/66 split.
    deck.send_keys(b"\x0e"); // Ctrl+n -> directory picker
    deck.send_keys(b" "); // Space -> confirm current dir -> new-pane form
    deck.wait_for_string("No mode"); // form up, Mode field focused
    deck.send_keys(b"\x1b[C"); // Right -> [Orch: demo-orch]
    deck.send_keys(b"\r"); // Mode -> Name
    deck.send_keys(b"\r"); // submit (Command hidden for an orchestration)
    deck.wait_for_absence("New Agent"); // new-pane form closed -> the orchestration tab is up

    let orch_default_edge = pane_box_left_edge(&deck.snapshot_grid(), "orchestrator");
    assert_eq!(
        orch_default_edge,
        34,
        "a brand-new Orchestration tab must open at its OWN default 34/66 \
         split regardless of the Dashboard tab's stage, got {orch_default_edge}\
         \nGrid:\n{}",
        deck.snapshot_grid()
    );

    // Toggle the Orchestration tab to Narrow. Opening it (Ctrl+n new-pane
    // flow) left the deck in PaneInput mode, focused on its start-role pane
    // — PRD #387 M1 scopes Ctrl+l to command mode on every tab type, so
    // Ctrl+D must enter Normal mode first or the byte forwards straight to
    // the pane instead of cycling the split.
    deck.send_bytes(b"\x04"); // Ctrl+D -> Normal mode
    deck.send_bytes(b"\x0c");
    let orch_narrowed = deck.wait_for_grid_predicate_within(Duration::from_secs(3), |grid| {
        pane_box_left_edge(grid, "orchestrator") == 25
    });
    assert!(
        orch_narrowed,
        "Ctrl+l did not narrow the Orchestration tab's sidebar within 3s\n\
         Grid:\n{}",
        deck.snapshot_grid()
    );

    // Switch back to the Dashboard tab (Shift+Tab -> previous tab) and
    // confirm ITS split is still Default (33/67) — untouched by the
    // Orchestration tab's toggle, even though both tabs were just driven
    // through the exact same Ctrl+l chord. The deck is ALREADY in Normal
    // mode here (from the Ctrl+D sent before the Orchestration tab's own
    // toggle above), which `cycle_tab_action` requires for Shift+Tab to
    // switch tabs rather than forward the bytes to the pane — so no further
    // Ctrl+D is needed. Sending one anyway would be actively harmful: Ctrl+D
    // TOGGLES, and since the Orchestration tab's start-role pane is still
    // the deck's resume target from Normal mode, a second press here would
    // re-enter PaneInput on that tab and break the Shift+Tab below.
    deck.send_bytes(b"\x1b[Z"); // Shift+Tab -> previous tab -> Dashboard
    // Confirm the tab switch itself landed (DASH_PANE's title is only ever
    // drawn on the Dashboard tab) before polling for its exact edge — a
    // panicking `pane_box_left_edge` used directly as a
    // `wait_for_grid_predicate_within` predicate aborts on the first sampled
    // grid instead of retrying, and that first sample can still show the
    // Orchestration tab (DASH_PANE genuinely absent, not just moved) in the
    // brief window before the switch renders.
    deck.wait_for_string(DASH_PANE);
    let dash_still_default = deck.wait_for_grid_predicate_within(Duration::from_secs(3), |grid| {
        pane_box_left_edge(grid, DASH_PANE) == 33
    });
    assert!(
        dash_still_default,
        "toggling the Orchestration tab's split must not move the Dashboard \
         tab's split — expected the Dashboard tab still at its 33/67 \
         default after switching back, got {}\nGrid:\n{}",
        pane_box_left_edge(&deck.snapshot_grid(), DASH_PANE),
        deck.snapshot_grid()
    );
}
