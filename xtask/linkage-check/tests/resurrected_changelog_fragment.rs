//! Issue #259 (the #258 shape) — post-review fix round.
//!
//! `xtask/linkage-check/src/work_type.rs`'s unit tests exercise
//! `check_resurrected_fragments` directly; nothing exercised `main.rs`'s
//! check-12 WIRING — the `match work_type::resolve_base(...)` arm that
//! turns a resolved base into `[12]`-prefixed failures, or an unresolvable
//! one into a printed skip. That gap is exactly what caused B1: check 12's
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

/// Ambient git location- and config-injection environment variables that
/// must never leak into a fixture `git` invocation below (issue #669) — the
/// first 8 mirror `list_tests.rs:808`'s own `GIT_ENV_VARS_TO_CLEAR`
/// byte-for-byte (as does `work_type.rs`'s own copy of this same list), and
/// the last 3 mirror what `repo_state.rs`'s `mod real_git::Sandbox`'s
/// `AMBIENT_LOCATION_VARS` carries beyond those 8 (issue #579, PR #663).
/// Duplicated here rather than imported — unlike `work_type.rs`'s own two
/// internal call sites, which share one copy within that file — because
/// `xtask/linkage-check` is bin-only (`Cargo.toml` declares `[[bin]]` and
/// there is no `src/lib.rs`), so an integration test under `tests/` has
/// nothing to import a const from; this is the one genuinely unavoidable
/// copy of the four such lists in this crate (issue #669 N3).
///
/// Plain removal (not a bound) is correct for all 11, including
/// `GIT_CEILING_DIRECTORIES` — cleared here unlike `repo_state.rs`'s
/// `Sandbox::git`, which *sets* it via `Sandbox::ceiling` — because every
/// fixture command this helper runs targets an already-initialized repo,
/// never a walk that could resolve past `dir` into nothing. The last 3 —
/// `GIT_CONFIG_PARAMETERS`/`GIT_CONFIG_COUNT`/`GIT_DISCOVERY_ACROSS_FILESYSTEM`
/// — close the config-injection channel issue #669 auditor A2 found still
/// open: git accepts config values (including `core.hooksPath`) directly
/// from `GIT_CONFIG_PARAMETERS`/`GIT_CONFIG_COUNT`+`GIT_CONFIG_KEY_<n>`/
/// `GIT_CONFIG_VALUE_<n>`, bypassing `GIT_CONFIG_NOSYSTEM`/
/// `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM` entirely; `GIT_DISCOVERY_ACROSS_FILESYSTEM`
/// is one of only two things that bound an otherwise-unbounded upward walk.
const GIT_ENV_VARS_TO_CLEAR: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CEILING_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
];

/// Same minimal `## Test Case Catalog` fixture shape as
/// `duplicate_catalog_id.rs`'s `write_fixture`, plus a git repository (check
/// 11 needs one) with `origin/main` faked at the base commit — the same
/// `git update-ref refs/remotes/origin/main HEAD` trick
/// `work_type.rs::init_self_test_repo` uses for `--self-test`'s own scratch
/// repos.
fn git(args: &[&str], dir: &Path) {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null");
    for var in GIT_ENV_VARS_TO_CLEAR {
        cmd.env_remove(var);
    }
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("run git {args:?} in {dir:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} in {dir:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Writes the catalog/test/src scaffolding every check needs to pass other
/// than check 12 (checks 1, 2, 4 and 7 in particular), then `git init`s the
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
/// `[12]`-tagged failure line naming the resurrected fragment.
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
        stderr.contains("[12]") && stderr.contains("259.bugfix.md"),
        "the failure must be tagged [12] and name the resurrected fragment: {stderr}"
    );
}

/// Control case: the same fixture with a genuinely new, unrelated fragment
/// (no resurrection) must still pass through the real binary — and the
/// success line must say what check 12 actually compared (P2: a passing
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
        "a passing run must name what check 12 actually compared, not just print an \
         unchanged success line (P2): {stdout}"
    );
}

/// **Read/write escape — issue #669**, the same shape `repo_state.rs`'s
/// `mod real_git::sandbox_git_ignores_ambient_git_dir_and_git_work_tree`
/// pins for `Sandbox::git()` (issue #579 / PR #663). This file's own `git()`
/// above — its own doc comment says it is copied from `work_type.rs`'s
/// pattern — sets only `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM` and clears
/// nothing else, so an ambient `GIT_DIR`/`GIT_WORK_TREE` steers a fixture
/// invocation past `dir` and onto whatever repository those vars name.
///
/// Verification reads `HEAD` with a bare, unrelated `Command` rather than
/// this file's `git()` (which has no return value to read from), and only
/// *after* the ambient vars are removed from the process — at that point a
/// plain invocation is exactly as reliable as an isolated one, so nothing
/// about the verification step depends on the helper under test.
#[test]
fn git_test_helper_leaks_ambient_git_dir_and_git_work_tree() {
    fn head_of(dir: &Path) -> String {
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir)
            .output()
            .unwrap_or_else(|e| panic!("git rev-parse HEAD in {dir:?}: {e}"));
        assert!(
            out.status.success(),
            "git rev-parse HEAD in {dir:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();

    let fixture = root.join("fixture");
    fs::create_dir_all(&fixture).expect("mkdir fixture");
    git(&["init", "-q", "-b", "main"], &fixture);
    git(&["config", "user.email", "test@example.com"], &fixture);
    git(&["config", "user.name", "test"], &fixture);
    git(
        &["commit", "-q", "--allow-empty", "-m", "fixture first"],
        &fixture,
    );

    let ambient = root.join("ambient");
    fs::create_dir_all(&ambient).expect("mkdir ambient");
    git(&["init", "-q", "-b", "main"], &ambient);
    git(&["config", "user.email", "test@example.com"], &ambient);
    git(&["config", "user.name", "test"], &ambient);
    git(
        &["commit", "-q", "--allow-empty", "-m", "ambient first"],
        &ambient,
    );
    let ambient_head_before = head_of(&ambient);

    // `cargo-nextest` runs each test in its own process, so mutating the
    // process environment here cannot bleed into any other test.
    unsafe {
        std::env::set_var("GIT_DIR", ambient.join(".git"));
        std::env::set_var("GIT_WORK_TREE", &ambient);
    }
    git(
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "escape via ambient GIT_DIR/GIT_WORK_TREE",
        ],
        &fixture,
    );
    unsafe {
        std::env::remove_var("GIT_DIR");
        std::env::remove_var("GIT_WORK_TREE");
    }

    let ambient_head_after = head_of(&ambient);
    assert_eq!(
        ambient_head_after, ambient_head_before,
        "issue #669: `resurrected_changelog_fragment.rs`'s `git()` fixture helper leaked \
         ambient GIT_DIR/GIT_WORK_TREE, so a commit run \"in\" the fixture landed in the \
         ambient repo instead — ambient HEAD moved from {ambient_head_before} to \
         {ambient_head_after}"
    );
}
