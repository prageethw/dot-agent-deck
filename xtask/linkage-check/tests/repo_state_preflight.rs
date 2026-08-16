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
use std::path::{Path, PathBuf};
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

/// `git worktree add <path> -b <branch>` in `base`.
fn add_worktree(base: &Path, path: &Path, branch: &str) {
    git(
        &[
            "worktree",
            "add",
            path.to_str().expect("utf8 path"),
            "-b",
            branch,
        ],
        base,
    );
}

/// Build a two-commit `remote` repo under `tmp`, shallow-clone it into
/// `tmp/dest`, and return `dest`. `--no-local` is load-bearing: git silently
/// ignores `--depth` for a same-filesystem local clone unless `--no-local`
/// (or a `file://` URL) forces it through the normal fetch-negotiation path
/// instead of the hardlink/copy fast path — without it the "shallow" clone
/// would actually be a full, non-shallow copy and every test using this
/// would test nothing. The second commit exists so depth-1 genuinely
/// truncates history, rather than coinciding with "the whole repo".
fn build_shallow_clone(tmp: &Path) -> PathBuf {
    let remote = tmp.join("remote");
    fs::create_dir_all(&remote).expect("mkdir remote");
    init_committed_repo(&remote);
    fs::write(remote.join("README.md"), "second commit\n").expect("write README.md");
    git(&["add", "README.md"], &remote);
    git(&["commit", "-q", "-m", "second commit"], &remote);

    let dest = tmp.join("dest");
    git(
        &[
            "clone",
            "--no-local",
            "--depth",
            "1",
            remote.to_str().expect("utf8 path"),
            dest.to_str().expect("utf8 path"),
        ],
        tmp,
    );

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

    dest
}

/// Run the compiled `xtask-linkage-check` binary in `dir`, returning
/// `(success, stdout, stderr)` as owned strings.
fn run_linkage_check(dir: &Path) -> (bool, String, String) {
    let bin = env!("CARGO_BIN_EXE_xtask-linkage-check");
    let output = Command::new(bin)
        .current_dir(dir)
        .output()
        .expect("run xtask-linkage-check");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// A shallow clone with exactly one worktree must NOT fail — a CI runner
/// clones fresh and shallow, and the preflight is exempt by construction in
/// that shape (never via an `if CI` env-var check). This is the central
/// constraint the whole design turns on.
#[test]
fn linkage_check_ok_on_single_worktree_even_when_shallow() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = build_shallow_clone(tmp.path());

    let (success, stdout, stderr) = run_linkage_check(&dest);
    assert!(
        success,
        "linkage-check failed on a shallow SINGLE-worktree clone, which must be exempt by \
         construction\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stderr.contains("[10]"),
        "single-worktree shallow clone must not trip check 10\nstderr: {stderr}"
    );
    // B2: the success line must report that checks 10-12 were exempt here,
    // not the fixed `12 rules` it used to print on every run regardless of
    // whether they actually executed.
    assert!(
        stdout.contains("9 rules") && stdout.contains("10-12 exempt"),
        "expected the success line to name the 10-12 exemption on a single-worktree checkout\n\
         stdout: {stdout}"
    );
}

/// A shallow clone that also has a second, linked worktree must fail check
/// 10, naming the exact remedy.
#[test]
fn linkage_check_fails_on_shallow_repository_in_multi_worktree_checkout() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = build_shallow_clone(tmp.path());
    add_worktree(&dest, &tmp.path().join("dest-linked"), "linked");

    let (success, stdout, stderr) = run_linkage_check(&dest);
    assert!(
        !success,
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
    add_worktree(&main, &sibling, "sibling");

    // Simulate a second concurrent process deleting the worktree directory
    // directly (issue #325's actual incident shape) rather than going
    // through `git worktree remove`, which would have pruned the registry
    // entry too.
    fs::remove_dir_all(&sibling).expect("rm -rf sibling-worktree");

    let (success, stdout, stderr) = run_linkage_check(&main);
    assert!(
        !success,
        "linkage-check reported success with a stale worktree registry entry\nstdout: {stdout}\n\
         stderr: {stderr}"
    );
    assert!(
        stderr.contains("[11]") && stderr.contains("git worktree prune"),
        "expected check 11 to name the stale path and the `git worktree prune` remedy\n\
         stderr: {stderr}"
    );
    // Match on the leaf directory name only, not the full path: on Windows
    // CI runners `sibling` (built from `tmp.path()`, which can inherit the
    // short 8.3 form of TEMP, e.g. `RUNNER~1`) and git's own
    // `--is-shallow-repository`-adjacent path resolution (which reports the
    // long form) can print the SAME directory two different ways, so a
    // full-path substring match is not portable — observed live in CI.
    assert!(
        stderr.contains("sibling-worktree"),
        "expected the stale worktree's own directory name to appear in the failure message\n\
         stderr: {stderr}"
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

/// The CURRENT worktree's own directory disappearing out from under the
/// running process — check 12's own scenario (issue #325 B1). Before the
/// fix this crashed `repo_root()`'s `current_dir().expect(...)` with a raw
/// panic instead of producing check 12's diagnosis; the fix handles the
/// `current_dir()` failure explicitly.
///
/// `Command::current_dir` cannot reproduce this directly: its `chdir` is
/// validated at spawn, so pointing it at an already-removed path makes the
/// spawn itself fail before the child ever runs. The real-world shape (a
/// shell `cd`s into a worktree, something else removes it, then a command
/// is run from that shell) is instead: the PARENT process's cwd breaks,
/// and a child inherits that already-broken cwd via `fork()` with no
/// chdir validation at all. So this test breaks the TEST PROCESS's own cwd
/// the same way, then spawns the binary with no `.current_dir()` override
/// so it inherits the broken cwd exactly as a shell-launched child would.
///
/// Relies on nextest's one-process-per-test isolation (CLAUDE.md rule 5):
/// `std::env::set_current_dir` mutates process-global state that would
/// race with any sibling test sharing this process under a threaded
/// `cargo test` run.
#[test]
fn linkage_check_reports_check_12_when_the_current_worktree_is_gone() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let doomed = tmp.path().join("doomed-worktree");
    fs::create_dir_all(&doomed).expect("mkdir doomed-worktree");

    let original_cwd = std::env::current_dir().expect("read original cwd");
    std::env::set_current_dir(&doomed).expect("cd into doomed-worktree");
    // Verified (auditor, macOS, git/libc getcwd): rmdir-ing a directory that
    // happens to be the calling process's own cwd succeeds, and a later
    // getcwd() in that process then fails with ENOENT.
    fs::remove_dir(&doomed).expect("rmdir doomed-worktree out from under the process");

    let bin = env!("CARGO_BIN_EXE_xtask-linkage-check");
    // Deliberately no `.current_dir(...)`: this inherits the TEST process's
    // now-broken cwd via `fork()`, which is the real-world mechanism -- not
    // a `chdir` into a path that would fail the spawn itself.
    let output = Command::new(bin).output();

    std::env::set_current_dir(&original_cwd).expect("restore cwd");

    let output = output.expect("spawn xtask-linkage-check (inherits the broken cwd via fork)");
    assert!(
        !output.status.success(),
        "linkage-check must fail, not succeed, when its own cwd no longer exists"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[12]"),
        "expected check 12's diagnosis for a missing current-worktree directory\nstderr: {stderr}"
    );
    assert!(
        !stderr.contains("panicked"),
        "must fail cleanly with check 12's message, not panic\nstderr: {stderr}"
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
    add_worktree(&main, &sibling, "sibling");

    let (success, stdout, stderr) = run_linkage_check(&main);
    assert!(
        success,
        "linkage-check failed on a clean multi-worktree checkout\nstdout: {stdout}\n\
         stderr: {stderr}"
    );
    // B2: this is the one shape where all 12 rules genuinely ran, so the
    // success line must say so with no exemption note.
    assert!(
        stdout.contains("12 rules") && !stdout.contains("exempt"),
        "expected the success line to report all 12 rules ran, with no exemption\n\
         stdout: {stdout}"
    );
}
