//! Issue #259 (the #258 shape) — post-review fix round.
//!
//! `xtask/linkage-check/src/work_type.rs`'s unit tests exercise
//! `check_resurrected_fragments` directly; nothing exercised `main.rs`'s
//! check-11 WIRING — the `match work_type::resolve_base(...)` arm that
//! turns a resolved base into `[11]`-prefixed failures, or an unresolvable
//! one into a printed skip. That gap is exactly what caused B1: check 11's
//! `Err` arm turned every unresolvable-base run into a hard failure,
//! breaking `duplicate_catalog_id.rs`'s
//! `linkage_check_passes_once_the_duplicate_heading_is_resolved` control
//! test, and nothing caught it because no test drove the compiled binary
//! against a fixture that IS a git repository with a resurrected fragment
//! in it.
//!
//! Like `duplicate_catalog_id.rs`, this drives the compiled
//! `xtask-linkage-check` binary against a throwaway fixture tree — the
//! bug this file guards against is that the WHOLE TOOL answers wrong when
//! wired together, not that some internal function lacks a case.

use std::fs;
use std::path::Path;
use std::process::Command;

/// Same minimal `## Test Case Catalog` fixture shape as
/// `duplicate_catalog_id.rs`'s `write_fixture`, plus a git repository (check
/// 11 needs one) with `origin/main` faked at the base commit — the same
/// `git update-ref refs/remotes/origin/main HEAD` trick
/// `work_type.rs::init_self_test_repo` uses for `--self-test`'s own scratch
/// repos.
fn git(args: &[&str], dir: &Path) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap_or_else(|e| panic!("run git {args:?} in {dir:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} in {dir:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Writes the catalog/test/src scaffolding every check needs to pass other
/// than check 11 (checks 1, 2, 4 and 7 in particular), then `git init`s the
/// tree, commits it as the base, and fakes `origin/main` at that commit so
/// `resolve_base` succeeds. Returns the base commit's SHA, in case a caller
/// wants it (neither current case does — both diffs happen in the same
/// single follow-up commit relative to this base).
fn write_and_commit_base_fixture(root: &Path) {
    fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").expect("write Cargo.toml");
    let tests_dir = root.join("tests");
    fs::create_dir_all(&tests_dir).expect("mkdir tests/");
    fs::write(
        tests_dir.join("CATALOG.md"),
        "## Test Case Catalog\n\n##### fixture/resurrect/001 — Entry\n- **Layer:** L1.\n",
    )
    .expect("write CATALOG.md");
    fs::write(
        tests_dir.join("fixture_test.rs"),
        "#[spec(\"fixture/resurrect/001\")]\n\
         #[test]\n\
         /// Scenario: pins issue #259 — the resurrected-changelog-fragment wiring.\n\
         fn resurrect_001_pins_the_wiring() {}\n",
    )
    .expect("write fixture_test.rs");
    let src_dir = root.join("src");
    fs::create_dir_all(&src_dir).expect("mkdir src/");
    fs::write(src_dir.join("test_temp.rs"), "// fixture stand-in\n")
        .expect("write src/test_temp.rs");

    fs::create_dir_all(root.join("changelog.d")).expect("mkdir changelog.d");
    fs::write(
        root.join("CHANGELOG.md"),
        "# Changelog\n\n## [Unreleased]\n\n### Fixed\n\n  Fixed a widget race in the \
         scheduler where two ticks could clobber the same slot.\n",
    )
    .expect("write CHANGELOG.md");

    git(&["init", "-q"], root);
    git(&["config", "user.email", "test@example.com"], root);
    git(&["config", "user.name", "test"], root);
    git(&["add", "."], root);
    git(&["commit", "-q", "-m", "base"], root);
    git(&["update-ref", "refs/remotes/origin/main", "HEAD"], root);
}

/// The bug this file exists to catch, end to end: a resurrected fragment
/// added on top of the base fixture must fail the real binary, with a
/// `[11]`-tagged failure line naming the resurrected fragment.
#[test]
fn linkage_check_fails_when_a_resurrected_fragment_is_added() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_and_commit_base_fixture(tmp.path());

    fs::write(
        tmp.path().join("changelog.d/259.bugfix.md"),
        "Fixed a widget race in the scheduler where two ticks could clobber the same slot.\n",
    )
    .expect("write resurrected fragment");
    git(&["add", "changelog.d/259.bugfix.md"], tmp.path());
    git(
        &["commit", "-q", "-m", "add resurrected fragment"],
        tmp.path(),
    );

    let bin = env!("CARGO_BIN_EXE_xtask-linkage-check");
    let output = Command::new(bin)
        .current_dir(tmp.path())
        .output()
        .expect("run xtask-linkage-check");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "linkage-check reported success on a fixture whose new fragment resurrects \
         already-shipped CHANGELOG.md content (issue #259)\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("[11]") && stderr.contains("259.bugfix.md"),
        "the failure must be tagged [11] and name the resurrected fragment: {stderr}"
    );
}

/// Control case: the same fixture with a genuinely new, unrelated fragment
/// (no resurrection) must still pass through the real binary — and the
/// success line must say what check 11 actually compared (P2: a passing
/// run must be distinguishable from a no-op).
#[test]
fn linkage_check_passes_and_reports_the_checked_count_when_nothing_is_resurrected() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_and_commit_base_fixture(tmp.path());

    fs::write(
        tmp.path().join("changelog.d/260.bugfix.md"),
        "Fixed an unrelated memory leak in the daemon's PTY reader.\n",
    )
    .expect("write unrelated fragment");
    git(&["add", "changelog.d/260.bugfix.md"], tmp.path());
    git(
        &["commit", "-q", "-m", "add unrelated fragment"],
        tmp.path(),
    );

    let bin = env!("CARGO_BIN_EXE_xtask-linkage-check");
    let output = Command::new(bin)
        .current_dir(tmp.path())
        .output()
        .expect("run xtask-linkage-check");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "linkage-check failed on a fixture with a genuinely new, unrelated fragment\nstdout: \
         {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("added changelog fragment"),
        "a passing run must name what check 11 actually compared, not just print an \
         unchanged success line (P2): {stdout}"
    );
}
