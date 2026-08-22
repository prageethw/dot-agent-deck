#![cfg(feature = "e2e")]

//! L2 real-binary proof of fork #122's worktree-provisioning feature and PRD
//! fork#544 M2/M2b's naming-derivation and unconditional-isolation
//! follow-up, the CLAUDE.md rule 4 PTY-attached tests for both.
//! `orchestration/worktree/002`-`004` (`src/ui.rs` unit tests) each
//! characterize one mechanism in isolation — the fail-loud refusal, the
//! pre-existing cwd-threading, the actual on-disk creation via
//! `dispatch_action` — but none of them drives the real keyboard path a user
//! actually types: `Ctrl+n` -> directory picker -> form -> cycle Mode to an
//! orchestration -> type/clear the Name -> submit. `orchestration/workspace/001`
//! /`002` below are what drive that path on the real binary post-M2/M2b (the
//! separate Worktree-slug field `orchestration/worktree/005` used to exercise
//! is retired outright — Name is now the sole input to the resolved path).
//!
//! Provisioning shells out to real `git`, so the directory the deck is
//! launched in must be a git repository with at least one commit before the
//! form is submitted. `TuiDeck::try_launch_inner` already runs `git init
//! --quiet` in the copied fixture unconditionally (some deck paths probe
//! `.git`) — every test here adds the one thing that's missing, a commit,
//! using the same inline-identity pattern as `init_remote_with_orch_toml`
//! (`tests/e2e_issue_dispatch.rs`) and `worktree_004`'s fixture (`src/ui.rs`),
//! since CI runners carry no global git config.
//!
//! `orchestration/worktree/014`/`016` and `orchestration/workspace/001`/`002`
//! all reuse the `orch-clone-gate` fixture: a single orchestration whose one
//! role appends its own `DOT_AGENT_DECK_WORKTREE_OWNER` identity plus `pwd`
//! to one shared, HOME-relative log file — the after-the-fact "where did
//! this instance actually land on disk, and under which identity" evidence
//! every test in this family needs.

mod common;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use common::{TuiDeck, commit_fixture, run_git};
use spec::spec;

/// Resolve `dir`'s git COMMON dir via `git -C dir rev-parse --git-common-dir`
/// — for a linked worktree this resolves to the MAIN repository's `.git` (the
/// same value every other worktree of that repository resolves to); for a
/// genuinely separate clone it resolves to that clone's own `.git`. This is
/// the observable signature PRD fork#325 M3's gate — and, since PRD
/// fork#544 M2b, EVERY orchestration's own isolation — is about: whether an
/// orchestration shares the launch directory's object store (never, as of
/// M2b) or gets its own (always). Asserts the OBSERVABLE path a user/tool
/// would see, not a shared internal helper.
fn git_common_dir(dir: &std::path::Path) -> PathBuf {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .expect("git rev-parse --git-common-dir must spawn");
    assert!(
        out.status.success(),
        "git rev-parse --git-common-dir failed in {dir:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let common_dir = PathBuf::from(raw);
    let resolved = if common_dir.is_absolute() {
        common_dir
    } else {
        dir.join(common_dir)
    };
    resolved
        .canonicalize()
        .unwrap_or_else(|e| panic!("canonicalize git common dir {resolved:?} for {dir:?}: {e}"))
}

/// Read `dir`'s currently checked-out branch name via `git rev-parse
/// --abbrev-ref HEAD` — the observable proof that issue #325 reviewer P1-2's
/// fix landed the isolated clone on the user's typed slug rather than
/// silently staying on the source's HEAD branch.
fn current_branch(dir: &std::path::Path) -> String {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .expect("git rev-parse --abbrev-ref HEAD must spawn");
    assert!(
        out.status.success(),
        "git rev-parse --abbrev-ref HEAD failed in {dir:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Read `dir`'s configured `origin` remote URL, or `None` when no `origin`
/// is configured at all — the observable proof of issue #325 reviewer
/// P1-1's fix: the clone's `origin` must be the SOURCE's own origin URL
/// (when the source has one), never the local filesystem path a plain `git
/// clone` defaults to.
fn remote_origin_url(dir: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(["remote", "get-url", "origin"])
        .output()
        .expect("git remote get-url origin must spawn");
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Drive the new-pane form's real keyboard path to open the `orch-clone-gate`
/// fixture's one orchestration with a TYPED Name — `Ctrl+n` -> directory
/// picker -> confirm -> Mode cycled to the orchestration -> clear the
/// pre-filled suggested Name -> type `name` -> submit directly from Name
/// (Command is hidden for an orchestration). PRD fork#544 M2 retires the
/// separate Worktree-slug field this helper used to type into — Name is now
/// the sole input to the resolved path, so a distinct typed Name is what
/// this family of tests needs instead of a distinct typed slug.
fn open_orchestration_with_name(deck: &TuiDeck, name: &str) {
    deck.send_keys(b"\x0e"); // Ctrl+n -> directory picker
    deck.send_keys(b" "); // Space -> confirm current dir -> new-pane form
    deck.wait_for_string("No mode"); // form up, Mode field focused at "No mode"
    deck.send_keys(b"\x1b[C"); // Right -> [Orch: clone-gate-demo] (the fixture's only orchestration)
    deck.send_keys(b"\r"); // Mode -> Name
    // Backspace x80 -> clear the pre-filled suggested Name (fork#192 M1.0);
    // extra pops on an already-empty field are no-ops, same idiom as
    // `workspace_001`/`002` below.
    deck.send_keys(&[0x7fu8; 80]);
    deck.send_keys(name.as_bytes());
    deck.send_keys(b"\r"); // submit
}

/// Scenario: launch the deck in the `orch-clone-gate` fixture and open its
/// one orchestration TWICE against the SAME directory — with distinct typed
/// Names, the second one while the first is still live — mirroring the
/// N-concurrent-orchestrations shape of issue #325's actual incident
/// (three-plus, here reduced to the PRD fork#325 M3 starting case: the
/// simplest 2nd-against-a-live-1st collision). Each instance's role pane
/// appends its own `DOT_AGENT_DECK_WORKTREE_OWNER` identity plus its own
/// `pwd` to one shared, HOME-relative log file, so the 1st and 2nd
/// instances' actual working directories can be read back and told apart by
/// owner string once both role panes have run — without this test needing to
/// know in advance where either instance's worktree/clone lands on disk.
/// PRD fork#544 M2b inverts this test's original "1st shares, 2nd isolates"
/// shape: isolation is now unconditional, so BOTH instances' directories
/// must have their own isolated clone (a distinct git object store) —
/// neither may share the launch directory's, and the two must not share
/// each other's either.
#[spec("orchestration/worktree/014")]
#[test]
fn worktree_014_nth_concurrent_orchestration_gets_isolated_clone() {
    let deck = TuiDeck::launch_with_fixture("orch-clone-gate");
    let work = deck.workdir().to_path_buf();
    commit_fixture(&work);
    // Issue #325 reviewer P1-1: give the source checkout a real `origin` so
    // the isolated clone's origin-URL fixup below has something to prove —
    // never fetched/pushed, just read back with `git remote get-url`.
    let fake_origin = "https://example.invalid/orch-clone-gate-fixture.git";
    run_git(&work, &["remote", "add", "origin", fake_origin]);

    deck.wait_for_string("No active sessions");

    // PRD fork#544 M2: the typed Name is now the sole input to both the
    // resolved workspace path and the creator/owner identity — no more
    // reading the pre-filled `<basename>-orchestrator-N` suggestion, since
    // this helper clears and types over it.
    let owner_1 = "orchestration:clonegate1".to_string();
    let owner_2 = "orchestration:clonegate2".to_string();
    let second_label = " clonegate2 ";

    open_orchestration_with_name(&deck, "clonegate1");
    deck.wait_for_absence("New Agent"); // first orchestration's form closed -> tab up

    // Back to the Dashboard to open a second orchestration in the same dir,
    // while the first is still live.
    deck.send_keys(b"\x04"); // Ctrl+D -> Normal mode (still on the orchestration tab)
    deck.send_keys(b"\x1b[D"); // Left -> previous tab -> Dashboard
    deck.wait_for_string("session(s)");

    open_orchestration_with_name(&deck, "clonegate2");
    deck.wait_for_string(second_label); // second orchestration deck is up, distinctly labeled

    let log_path = deck.home_dir().join("clone-gate-pwd.log");
    common::wait_for_file_lines(&log_path, 2, Duration::from_secs(15)).unwrap_or_else(|e| {
        panic!(
            "both role panes must have appended their owner+pwd line to \
             {log_path:?}: {e}\n=== rendered grid ===\n{}",
            deck.snapshot_grid()
        )
    });

    let contents =
        std::fs::read_to_string(&log_path).unwrap_or_else(|e| panic!("read {log_path:?}: {e}"));
    let mut pwd_by_owner: HashMap<String, PathBuf> = HashMap::new();
    for line in contents.lines() {
        if let Some((owner, pwd)) = line.split_once(' ') {
            pwd_by_owner.insert(owner.to_string(), PathBuf::from(pwd));
        }
    }

    let pwd_1 = pwd_by_owner.get(&owner_1).unwrap_or_else(|| {
        panic!("no line in {log_path:?} for owner {owner_1:?}; got: {contents:?}")
    });
    let pwd_2 = pwd_by_owner.get(&owner_2).unwrap_or_else(|| {
        panic!("no line in {log_path:?} for owner {owner_2:?}; got: {contents:?}")
    });

    let work_common = git_common_dir(&work);
    let pwd_1_common = git_common_dir(pwd_1);
    let pwd_2_common = git_common_dir(pwd_2);

    assert_ne!(
        pwd_1_common,
        work_common,
        "PRD fork#544 M2b: isolation is unconditional now — even the 1ST \
         orchestration against a root checkout must provision its own \
         isolated clone, never a `git worktree add` sibling sharing {}'s \
         own object store",
        work.display()
    );
    assert_ne!(
        pwd_2_common,
        work_common,
        "the 2ND (Nth-concurrent) orchestration against the SAME root \
         checkout, spawned while the 1st is still live, must provision its \
         OWN isolated clone — a distinct git common dir/object store — \
         instead of sharing {}'s",
        work.display()
    );
    assert_ne!(
        pwd_1_common, pwd_2_common,
        "the 1st and 2nd orchestrations must not share EACH OTHER's isolated \
         clone either — each gets its own separate object store"
    );

    // Issue #325 reviewer P1-2: both must land on the Name the user typed,
    // not silently stay on the source's HEAD branch — the absence of this
    // assertion is exactly what let the isolated arm's dropped `branch`
    // argument through a green suite.
    assert_eq!(
        current_branch(pwd_1),
        "clonegate1",
        "the 1st orchestration must be checked out on its typed Name's branch"
    );
    assert_eq!(
        current_branch(pwd_2),
        "clonegate2",
        "the 2nd orchestration must be checked out on its typed Name's \
         branch, not the source's HEAD branch — this is issue #325 \
         reviewer P1-2's fix"
    );

    // Issue #325 reviewer P1-1: every isolated clone's `origin` must be the
    // SOURCE's own origin URL, never the local filesystem path a plain
    // `git clone` defaults `origin` to — a `git push origin` from the clone
    // would otherwise land silently in `work`, the user's own root checkout.
    assert_eq!(
        remote_origin_url(pwd_1).as_deref(),
        Some(fake_origin),
        "the 1st orchestration's isolated clone must carry the source \
         checkout's own origin URL, not a local path pointing back at {}",
        work.display()
    );
    assert_eq!(
        remote_origin_url(pwd_2).as_deref(),
        Some(fake_origin),
        "the 2nd orchestration's isolated clone must carry the source \
         checkout's own origin URL, not a local path pointing back at {}",
        work.display()
    );
}

/// Read `dir`'s HEAD commit SHA via `git rev-parse HEAD` — used to prove a
/// directory's git state is byte-identical before and after an operation
/// performed against a DIFFERENT orchestration's directory.
fn head_sha(dir: &std::path::Path) -> String {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git rev-parse HEAD must spawn");
    assert!(
        out.status.success(),
        "git rev-parse HEAD failed in {dir:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Read whether `dir` is currently a shallow repository via `git rev-parse
/// --is-shallow-repository` — the exact observable signature issue #325's
/// own repro used (`$ git rev-parse --is-shallow-repository` / `true`) to
/// detect the shared-object-store corruption from failure #2.
fn is_shallow_repository(dir: &std::path::Path) -> bool {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(["rev-parse", "--is-shallow-repository"])
        .output()
        .expect("git rev-parse --is-shallow-repository must spawn");
    assert!(
        out.status.success(),
        "git rev-parse --is-shallow-repository failed in {dir:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim() == "true"
}

/// Attempt `git worktree remove --force <target>` FROM `from_dir` — i.e.
/// against whichever repository `from_dir` itself belongs to. This is the
/// exact shape of issue #325 failure #1 ("something else ran a plain `git
/// worktree remove` … against paths it had no ownership claim on"). Returns
/// the exit status and captured stderr rather than panicking either way: a
/// SUCCESSFUL removal here would itself be the regression this test exists
/// to catch, so the caller decides what the result means.
fn attempt_worktree_remove(from_dir: &std::path::Path, target: &std::path::Path) -> (bool, String) {
    let out = std::process::Command::new("git")
        .current_dir(from_dir)
        // `--` before the path matches the end-of-options separator
        // `worktree_remove_argv` (`src/issue_dispatch.rs`) deliberately
        // carries — issue #325 auditor A7. Not exploitable here (every
        // path comes from `$(pwd)` and is absolute, never dash-prefixed),
        // kept for consistency with the pattern this repo already
        // established for this exact command.
        .args(["worktree", "remove", "--force", "--"])
        .arg(target)
        .output()
        .expect("git worktree remove must spawn");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Navigate from the currently active tab back to the Dashboard (index 0).
/// `Ctrl+D` enters Normal mode; `Left` (`Action::CycleTabPrev`) steps the
/// active tab index back by exactly one, wrapping — it is NOT a direct jump
/// to Dashboard (`src/ui.rs`'s `CycleTabPrev` handler:
/// `(prev_idx + count - 1) % count`) — so reaching Dashboard from the Nth
/// tab (tabs are strictly appended, never reordered — `src/tab.rs`'s three
/// `self.tabs.push` call sites) needs exactly `steps_back` presses: 1 from
/// the 1st-opened orchestration tab, 2 from the 2nd, etc.
///
/// This is a fixed-count navigation with no confirmation that it actually
/// landed on Dashboard: `Tab::Dashboard` and `Tab::Orchestration` render
/// through the identical `FrameContent::Cards` arm (`src/ui.rs`'s
/// `render_frame`), so no on-screen text distinguishes them, and the active
/// tab is marked only by cell style (`src/ui.rs`'s tab-bar rendering), which
/// the vt100 harness's plain-text `snapshot_grid`/`wait_for_string` cannot
/// see. A wrong step count is harmless regardless: `Ctrl+n`'s directory
/// picker roots at the deck process's own `std::env::current_dir()`
/// (`src/ui.rs`), not at anything tab-scoped, so `open_orchestration_with_name`
/// resolves to the same launch directory no matter which tab is active when
/// it runs.
fn go_to_dashboard(deck: &TuiDeck, steps_back: usize) {
    deck.send_keys(b"\x04"); // Ctrl+D -> Normal mode (still on the current tab)
    for _ in 0..steps_back {
        deck.send_keys(b"\x1b[D"); // Left -> CycleTabPrev, one step back
    }
}

/// Scenario: launch the deck in the `orch-clone-gate` fixture and open its
/// one orchestration THREE times against the SAME directory — with distinct
/// typed Names, the 2nd and 3rd each while an earlier one is still live —
/// reproducing issue #325's own original incident shape verbatim (the
/// issue's own words: "three-plus concurrent orchestrations... on one
/// deck"), not `014`'s reduced 2-orchestration starting case. PRD fork#544
/// M2b: isolation is unconditional now, so ALL THREE — not just the 2nd and
/// 3rd — must land on their own distinct git common dir; no two of the
/// three, including the launch directory itself, may share one. Then
/// reproduces the issue's own two concrete failures directly and proves
/// neither can recur: (1) with all three live, attempts `git worktree
/// remove` from one orchestration's repository targeting another's
/// directory — in every direction the incident named — must now fail
/// outright with a `… is not a working tree` stderr (the target isn't even
/// registered in the attacking repo's worktree list), leaving the target's
/// directory, branch, and HEAD commit completely undisturbed — reframed from
/// `014`'s pre-M2b "1st shares, Nth isolates" shape to "the protection holds
/// between any N isolated clones", which is what M2b's unconditional
/// isolation actually produces; (2) a `git fetch --depth 1` performed in the
/// 1st orchestration's OWN isolated clone makes ONLY that clone's object
/// store shallow — since M2b it no longer shares the launch directory's
/// common dir, so the launch directory itself, and the 2nd/3rd
/// orchestrations' own isolated clones, must all remain unaffected and their
/// HEAD commits byte-identical to before the fetch.
#[spec("orchestration/worktree/016")]
#[test]
fn worktree_016_three_concurrent_orchestrations_reproduce_325_incident_shape_without_colliding() {
    let deck = TuiDeck::launch_with_fixture("orch-clone-gate");
    let work = deck.workdir().to_path_buf();
    commit_fixture(&work);
    let fake_origin = "https://example.invalid/orch-clone-gate-fixture.git";
    run_git(&work, &["remote", "add", "origin", fake_origin]);

    deck.wait_for_string("No active sessions");

    // PRD fork#544 M2: the typed Name is now the sole input to both the
    // resolved workspace path and the creator/owner identity.
    let owner_1 = "orchestration:clonegate1".to_string();
    let owner_2 = "orchestration:clonegate2".to_string();
    let owner_3 = "orchestration:clonegate3".to_string();
    let second_label = " clonegate2 ";
    let third_label = " clonegate3 ";

    // 1st orchestration — PRD fork#544 M2b: isolates too, same as 2nd/3rd.
    open_orchestration_with_name(&deck, "clonegate1");
    deck.wait_for_absence("New Agent");
    go_to_dashboard(&deck, 1); // orch1 (index 1) -> Dashboard (index 0)

    // 2nd, while the 1st is still live: must get its own isolated clone.
    open_orchestration_with_name(&deck, "clonegate2");
    deck.wait_for_string(second_label);
    go_to_dashboard(&deck, 2); // orch2 (index 2) -> orch1 (1) -> Dashboard (0)

    // 3rd, while BOTH the 1st and 2nd are still live: this is #325's own
    // actual incident shape (three-plus concurrent, not two).
    open_orchestration_with_name(&deck, "clonegate3");
    deck.wait_for_string(third_label);

    let log_path = deck.home_dir().join("clone-gate-pwd.log");
    common::wait_for_file_lines(&log_path, 3, Duration::from_secs(15)).unwrap_or_else(|e| {
        panic!(
            "all three role panes must have appended their owner+pwd line to \
             {log_path:?}: {e}\n=== rendered grid ===\n{}",
            deck.snapshot_grid()
        )
    });

    let contents =
        std::fs::read_to_string(&log_path).unwrap_or_else(|e| panic!("read {log_path:?}: {e}"));
    let mut pwd_by_owner: HashMap<String, PathBuf> = HashMap::new();
    for line in contents.lines() {
        if let Some((owner, pwd)) = line.split_once(' ') {
            pwd_by_owner.insert(owner.to_string(), PathBuf::from(pwd));
        }
    }

    let pwd_1 = pwd_by_owner.get(&owner_1).unwrap_or_else(|| {
        panic!("no line in {log_path:?} for owner {owner_1:?}; got: {contents:?}")
    });
    let pwd_2 = pwd_by_owner.get(&owner_2).unwrap_or_else(|| {
        panic!("no line in {log_path:?} for owner {owner_2:?}; got: {contents:?}")
    });
    let pwd_3 = pwd_by_owner.get(&owner_3).unwrap_or_else(|| {
        panic!("no line in {log_path:?} for owner {owner_3:?}; got: {contents:?}")
    });

    // --- Structural proof: no two of the four (the launch dir plus all
    // three orchestrations) share a git common dir — PRD fork#544 M2b's
    // unconditional isolation means the 1st is no longer the exception. ---
    let work_common = git_common_dir(&work);
    let pwd_1_common = git_common_dir(pwd_1);
    let pwd_2_common = git_common_dir(pwd_2);
    let pwd_3_common = git_common_dir(pwd_3);

    assert_ne!(
        pwd_1_common,
        work_common,
        "PRD fork#544 M2b: the 1ST orchestration must provision its own \
         isolated clone too, not share the launch dir's ({}) object store",
        work.display()
    );
    assert_ne!(
        pwd_2_common,
        work_common,
        "the 2ND orchestration must provision its OWN isolated clone, not \
         share the launch dir's ({}) object store",
        work.display()
    );
    assert_ne!(
        pwd_3_common,
        work_common,
        "the 3RD orchestration must provision its OWN isolated clone too, \
         not share the launch dir's ({}) object store",
        work.display()
    );
    assert_ne!(
        pwd_1_common, pwd_2_common,
        "the 1ST and 2ND orchestrations must not share each other's \
         isolated clone either — each gets its own separate object store"
    );
    assert_ne!(
        pwd_1_common, pwd_3_common,
        "the 1ST and 3RD orchestrations must not share each other's \
         isolated clone either — each gets its own separate object store"
    );
    assert_ne!(
        pwd_2_common, pwd_3_common,
        "the 2ND and 3RD orchestrations must NOT share each other's \
         isolated clone either — each gets its own separate object store"
    );

    assert_eq!(
        current_branch(pwd_1),
        "clonegate1",
        "the 1st orchestration must be on its typed Name's branch"
    );
    assert_eq!(
        current_branch(pwd_2),
        "clonegate2",
        "the 2nd orchestration must be on its typed Name's branch"
    );
    assert_eq!(
        current_branch(pwd_3),
        "clonegate3",
        "the 3rd orchestration must be on its typed Name's branch"
    );
    assert_eq!(
        remote_origin_url(pwd_1).as_deref(),
        Some(fake_origin),
        "the 1st orchestration's isolated clone must carry the source's own origin URL"
    );
    assert_eq!(
        remote_origin_url(pwd_2).as_deref(),
        Some(fake_origin),
        "the 2nd orchestration's isolated clone must carry the source's own origin URL"
    );
    assert_eq!(
        remote_origin_url(pwd_3).as_deref(),
        Some(fake_origin),
        "the 3rd orchestration's isolated clone must carry the source's own origin URL"
    );

    let work_head_baseline = head_sha(&work);
    let pwd_1_head_baseline = head_sha(pwd_1);
    let pwd_2_head_baseline = head_sha(pwd_2);
    let pwd_3_head_baseline = head_sha(pwd_3);

    // Any cross-repo `git worktree remove` here must fail specifically
    // because the target is not registered in the attacking repo's
    // worktree list — not merely fail to succeed for some unrelated reason
    // (a lock, a permissions error, a future git tightening `--force`),
    // which would let a genuine isolation regression still read green
    // (issue #325 reviewer P2-2 / auditor F1).
    const NOT_A_WORKING_TREE: &str = "is not a working tree";

    // --- Behavioral proof of failure #1: "an orchestration deleted five
    // worktrees it did not own, mid-use". Attempt cross-repo `git worktree
    // remove` in every direction the incident's own report names, and
    // between every pair of the three orchestrations' own isolated clones
    // too — reframed from the pre-M2b "1st shares, Nth isolates" shape to
    // "the protection holds between any N isolated clones", since PRD
    // fork#544 M2b's unconditional isolation means all three (not just the
    // 2nd and 3rd) are now isolated clones. Every attempt must fail outright
    // (the target is not even registered in the attacking repo's worktree
    // list), and every target must remain completely intact afterward. ---
    let (removed_2_from_1, stderr_2_from_1) = attempt_worktree_remove(pwd_1, pwd_2);
    assert!(
        !removed_2_from_1,
        "`git worktree remove` run from the 1st orchestration's directory \
         must NOT be able to remove the 2nd's isolated clone at {} — this \
         is issue #325 failure #1, and it succeeding here would mean the \
         isolation regressed\nstderr: {}",
        pwd_2.display(),
        stderr_2_from_1
    );
    assert!(
        stderr_2_from_1.contains(NOT_A_WORKING_TREE),
        "the removal must fail specifically because the 2nd orchestration's \
         clone is not registered in the 1st's worktree list, got: {}",
        stderr_2_from_1
    );

    let (removed_3_from_1, stderr_3_from_1) = attempt_worktree_remove(pwd_1, pwd_3);
    assert!(
        !removed_3_from_1,
        "`git worktree remove` run from the 1st orchestration's directory \
         must NOT be able to remove the 3rd's isolated clone at {}\nstderr: {}",
        pwd_3.display(),
        stderr_3_from_1
    );
    assert!(
        stderr_3_from_1.contains(NOT_A_WORKING_TREE),
        "the removal must fail specifically because the 3rd orchestration's \
         clone is not registered in the 1st's worktree list, got: {}",
        stderr_3_from_1
    );

    let (removed_3_from_2, stderr_3_from_2) = attempt_worktree_remove(pwd_2, pwd_3);
    assert!(
        !removed_3_from_2,
        "`git worktree remove` run from the 2nd orchestration's isolated \
         clone must NOT be able to remove the 3rd's isolated clone at {} — \
         two Nth-concurrent orchestrations must be isolated from EACH \
         OTHER, not just from the 1st\nstderr: {}",
        pwd_3.display(),
        stderr_3_from_2
    );
    assert!(
        stderr_3_from_2.contains(NOT_A_WORKING_TREE),
        "the removal must fail specifically because the 3rd orchestration's \
         clone is not registered in the 2nd's worktree list, got: {}",
        stderr_3_from_2
    );

    // The direction issue #325 actually reported: one orchestration's clone
    // attacking another's, including the 1st's — the higher-consequence
    // direction the incident named, now applicable to the 1st too since it
    // is no longer the launch directory's own worktree.
    let (removed_1_from_2, stderr_1_from_2) = attempt_worktree_remove(pwd_2, pwd_1);
    assert!(
        !removed_1_from_2,
        "`git worktree remove` run from the 2nd orchestration's isolated \
         clone must NOT be able to remove the 1st orchestration's isolated \
         clone at {} — this is the exact direction issue #325 reported: an \
         orchestration deleting a worktree/clone it did not own\nstderr: {}",
        pwd_1.display(),
        stderr_1_from_2
    );
    assert!(
        stderr_1_from_2.contains(NOT_A_WORKING_TREE),
        "the removal must fail specifically because the 1st orchestration's \
         worktree is not registered in the 2nd's worktree list, got: {}",
        stderr_1_from_2
    );

    // The scenario issue #325 reported LITERALLY: an orchestration deleting
    // a worktree belonging to the ROOT CHECKOUT itself, not to another
    // orchestration's clone. Under M2b every orchestration is its own
    // isolated clone, so this is the one direction the three pairwise
    // attempts above don't already cover.
    let (removed_work_from_1, stderr_work_from_1) = attempt_worktree_remove(pwd_1, &work);
    assert!(
        !removed_work_from_1,
        "`git worktree remove` run from the 1st orchestration's isolated \
         clone must NOT be able to remove the LAUNCH DIRECTORY's own \
         worktree at {} — this is issue #325's original incident shape: an \
         orchestration deleting worktrees belonging to the root checkout it \
         did not own\nstderr: {}",
        work.display(),
        stderr_work_from_1
    );
    assert!(
        stderr_work_from_1.contains(NOT_A_WORKING_TREE),
        "the removal must fail specifically because the launch directory is \
         not registered in the 1st orchestration's worktree list, got: {}",
        stderr_work_from_1
    );
    assert!(
        work.is_dir(),
        "the launch directory at {} must still exist after the failed \
         removal attempt targeting it",
        work.display()
    );
    assert_eq!(
        head_sha(&work),
        work_head_baseline,
        "the launch directory's HEAD commit must be byte-identical to \
         before the failed removal attempt — nothing in its git state was \
         disturbed"
    );

    for (label, pwd, expected_branch, expected_head) in [
        ("1st", pwd_1, "clonegate1", &pwd_1_head_baseline),
        ("2nd", pwd_2, "clonegate2", &pwd_2_head_baseline),
        ("3rd", pwd_3, "clonegate3", &pwd_3_head_baseline),
    ] {
        assert!(
            pwd.is_dir(),
            "the {label} orchestration's directory at {} must still exist \
             after the failed removal attempts targeting it",
            pwd.display()
        );
        assert_eq!(
            &current_branch(pwd),
            expected_branch,
            "the {label} orchestration's checked-out branch must be \
             undisturbed by the failed removal attempts"
        );
        assert_eq!(
            &head_sha(pwd),
            expected_head,
            "the {label} orchestration's HEAD commit must be byte-identical \
             to before the failed removal attempts — nothing in its git \
             state was disturbed"
        );
    }

    // --- Behavioral proof of failure #2: "an orchestration made the shared
    // repository shallow, breaking merges in every worktree". Perform a real
    // `git fetch --depth 1` in the 1st orchestration's own isolated clone —
    // the exact operation the issue reported — and confirm the LAUNCH
    // DIRECTORY itself, plus the 2nd and 3rd orchestrations' isolated
    // clones, are all structurally unable to be affected: PRD fork#544 M2b
    // means none of them shares the 1st's object store any more, which
    // inverts this test's pre-M2b shape (a shallow fetch in the 1st USED to
    // reach the launch directory, since the two shared a common dir; now it
    // must not). ---
    let fake_upstream_root = common::race_safe_tempdir();
    let fake_upstream = fake_upstream_root.path().join("fake-upstream");
    run_git(
        fake_upstream_root.path(),
        &[
            "clone",
            "-q",
            work.to_str().expect("work path must be valid UTF-8"),
            fake_upstream
                .to_str()
                .expect("fake upstream path must be valid UTF-8"),
        ],
    );
    // Extra commits so the depth-1 fetch below actually truncates real
    // history, mirroring the issue's own upstream/main having far more
    // history than a depth-1 fetch would carry over.
    for msg in ["extra-upstream-1", "extra-upstream-2"] {
        run_git(
            &fake_upstream,
            &[
                "-c",
                "user.email=test@example.com",
                "-c",
                "user.name=Test",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                msg,
            ],
        );
    }

    assert!(
        !is_shallow_repository(&work),
        "the launch directory's checkout must not already be shallow before \
         the reproduction fetch"
    );
    run_git(
        pwd_1,
        &[
            "fetch",
            "--depth",
            "1",
            fake_upstream
                .to_str()
                .expect("fake upstream path must be valid UTF-8"),
            "HEAD",
        ],
    );

    assert!(
        is_shallow_repository(pwd_1),
        "the depth-1 fetch performed in the 1st orchestration's own \
         directory must have made ITS repository shallow — otherwise this \
         reproduction never exercised the issue's own failure mode at all"
    );
    assert!(
        !is_shallow_repository(&work),
        "PRD fork#544 M2b: the 1st orchestration no longer shares the \
         launch directory's git common dir (it is its own isolated clone \
         now), so the shallow fetch performed inside it must be \
         structurally unable to reach the launch directory — the exact \
         inverse of what issue #325 originally reported, and the whole \
         point of M2b's unconditional isolation"
    );
    assert_eq!(
        head_sha(&work),
        work_head_baseline,
        "the launch directory's HEAD commit must be byte-identical to \
         before the shallow fetch — nothing in its git state was disturbed"
    );

    for (label, pwd, expected_head) in [
        ("2nd", pwd_2, &pwd_2_head_baseline),
        ("3rd", pwd_3, &pwd_3_head_baseline),
    ] {
        assert!(
            !is_shallow_repository(pwd),
            "the {label} orchestration's isolated clone at {} must remain a \
             FULL, non-shallow repository — the shallow fetch performed \
             against the 1st orchestration's own isolated clone must be \
             structurally unable to reach a separate object store",
            pwd.display()
        );
        assert_eq!(
            &head_sha(pwd),
            expected_head,
            "the {label} orchestration's HEAD commit must be byte-identical \
             to before the shallow fetch — nothing in its git state was \
             disturbed"
        );
    }
}

/// Read a `owner pwd` log line written by the `orch-clone-gate` fixture's
/// `solo` role (`$DOT_AGENT_DECK_WORKTREE_OWNER` followed by `$(pwd)`,
/// space-separated) and return the `pwd` recorded for `owner`, if any line
/// matches. Shared by the `orchestration/workspace/*` tests below —
/// `orchestration/worktree/014`/`016` inline the same parse themselves
/// rather than share a helper, since each of those has more than one owner
/// to look up from a single read; the workspace tests below each look up
/// exactly one, so a helper is worth it here instead.
fn pwd_for_owner(log_contents: &str, owner: &str) -> Option<PathBuf> {
    log_contents.lines().find_map(|line| {
        line.split_once(' ')
            .filter(|(found, _)| *found == owner)
            .map(|(_, pwd)| PathBuf::from(pwd))
    })
}

/// Scenario: launch the deck in the `orch-clone-gate` fixture and open its
/// one orchestration exactly ONCE — no concurrent sibling orchestration
/// exists at all, the PRD's own "1st orchestration against a root checkout"
/// case. Clear the form's suggested Name and type a distinctive one instead,
/// then submit directly from the Name field (Command is hidden for an
/// orchestration, so Enter there submits) with the Worktree-slug field left
/// untouched/blank. PRD fork#544 M2b retires the Nth-concurrent-only
/// isolation gate: even this very first orchestration against the root
/// checkout must land in its own isolated workspace — never a `git worktree
/// add` sibling sharing the launch directory's own git object store, which
/// is what today's code still does for a 1st orchestration. M2's naming
/// derivation says that workspace must be named
/// `<root-checkout-basename>-<sanitized-name>`, derived from the typed Name
/// alone — never the separate Worktree-slug field (blank here) and never
/// `auto_generate_worktree_slug`'s `orchestrator-N` counter.
#[spec("orchestration/workspace/001")]
#[test]
fn workspace_001_first_orchestration_gets_named_isolated_workspace() {
    const NAME: &str = "workspace001fixed";

    let deck = TuiDeck::launch_with_fixture("orch-clone-gate");
    let work = deck.workdir().to_path_buf();
    commit_fixture(&work);

    deck.wait_for_string("No active sessions");

    let launch_dir_basename = work
        .file_name()
        .expect("launch dir must have a basename")
        .to_string_lossy()
        .into_owned();

    deck.send_keys(b"\x0e"); // Ctrl+n -> directory picker
    deck.send_keys(b" "); // Space -> confirm current dir -> new-pane form
    deck.wait_for_string("No mode"); // form up, Mode field focused at "No mode"
    deck.send_keys(b"\x1b[C"); // Right -> [Orch: clone-gate-demo] (the fixture's only orchestration)
    deck.send_keys(b"\r"); // Mode -> Name
    // Backspace x80 -> clear the pre-filled suggested Name (fork#192 M1.0);
    // extra pops on an already-empty field are no-ops, same idiom as
    // `e2e_new_pane_seed.rs`/`e2e_mode_seed_prompt.rs`.
    deck.send_keys(&[0x7fu8; 80]);
    deck.send_keys(NAME.as_bytes());
    deck.send_keys(b"\r"); // submit directly from Name; Worktree-slug stays blank

    deck.wait_for_absence("New Agent"); // form closed -> orchestration tab up

    let log_path = deck.home_dir().join("clone-gate-pwd.log");
    common::wait_for_file_lines(&log_path, 1, Duration::from_secs(15)).unwrap_or_else(|e| {
        panic!(
            "the role pane must have appended its owner+pwd line to {log_path:?}: {e}\n\
             === rendered grid ===\n{}",
            deck.snapshot_grid()
        )
    });

    let contents =
        std::fs::read_to_string(&log_path).unwrap_or_else(|e| panic!("read {log_path:?}: {e}"));
    let owner = format!("orchestration:{NAME}");
    let pwd = pwd_for_owner(&contents, &owner).unwrap_or_else(|| {
        panic!("no line in {log_path:?} for owner {owner:?}; got: {contents:?}")
    });

    let expected_path = work.with_file_name(format!("{launch_dir_basename}-{NAME}"));
    assert_eq!(
        pwd,
        expected_path,
        "PRD fork#544 M2: the workspace directory must be derived from the orchestration Name \
         alone (`<root-checkout-basename>-<sanitized-name>`) — not from the separate \
         Worktree-slug field (blank here) and not from `orchestrator-N` auto-numbering; today's \
         code, for a 1st orchestration with a blank slug, leaves the pane rooted in the launch \
         directory itself ({}) instead",
        work.display()
    );

    let work_common = git_common_dir(&work);
    let pwd_common = git_common_dir(&pwd);
    assert_ne!(
        pwd_common,
        work_common,
        "PRD fork#544 M2b: the isolation gate is unconditional now — even the FIRST \
         orchestration against a root checkout must provision its own isolated clone (a \
         distinct git common dir/object store), never a `git worktree add` sibling sharing \
         {}'s own object store the way today's code still does for a 1st orchestration",
        work.display()
    );
}

/// Scenario: run the identical single-orchestration flow `workspace_001`
/// drives — clear the suggested Name, type one fixed literal, submit with a
/// blank Worktree-slug — TWICE, as two fully independent scenarios (two
/// separate `TuiDeck` launches, each its own fresh root checkout with its
/// own randomly-named tempdir, the second launched only after the first has
/// been torn down). Both runs type the exact same Name. PRD fork#544 M2's
/// naming derivation must be a pure function of `(root-checkout-basename,
/// Name)` — not of any session-local counter state such as
/// `auto_generate_worktree_slug`'s `orchestrator-N`, which would happily
/// produce the identical `orchestrator-1` slug on both independent runs too,
/// so this test's point is specifically the FORMULA holding independently
/// in each scenario, not merely that some slug repeats. So each run's
/// resulting workspace directory must resolve to that run's OWN
/// `<its-own-root-checkout-basename>-<the-shared-typed-name>` — proving the
/// same deterministic rule applies each time, regardless of which root
/// checkout or how many prior orchestrations either one has seen.
#[spec("orchestration/workspace/002")]
#[test]
fn workspace_002_naming_is_deterministic_from_name_alone_across_separate_runs() {
    const NAME: &str = "detnamefixed";

    fn run_once(basename_out: &mut String, pwd_out: &mut PathBuf) {
        let deck = TuiDeck::launch_with_fixture("orch-clone-gate");
        let work = deck.workdir().to_path_buf();
        commit_fixture(&work);

        deck.wait_for_string("No active sessions");

        *basename_out = work
            .file_name()
            .expect("launch dir must have a basename")
            .to_string_lossy()
            .into_owned();

        deck.send_keys(b"\x0e");
        deck.send_keys(b" ");
        deck.wait_for_string("No mode");
        deck.send_keys(b"\x1b[C");
        deck.send_keys(b"\r");
        deck.send_keys(&[0x7fu8; 80]);
        deck.send_keys(NAME.as_bytes());
        deck.send_keys(b"\r");

        deck.wait_for_absence("New Agent");

        let log_path = deck.home_dir().join("clone-gate-pwd.log");
        common::wait_for_file_lines(&log_path, 1, Duration::from_secs(15)).unwrap_or_else(|e| {
            panic!(
                "the role pane must have appended its owner+pwd line to {log_path:?}: {e}\n\
                 === rendered grid ===\n{}",
                deck.snapshot_grid()
            )
        });

        let contents =
            std::fs::read_to_string(&log_path).unwrap_or_else(|e| panic!("read {log_path:?}: {e}"));
        let owner = format!("orchestration:{NAME}");
        *pwd_out = pwd_for_owner(&contents, &owner).unwrap_or_else(|| {
            panic!("no line in {log_path:?} for owner {owner:?}; got: {contents:?}")
        });

        // Explicit, not left to scope-exit ordering: the second run must not
        // start until this deck's process tree/tempdir/socket are fully torn
        // down, so the two runs are genuinely independent scenarios rather
        // than two decks alive at once.
        drop(deck);
    }

    let mut basename_1 = String::new();
    let mut pwd_1 = PathBuf::new();
    run_once(&mut basename_1, &mut pwd_1);

    let mut basename_2 = String::new();
    let mut pwd_2 = PathBuf::new();
    run_once(&mut basename_2, &mut pwd_2);

    assert_ne!(
        basename_1, basename_2,
        "sanity precondition: the two runs' root checkouts must have DIFFERENT basenames \
         (independent tempdirs) — otherwise this test can't tell a name-derived path from a \
         coincidentally-matching one"
    );

    let expected_1 = PathBuf::from(pwd_1.parent().expect("pwd_1 must have a parent"))
        .join(format!("{basename_1}-{NAME}"));
    let expected_2 = PathBuf::from(pwd_2.parent().expect("pwd_2 must have a parent"))
        .join(format!("{basename_2}-{NAME}"));

    assert_eq!(
        pwd_1, expected_1,
        "PRD fork#544 M2: run 1's workspace directory must equal \
         `<run-1's-own-root-checkout-basename>-{NAME}`"
    );
    assert_eq!(
        pwd_2, expected_2,
        "PRD fork#544 M2: run 2's workspace directory must equal \
         `<run-2's-own-root-checkout-basename>-{NAME}` — the SAME derivation rule as run 1, \
         applied independently, proving the naming is a pure function of \
         (root-checkout-basename, Name) rather than of session-local counter state like \
         `orchestrator-N`"
    );
}
