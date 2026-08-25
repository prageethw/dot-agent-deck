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

use common::{TuiDeck, commit_fixture, run_git};
use spec::spec;

/// `role` appears somewhere on the settled grid as its own token — bounded
/// on both sides by anything that is NOT an identifier character (a card
/// border glyph, whitespace, a newline, or start/end of the grid) rather
/// than specifically a space. PRD fork#405 M1 moved the role name off the
/// card's title/border row onto its own unconditional body row directly
/// beneath it, and `render_session_card` (`src/ui.rs`) pushes that row's
/// text with NO leading or trailing pad — so a `" {role} "`-style
/// space-bounded needle can never match post-M1 (the character immediately
/// to the left of the role name is the card's own border glyph, not a
/// space). Mirrors `grid_has_role` in `e2e_dashboard_selection.rs`, which
/// hit the identical breakage.
fn grid_has_role(grid: &str, role: &str) -> bool {
    common::contains_word_token(grid, role)
}

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
/// orchestration TWICE against the SAME directory picked in the form,
/// accepting the form's suggested Name both times with a single Enter each,
/// never typing a character into it. PRD fork#544 M2b retires the
/// blank-slug exact-cwd collision refusal issue #489 introduced (isolation
/// is unconditional now, so there is no longer a shared-checkout case for a
/// second blank open to collide with) — the second open no longer needs a
/// typed Worktree slug (a field M2 also retires outright) to route around
/// it, so both opens use the identical plain path. Confirm the tab strip
/// shows two DISTINCT, non-blank labels for the two orchestration tabs —
/// `<basename>-orchestrator-1` and `<basename>-orchestrator-2` — never the
/// identical basename-derived title recorded twice, which is the exact
/// fork #74 collision this PRD exists to stop.
#[spec("orchestration/identity/013")]
#[test]
fn identity_013_two_orchestration_opens_land_as_distinctly_named_tabs() {
    let deck = TuiDeck::launch_with_fixture("orch-deck");
    let work = deck.workdir().to_path_buf();
    // The second open's isolated-clone provisioning needs a ref to branch
    // from — it fails against an unborn HEAD (the harness's own bare
    // `git init`).
    commit_fixture(&work);
    deck.wait_for_string("No active sessions");

    let launch_dir_basename = work
        .file_name()
        .expect("launch dir must have a basename")
        .to_string_lossy()
        .into_owned();
    let first_label = format!(" {launch_dir_basename}-orchestrator-1 ");
    let second_label = format!(" {launch_dir_basename}-orchestrator-2 ");

    open_orchestration(&deck);
    // PRD fork#405 M1: " worker " (space-bounded) can never match the role's
    // own body row post-M1 — see `grid_has_role`.
    deck.wait_until_grid("worker role card rendered", |g| grid_has_role(g, "worker")); // first orchestration deck is up

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

/// Scenario: PRD fork#603 — the cross-directory counterpart to
/// `identity_013`. Seed two SIBLING subtrees under the launch dir, each
/// carrying an inner directory literally named `proj` (`team-a/proj` and
/// `team-b/proj`, each with its own copy of the fixture's
/// `.dot-agent-deck.toml`) — two genuinely distinct absolute paths that
/// share the identical basename `suggest_orchestration_name` derives its
/// suggestion from. Navigate the directory picker into each in turn (mouse
/// clicks to descend, mirroring `e2e_scheduler_manager.rs`'s
/// `form_006_edit_repick_different_dir_wins_in_seed`), accepting the form's
/// suggested Name both times with a single Enter, never typing over it.
/// Both opens must land un-refused as their own live orchestration tab
/// titled EXACTLY `proj-orchestrator-1` — never forced apart into `-1`/`-2`
/// (today's name-only global uniqueness check would bump the second open to
/// `-2` even though the two directories share nothing).
#[spec("orchestration/identity/033")]
#[test]
fn identity_033_directories_with_the_same_basename_both_suggest_orchestrator_1() {
    let deck = TuiDeck::launch_with_fixture("orch-deck");
    let work = deck.workdir().to_path_buf();
    commit_fixture(&work);
    deck.wait_for_string("No active sessions");

    let toml = std::fs::read_to_string(work.join(".dot-agent-deck.toml"))
        .expect("read fixture's own .dot-agent-deck.toml to duplicate into each leaf");
    for team in ["team-a", "team-b"] {
        let leaf = work.join(team).join("proj");
        std::fs::create_dir_all(&leaf).expect("create leaf project dir");
        std::fs::write(leaf.join(".dot-agent-deck.toml"), &toml)
            .expect("seed leaf .dot-agent-deck.toml");
    }
    // `commit_fixture` only ever adds its own top-level `.dot-agent-deck.toml`
    // (deliberately, per its own doc comment — a blanket `add`/`-A` would also
    // try to walk the harness's `home/` dir and Unix sockets). The two leaf
    // fixtures above are therefore untracked unless committed explicitly here,
    // and every orchestration provisions via `git clone`, which only ever
    // reproduces tracked, committed content (src/ui.rs:9941, fork issue #595)
    // — an untracked leaf is invisible to the clone and fails
    // `resolved_dir()`'s nested-subpath check for BOTH opens, not just the
    // second (PRD fork#603 / PR #604).
    run_git(&work, &["add", "team-a/proj/.dot-agent-deck.toml"]);
    run_git(&work, &["add", "team-b/proj/.dot-agent-deck.toml"]);
    run_git(
        &work,
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "add team-a/team-b fixture leaves",
        ],
    );

    const LABEL: &str = " proj-orchestrator-1 ";

    // First open: team-a/proj.
    deck.send_bytes(b"\x0e"); // Ctrl+n -> directory picker
    let (col, row) = deck.wait_for_in_grid("team-a");
    deck.click(col, row);
    deck.click(col, row); // double-click -> descend into team-a
    let (col, row) = deck.wait_for_in_grid("proj");
    deck.click(col, row);
    deck.click(col, row); // double-click -> descend into team-a/proj
    deck.send_bytes(b" "); // Space -> confirm current dir -> new-pane form
    deck.wait_for_string("No mode"); // form up, Mode field focused at "No mode"
    deck.send_bytes(b"\x1b[C"); // Right -> [Orch: demo-orch]
    deck.send_bytes(b"\r"); // Mode -> Name
    deck.send_bytes(b"\r"); // submit the suggested name, unedited
    deck.wait_for_string(LABEL); // first open's tab is up, labeled -orchestrator-1

    // Back to the Dashboard to open the second orchestration.
    deck.send_bytes(b"\x04"); // Ctrl+D -> Normal mode (still on the orchestration tab)
    deck.send_bytes(b"\x1b[D"); // Left -> previous tab -> Dashboard
    deck.wait_for_string("session(s)");

    // Second open: team-b/proj — a DIFFERENT absolute path with the
    // IDENTICAL basename `proj`.
    deck.send_bytes(b"\x0e");
    let (col, row) = deck.wait_for_in_grid("team-b");
    deck.click(col, row);
    deck.click(col, row); // double-click -> descend into team-b
    let (col, row) = deck.wait_for_in_grid("proj");
    deck.click(col, row);
    deck.click(col, row); // double-click -> descend into team-b/proj
    deck.send_bytes(b" ");
    deck.wait_for_string("No mode");
    deck.send_bytes(b"\x1b[C");
    deck.send_bytes(b"\r");
    deck.send_bytes(b"\r"); // submit the suggested name, unedited

    // `wait_for_string(LABEL)` alone would be vacuously satisfied by the
    // FIRST tab's own identical label (fork#192 review F7's same trap, here
    // because both suggestions are meant to be identical rather than
    // incidentally so) — wait until BOTH tabs carry it instead.
    deck.wait_until_grid("both orchestration tabs labeled -orchestrator-1", |g| {
        g.matches(LABEL).count() == 2
    });

    let grid = deck.snapshot_grid();
    assert_eq!(
        grid.matches(LABEL).count(),
        2,
        "both `team-a/proj` and `team-b/proj` must land as their own, un-refused, live \
         orchestration tab titled exactly {LABEL:?} — two directories with the same basename \
         must each get their own `-orchestrator-1` rather than being forced apart (PRD \
         fork#603)\n=== rendered grid ===\n{grid}"
    );
    assert!(
        !grid.contains("-orchestrator-2"),
        "the second open must never be bumped to `-orchestrator-2` — that would mean the \
         client-side suggestion is still scoped to name alone rather than (directory, name) \
         (PRD fork#603)\n=== rendered grid ===\n{grid}"
    );
}
