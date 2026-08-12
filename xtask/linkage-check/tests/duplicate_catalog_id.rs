//! Fork #148 — `cargo xtask linkage-check` must not report `ok` on a tree
//! whose `tests/CATALOG.md` has two `##### <id>` headings sharing one id.
//!
//! Reproduced for real during the 2026-08-08/09 upstream sync: two
//! `##### tabs/orchestration/011` headings landed in `tests/CATALOG.md`
//! (one from a renumbered fork test, one from a pre-existing fork test),
//! and `cargo xtask linkage-check` printed `ok`. The collision was found
//! only by hand (`grep -E '^##### ' tests/CATALOG.md | sed ... | sort |
//! uniq -d`).
//!
//! This drives the compiled `xtask-linkage-check` binary against a
//! throwaway fixture tree — its own `Cargo.toml` carries the literal
//! `[workspace]` marker `repo_root()` looks for — so the check runs
//! exactly as it does in the real repo. No internal API is touched, only
//! the tool's actual CLI entry point: the bug is that the WHOLE TOOL
//! answers wrong, not that some internal function lacks a case, and the
//! catalog-id parser (`parse_catalog_ids`) returns a `BTreeSet<String>` —
//! a shape that cannot represent "this id appeared twice" no matter what
//! it's fed, so pinning this at that level would require inventing a new
//! return type rather than testing the tool that exists today.

use std::fs;
use std::path::Path;
use std::process::Command;

/// Minimal `## Test Case Catalog` fixture tree: one matching `#[spec]`
/// test so checks 1 (every catalog id has an annotation), 2 (annotation
/// resolves to a real id), 4 (fn name prefix) and 7 (docs generator +
/// Scenario comment) all pass — leaving the duplicate-heading condition as
/// the only thing that should fail the run.
fn write_fixture(root: &Path, catalog_body: &str) {
    fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").expect("write Cargo.toml");
    let tests_dir = root.join("tests");
    fs::create_dir_all(&tests_dir).expect("mkdir tests/");
    fs::write(tests_dir.join("CATALOG.md"), catalog_body).expect("write CATALOG.md");
    fs::write(
        tests_dir.join("dup_test.rs"),
        "#[spec(\"fixture/dupcat/001\")]\n\
         #[test]\n\
         /// Scenario: pins fork issue #148 — duplicate catalog heading.\n\
         fn dupcat_001_pins_duplicate_heading() {}\n",
    )
    .expect("write dup_test.rs");
}

#[test]
fn linkage_check_fails_on_two_catalog_headings_sharing_one_id() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let catalog = "\
## Test Case Catalog

##### fixture/dupcat/001 — First entry
- **Layer:** L1.

##### fixture/dupcat/001 — Second entry
- **Layer:** L1.
";
    write_fixture(tmp.path(), catalog);

    let bin = env!("CARGO_BIN_EXE_xtask-linkage-check");
    let output = Command::new(bin)
        .current_dir(tmp.path())
        .output()
        .expect("run xtask-linkage-check");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "linkage-check reported success on a catalog with two \
         `##### fixture/dupcat/001` headings (fork #148)\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// Control case: the same fixture with the duplicate heading resolved
/// (only ONE `##### fixture/dupcat/001` heading) must still pass, so the
/// fix that satisfies the test above cannot be "fail unconditionally".
#[test]
fn linkage_check_passes_once_the_duplicate_heading_is_resolved() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let catalog = "\
## Test Case Catalog

##### fixture/dupcat/001 — First entry
- **Layer:** L1.
";
    write_fixture(tmp.path(), catalog);

    let bin = env!("CARGO_BIN_EXE_xtask-linkage-check");
    let output = Command::new(bin)
        .current_dir(tmp.path())
        .output()
        .expect("run xtask-linkage-check");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "linkage-check failed on a fixture with no duplicate heading\nstdout: {stdout}\nstderr: {stderr}"
    );
}
