//! Issue #325 — `cargo xtask linkage-check`'s repository-state preflight
//! (checks 10-12): a shallow fetch or a worktree removed without pruning in
//! a shared multi-worktree checkout must fail loudly, but a fresh
//! single-worktree clone (a CI runner) must stay exempt by construction —
//! never via an `if CI` escape hatch.
//!
//! Drives the compiled `xtask-linkage-check` binary against real, throwaway
//! git repositories built with real `git worktree add` / `git clone
//! --depth 1` — the same shape `tests/duplicate_catalog_id.rs` uses for
//! check 9, and the only way to prove the WHOLE TOOL gates correctly rather
//! than some internal function in isolation.

use std::fs;
use std::path::Path;
use std::process::Command;

/// Minimal fixture tree that satisfies checks 1-9 on its own (one catalog
/// entry, one matching `#[spec]` test with a Scenario comment) so a failure
/// from checks 10-12 is never confused with an unrelated pre-existing
/// failure. Mirrors `duplicate_catalog_id.rs`'s `write_fixture`.
fn write_fixture(root: &Path) {
    fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").expect("write Cargo.toml");
    let tests_dir = root.join("tests");
    fs::create_dir_all(&tests_dir).expect("mkdir tests/");
    fs::write(
        tests_dir.join("CATALOG.md"),
        "\
## Test Case Catalog

##### fixture/repostate/001 — Preflight fixture
- **Layer:** L1.
",
    )
    .expect("write CATALOG.md");
    fs::write(
        tests_dir.join("repostate_test.rs"),
        "#[spec(\"fixture/repostate/001\")]\n\
         #[test]\n\
         /// Scenario: pins issue #325 — repository-state preflight fixture.\n\
         fn repostate_001_pins_preflight_fixture() {}\n",
    )
    .expect("write repostate_test.rs");
}

/// Run `git <args>` in `dir`, panicking with full context on failure.
/// `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM` are pinned to `/dev/null` so a
/// developer's own gitconfig (signing, hooks, templates) cannot make these
/// scratch repos behave differently — the same isolation `work_type.rs`'s
/// `--self-test` scratch repos use.
fn git(args: &[&str], dir: &Path) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap_or_else(|e| panic!("invoke git {args:?} in {}: {e}", dir.display()));
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Init a repo at `dir`, write the fixture tree, and commit it — the shared
/// starting point every scenario below builds on.
fn init_committed_repo(dir: &Path) {
    git(&["init", "-q"], dir);
    git(
        &[
            "config",
            "user.email",
            "repo-state-preflight@example.invalid",
        ],
        dir,
    );
    git(&["config", "user.name", "repo-state-preflight test"], dir);
    write_fixture(dir);
    git(&["add", "."], dir);
    git(&["commit", "-q", "-m", "fixture"], dir);
}

fn run_linkage_check(dir: &Path) -> std::process::Output {
    let bin = env!("CARGO_BIN_EXE_xtask-linkage-check");
    Command::new(bin)
        .current_dir(dir)
        .output()
        .expect("run xtask-linkage-check")
}

/// A shallow clone with exactly one worktree must NOT fail — a CI runner
/// clones fresh and shallow, and the preflight is exempt by construction in
/// that shape (never via an `if CI` env-var check). This is the central
/// constraint the whole design turns on.
#[test]
fn linkage_check_ok_on_single_worktree_even_when_shallow() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let remote = tmp.path().join("remote");
    fs::create_dir_all(&remote).expect("mkdir remote");
    init_committed_repo(&remote);
    // A second commit so the shallow clone genuinely truncates history,
    // rather than depth-1 coinciding with "the whole repo".
    fs::write(remote.join("README.md"), "second commit\n").expect("write README.md");
    git(&["add", "README.md"], &remote);
    git(&["commit", "-q", "-m", "second commit"], &remote);

    let dest = tmp.path().join("dest");
    git(
        &[
            "clone",
            "--depth",
            "1",
            remote.to_str().expect("utf8 path"),
            dest.to_str().expect("utf8 path"),
        ],
        tmp.path(),
    );

    // Sanity: the clone really is shallow.
    let out = Command::new("git")
        .args(["rev-parse", "--is-shallow-repository"])
        .current_dir(&dest)
        .output()
        .expect("git rev-parse --is-shallow-repository");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "true",
        "test setup did not actually produce a shallow clone"
    );

    let output = run_linkage_check(&dest);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "linkage-check failed on a shallow SINGLE-worktree clone, which must be exempt by \
         construction\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stderr.contains("[10]"),
        "single-worktree shallow clone must not trip check 10\nstderr: {stderr}"
    );
}

/// A shallow clone that also has a second, linked worktree must fail check
/// 10, naming the exact remedy.
#[test]
fn linkage_check_fails_on_shallow_repository_in_multi_worktree_checkout() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let remote = tmp.path().join("remote");
    fs::create_dir_all(&remote).expect("mkdir remote");
    init_committed_repo(&remote);
    fs::write(remote.join("README.md"), "second commit\n").expect("write README.md");
    git(&["add", "README.md"], &remote);
    git(&["commit", "-q", "-m", "second commit"], &remote);

    let dest = tmp.path().join("dest");
    git(
        &[
            "clone",
            "--depth",
            "1",
            remote.to_str().expect("utf8 path"),
            dest.to_str().expect("utf8 path"),
        ],
        tmp.path(),
    );

    let linked = tmp.path().join("dest-linked");
    git(
        &[
            "worktree",
            "add",
            linked.to_str().expect("utf8 path"),
            "-b",
            "linked",
        ],
        &dest,
    );

    let output = run_linkage_check(&dest);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "linkage-check reported success on a shallow repository with a second linked worktree\n\
         stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("[10]") && stderr.contains("git fetch --unshallow"),
        "expected check 10 to name the exact remedy `git fetch --unshallow <remote>`\n\
         stderr: {stderr}"
    );
}

/// A worktree removed by deleting its directory directly (never `git
/// worktree remove`/`prune`) must fail check 11, naming the stale path and
/// the remedy.
#[test]
fn linkage_check_fails_on_stale_worktree_registry_entry() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let main = tmp.path().join("main");
    fs::create_dir_all(&main).expect("mkdir main");
    init_committed_repo(&main);

    let sibling = tmp.path().join("sibling-worktree");
    git(
        &[
            "worktree",
            "add",
            sibling.to_str().expect("utf8 path"),
            "-b",
            "sibling",
        ],
        &main,
    );

    // Simulate a second concurrent process deleting the worktree directory
    // directly (issue #325's actual incident shape) rather than going
    // through `git worktree remove`, which would have pruned the registry
    // entry too.
    fs::remove_dir_all(&sibling).expect("rm -rf sibling-worktree");

    let output = run_linkage_check(&main);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "linkage-check reported success with a stale worktree registry entry\nstdout: {stdout}\n\
         stderr: {stderr}"
    );
    assert!(
        stderr.contains("[11]") && stderr.contains("git worktree prune"),
        "expected check 11 to name the stale path and the `git worktree prune` remedy\n\
         stderr: {stderr}"
    );
    assert!(
        stderr.contains(sibling.to_str().expect("utf8 path")),
        "expected the stale path itself to appear in the failure message\nstderr: {stderr}"
    );
    assert!(
        !stderr.contains("[10]"),
        "a non-shallow repository must not trip check 10\nstderr: {stderr}"
    );
    assert!(
        !stderr.contains("[12]"),
        "the CURRENT worktree (main) still exists; only the sibling is stale\nstderr: {stderr}"
    );
}

/// Control case: a clean multi-worktree checkout — gated (more than one
/// worktree), but neither shallow nor drifted — must still pass. Proves the
/// gate does not false-positive merely from being active.
#[test]
fn linkage_check_passes_on_clean_multi_worktree_checkout() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let main = tmp.path().join("main");
    fs::create_dir_all(&main).expect("mkdir main");
    init_committed_repo(&main);

    let sibling = tmp.path().join("sibling-worktree");
    git(
        &[
            "worktree",
            "add",
            sibling.to_str().expect("utf8 path"),
            "-b",
            "sibling",
        ],
        &main,
    );

    let output = run_linkage_check(&main);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "linkage-check failed on a clean multi-worktree checkout\nstdout: {stdout}\n\
         stderr: {stderr}"
    );
}
