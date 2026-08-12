#![cfg(feature = "e2e")]

//! L2 real-binary proof for fork#192 M1.0: two orchestration opens in the
//! SAME directory must land as two DISTINCTLY named, both-visible tabs, not
//! the identical basename-derived title every orchestration in one directory
//! records today (the fork #74 collision fork#166/fork#192 exist to fix).
//! `orchestration/identity/002`/`003` (`src/ui.rs`) pin the
//! suggestion/refusal MECHANISMS in isolation, injected via test-only
//! builders; nothing before this test drives the real keyboard path twice in
//! one directory and checks the tab bar a human would actually see.
//!
//! Uses the `orch-deck` fixture (`demo-orch`, two `cat`-stand-in roles, no
//! real LLM tokens spent), already shared by several other e2e files
//! (`e2e_dashboard_selection.rs`, `e2e_orchestration_focus.rs`, etc.).

mod common;

use common::TuiDeck;
use spec::spec;

/// Drive the new-pane dialog to open the fixture's one orchestration,
/// accepting the form's Name-field suggestion with a single Enter — no
/// character typed. Mirrors `e2e_orchestration_focus.rs::open_orchestration`'s
/// keyboard shape.
fn open_orchestration(deck: &TuiDeck) {
    deck.send_bytes(b"\x0e"); // Ctrl+n -> directory picker
    deck.send_bytes(b" "); // Space -> confirm current dir -> new-pane form
    deck.wait_for_string("No mode"); // form up, Mode field focused at "No mode"
    deck.send_bytes(b"\x1b[C"); // Right -> [Orch: demo-orch]
    deck.send_bytes(b"\r"); // Mode -> Name
    deck.send_bytes(b"\r"); // submit (Command hidden for an orchestration), unedited
}

/// Scenario: launch the deck in the `orch-deck` fixture and open its one
/// orchestration TWICE from the SAME directory, accepting the form's
/// suggested Name both times with a single Enter each, never typing a
/// character. Confirm the tab strip shows two DISTINCT, non-blank labels for
/// the two orchestration tabs — `<basename>-orchestrator-1` and
/// `<basename>-orchestrator-2` — never the identical basename-derived title
/// recorded twice, which is the exact fork #74 collision this PRD exists to
/// stop.
#[spec("orchestration/identity/009")]
#[test]
fn identity_009_two_orchestration_opens_land_as_distinctly_named_tabs() {
    let deck = TuiDeck::launch_with_fixture("orch-deck");
    let work = deck.workdir().to_path_buf();
    deck.wait_for_string("No active sessions");

    let launch_dir_basename = work
        .file_name()
        .expect("launch dir must have a basename")
        .to_string_lossy()
        .into_owned();
    let first_label = format!(" {launch_dir_basename}-orchestrator-1 ");
    let second_label = format!(" {launch_dir_basename}-orchestrator-2 ");

    open_orchestration(&deck);
    deck.wait_for_string(" worker "); // first orchestration deck is up

    // Back to the Dashboard to open a second orchestration in the same dir.
    deck.send_bytes(b"\x04"); // Ctrl+D -> Normal mode (still on the orchestration tab)
    deck.send_bytes(b"\x1b[D"); // Left -> previous tab -> Dashboard
    deck.wait_for_string("session(s)");

    open_orchestration(&deck);
    // fork#192 review F7: waiting on " worker " here was vacuous — the grid
    // already shows it from the FIRST orchestration's panes before the
    // second has even opened (the fixture's second role is literally named
    // "worker"), so the wait was satisfiable by stale content and returned
    // before the second open actually finished. Wait on what the test
    // actually asserts instead — both a correct barrier and a stronger check.
    deck.wait_for_string(&second_label); // second orchestration deck is up, distinctly labeled

    let grid = deck.snapshot_grid();
    assert!(
        grid.contains(&first_label),
        "the first orchestration's tab must be labeled {first_label:?}\n=== rendered grid ===\n{grid}"
    );
    assert!(
        grid.contains(&second_label),
        "the second orchestration's tab must be labeled {second_label:?}, distinct from the \
         first — not the identical basename-derived title fork#192 exists to stop recording \
         twice\n=== rendered grid ===\n{grid}"
    );
}
