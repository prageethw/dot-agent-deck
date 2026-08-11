#![cfg(feature = "e2e")]

//! L2 end-to-end coverage for the Dashboard tab's pane column split toggle
//! (PRD #361 Item 4 — extends PRD #336's orchestration-only Ctrl+l toggle,
//! covered by `tests/e2e_orchestration_pane_column.rs`, to Dashboard tabs).
//!
//! Decision 6: gated behind the `e2e` feature so `cargo test-fast` never
//! compiles it.

mod common;

use std::time::Duration;

use common::{TuiDeck, find_pane_box_left_edge, pane_box_left_edge};
use spec::spec;

/// Scenario: Extends the Ctrl+l split-toggle to Dashboard tabs — launch with
/// a live Dashboard pane, Ctrl+l cycle through Default (33%) -> Narrow (25%)
/// -> Hidden (0%) -> Default, asserting the pane column's left edge at each
/// stage. PRD #387 decision 2: the split stage is ONE deck-global value —
/// open a second Orchestration tab and confirm it adopts the Dashboard tab's
/// CURRENT (Narrow) stage rather than its own untoggled Default, then prove
/// sharing in both directions: cycling the Orchestration tab moves the
/// Dashboard tab, and cycling back on the Dashboard tab moves the
/// Orchestration tab right back. PRD #387 M1 scopes Ctrl+l to command mode
/// on every tab type; opening the Orchestration tab lands the deck back in
/// PaneInput mode on its start-role pane, so a Ctrl+D precedes that tab's
/// own toggle press.
#[spec("tabs/dashboard/001")]
#[test]
fn dashboard_001_ctrl_l_cycles_dashboard_split_stage_shared_with_orchestration() {
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

    // Toggle the Dashboard tab back to Narrow, setting up the shared-stage
    // precondition the Orchestration tab must adopt below.
    deck.send_bytes(b"\x0c");
    let narrowed_again = deck.wait_for_grid_predicate_within(Duration::from_secs(3), |grid| {
        pane_box_left_edge(grid, DASH_PANE) == 25
    });
    assert!(
        narrowed_again,
        "Ctrl+l did not narrow the Dashboard sidebar to the 25/75 split \
         within 3s (second time) — pane-column edge stayed at {}\nGrid:\n{}",
        pane_box_left_edge(&deck.snapshot_grid(), DASH_PANE),
        deck.snapshot_grid()
    );

    // Open a SECOND, real Orchestration tab in the same directory (Ctrl+n
    // new-pane flow). PRD #387 decision 2: it must ADOPT the deck-global
    // Narrow stage the Dashboard tab was just toggled to, not its own
    // untoggled Default (34/66).
    deck.send_keys(b"\x0e"); // Ctrl+n -> directory picker
    deck.send_keys(b" "); // Space -> confirm current dir -> new-pane form
    deck.wait_for_string("No mode"); // form up, Mode field focused
    deck.send_keys(b"\x1b[C"); // Right -> [Orch: demo-orch]
    deck.send_keys(b"\r"); // Mode -> Name
    deck.send_keys(b"\r"); // submit (Command hidden for an orchestration)
    deck.wait_for_absence("New Agent"); // new-pane form closed -> the orchestration tab is up
    // The form closing only means the modal is gone, not that the active
    // view has switched to the new Orchestration tab yet — wait for its
    // role pane box to actually render before reading the exact edge. A
    // panicking `pane_box_left_edge` used directly as a
    // `wait_for_grid_predicate_within` predicate would abort on the first
    // sampled grid instead of retrying if that switch hasn't rendered yet
    // (the first sample can still show the Dashboard tab, in the brief
    // window before the switch renders).
    let orch_tab_rendered = deck.wait_for_grid_predicate_within(Duration::from_secs(3), |grid| {
        find_pane_box_left_edge(grid, "orchestrator").is_some()
    });
    assert!(
        orch_tab_rendered,
        "the new Orchestration tab's role pane box never rendered within \
         3s after the new-pane form closed\nGrid:\n{}",
        deck.snapshot_grid()
    );

    let orch_narrow_edge = pane_box_left_edge(&deck.snapshot_grid(), "orchestrator");
    assert_eq!(
        orch_narrow_edge,
        25,
        "a brand-new Orchestration tab must open AT the deck-global Narrow \
         stage the Dashboard tab was just toggled to, not its own untoggled \
         Default, got {orch_narrow_edge}\nGrid:\n{}",
        deck.snapshot_grid()
    );

    // Continue the SHARED cycle from the Orchestration tab: Narrow -> Hidden.
    // Opening it (Ctrl+n new-pane flow) left the deck in PaneInput mode,
    // focused on its start-role pane — PRD #387 M1 scopes Ctrl+l to command
    // mode on every tab type, so Ctrl+D must enter Normal mode first or the
    // byte forwards straight to the pane instead of cycling the split.
    deck.send_bytes(b"\x04"); // Ctrl+D -> Normal mode
    deck.send_bytes(b"\x0c");
    // Non-panicking form (see `find_pane_box_left_edge`'s doc comment in
    // `tests/common/mod.rs`, and the same fix applied below): the panicking
    // `pane_box_left_edge` would abort on the first sampled grid if the box
    // were momentarily absent instead of letting this 3s loop actually retry.
    let orch_hidden = deck.wait_for_grid_predicate_within(Duration::from_secs(3), |grid| {
        find_pane_box_left_edge(grid, "orchestrator") == Some(0)
    });
    assert!(
        orch_hidden,
        "Ctrl+l did not collapse the Orchestration tab's sidebar to the \
         Hidden stage within 3s\nGrid:\n{}",
        deck.snapshot_grid()
    );

    // Switch back to the Dashboard tab (Shift+Tab -> previous tab). Shared-
    // stage proof (PRD #387 decision 2): the Dashboard tab must NOW ALSO
    // read Hidden — toggling on the Orchestration tab moved the SAME
    // deck-global stage the Dashboard tab reads, even though the Dashboard
    // tab itself was never touched after its own Default -> Narrow press
    // above. The deck is ALREADY in Normal mode here (from the Ctrl+D sent
    // before the Orchestration tab's own toggle above), which
    // `cycle_tab_action` requires for Shift+Tab to switch tabs rather than
    // forward the bytes to the pane — so no further Ctrl+D is needed.
    // Sending one anyway would be actively harmful: Ctrl+D TOGGLES, and
    // since the Orchestration tab's start-role pane is still the deck's
    // resume target from Normal mode, a second press here would re-enter
    // PaneInput on that tab and break the Shift+Tab below.
    deck.send_bytes(b"\x1b[Z"); // Shift+Tab -> previous tab -> Dashboard
    // Confirm the tab switch itself landed (DASH_PANE's title is only ever
    // drawn on the Dashboard tab) before polling for its exact edge — a
    // panicking `pane_box_left_edge` used directly as a
    // `wait_for_grid_predicate_within` predicate aborts on the first sampled
    // grid instead of retrying, and that first sample can still show the
    // Orchestration tab (DASH_PANE genuinely absent, not just moved) in the
    // brief window before the switch renders.
    deck.wait_for_string(DASH_PANE);
    let dash_also_hidden = deck.wait_for_grid_predicate_within(Duration::from_secs(3), |grid| {
        pane_box_left_edge(grid, DASH_PANE) == 0
    });
    assert!(
        dash_also_hidden,
        "toggling the Orchestration tab's split must move the Dashboard \
         tab's split too — expected the Dashboard tab ALSO Hidden (edge 0) \
         after switching back, got {}\nGrid:\n{}",
        pane_box_left_edge(&deck.snapshot_grid(), DASH_PANE),
        deck.snapshot_grid()
    );

    // Finish the shared cycle from the Dashboard tab: Hidden -> Default. No
    // Ctrl+D needed: switching tabs (`switch_tab_with_focus`) never touches
    // `UiMode`, so the deck is still in Normal mode from above.
    deck.send_bytes(b"\x0c");
    let dash_restored = deck.wait_for_grid_predicate_within(Duration::from_secs(3), |grid| {
        pane_box_left_edge(grid, DASH_PANE) == 33
    });
    assert!(
        dash_restored,
        "Ctrl+l did not restore the Dashboard tab's 33/67 default split \
         within 3s — pane-column edge stayed at {}\nGrid:\n{}",
        pane_box_left_edge(&deck.snapshot_grid(), DASH_PANE),
        deck.snapshot_grid()
    );

    // Switch to the Orchestration tab one last time (Right -> CycleTabNext,
    // the established forward tab-cycle chord — see
    // `e2e_dashboard_selection.rs`'s SC1 round-trip) and confirm IT is ALSO
    // back at its OWN Default (34/66) — completing the loop in both
    // directions: the Dashboard tab's final toggle propagated to the
    // Orchestration tab just as the Orchestration tab's earlier toggle
    // propagated to the Dashboard tab.
    deck.send_bytes(b"\x1b[C"); // Right -> next tab -> Orchestration
    // Round 4 (fork issue #224): `wait_for_string("orchestrator")` here was a
    // guard that did not guard anything. The Orchestration tab's label (e.g.
    // `.tmp6ozm36-orchestrator-1`, fork#192's suggested name) appears in the
    // tab strip as soon as the tab exists, whether or not it is the active
    // tab — so the wait matched immediately after the tab was created, before
    // the `Right` keypress had any effect, every time. Use DASH_PANE's
    // absence instead (DASH_PANE's title is only ever drawn on the Dashboard
    // tab), which genuinely distinguishes having switched off of it, before
    // polling for the Orchestration tab's exact edge with the non-panicking
    // `find_pane_box_left_edge` form (see its doc comment in
    // `tests/common/mod.rs`).
    deck.wait_for_absence(DASH_PANE);
    let orch_also_restored = deck.wait_for_grid_predicate_within(Duration::from_secs(3), |grid| {
        find_pane_box_left_edge(grid, "orchestrator") == Some(34)
    });
    assert!(
        orch_also_restored,
        "toggling the Dashboard tab's split back to Default must move the \
         Orchestration tab's split too — expected the Orchestration tab \
         ALSO at its own 34/66 Default after switching to it, got {:?}\
         \nGrid:\n{}",
        find_pane_box_left_edge(&deck.snapshot_grid(), "orchestrator"),
        deck.snapshot_grid()
    );
}
