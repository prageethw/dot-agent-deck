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
//! `common::isolated_clone_sibling_path`'s formula (fork issue #373 R3): it
//! mirrors, rather than calls, part of `src/ui.rs`'s private
//! `suggest_orchestration_name`/`sanitize_workspace_segment`/
//! `resolve_workspace_path` chain, so nothing else pins it against the
//! production formula it predicts. Pinned here against a table of inputs
//! including the ones this project's own review round found to already
//! diverge from production for non-`tempfile`-shaped basenames.

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
/// several adversarial basenames a fork#373 review round traced against
/// production by hand — and assert each predicted sibling path matches the
/// value production's `suggest_orchestration_name` +
/// `sanitize_workspace_segment` + `resolve_workspace_path` chain would
/// compute for the same directory, so a future change to either side has
/// something in the fast tier to fail against instead of silently drifting
/// into a real-agent trust-dialog failure nothing in CI can observe.
#[test]
fn isolated_clone_sibling_path_matches_production_formula_for_known_inputs() {
    // (basename, expected sibling basename) — the "expected" column is the
    // value production's `suggest_orchestration_name` (trims the basename,
    // then appends "-orchestrator-1") -> `sanitize_workspace_segment` (runs
    // `sanitize_clone_segment`, then strips a leading '-'/'.') ->
    // `resolve_workspace_path` (`work.with_file_name("{dir_name}-{segment}")`,
    // using the ORIGINAL untrimmed dir_name) chain produces, worked by hand
    // and cross-checked against fork issue #373's review/audit findings.
    let cases: &[(&str, &str)] = &[
        // The only shape this harness's own callers actually produce today —
        // matches the byte-for-byte captured production path in
        // `isolated_clone_sibling_path`'s own doc comment.
        (".tmpUxkQzS", ".tmpUxkQzS-tmpUxkQzS-orchestrator-1"),
        // Adversarial: leading whitespace. Production trims the basename
        // before building the candidate Name; this mirror's `.trim()` (via
        // `sanitize_clone_segment`) runs on the already-concatenated
        // "{dir_name}-orchestrator-1" string, which happens to strip the
        // same leading whitespace since it sits at the very front.
        (" myproj", " myproj-myproj-orchestrator-1"),
        // Adversarial: an interior `..`. `sanitize_clone_segment` strips
        // every `".."` occurrence, wherever it sits, so this matches
        // regardless of trim-before-vs-after-concatenation ordering.
        ("my..proj", "my..proj-myproj-orchestrator-1"),
        // Adversarial: a backslash, which `sanitize_clone_segment` maps to
        // `-`.
        ("a\\b", "a\\b-a-b-orchestrator-1"),
    ];

    for (basename, expected_suffix) in cases {
        let work = std::path::Path::new("/tmp/dad-e2e-fixture-root").join(basename);
        let predicted = common::isolated_clone_sibling_path(&work, 1);
        let got = predicted
            .file_name()
            .expect("predicted sibling path has a file name")
            .to_str()
            .expect("predicted sibling path is UTF-8");
        assert_eq!(
            got, *expected_suffix,
            "isolated_clone_sibling_path({basename:?}, 1) diverged from the \
             hand-traced production formula (fork issue #373 R3)"
        );
    }
}
