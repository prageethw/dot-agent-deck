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

use std::collections::HashMap;
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
/// orchestration:<launch-dir-basename>-orchestrator-1` — on this keyboard
/// path the Name field is never typed into, so it keeps the value selecting
/// the orchestration pre-filled it with (fork#192 M1.0): the next free
/// `<basename>-orchestrator-N` suggestion, `N=1` since no orchestration is
/// live yet in this fresh single-open test. That typed-Name value takes
/// precedence over `orch_config.name` at the `create_worktree_sync` call
/// site, so it — not the fixture's config name `worktree-demo` — is the
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

    // fork issue #425 follow-up, updated for fork#192 M1.0: the Name field
    // was never typed into on this keyboard path (Mode -> Name -> Tab
    // straight to Worktree), so it keeps its pre-filled value -- since M1.0,
    // the suggested `<launch-dir-basename>-orchestrator-1` that selecting the
    // orchestration writes into the Name field (`N=1`: no orchestration is
    // live yet in this fresh single-open test). That typed-Name value takes
    // precedence over `orch_config.name` at the `create_worktree_sync` call
    // site (`src/ui.rs`), so the creator this path actually derives is
    // `orchestration:<launch-dir basename>-orchestrator-1`, not the fixture's
    // config name `worktree-demo`. Compute the expected basename from the
    // launch dir this test itself set up, rather than hardcoding it.
    let launch_dir_basename = work
        .file_name()
        .expect("launch dir must have a basename")
        .to_string_lossy()
        .into_owned();
    let expected_creator =
        format!("created-by: orchestration:{launch_dir_basename}-orchestrator-1");
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

/// Resolve `dir`'s git COMMON dir via `git -C dir rev-parse --git-common-dir`
/// — for a linked worktree this resolves to the MAIN repository's `.git` (the
/// same value every other worktree of that repository resolves to); for a
/// genuinely separate clone it resolves to that clone's own `.git`. This is
/// the observable signature PRD fork#325 M3's gate is about: whether the
/// Nth-concurrent orchestration shares the 1st's object store (today, always)
/// or gets its own (the fix). Mirrors `resolve_git_dir` above, which reads
/// `--git-dir` instead — deliberately duplicated rather than generalized,
/// following this file's existing convention of asserting the OBSERVABLE
/// path a user/tool would see, not a shared internal helper.
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
/// fixture's one orchestration with a TYPED worktree slug — `Ctrl+n` ->
/// directory picker -> confirm -> Mode cycled to the orchestration -> Tab to
/// Worktree -> slug -> submit. A typed (non-blank) slug is required to reach
/// `create_worktree_sync`/the M3 gate at all — an accepted-blank Worktree
/// field (as `e2e_orchestration_identity.rs::open_orchestration` uses) never
/// provisions a worktree at all, so it can't exercise this feature.
fn open_orchestration_with_slug(deck: &TuiDeck, slug: &str) {
    deck.send_keys(b"\x0e"); // Ctrl+n -> directory picker
    deck.send_keys(b" "); // Space -> confirm current dir -> new-pane form
    deck.wait_for_string("No mode"); // form up, Mode field focused at "No mode"
    deck.send_keys(b"\x1b[C"); // Right -> [Orch: clone-gate-demo] (the fixture's only orchestration)
    deck.send_keys(b"\r"); // Mode -> Name
    deck.send_keys(b"\t"); // Tab: Name -> Worktree (Command is hidden for an orchestration)
    deck.send_keys(slug.as_bytes());
    deck.send_keys(b"\r"); // submit
}

/// Scenario: launch the deck in the `orch-clone-gate` fixture and open its
/// one orchestration TWICE against the SAME directory — with distinct typed
/// worktree slugs, the second one while the first is still live — mirroring
/// the N-concurrent-orchestrations shape of issue #325's actual incident
/// (three-plus, here reduced to the PRD's own stated M3 starting case: the
/// simplest 2nd-against-a-live-1st collision). Each instance's role pane
/// appends its own `DOT_AGENT_DECK_WORKTREE_OWNER` identity plus its own
/// `pwd` to one shared, HOME-relative log file, so the 1st and 2nd
/// instances' actual working directories can be read back and told apart by
/// owner string once both role panes have run — without this test needing to
/// know in advance where either instance's worktree/clone lands on disk.
/// Assert the 1st instance's directory shares the launch dir's git common
/// dir (unaffected, per the PRD's own "the 1st orchestration is out of
/// scope" note — no behavior change for the common case), and the 2nd
/// instance's directory does NOT — it must have its own isolated clone (a
/// distinct git object store), never a `git worktree add` sibling reusing
/// the shared checkout's common dir the way `create_worktree_sync` does
/// today unconditionally, regardless of how many orchestrations are already
/// live against it.
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

    let launch_dir_basename = work
        .file_name()
        .expect("launch dir must have a basename")
        .to_string_lossy()
        .into_owned();
    // fork#192 M1.0: the Name field is never typed into on this keyboard
    // path (Mode -> Tab straight to Worktree), so it keeps its pre-filled
    // suggestion — the next free `<basename>-orchestrator-N` — and that
    // value is what `Action::SpawnPane` actually derives as the
    // creator/owner identity (see `orchestration/worktree/005` above).
    let owner_1 = format!("orchestration:{launch_dir_basename}-orchestrator-1");
    let owner_2 = format!("orchestration:{launch_dir_basename}-orchestrator-2");
    let second_label = format!(" {launch_dir_basename}-orchestrator-2 ");

    open_orchestration_with_slug(&deck, "clonegate1");
    deck.wait_for_absence("New Agent"); // first orchestration's form closed -> tab up

    // Back to the Dashboard to open a second orchestration in the same dir,
    // while the first is still live.
    deck.send_keys(b"\x04"); // Ctrl+D -> Normal mode (still on the orchestration tab)
    deck.send_keys(b"\x1b[D"); // Left -> previous tab -> Dashboard
    deck.wait_for_string("session(s)");

    open_orchestration_with_slug(&deck, "clonegate2");
    deck.wait_for_string(&second_label); // second orchestration deck is up, distinctly labeled

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

    assert_eq!(
        pwd_1_common, work_common,
        "the 1ST orchestration against a root checkout must be unaffected — \
         it shares the launch dir's git common dir exactly as today, per the \
         PRD's own out-of-scope note"
    );
    assert_ne!(
        pwd_2_common,
        work_common,
        "the 2ND (Nth-concurrent) orchestration against the SAME root \
         checkout, spawned while the 1st is still live, must provision its \
         OWN isolated clone — a distinct git common dir/object store — \
         instead of sharing {}'s via a `git worktree add` sibling the way \
         `create_worktree_sync` does today unconditionally, regardless of \
         how many orchestrations are already live against it",
        work.display()
    );

    // Issue #325 reviewer P1-2: both arms must land on the SLUG the user
    // typed, not silently stay on the source's HEAD branch — the absence of
    // this assertion is exactly what let the isolated arm's dropped
    // `branch` argument through a green suite.
    assert_eq!(
        current_branch(pwd_1),
        "clonegate1",
        "the 1st (shared-worktree) orchestration must be on the typed slug's branch"
    );
    assert_eq!(
        current_branch(pwd_2),
        "clonegate2",
        "the 2nd (isolated-clone) orchestration must be checked out on the typed \
         slug's branch, not the source's HEAD branch — this is issue #325 \
         reviewer P1-2's fix"
    );

    // Issue #325 reviewer P1-1: the isolated clone's `origin` must be the
    // SOURCE's own origin URL, never the local filesystem path a plain
    // `git clone` defaults `origin` to — a `git push origin` from the clone
    // would otherwise land silently in `work`, the user's own root checkout.
    assert_eq!(
        remote_origin_url(pwd_2).as_deref(),
        Some(fake_origin),
        "the isolated clone's origin must be the source checkout's own \
         origin URL, not a local path pointing back at {}",
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
        .args(["worktree", "remove", "--force"])
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
fn go_to_dashboard(deck: &TuiDeck, steps_back: usize) {
    deck.send_keys(b"\x04"); // Ctrl+D -> Normal mode (still on the current tab)
    for _ in 0..steps_back {
        deck.send_keys(b"\x1b[D"); // Left -> CycleTabPrev, one step back
    }
    deck.wait_for_string("session(s)");
}

/// Scenario: launch the deck in the `orch-clone-gate` fixture and open its
/// one orchestration THREE times against the SAME directory — with distinct
/// typed worktree slugs, the 2nd and 3rd each while an earlier one is still
/// live — reproducing issue #325's own original incident shape verbatim
/// (the issue's own words: "three-plus concurrent orchestrations... on one
/// deck"), not `014`'s reduced 2-orchestration starting case. First proves
/// the PRD fork#325 M3 gate holds at N=3: the 1st shares the launch dir's
/// git common dir exactly as today, the 2nd and 3rd each land on their OWN
/// distinct common dir — no two of the three sharing one, so the 2nd and
/// 3rd are isolated from EACH OTHER too, not just from the 1st. Then
/// reproduces the issue's own two concrete failures directly and proves
/// neither can recur: (1) with all three live, attempts `git worktree
/// remove` from one orchestration's repository targeting another's
/// directory — cross-repo, this must now fail outright (the target isn't
/// even registered in the attacking repo's worktree list), leaving the
/// target's directory, branch, and HEAD commit completely undisturbed; (2)
/// a `git fetch --depth 1` performed in the 1st (shared-checkout)
/// orchestration's directory makes the LAUNCH DIRECTORY'S shared object
/// store shallow, exactly as the issue reported, while the 2nd and 3rd
/// orchestrations' isolated clones — genuinely separate object stores now —
/// remain unaffected and their HEAD commits stay byte-identical to before
/// the fetch.
#[spec("orchestration/worktree/016")]
#[test]
fn worktree_016_three_concurrent_orchestrations_reproduce_325_incident_shape_without_colliding() {
    let deck = TuiDeck::launch_with_fixture("orch-clone-gate");
    let work = deck.workdir().to_path_buf();
    commit_fixture(&work);
    let fake_origin = "https://example.invalid/orch-clone-gate-fixture.git";
    run_git(&work, &["remote", "add", "origin", fake_origin]);

    deck.wait_for_string("No active sessions");

    let launch_dir_basename = work
        .file_name()
        .expect("launch dir must have a basename")
        .to_string_lossy()
        .into_owned();
    let owner_1 = format!("orchestration:{launch_dir_basename}-orchestrator-1");
    let owner_2 = format!("orchestration:{launch_dir_basename}-orchestrator-2");
    let owner_3 = format!("orchestration:{launch_dir_basename}-orchestrator-3");
    let second_label = format!(" {launch_dir_basename}-orchestrator-2 ");
    let third_label = format!(" {launch_dir_basename}-orchestrator-3 ");

    // 1st orchestration: shares the launch dir's checkout, as today.
    open_orchestration_with_slug(&deck, "clonegate1");
    deck.wait_for_absence("New Agent");
    go_to_dashboard(&deck, 1); // orch1 (index 1) -> Dashboard (index 0)

    // 2nd, while the 1st is still live: must get its own isolated clone.
    open_orchestration_with_slug(&deck, "clonegate2");
    deck.wait_for_string(&second_label);
    go_to_dashboard(&deck, 2); // orch2 (index 2) -> orch1 (1) -> Dashboard (0)

    // 3rd, while BOTH the 1st and 2nd are still live: this is #325's own
    // actual incident shape (three-plus concurrent, not two).
    open_orchestration_with_slug(&deck, "clonegate3");
    deck.wait_for_string(&third_label);

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

    // --- Structural proof: no two of the three share a git common dir,
    // except the 1st, which shares the launch dir's exactly as intended. ---
    let work_common = git_common_dir(&work);
    let pwd_1_common = git_common_dir(pwd_1);
    let pwd_2_common = git_common_dir(pwd_2);
    let pwd_3_common = git_common_dir(pwd_3);

    assert_eq!(
        pwd_1_common, work_common,
        "the 1ST orchestration must be unaffected — it shares the launch \
         dir's git common dir exactly as today"
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
        pwd_2_common, pwd_3_common,
        "the 2ND and 3RD orchestrations must NOT share each other's \
         isolated clone either — each gets its own separate object store"
    );

    assert_eq!(
        current_branch(pwd_1),
        "clonegate1",
        "the 1st (shared-worktree) orchestration must be on its typed slug's branch"
    );
    assert_eq!(
        current_branch(pwd_2),
        "clonegate2",
        "the 2nd (isolated-clone) orchestration must be on its typed slug's branch"
    );
    assert_eq!(
        current_branch(pwd_3),
        "clonegate3",
        "the 3rd (isolated-clone) orchestration must be on its typed slug's branch"
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

    let pwd_2_head_baseline = head_sha(pwd_2);
    let pwd_3_head_baseline = head_sha(pwd_3);

    // --- Behavioral proof of failure #1: "an orchestration deleted five
    // worktrees it did not own, mid-use". Attempt cross-repo `git worktree
    // remove` in both directions — from the shared worktree against an
    // isolated clone, and from one isolated clone against another. Every
    // attempt must fail outright (the target is not even registered in the
    // attacking repo's worktree list), and every target must remain
    // completely intact afterward. ---
    let (removed_2_from_1, stderr_2_from_1) = attempt_worktree_remove(pwd_1, pwd_2);
    assert!(
        !removed_2_from_1,
        "`git worktree remove` run from the 1st (shared-checkout) \
         orchestration's directory must NOT be able to remove the 2nd's \
         isolated clone at {} — this is issue #325 failure #1, and it \
         succeeding here would mean the isolation regressed\nstderr: {}",
        pwd_2.display(),
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

    for (label, pwd, expected_branch, expected_head) in [
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
    // repository shallow, breaking merges in every worktree". Perform a
    // real `git fetch --depth 1` in the 1st (shared-checkout) orchestration's
    // directory — the exact operation the issue reported (a shallow fetch
    // against the shared object store) — and confirm the 2nd and 3rd
    // orchestrations' isolated clones, genuinely separate object stores now,
    // are structurally unable to be affected. ---
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
        is_shallow_repository(&work),
        "because the 1st orchestration shares the launch directory's git \
         common dir, the shallow fetch must be visible from the launch \
         directory too — exactly as issue #325 reported \
         (`$ git rev-parse --is-shallow-repository` / `true`, observed from \
         a DIFFERENT worktree than the one that ran the fetch)"
    );

    for (label, pwd, expected_head) in [
        ("2nd", pwd_2, &pwd_2_head_baseline),
        ("3rd", pwd_3, &pwd_3_head_baseline),
    ] {
        assert!(
            !is_shallow_repository(pwd),
            "the {label} orchestration's isolated clone at {} must remain a \
             FULL, non-shallow repository — the shallow fetch performed \
             against the 1st orchestration's shared checkout must be \
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
