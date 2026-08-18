#![cfg(unix)]

//! Fast-tier test for `common::write_codex_project_trust`'s handling of the
//! fixture project path (issue #439).
//!
//! Codex's own subprocess resolves its cwd through any symlinks in the path
//! before it looks up trust in `~/.codex/config.toml` — on macOS, `/tmp` and
//! `/var/tmp` are themselves symlinks into `/private/...`. If the harness
//! writes the raw, uncanonicalized fixture path as the trust key, Codex's
//! canonical-path lookup misses it. This test fabricates an explicit symlink
//! so the defect is exercised deterministically regardless of where the
//! test's own tempdir happens to land.

mod common;

/// Scenario: write a Codex project-trust entry for a fixture directory
/// reached only via a symlink, read back `config.toml`, and assert the
/// `[projects."..."]` key names the canonicalized target — not the raw,
/// symlink-containing path that was passed in.
#[test]
fn codex_project_trust_canonicalizes_symlinked_project_path() {
    let target = common::race_safe_tempdir();
    let link_parent = common::race_safe_tempdir();
    let link = link_parent.path().join("project-link");
    std::os::unix::fs::symlink(target.path(), &link).expect("create project symlink fixture");

    let canonical = std::fs::canonicalize(&link).expect("canonicalize symlinked project dir");
    assert_ne!(
        canonical, link,
        "test fixture bug: the symlink did not actually differ from its target"
    );

    let codex_home = link_parent.path().join(".codex");
    std::fs::create_dir_all(&codex_home).expect("create codex home dir");

    common::write_codex_project_trust(&codex_home, &link)
        .expect("write codex project trust config");

    let config =
        std::fs::read_to_string(codex_home.join("config.toml")).expect("read written config.toml");

    let expected_key = format!("[projects.\"{}\"]", canonical.to_str().expect("utf8 path"));
    let raw_key = format!("[projects.\"{}\"]", link.to_str().expect("utf8 path"));

    assert!(
        config.contains(&expected_key),
        "config.toml did not key the trust entry by the canonicalized project \
         path {canonical:?}; wrote:\n{config}\n(uncanonicalized key present instead: {})",
        config.contains(&raw_key),
    );
}
