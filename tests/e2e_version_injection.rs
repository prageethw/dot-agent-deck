#![cfg(feature = "e2e")]

//! Issue #250 `lifecycle/version/001` — a build environment that pre-sets
//! `DAD_VERSION` / `DAD_BUILD_ID` produces a binary that reports those values,
//! and *changing* either one invalidates the cached build.
//!
//! This is the only tier that can observe the whole chain: `build.rs` reading
//! the injected env, the `cargo:rerun-if-env-changed` directives that let a
//! *changed* injection take effect at all, and the `env!()` sites in
//! `src/main.rs` / `src/daemon_protocol.rs` that surface it. The pure resolver
//! tests in `tests/build_version.rs` cover the precedence and validation rules
//! themselves.
//!
//! Issue #398 review finding F1 widened this to a third injected value,
//! `DAD_RELEASE_REPO` (the upgrade nudge's poll target — `src/version.rs`'s
//! `GITHUB_RELEASES_URL` `concat!`). That seam was previously pinned nowhere:
//! the resolver tests in `tests/build_version.rs` cover the pure
//! slug-normalisation function, and `lifecycle/version/002`-`004` cover badge
//! rendering, but nothing crossed the build boundary the bug actually lived
//! on. Reverting `src/version.rs` to a hardcoded upstream URL, or losing the
//! `.cargo/config.toml` `[env]` line in a fork sync, left every other gate on
//! this repo green.
//!
//! # Cost — read this before assuming the test is hung
//!
//! It runs **four** real `cargo build`s, all into the same scratch
//! `CARGO_TARGET_DIR` under `target/`:
//!
//! 1. `(VERSION_A, BUILD_ID_A, RELEASE_REPO_A)` — the cold one. On a fresh
//!    worktree this compiles the whole dependency graph: **~10 minutes and
//!    ~1.5 GB of disk, once per worktree**. Every later run of this test is
//!    incremental.
//! 2. `(VERSION_B, BUILD_ID_A, RELEASE_REPO_A)` — only `DAD_VERSION` changed.
//! 3. `(VERSION_B, BUILD_ID_B, RELEASE_REPO_A)` — only `DAD_BUILD_ID` changed.
//! 4. `(VERSION_B, BUILD_ID_B, RELEASE_REPO_B)` — only `DAD_RELEASE_REPO`
//!    changed.
//!
//! Builds 2-4 re-run the build script and relink (tens of seconds), which is
//! exactly why the scratch dir is stable and shared rather than a fresh
//! tempdir per step. The widened `slow-timeout` override for this test in
//! `.config/nextest.toml` exists for the cold case.
//!
//! Two containment choices worth keeping:
//!
//! - The build goes to its **own** `CARGO_TARGET_DIR`, so it can neither
//!   clobber the `target/debug/dot-agent-deck` that every other e2e test spawns
//!   nor force a full rebuild of it afterwards (`rerun-if-env-changed` would
//!   otherwise invalidate the build script on the very next plain `cargo
//!   build`).
//! - The nested build's parallelism is **capped at half the machine's cores**
//!   (`--jobs`), because it runs while nextest is executing the rest of the e2e
//!   suite and a full-parallel nested compile can starve the timed tests around
//!   it into flaking.
//!
//! Decision 6: gated behind the `e2e` feature so `cargo test-fast` never
//! compiles it.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use spec::spec;

/// Values a packager would inject. Deliberately unlike any real tag of this
/// project — and unlike the `0.1.0` placeholder that is the bug's signature —
/// so a passing assertion cannot be a coincidence.
const VERSION_A: &str = "42.7.13";
const BUILD_ID_A: &str = "42.7.13-ginjected0";
/// The second pair. Each is changed on its own, one build apart, so a single
/// missing `cargo:rerun-if-env-changed` line fails the test: changing both at
/// once would prove neither, since either one alone re-runs the whole script.
const VERSION_B: &str = "58.1.2";
const BUILD_ID_B: &str = "58.1.2-ginjected1";

/// A `DAD_RELEASE_REPO` value held constant across builds 1-3, deliberately
/// unlike either real slug this fork cares about (`vfarcic/dot-agent-deck`,
/// `prageethw/dot-agent-deck`), so a passing URL assertion cannot be a
/// coincidence of matching the fork's own default.
const RELEASE_REPO_A: &str = "injected-owner-a/injected-repo-a";
/// Changed on its own in build 4, after `DAD_VERSION` / `DAD_BUILD_ID` are
/// already at their "B" values — the one-at-a-time change that pins
/// `cargo:rerun-if-env-changed=DAD_RELEASE_REPO` individually, the same
/// argument steps 2 and 3 already make for the other two variables.
const RELEASE_REPO_B: &str = "injected-owner-b/injected-repo-b";

/// The `Cargo.toml` placeholder a source build wrongly reported before this fix.
const PLACEHOLDER_VERSION: &str = "0.1.0";

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The isolated `CARGO_TARGET_DIR` every injected build writes into.
fn scratch_target_dir() -> PathBuf {
    let base = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root().join("target"));
    base.join("version-injection-e2e")
}

/// The rustc host triple, e.g. `x86_64-unknown-linux-gnu`.
///
/// The build is pinned to this target explicitly rather than inheriting
/// whatever `CARGO_BUILD_TARGET` / `[build] target` the environment carries:
/// a configured or cross target would put the artifact under
/// `<scratch>/<triple>/debug/` instead of `<scratch>/debug/` (an
/// environment-dependent failure) and might not even produce a binary this
/// machine can run. Pinning it makes both the path and the runnability
/// deterministic.
fn host_target() -> String {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let out = Command::new(&rustc)
        .arg("-vV")
        .output()
        .unwrap_or_else(|e| panic!("failed to run `{rustc} -vV`: {e}"));
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .unwrap_or_else(|| panic!("`{rustc} -vV` did not report a host triple:\n{stdout}"))
        .trim()
        .to_string()
}

/// Half the machine's cores, at least one — see the cost note at the top of the
/// file for why the nested build is capped.
fn capped_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|n| (n.get() / 2).max(1))
        .unwrap_or(1)
}

/// Build `dot-agent-deck` with `version` / `build_id` pre-set as plain OS
/// environment variables, and `release_repo` pre-set via `--config` (see
/// below for why), into the shared scratch target dir. Returns the path of
/// the produced binary.
fn build_with_injected(version: &str, build_id: &str, release_repo: &str) -> PathBuf {
    let scratch = scratch_target_dir();
    let target = host_target();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let jobs = capped_jobs().to_string();

    // DAD_RELEASE_REPO cannot be injected as a plain OS environment variable
    // the way DAD_VERSION / DAD_BUILD_ID are: this checkout's own
    // `.cargo/config.toml` sets `force = true` on its `[env]` entry for this
    // key (issue #398 review finding F3), and `force = true` means the
    // config value is NOT overridable by the environment — a plain
    // `.env("DAD_RELEASE_REPO", ...)` here would be silently discarded in
    // favour of the fork's own default slug, and every build in this test
    // would produce the SAME composed URL regardless of `release_repo`,
    // defeating the point of injecting different values across steps.
    //
    // `--config` sidesteps this: force governs precedence between a config
    // FILE's [env] entry and the OS environment specifically; it says
    // nothing about precedence between two config SOURCES. Cargo's own
    // reference is explicit that `--config` outranks every config file
    // ("Configuration values specified this way take precedence over
    // environment variables, which take precedence over configuration
    // files" — doc.rust-lang.org/cargo/reference/config.html#command-line-overrides),
    // so a `--config env.DAD_RELEASE_REPO="..."` here replaces the whole
    // `[env]` entry the project file defines for this key — `force` and
    // all — before force's OS-environment rule is ever consulted. Nothing
    // sets DAD_RELEASE_REPO as a plain OS env var in this function, so
    // there is no OS-env value for that rule to act on either way.
    let release_repo_override = format!("env.DAD_RELEASE_REPO=\"{release_repo}\"");

    let out = Command::new(&cargo)
        .args([
            "--config",
            &release_repo_override,
            "build",
            "--bin",
            "dot-agent-deck",
            "--target",
            &target,
            "--jobs",
            &jobs,
        ])
        .current_dir(workspace_root())
        .env("CARGO_TARGET_DIR", &scratch)
        .env("DAD_VERSION", version)
        .env("DAD_BUILD_ID", build_id)
        // Cargo passes its jobserver to child processes through this variable;
        // handing it to a nested cargo makes it warn about an unusable
        // jobserver fd. Dropping it just costs the child its own job slots —
        // which `--jobs` above bounds anyway.
        .env_remove("CARGO_MAKEFLAGS")
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn `{cargo} build`: {e}"));

    assert!(
        out.status.success(),
        "building with DAD_VERSION={version} / DAD_BUILD_ID={build_id} / \
         DAD_RELEASE_REPO={release_repo} injected must succeed, \
         got {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let bin = scratch
        .join(&target)
        .join("debug")
        .join(format!("dot-agent-deck{}", std::env::consts::EXE_SUFFIX));
    assert!(
        bin.is_file(),
        "the injected build should have produced {}",
        bin.display()
    );
    bin
}

/// Run the freshly built binary under a scrubbed, isolated environment and
/// return its combined stdout+stderr. Notably the injection vars are NOT set
/// here — a build-time value that only reappears because the runtime env
/// still carries it would prove nothing.
fn run_binary(bin: &Path, args: &[&str]) -> String {
    let home = common::race_safe_tempdir();
    let h = home.path();
    let mut cmd = Command::new(bin);
    cmd.args(args);
    cmd.env_clear();
    if let Ok(path) = std::env::var("PATH") {
        cmd.env("PATH", path);
    }
    cmd.env("HOME", h);
    cmd.env("TERM", "xterm-256color");
    cmd.env("DOT_AGENT_DECK_SOCKET", h.join("hook.sock"));
    cmd.env("DOT_AGENT_DECK_ATTACH_SOCKET", h.join("attach.sock"));
    cmd.env("DOT_AGENT_DECK_STATE_DIR", h.join("state"));
    cmd.env("DOT_AGENT_DECK_IDLE_SHUTDOWN_SECS", "1");
    cmd.env("DOT_AGENT_DECK_TEST_MAX_LIFETIME_SECS", "30");
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let out = cmd.output().expect("spawn the injected dot-agent-deck");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "`dot-agent-deck {}` should exit 0, got {:?}\noutput:\n{combined}",
        args.join(" "),
        out.status.code()
    );
    drop(home);
    combined
}

/// Assert that `bin`'s own bytes contain the release URL composed from
/// `release_repo` — i.e. that the injected `DAD_RELEASE_REPO` reached
/// `src/version.rs`'s `GITHUB_RELEASES_URL` `concat!`. Unlike `DAD_VERSION` /
/// `DAD_BUILD_ID`, nothing on the CLI or the daemon-hello wire surfaces the
/// composed URL, so this is the only way to observe it: `concat!` expands
/// `env!` eagerly, which rustc const-folds into a single string literal in
/// rodata (reviewer verified this against the produced binary), so the whole
/// composed URL is greppable in the artifact — the same property `release.yml`
/// relies on for its own post-build assertion. Checking the FULL URL, not the
/// bare slug, proves the injected value reached the composition rather than
/// merely appearing somewhere by coincidence.
fn assert_binary_contains_composed_url(bin: &Path, step: &str, release_repo: &str) {
    let expected = format!("https://api.github.com/repos/{release_repo}/releases/latest");
    let bytes = std::fs::read(bin)
        .unwrap_or_else(|e| panic!("{step}: failed to read {}: {e}", bin.display()));
    let found = bytes
        .windows(expected.len())
        .any(|window| window == expected.as_bytes());
    assert!(
        found,
        "{step}: the built binary must contain the composed release URL \
         `{expected}` — DAD_RELEASE_REPO={release_repo} must have reached \
         src/version.rs's GITHUB_RELEASES_URL const, got a binary with no \
         occurrence of it"
    );
}

/// Assert that `bin` reports exactly `version` / `build_id` on both surfaces
/// that matter: `--version` (what a packager and the remote pre-flight read)
/// and `daemon hello` (the handshake, which carries both values on the wire).
fn assert_reports(bin: &Path, step: &str, version: &str, build_id: &str) {
    let version_out = run_binary(bin, &["--version"]);
    assert!(
        version_out.contains(version),
        "{step}: `--version` must report the injected DAD_VERSION \
         `{version}`.\noutput:\n{version_out}"
    );
    assert!(
        !version_out.contains(PLACEHOLDER_VERSION),
        "{step}: `--version` must not fall back to the `{PLACEHOLDER_VERSION}` \
         CARGO_PKG_VERSION placeholder when a version was injected.\noutput:\n{version_out}"
    );

    let hello_out = run_binary(bin, &["daemon", "hello"]);
    let hello: serde_json::Value = serde_json::from_str(hello_out.trim())
        .unwrap_or_else(|e| panic!("{step}: `daemon hello` should print JSON ({e}):\n{hello_out}"));
    assert_eq!(
        hello["daemon_version"].as_str(),
        Some(version),
        "{step}: the hello handshake must advertise the injected DAD_VERSION.\noutput:\n{hello_out}"
    );
    assert_eq!(
        hello["build_version"].as_str(),
        Some(build_id),
        "{step}: the hello handshake must advertise the injected DAD_BUILD_ID, not one composed \
         from git.\noutput:\n{hello_out}"
    );
}

/// Scenario: Rebuild the `dot-agent-deck` binary into a scratch target dir four
/// times with different `DAD_VERSION` / `DAD_BUILD_ID` / `DAD_RELEASE_REPO`
/// values pre-set in the build environment, and after each build check the
/// produced binary with those vars absent from its runtime environment. The
/// first build must report the injected `42.7.13` / `42.7.13-ginjected0` (not
/// the `0.1.0` placeholder, not the checkout's git tag) on both `--version`
/// and `daemon hello`, and must contain the release URL composed from the
/// injected `DAD_RELEASE_REPO`; then changing only `DAD_VERSION`, then only
/// `DAD_BUILD_ID`, then only `DAD_RELEASE_REPO`, must each be picked up by the
/// next build rather than served from cache (issue #398 review finding F1
/// widened this from two injected values to three).
#[spec("lifecycle/version/001")]
#[test]
fn version_001_injected_build_env_reaches_the_binary() {
    // Step 1 — the injection is honoured at all (the cold build; see the cost
    // note at the top of this file).
    let bin = build_with_injected(VERSION_A, BUILD_ID_A, RELEASE_REPO_A);
    assert_reports(&bin, "initial injected build", VERSION_A, BUILD_ID_A);
    assert_binary_contains_composed_url(&bin, "initial injected build", RELEASE_REPO_A);

    // Step 2 — change ONLY DAD_VERSION, in the SAME target dir. Without
    // `cargo:rerun-if-env-changed=DAD_VERSION` cargo has no reason to re-run the
    // build script (no source file and no watched git path moved), so the binary
    // would still report VERSION_A and this assertion fails. The unchanged build
    // id and release repo must survive as-is.
    let bin = build_with_injected(VERSION_B, BUILD_ID_A, RELEASE_REPO_A);
    assert_reports(
        &bin,
        "after changing only DAD_VERSION",
        VERSION_B,
        BUILD_ID_A,
    );
    assert_binary_contains_composed_url(&bin, "after changing only DAD_VERSION", RELEASE_REPO_A);

    // Step 3 — now change ONLY DAD_BUILD_ID, again in the same target dir. Same
    // argument for the second rerun directive; the version and release repo
    // must stay at VERSION_B / RELEASE_REPO_A.
    let bin = build_with_injected(VERSION_B, BUILD_ID_B, RELEASE_REPO_A);
    assert_reports(
        &bin,
        "after changing only DAD_BUILD_ID",
        VERSION_B,
        BUILD_ID_B,
    );
    assert_binary_contains_composed_url(&bin, "after changing only DAD_BUILD_ID", RELEASE_REPO_A);

    // Step 4 — now change ONLY DAD_RELEASE_REPO, again in the same target dir.
    // Without `cargo:rerun-if-env-changed=DAD_RELEASE_REPO` (`build.rs:33`)
    // cargo would serve the stale binary from build 3, whose composed URL
    // still names RELEASE_REPO_A, and this assertion fails. Version and build
    // id must stay at VERSION_B / BUILD_ID_B, which `assert_reports` re-checks
    // so a change here cannot be mistaken for a fully stale build.
    let bin = build_with_injected(VERSION_B, BUILD_ID_B, RELEASE_REPO_B);
    assert_reports(
        &bin,
        "after changing only DAD_RELEASE_REPO",
        VERSION_B,
        BUILD_ID_B,
    );
    assert_binary_contains_composed_url(
        &bin,
        "after changing only DAD_RELEASE_REPO",
        RELEASE_REPO_B,
    );
}
