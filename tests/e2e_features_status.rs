#![cfg(feature = "e2e")]

//! Fork issue #303 Phase 2 — diagnosability for the experimental-feature-flag
//! resolution.
//!
//! Phase 1 kept the resolution mechanism itself unchanged (the ancestor walk
//! from cwd, `DOT_AGENT_DECK_FEATURES_CONFIG` as the escape hatch, the
//! process-global `Features` shape) and identified the actual defect as
//! silence: a deck launched from outside the project tree (the maintainer's
//! own case — launched from `$HOME`) resolves every experimental surface OFF
//! with no signal beyond a `tracing::info!`/`warn!` pair gated behind
//! `DOT_AGENT_DECK_LOG`, which is why it took `lsof` to diagnose.
//!
//! This file covers the two diagnosability surfaces Phase 2 adds:
//!   - `dot-agent-deck features status` (on-demand, works whether or not the
//!     deck is running) — `features/status/00N`.
//!   - A conditional startup warning on stderr, ahead of the alternate
//!     screen, requiring neither `DOT_AGENT_DECK_LOG` nor a restart —
//!     `features/startup-warning/00N`.
//!
//! Both are thin real-binary subprocess spawns (mirrors
//! `e2e_session_snapshot.rs` / `e2e_handshake.rs`'s non-PTY `handshake_004`
//! drive) — no PTY is needed for either. Gated behind the `e2e` feature
//! (Decision 6) so `cargo test-fast` never compiles this file.

mod common;

use std::path::Path;
use std::process::{Command, Stdio};

use spec::spec;

/// Path to the freshly-built binary under test (Cargo sets this at
/// integration-test build time).
fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_dot-agent-deck")
}

/// Run `dot-agent-deck features status` as a plain subprocess with the
/// process cwd set to `dir` (what the ancestor walk in
/// `features_config_path()` resolves against) and `extra_env` applied on top
/// of a scrubbed environment. No daemon is involved — `features status` is a
/// pure config read — so this needs no socket/HOME isolation beyond keeping
/// the resolution off the developer's real cwd and env.
fn run_features_status(dir: &Path, extra_env: &[(&str, &str)]) -> (std::process::Output, String) {
    let mut cmd = Command::new(bin());
    cmd.args(["features", "status"]);
    cmd.current_dir(dir);
    cmd.env_clear();
    if let Ok(path) = std::env::var("PATH") {
        cmd.env("PATH", path);
    }
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let out = cmd.output().expect("spawn dot-agent-deck features status");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out, combined)
}

/// Run the TUI entry point far enough to hit `init_and_watch` and the daemon
/// handshake, then exit via `DOT_AGENT_DECK_EXIT_AFTER_HANDSHAKE` before it
/// ever touches the terminal — mirrors `e2e_handshake.rs`'s `handshake_004`
/// non-PTY drive. `dir` is used as BOTH the process cwd (what the ancestor
/// walk resolves against) and `HOME`/socket root, so a lazily spawned daemon
/// stays fully isolated per test and per-test cwd control is independent of
/// where sockets/state land. `extra_env` is applied on top of the isolated
/// base (e.g. `DOT_AGENT_DECK_FEATURES_CONFIG` for the override axis).
fn run_tui_startup_isolated(
    dir: &Path,
    extra_env: &[(&str, &str)],
) -> (std::process::Output, String) {
    let mut cmd = Command::new(bin());
    cmd.current_dir(dir);
    cmd.env_clear();
    if let Ok(path) = std::env::var("PATH") {
        cmd.env("PATH", path);
    }
    cmd.env("HOME", dir);
    cmd.env("TERM", "xterm-256color");
    cmd.env("DOT_AGENT_DECK_SOCKET", dir.join("hook.sock"));
    cmd.env("DOT_AGENT_DECK_ATTACH_SOCKET", dir.join("attach.sock"));
    cmd.env("DOT_AGENT_DECK_STATE_DIR", dir.join("state"));
    // Reap any daemon this spawn lazily starts quickly (no clients/agents)
    // and cap its lifetime as a backstop, mirroring
    // `e2e_session_snapshot.rs`'s `run_isolated`.
    cmd.env("DOT_AGENT_DECK_IDLE_SHUTDOWN_SECS", "1");
    cmd.env("DOT_AGENT_DECK_TEST_MAX_LIFETIME_SECS", "30");
    // Exit right after the build-version handshake, before `run_tui` ever
    // calls `ratatui::init()` — the same safety net `handshake_004` uses to
    // drive the startup path without a PTY.
    cmd.env("DOT_AGENT_DECK_EXIT_AFTER_HANDSHAKE", "1");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let out = cmd.output().expect("spawn dot-agent-deck TUI startup path");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out, combined)
}

/// Scenario: Run `dot-agent-deck features status` from an isolated tempdir
/// with no `.dot-agent-deck.toml` anywhere in its ancestry and
/// `DOT_AGENT_DECK_EXPERIMENTAL=1` set. The output must name the env
/// override as the winning source and report the resolved value as ON.
#[spec("features/status/001")]
#[test]
fn status_001_env_override_wins() {
    let dir = common::race_safe_tempdir();
    let (out, text) = run_features_status(dir.path(), &[("DOT_AGENT_DECK_EXPERIMENTAL", "1")]);

    assert!(
        out.status.success(),
        "features status should exit 0, got {:?}\n{text}",
        out.status.code()
    );
    assert!(
        text.contains("DOT_AGENT_DECK_EXPERIMENTAL env override"),
        "expected the env override to be named as the winning source, got:\n{text}"
    );
    assert!(
        text.contains("experimental: on"),
        "expected experimental: on with the env override set, got:\n{text}"
    );
}

/// Scenario: Write a `.dot-agent-deck.toml` with `[features] experimental =
/// true` into an isolated tempdir, then run `dot-agent-deck features status`
/// from that directory with no env override. The output must report the
/// config path as existing, name the project file as the winning source, and
/// report the resolved value as ON.
#[spec("features/status/002")]
#[test]
fn status_002_project_file_found() {
    let dir = common::race_safe_tempdir();
    std::fs::write(
        dir.path().join(".dot-agent-deck.toml"),
        "[features]\nexperimental = true\n",
    )
    .expect("write project config");

    let (out, text) = run_features_status(dir.path(), &[]);

    assert!(
        out.status.success(),
        "features status should exit 0, got {:?}\n{text}",
        out.status.code()
    );
    assert!(
        text.contains("config path exists: true"),
        "expected the project file to be found, got:\n{text}"
    );
    assert!(
        text.contains("(project file)"),
        "expected the project file to be named as the winning source, got:\n{text}"
    );
    assert!(
        text.contains("experimental: on"),
        "expected experimental: on from the project file, got:\n{text}"
    );
}

/// Scenario: Run `dot-agent-deck features status` from an isolated, empty
/// tempdir with no `.dot-agent-deck.toml` anywhere in its ancestry and no env
/// override — the exact silent-failure state fork issue #303 was filed
/// against. The output must report the config path as not found, name the
/// default (no config found) case as the source, and report the resolved
/// value as OFF.
#[spec("features/status/003")]
#[test]
fn status_003_no_config_found_defaults_off() {
    let dir = common::race_safe_tempdir();
    let (out, text) = run_features_status(dir.path(), &[]);

    assert!(
        out.status.success(),
        "features status should exit 0, got {:?}\n{text}",
        out.status.code()
    );
    assert!(
        text.contains("config path exists: false"),
        "expected no config file to be found, got:\n{text}"
    );
    assert!(
        text.contains("default (no .dot-agent-deck.toml found)"),
        "expected the default-no-config-found case to be named as the source, got:\n{text}"
    );
    assert!(
        text.contains("experimental: off"),
        "expected experimental: off with no config found, got:\n{text}"
    );
}

/// Scenario: Launch the real binary's TUI startup path (through
/// `init_and_watch` and the daemon handshake, exiting via
/// `DOT_AGENT_DECK_EXIT_AFTER_HANDSHAKE` before it ever touches the terminal)
/// from an isolated tempdir with no `.dot-agent-deck.toml` anywhere in its
/// ancestry. Stderr must carry the fork issue #303 diagnosability warning —
/// visible with no `DOT_AGENT_DECK_LOG` set and no restart, the exact
/// visibility gap that used to need `lsof` to diagnose.
#[spec("features/startup-warning/001")]
#[test]
fn startup_warning_001_fires_when_no_config_found() {
    let dir = common::race_safe_tempdir();
    let (out, text) = run_tui_startup_isolated(dir.path(), &[]);

    assert!(
        out.status.success(),
        "the exit-after-handshake path should still exit 0, got {:?}\n{text}",
        out.status.code()
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no .dot-agent-deck.toml found")
            && stderr.contains("experimental flags default to OFF"),
        "expected the missing-config warning on stderr with no DOT_AGENT_DECK_LOG and no \
         restart, got:\n{text}"
    );
}

/// Scenario: Same startup path as `startup_warning_001`, but with a
/// `.dot-agent-deck.toml` present at the launch directory. Stderr must carry
/// NO missing-config warning — proportionate: completely silent for anyone
/// who already has a config, which is what makes the warning acceptable
/// instead of noisy for the common case.
#[spec("features/startup-warning/002")]
#[test]
fn startup_warning_002_silent_when_config_present() {
    let dir = common::race_safe_tempdir();
    std::fs::write(
        dir.path().join(".dot-agent-deck.toml"),
        "[features]\nexperimental = false\n",
    )
    .expect("write project config");

    let (out, text) = run_tui_startup_isolated(dir.path(), &[]);

    assert!(
        out.status.success(),
        "the exit-after-handshake path should still exit 0, got {:?}\n{text}",
        out.status.code()
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("experimental flags default to OFF"),
        "expected NO missing-config warning when a project config is present, got:\n{text}"
    );
}

/// Scenario: Launch the real binary's TUI startup path with
/// `DOT_AGENT_DECK_FEATURES_CONFIG` pointed at a path that does not exist,
/// from an isolated tempdir with no `.dot-agent-deck.toml` anywhere in its
/// own ancestry either. Stderr must carry NO missing-config warning —
/// `missing_config_warning` deliberately treats an operator-supplied
/// override pointing nowhere as a different problem from nobody having set
/// one up, and stays silent whenever the override is set at all, regardless
/// of whether its target exists.
#[spec("features/startup-warning/003")]
#[test]
fn startup_warning_003_silent_when_override_points_at_missing_file() {
    let dir = common::race_safe_tempdir();
    let missing = dir.path().join("does-not-exist.dot-agent-deck.toml");

    let (out, text) = run_tui_startup_isolated(
        dir.path(),
        &[(
            "DOT_AGENT_DECK_FEATURES_CONFIG",
            missing.to_str().expect("utf8 override path"),
        )],
    );

    assert!(
        out.status.success(),
        "the exit-after-handshake path should still exit 0, got {:?}\n{text}",
        out.status.code()
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("experimental flags default to OFF"),
        "expected NO missing-config warning when DOT_AGENT_DECK_FEATURES_CONFIG is set, even to \
         a missing target, got:\n{text}"
    );
}

/// Scenario: Run `dot-agent-deck features status` with
/// `DOT_AGENT_DECK_FEATURES_CONFIG` pointed at an existing file outside the
/// isolated process cwd's own ancestry. The config-path line must name the
/// override (not the ancestor walk) as the winning path source, and the
/// value-source line must name the override target — not "project file" —
/// as the winning value source, since the two are different things once an
/// override is in play.
#[spec("features/status/004")]
#[test]
fn status_004_override_names_override_target() {
    let dir = common::race_safe_tempdir();
    let override_dir = common::race_safe_tempdir();
    let override_path = override_dir.path().join("override.toml");
    std::fs::write(&override_path, "[features]\nexperimental = true\n")
        .expect("write override config");

    let (out, text) = run_features_status(
        dir.path(),
        &[(
            "DOT_AGENT_DECK_FEATURES_CONFIG",
            override_path.to_str().expect("utf8 override path"),
        )],
    );

    assert!(
        out.status.success(),
        "features status should exit 0, got {:?}\n{text}",
        out.status.code()
    );
    assert!(
        text.contains("DOT_AGENT_DECK_FEATURES_CONFIG override"),
        "expected the override to be named as the winning path source, got:\n{text}"
    );
    assert!(
        text.contains("(override target)"),
        "expected the override target — not \"project file\" — to be named as the winning value \
         source, got:\n{text}"
    );
    assert!(
        text.contains("experimental: on"),
        "expected experimental: on from the override target, got:\n{text}"
    );
}
