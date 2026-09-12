#![cfg(unix)]

//! Fast-tier tests for two harness-formula concerns that the real-agent
//! orchestration-seed tests (`tests/e2e_orchestration_seed_real.rs`,
//! `tests/e2e_orchestration_seed_retry_real.rs`) rely on but cannot
//! themselves verify, since that whole tier self-skips without real agent
//! credentials.
//!
//! `common::write_codex_project_trust`'s handling of the fixture project path
//! (issue #439): Codex's own subprocess resolves its cwd through any symlinks
//! in the path before it looks up trust in `~/.codex/config.toml` — on
//! macOS, `/tmp` and `/var/tmp` are themselves symlinks into `/private/...`.
//! If the harness writes the raw, uncanonicalized fixture path as the trust
//! key, Codex's canonical-path lookup misses it. This test fabricates an
//! explicit symlink so the defect is exercised deterministically regardless
//! of where the test's own tempdir happens to land.
//!
//! `common::isolated_clone_sibling_path`'s formula (fork issue #373 R3,
//! reformulated by PRD fork#760 Part A): it mirrors, rather than calls, part
//! of `src/ui.rs`'s private `auto_generate_worktree_slug`/
//! `resolve_workspace_path` chain (the blank-Worktree-slug case), so nothing
//! else pins it against the production formula it predicts. Pinned here
//! against a table of inputs all hand-traced to agree exactly with the
//! production chain — the table proves agreement for those traced cases, not
//! divergence.

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

/// Scenario: call `common::isolated_clone_sibling_path` with a table of
/// directory basenames — the one shape every real-agent orchestration-seed
/// test actually produces (a bare `tempfile`-generated `.tmpXXXXXX` dir) plus
/// a couple of basenames that mattered under the pre-fork#760 formula this
/// mirror used to predict — and assert each predicted sibling path matches
/// the value production's `auto_generate_worktree_slug` +
/// `resolve_workspace_path` chain (PRD fork#760 Part A's blank-Worktree-slug
/// case) would compute for the same directory, so a future change to either
/// side has something in the fast tier to fail against instead of silently
/// drifting into a real-agent trust-dialog failure nothing in CI can
/// observe.
#[test]
fn isolated_clone_sibling_path_matches_production_formula_for_known_inputs() {
    // PRD fork#760 Part A's blank-slug formula never sanitizes or re-embeds
    // `dir_name` — unlike the retired pre-fork#760 formula (PRD fork#544 M2),
    // which ran the SUGGESTED NAME (itself containing `dir_name`) through
    // `sanitize_workspace_segment`, then re-embedded the raw `dir_name` a
    // SECOND time via `resolve_workspace_path`'s own prefix — the shape that
    // produced this table's old adversarial cases
    // (`.tmpUxkQzS-tmpUxkQzS-orchestrator-1`, doubled). There is no longer a
    // second, sanitized copy of `dir_name` to exercise: the expected value is
    // simply `"{basename}-orchestrator-1"` for every basename below,
    // sanitized or not, since `dir_name` is used exactly once, raw.
    let cases: &[&str] = &[
        // The only shape this harness's own callers actually produce today.
        ".tmpUxkQzS",
        // Basenames that mattered under the retired double-embedding
        // formula — kept here so a future regression back toward that shape
        // would be caught by this table too.
        " myproj",
        "my..proj",
        "a\\b",
    ];

    for basename in cases {
        let work = std::path::Path::new("/tmp/dad-e2e-fixture-root").join(basename);
        let predicted = common::isolated_clone_sibling_path(&work, 1);
        let got = predicted
            .file_name()
            .expect("predicted sibling path has a file name")
            .to_str()
            .expect("predicted sibling path is UTF-8");
        let expected = format!("{basename}-orchestrator-1");
        assert_eq!(
            got, expected,
            "isolated_clone_sibling_path({basename:?}, 1) diverged from production's \
             auto-generate formula (PRD fork#760 Part A)"
        );
    }
}
