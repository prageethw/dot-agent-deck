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

/// Run a `git` subcommand against `dir`, panicking on non-zero exit — mirrors
/// `tests/e2e_orchestration_worktree.rs::run_git`.
fn run_git(dir: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed in {dir:?}");
}

/// Commit the fixture's own `.dot-agent-deck.toml` into `dir` so a typed
/// Worktree slug's `git worktree add`/isolated clone (issue #489's
/// unaffected typed-slug arm) has a ref to branch from — `git worktree add`
/// cannot create a worktree from an unborn HEAD. Mirrors
/// `tests/e2e_orchestration_worktree.rs::commit_fixture`; identity pinned
/// inline since CI runners carry no global git config.
fn commit_fixture(dir: &std::path::Path) {
    run_git(dir, &["add", ".dot-agent-deck.toml"]);
    run_git(
        dir,
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
            "init",
        ],
    );
}

/// Drive the new-pane dialog to open the fixture's one orchestration with a
/// TYPED Worktree slug instead of accepting the form's blank default, so the
/// request goes through issue #489's unaffected typed-slug arm
/// (`SiblingScope::ExactCwdOnly` only gates a BLANK slug) instead of being
/// refused as an exact-cwd collision with another live orchestration.
/// Otherwise identical to `open_orchestration`.
fn open_orchestration_with_slug(deck: &TuiDeck, slug: &str) {
    deck.send_bytes(b"\x0e"); // Ctrl+n -> directory picker
    deck.send_bytes(b" "); // Space -> confirm current dir -> new-pane form
    deck.wait_for_string("No mode"); // form up, Mode field focused at "No mode"
    deck.send_bytes(b"\x1b[C"); // Right -> [Orch: demo-orch]
    deck.send_bytes(b"\r"); // Mode -> Name
    deck.send_bytes(b"\t"); // Tab: Name -> Worktree (Command hidden for an orchestration)
    deck.send_bytes(slug.as_bytes());
    deck.send_bytes(b"\r"); // submit
}

/// Scenario: launch the deck in the `orch-deck` fixture and open its one
/// orchestration TWICE against the SAME directory picked in the form,
/// accepting the form's suggested Name both times with a single Enter each,
/// never typing a character into it. Issue #489 refuses a second BLANK-slug
/// open that exactly collides with the first orchestration's own live
/// directory, so the second open types a Worktree slug — unrelated to this
/// test's own point, which is the tab-naming behavior, not the Worktree
/// field — to go through the unaffected typed-slug arm instead. Confirm the
/// tab strip shows two DISTINCT, non-blank labels for the two orchestration
/// tabs — `<basename>-orchestrator-1` and `<basename>-orchestrator-2` —
/// never the identical basename-derived title recorded twice, which is the
/// exact fork #74 collision this PRD exists to stop.
#[spec("orchestration/identity/009")]
#[test]
fn identity_009_two_orchestration_opens_land_as_distinctly_named_tabs() {
    let deck = TuiDeck::launch_with_fixture("orch-deck");
    let work = deck.workdir().to_path_buf();
    // Issue #489: the second `open_orchestration_with_slug` call below needs
    // a ref to branch from — `git worktree add`/the isolated-clone arm it
    // goes through both fail against an unborn HEAD (the harness's own bare
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

    open_orchestration_with_slug(&deck, "identityb");
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
