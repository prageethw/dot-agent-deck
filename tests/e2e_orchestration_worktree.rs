#![cfg(feature = "e2e")]

//! L2 real-binary proof of fork #122's worktree-slug form field
//! (`orchestration/worktree/005`), the CLAUDE.md rule 4 PTY-attached test for
//! this feature. `orchestration/worktree/001`-`004` (`src/ui.rs` unit tests)
//! each characterize one mechanism in isolation — the slug-to-path resolution,
//! the fail-loud refusal, the pre-existing cwd-threading, the actual on-disk
//! creation via `dispatch_action` — but none of them drives the real keyboard
//! path a user actually types: `Ctrl+n` -> directory picker -> form -> cycle
//! Mode to an orchestration -> **Tab to the Worktree field** -> type a slug ->
//! submit. That exact path regressed once already on this PR (an Enter-chain
//! break was found only as collateral damage in seven unrelated e2e helpers,
//! not by a test driving this field directly) — this test exists to catch
//! that class of regression head-on.
//!
//! `create_worktree_sync` shells out to real `git`, so the directory the deck
//! is launched in must be a git repository with at least one commit before the
//! form is submitted. `TuiDeck::try_launch_inner` already runs `git init
//! --quiet` in the copied fixture unconditionally (some deck paths probe
//! `.git`) — this test adds the one thing that's missing, a commit, using the
//! same inline-identity pattern as `init_remote_with_orch_toml`
//! (`tests/e2e_issue_dispatch.rs`) and `worktree_004`'s fixture
//! (`src/ui.rs`), since CI runners carry no global git config.
//!
//! Uses the `orch-worktree` fixture: a single orchestration with two roles,
//! each dumping its OWN `pwd` to a role-named log file before staying alive on
//! `sleep` — no real LLM tokens spent. Reading the log CONTENT (not just its
//! presence) is what proves each role pane was spawned rooted in the created
//! worktree rather than the fixture directory itself.

mod common;

use std::path::PathBuf;
use std::time::Duration;

use common::TuiDeck;
use spec::spec;

/// Run a `git` subcommand against `dir`, panicking on non-zero exit — mirrors
/// `tests/e2e_issue_dispatch.rs::run_git`.
fn run_git(dir: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed in {dir:?}");
}

/// Commit the fixture's own `.dot-agent-deck.toml` into `dir` (which the
/// harness already ran `git init --quiet` on) so `git worktree add -b` — fork
/// #122's `create_worktree_sync` — has a ref to branch from. Identity pinned
/// inline: CI runners carry no global `user.email`/`user.name`. Adds that ONE
/// explicit path rather than `-A`/`.` — by the time this runs, `dir` also
/// holds the harness's own per-test `home/` dir and its hook/attach Unix
/// sockets, which a blanket add would try (and for the sockets, fail) to walk.
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

/// Resolve the sibling worktree path fork #122's `resolve_orchestration_worktree_path`
/// (`src/ui.rs`) would produce for `dir` + `slug`: `<dir>-<slug>`, next to `dir`
/// itself. Duplicated here deliberately rather than made `pub(crate)` and
/// imported — this test asserts the OBSERVABLE path a user would find on disk,
/// not the production helper's internals.
fn sibling_worktree_path(dir: &std::path::Path, slug: &str) -> PathBuf {
    let mut name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.push('-');
    name.push_str(slug);
    dir.with_file_name(name)
}

/// Resolve `worktree_dir`'s git metadata dir via `git rev-parse --git-dir` —
/// the same resolution `mark_worktree_owned` (`src/worktree_reclaim.rs`)
/// uses to place the `dot-agent-deck-owner` marker outside the working
/// tree, so writing it can never leave the worktree permanently dirty.
/// Mirrors `create_worktree_records_creator_identity`'s helper
/// (`src/issue_dispatch_run.rs`), which proves the same marker on the async
/// path.
fn resolve_git_dir(worktree_dir: &std::path::Path) -> PathBuf {
    let out = std::process::Command::new("git")
        .current_dir(worktree_dir)
        .args(["rev-parse", "--git-dir"])
        .output()
        .expect("git rev-parse --git-dir must spawn");
    assert!(
        out.status.success(),
        "git rev-parse --git-dir failed in {worktree_dir:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let git_dir = PathBuf::from(raw);
    if git_dir.is_absolute() {
        git_dir
    } else {
        worktree_dir.join(git_dir)
    }
}

/// Scenario: launch the deck in the `orch-worktree` fixture (a real git repo
/// after `commit_fixture`), then drive the exact user-facing keyboard path:
/// `Ctrl+n` -> directory picker (Space confirms cwd) -> new-pane form -> Right
/// selects the fixture's one orchestration -> Enter (Mode -> Name) -> Tab
/// (Name -> Worktree, since selecting an orchestration hides the Command
/// field) -> type a slug -> Enter submits. Assert the resolved sibling
/// worktree directory exists on disk, and that BOTH role panes' `pwd` logs —
/// written by each role's own shell command before it goes to sleep — report
/// the worktree path, not the fixture directory the deck was launched in.
/// Also assert the `dot-agent-deck-owner` marker (issue #425) written into
/// the worktree's git metadata dir records `created-by:
/// orchestration:<launch-dir-basename>` — on this keyboard path the Name
/// field is never typed into, so it keeps the value
/// `transition_after_dir_pick` (`src/ui.rs`) pre-filled it with: the basename
/// of the launch directory the form was opened for. That typed-Name value
/// takes precedence over `orch_config.name` at the `create_worktree_sync`
/// call site, so it — not the fixture's config name `worktree-demo` — is the
/// creator this path actually derives.
#[spec("orchestration/worktree/005")]
#[test]
fn worktree_005_form_worktree_field_creates_and_roots_role_panes_on_real_binary() {
    const SLUG: &str = "worktree005";

    let deck = TuiDeck::launch_with_fixture("orch-worktree");
    let work = deck.workdir().to_path_buf();
    commit_fixture(&work);

    deck.wait_for_string("No active sessions");

    deck.send_keys(b"\x0e"); // Ctrl+n -> directory picker
    deck.send_keys(b" "); // Space -> confirm current dir -> new-pane form
    deck.wait_for_string("No mode"); // form up, Mode field focused at "No mode"
    deck.send_keys(b"\x1b[C"); // Right -> [Orch: worktree-demo] (the fixture's only orchestration)
    deck.send_keys(b"\r"); // Mode -> Name
    deck.send_keys(b"\t"); // Tab: Name -> Worktree (Command is hidden for an orchestration)
    deck.send_keys(SLUG.as_bytes());
    deck.send_keys(b"\r"); // submit

    deck.wait_for_absence("New Agent"); // new-pane form closed -> tab up

    let worktree = sibling_worktree_path(&work, SLUG);
    assert!(
        common::wait_for_path(&worktree, Duration::from_secs(15)),
        "submitting the form with a typed Worktree slug must create the \
         resolved sibling worktree {} on disk\n=== rendered grid ===\n{}",
        worktree.display(),
        deck.snapshot_grid()
    );

    // fork issue #425 follow-up: the Name field was never typed into on this
    // keyboard path (Mode -> Name -> Tab straight to Worktree), so it keeps
    // its pre-filled value -- the launch directory's basename, set by
    // `transition_after_dir_pick` (`src/ui.rs`) when the form opened. That
    // typed-Name value takes precedence over `orch_config.name` at the
    // `create_worktree_sync` call site (`src/ui.rs`), so the creator this
    // path actually derives is `orchestration:<launch-dir basename>`, not the
    // fixture's config name `worktree-demo`. Compute the expected basename
    // from the launch dir this test itself set up, rather than hardcoding it.
    let launch_dir_basename = work
        .file_name()
        .expect("launch dir must have a basename")
        .to_string_lossy()
        .into_owned();
    let expected_creator = format!("created-by: orchestration:{launch_dir_basename}");
    let git_dir = resolve_git_dir(&worktree);
    let marker = std::fs::read_to_string(
        git_dir.join(dot_agent_deck::worktree_reclaim::OWNER_MARKER_FILENAME),
    )
    .expect("ownership marker must exist and be readable in the worktree's git-dir");
    assert!(
        marker.contains(&expected_creator),
        "the ownership marker written through the real keyboard path must \
         record the creator `src/ui.rs` actually derives from the launch \
         directory's basename (the Name field's pre-filled value), expected \
         {expected_creator:?}, got {marker:?}"
    );

    let orchestrator_log = worktree.join("pwd-orchestrator.log");
    let worker_log = worktree.join("pwd-worker.log");
    assert!(
        common::wait_for_path(&orchestrator_log, Duration::from_secs(15)),
        "the orchestrator role pane never wrote its pwd log inside the \
         worktree — expected it spawned rooted there\n=== rendered grid ===\n{}",
        deck.snapshot_grid()
    );
    assert!(
        common::wait_for_path(&worker_log, Duration::from_secs(15)),
        "the worker role pane never wrote its pwd log inside the worktree — \
         expected it spawned rooted there\n=== rendered grid ===\n{}",
        deck.snapshot_grid()
    );

    let canonical_worktree = worktree
        .canonicalize()
        .expect("created worktree must resolve to a real path");
    for (role, log) in [("orchestrator", &orchestrator_log), ("worker", &worker_log)] {
        let recorded = std::fs::read_to_string(log)
            .unwrap_or_else(|e| panic!("read {role}'s pwd log {log:?}: {e}"));
        let recorded_path = std::path::Path::new(recorded.trim());
        let canonical_recorded = recorded_path.canonicalize().unwrap_or_else(|e| {
            panic!("canonicalize {role}'s recorded pwd {recorded_path:?}: {e}")
        });
        assert_eq!(
            canonical_recorded, canonical_worktree,
            "the {role} role pane's own `pwd` (recorded as {recorded:?}) must \
             resolve to the created worktree, not the fixture directory the \
             deck was launched in — every role pane must be rooted in the \
             orchestration's own worktree"
        );
    }
}
