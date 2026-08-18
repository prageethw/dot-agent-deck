#![cfg(feature = "e2e")]

//! L2 coverage for issue #467: `init_logging_from_env` (`src/main.rs`)
//! resolved an empty or `"1"` `DOT_AGENT_DECK_LOG` to the hardcoded
//! `/tmp/dot-agent-deck.log`, opened with `create(true).append(true)` and no
//! symlink protection — a predictable path in a world-writable directory
//! (CWE-59/377). The fix relocates the default under the per-user state dir
//! (`state_dir().join("deck.log")`), created owner-only first via
//! `ensure_owner_only_dir` — the same pattern `daemon_attach.rs` already uses
//! for `daemon.log`.
//!
//! `daemon serve` calls `init_logging_from_env()` directly as the very first
//! thing it does (`main.rs`'s `DaemonCmd::Serve` arm), before anything else
//! touches `state_dir()`, so spawning it directly (bypassing the TUI's
//! lazy-spawn client, which is the only other caller that creates the state
//! dir) gives a clean signal: today, nothing creates `<state_dir>` at all
//! when `daemon serve` is invoked this way.

mod common;

use std::time::Duration;

use spec::spec;

/// Scenario: Spawn `dot-agent-deck daemon serve` with `DOT_AGENT_DECK_LOG=1`
/// and `DOT_AGENT_DECK_STATE_DIR` pointed at a fresh, not-yet-created
/// directory. Once the daemon has bound its attach socket (proving startup
/// logging has already run), assert the log file exists at
/// `<state_dir>/deck.log` with real tracing output, and that `<state_dir>`
/// itself was created at mode `0o700` (owner-only) rather than left at
/// whatever `/tmp` provides.
#[spec("lifecycle/log-path/001")]
#[test]
#[cfg(unix)]
fn log_path_001_default_log_lives_under_state_dir_with_owner_only_permissions() {
    use std::os::unix::fs::MetadataExt as _;

    let bin = env!("CARGO_BIN_EXE_dot-agent-deck");
    let dir = common::race_safe_tempdir();
    let work = dir.path();
    let home = work.join("home");
    std::fs::create_dir_all(&home).expect("create HOME");
    let attach_socket = work.join("attach.sock");
    let hook_socket = work.join("hook.sock");
    // Deliberately NOT pre-created: today nothing under the `daemon serve`
    // spawn path touches `state_dir()` at all, so its absence is exactly the
    // starting condition `ensure_owner_only_dir` needs to prove itself
    // against.
    let state_dir = work.join("state");
    let expected_log_path = state_dir.join("deck.log");

    // The behavior under test: the empty/"1" default-resolution branch.
    // `spawn_bare_daemon_and_wait_for_attach` already blocks until the attach
    // socket is bound — the readiness signal that startup logging (the first
    // thing `DaemonCmd::Serve` does) has already run.
    let mut daemon = common::spawn_bare_daemon_and_wait_for_attach(
        bin,
        &home,
        &hook_socket,
        &attach_socket,
        &state_dir,
        "1",
    );

    // The fix's log file is written synchronously (a plain `std::fs::File`
    // writer — see the PRD #170 doc comment on `init_logging_from_env`), but
    // give it a short bounded window past socket-bind for the first tracing
    // line to land, to avoid a hairline race with a different process reading
    // it.
    assert!(
        common::wait_until(Duration::from_secs(5), || expected_log_path.exists()),
        "expected the log file at {} (under DOT_AGENT_DECK_STATE_DIR), but it was never \
         created — today's `init_logging_from_env` hardcodes /tmp/dot-agent-deck.log instead \
         of resolving under state_dir()",
        expected_log_path.display()
    );
    let log = std::fs::read_to_string(&expected_log_path).unwrap_or_else(|e| {
        panic!(
            "failed to read expected log file {}: {e}",
            expected_log_path.display()
        )
    });
    assert!(
        !log.trim().is_empty(),
        "expected the relocated log file to contain startup tracing output, but it was empty"
    );

    // The actual defect fix, not just the path relocation: the parent dir
    // must be created owner-only, mirroring `daemon_attach.rs`'s
    // `ensure_owner_only_dir` call ahead of `daemon.log`.
    let state_dir_metadata = std::fs::metadata(&state_dir).unwrap_or_else(|e| {
        panic!(
            "expected DOT_AGENT_DECK_STATE_DIR ({}) to have been created by the logging \
             setup, but it does not exist: {e}",
            state_dir.display()
        )
    });
    let mode = state_dir_metadata.mode() & 0o777;
    assert_eq!(
        mode, 0o700,
        "expected the state dir to be created owner-only (mode 0o700), got {mode:#o}"
    );

    // Pins that `init_logging_from_env` actually reaches `open_deck_log_file`
    // on the default branch, not just that the log ends up under state_dir():
    // reverting the call site back to a plain `create(true).append(true)`
    // open would leave every unit test against `open_deck_log_file` green
    // while this end-to-end check catches it.
    let log_file_metadata = std::fs::metadata(&expected_log_path).unwrap_or_else(|e| {
        panic!(
            "expected the log file at {} to exist by now: {e}",
            expected_log_path.display()
        )
    });
    let log_file_mode = log_file_metadata.mode() & 0o777;
    assert_eq!(
        log_file_mode, 0o600,
        "expected the log file to be created owner-only (mode 0o600), got {log_file_mode:#o}"
    );

    // Cleanup: never leak the daemon out of the test. `kill()` + `wait()`
    // reaps the child; a `process_running` re-check after a successful reap
    // can only be true of a recycled pid, so there is no follow-up SIGKILL
    // to send.
    let _ = daemon.kill();
    let _ = daemon.wait();
}
