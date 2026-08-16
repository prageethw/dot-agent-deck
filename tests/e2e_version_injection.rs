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
//! ## What it costs, and what actually drives that (issue #928)
//!
//! The single largest factor is **how busy the machine is** — by more than the
//! whole rest of this section put together. Measured on one 16-core dev box,
//! `--jobs` capped to 8 as `capped_jobs` caps it, the three nested builds back
//! to back:
//!
//! | machine | cold | warm | scratch dir |
//! | --- | --- | --- | --- |
//! | quiet (load 3-8) | 65.0s | 24.0s | 1.8 GB |
//! | three other agents building (load 13-18) | 400.9s | 142.3s | 1.8 GB |
//! | quiet, `CARGO_INCREMENTAL=0` as CI sets it | 98.5s | — | 1.3 GB |
//! | busy, `CARGO_INCREMENTAL=0` (load 14-22) | 455.6s | 182.0s | 1.3 GB |
//!
//! That is a **6x** spread on identical work and identical flags. The whole
//! test through `cargo test-e2e version_001` measured 395.3s cold on the busy
//! box; on GitHub `ubuntu-latest`, across 13 `e2e-deterministic` runs read from
//! their own logs (2026-09-06 to 2026-09-09), 71.9s to 305.4s — 4.2x — with
//! two further runs that had no restored cache at 351.1s and 369.9s. So do not
//! read one slow run as a regression, and do not tune this test against a
//! single measurement.
//!
//! ## Issue #928's cache premise was wrong, which is why nothing changed here
//!
//! #928 read the scratch `CARGO_TARGET_DIR` as defeating CI's cache — "cold on
//! every run, forever". It does not. `Swatinem/rust-cache` caches the whole
//! `target/` tree, and its pre-save cleanup *recurses* into a nested target
//! dir: `cleanTargetDir` calls a directory a profile only if that directory
//! holds `build`, `.fingerprint` or `deps`, and `target/version-injection-e2e`
//! holds none of the three at its root, so it is walked rather than pruned.
//! `rmExcept` then keeps every entry belonging to a dependency and drops only
//! this workspace's own. Transcribed from the action at the SHA `ci.yml` pins
//! and run against a real populated scratch dir: **995 entries kept, 24
//! removed** — the 24 being this package's own artifacts and fingerprints plus
//! `incremental/`, `examples/` and the two cargo lock files.
//!
//! So a cache-hit run starts build 1 with the dependency graph already
//! compiled, and what it pays — three times, every run — is a recompile of
//! **this** package (140k lines under `src/`) plus a link. That is the floor
//! for the shape this test has, and two ways under it were considered and
//! rejected: collapsing the builds would stop pinning either
//! `rerun-if-env-changed` directive individually (changing both variables at
//! once proves neither, since either one alone re-runs the whole script), and
//! seeding the scratch dir from the main `target/debug` would mean reaching
//! into cargo's artifact layout so that a test whose job is to prove a BUILD
//! behaves correctly starts from another build's output. A shared compilation
//! cache (`sccache`) would genuinely reach it and is out of this issue's scope,
//! since it would touch every build in the repository rather than this one.
//!
//! ## The `debug = 0` profile that is deliberately NOT here
//!
//! Dropping debug info from these builds looks obviously right: the produced
//! binary is executed twice per build, for `--version` and `daemon hello`, and
//! neither reads a debugger or a symbolised backtrace. Measured on the quiet
//! box, a `dev`-inheriting profile with `debug = 0` against plain `dev`:
//!
//! | | cold | warm | scratch dir |
//! | --- | --- | --- | --- |
//! | `dev` | 65.0s | 24.0s | 1.8 GB |
//! | `debug = 0` | 55.5s | 19.5s / 22.3s | 1.1 GB |
//! | `dev`, `CARGO_INCREMENTAL=0` | 98.5s | — | 1.3 GB |
//! | `debug = 0`, `CARGO_INCREMENTAL=0` | 101.4s | — | 650 MB |
//!
//! The time saving is 10-15% where it appears at all and slightly **negative**
//! in the configuration CI actually uses — inside this box's noise either way,
//! which is the point of the 6x table above. (On the busy box it looked like a
//! halving. It was load, not debug info.) The disk saving is real and about
//! half.
//!
//! It is not here because of the cache. To take effect in CI the profile has to
//! be declared in `Cargo.toml`: rust-cache keys on that manifest and does not
//! re-save on an exact key match, so setting `CARGO_PROFILE_DEV_DEBUG=0` in
//! this file rotates no key, leaves the cache serving `debug` artifacts the
//! build can no longer use, and makes every run pay a cold build with nothing
//! written back. Declaring it rotates the key once — but the first cache
//! written after that carries the old `debug` artifacts forward for good, since
//! cargo never garbage collects a target dir and rust-cache re-saves whatever
//! it restored. CI would store *more*, not less, for a time saving inside the
//! noise. Halving what a worktree keeps on disk is still worth having; it
//! belongs with issue #927, which owns per-worktree build cost.
//!
//! Three containment choices worth keeping:
//!
//! - The build goes to its **own** `CARGO_TARGET_DIR`, so it can neither
//!   clobber the `target/debug/dot-agent-deck` that every other e2e test spawns
//!   nor force a full rebuild of it afterwards (`rerun-if-env-changed` would
//!   otherwise invalidate the build script on the very next plain `cargo
//!   build`).
//! - The nested build's parallelism is **capped at half the machine's cores**
//!   (`--jobs`), because it runs while nextest is executing the rest of the e2e
//!   suite and a full-parallel nested compile can starve the timed tests around
//!   it into flaking. That premise used to be mostly false and is now true: see
//!   the next point.
//! - nextest schedules this test at the **front** of its queue (`priority` in
//!   `.config/nextest.toml`) instead of at its natural binary-name position
//!   about 70% of the way down. It was the last of ~2,680 tests to finish in 13
//!   of 13 CI runs *and* the last to start, at t = 148.6-156.9s every time, so
//!   the job's whole test wall clock was that offset plus this test's duration.
//!   The full measurement is in that file's comment.
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
///
/// No other build in the repository targets it — `grep -r version-injection-e2e`
/// finds this file and the comments in `.config/nextest.toml` that describe it.
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
    // files" — doc.rust-lang.org/cargo/reference/config.html#command-line-overrides).
    //
    // Two things had to be true of the override's own syntax, both verified
    // directly against this checkout's cargo (1.97.0) rather than assumed:
    // - `--config env.DAD_RELEASE_REPO={ value = "..." }` (an inline table)
    //   is rejected outright — "sets a value to an inline table, which is
    //   not accepted" — cargo's CLI form does not accept inline tables.
    // - A bare `--config env.DAD_RELEASE_REPO="..."` merges against the
    //   project file's entry key-by-key rather than replacing it wholesale,
    //   and errors when the shapes disagree: "failed to merge key
    //   `DAD_RELEASE_REPO` … expected table, but found string", because the
    //   project file's entry is itself a table (`{ value = "...", force =
    //   true }`).
    // The dotted-path form below sidesteps both: it sets just the `value`
    // subkey, cargo merges it against the file's table entry field-by-field,
    // and `force = true` carries over unmodified from the file — confirmed
    // with `cargo --config '…DAD_RELEASE_REPO.value="X"' -Z unstable-options
    // config get env.DAD_RELEASE_REPO`, which reported exactly
    // `{ force = true, value = "X" }`. Nothing sets DAD_RELEASE_REPO as a
    // plain OS env var in this function, so `force`'s OS-environment rule
    // never has anything to act on regardless of what it resolves to.
    let release_repo_override = format!("env.DAD_RELEASE_REPO.value=\"{release_repo}\"");

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
