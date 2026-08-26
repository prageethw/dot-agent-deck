//! PRD #499 (reopened) M1-M10 — the monitored-wait CLI verb
//! (`dot-agent-deck wait start <label>` / `wait done <label> --outcome
//! <success|failure|cancelled|timeout>`) and its composition with the
//! existing `descendant_shell_activity` signal (PRD fork#370/#386).
//!
//! Mirrors `daemon_status.rs`'s "fast synthetic real-binary-subprocess
//! integration" layer rather than a PTY-attached `e2e_*.rs` harness: a real
//! in-process daemon (`common::spawn_inprocess_daemon`, which runs the
//! genuine `run_daemon_with` — including the real 500ms
//! `run_shell_activity_monitor` poll loop), a `cat`-stub pane registered
//! directly on the daemon's own `AgentPtyRegistry`, a synthetic `Idle`
//! `AgentEvent` written to the hook socket to seed a known session (the
//! same precondition `run_shell_activity_monitor`'s own doc comment names:
//! "a bare shell pane that has never emitted a single agent event has no
//! `SessionState` to update at all"), and the REAL `dot-agent-deck wait`
//! CLI run as a subprocess against that daemon. No PTY attach, no vt100
//! grid, no LLM, no `e2e` feature gate — `SessionStatus` is read directly
//! off `AppState.sessions`, exactly as `daemon_status_002` reads it.
//!
//! RED until M3/M7/M8 land: the `wait` subcommand does not exist on `main`
//! yet, so every `run_wait_cli` call below fails at the clap parse step
//! ("unrecognized subcommand 'wait'"), which is a runtime failure of the
//! subprocess this test spawns — not a compile failure of this file. This
//! file references no not-yet-existing Rust API; it only shells out to the
//! compiled binary and reads existing, already-shipped state.
//!
//! Design decisions pinned here for the coder to implement against exactly:
//!   - CLI shape: `dot-agent-deck wait start <label>` and `dot-agent-deck
//!     wait done <label> --outcome <success|failure|cancelled|timeout>`
//!     (outcome is always passed explicitly by these tests, never relied on
//!     as a default).
//!   - Transport: the HOOK socket (`DOT_AGENT_DECK_SOCKET`), the same one
//!     `delegate`/`work-done`/`agent-event` already use — not the attach
//!     socket. Like `delegate`, the calling pane is identified via
//!     `DOT_AGENT_DECK_PANE_ID` read from the CLI subprocess's own
//!     environment.
//!   - TTL override: `wait/monitored/009` sets `DOT_AGENT_DECK_WAIT_TTL_SECS`
//!     on the `wait start` subprocess's own environment (not a
//!     process-global var — this test binary runs under nextest, one
//!     process per test, but a subprocess-scoped env var needs that
//!     property even less). The CLI is expected to read this to override
//!     the production TTL default and carry the resolved value down to the
//!     daemon-side record, so the self-healing sweep in this test's process
//!     completes in seconds rather than needing a real production-length
//!     TTL.
//!   - Composition: `wait/monitored/002` and `/003` are written so today's
//!     `descendant_shell_activity` signal has nothing to see at all (no
//!     descendant process is ever spawned inside the stub pane) — any
//!     `Working` reading they observe once implemented can only come from
//!     the new monitored-wait signal, not a false-positive shared with the
//!     existing mechanism.

mod common;

use std::time::Duration;

use dot_agent_deck::agent_pty::{DOT_AGENT_DECK_PANE_ID, SpawnOptions};
use dot_agent_deck::event::{AgentEvent, AgentType, EventType};
use dot_agent_deck::state::SessionStatus;
#[cfg(unix)]
use spec::spec;

#[cfg(unix)]
struct CliResult {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

#[cfg(unix)]
struct TestPane {
    pane_id: String,
    agent_id: String,
}

/// A synthetic `Idle` `AgentEvent` naming `pane_id`/`agent_id` — the raw
/// shape a real agent's Stop hook would send, used here purely to seed a
/// known `SessionState` at a deterministic `Idle` baseline (`apply_event`'s
/// `EventType::Idle` arm sets `SessionStatus::Idle` unconditionally,
/// `src/state.rs`).
#[cfg(unix)]
fn idle_event(pane_id: &str, agent_id: &str, session_id: &str) -> AgentEvent {
    AgentEvent {
        session_id: session_id.to_string(),
        agent_type: AgentType::Pi,
        event_type: EventType::Idle,
        tool_name: None,
        tool_detail: None,
        cwd: None,
        timestamp: chrono::Utc::now(),
        user_prompt: None,
        metadata: std::collections::HashMap::new(),
        pane_id: Some(pane_id.to_string()),
        agent_id: Some(agent_id.to_string()),
        agent_version: None,
        schema_version: None,
        live_target: None,
        model: None,
    }
}

/// Register a `cat`-stub pane directly on the daemon's own live
/// `AgentPtyRegistry` (the same one `run_shell_activity_monitor` polls),
/// seed it to a known `Idle` session over the hook socket, and wait for
/// that baseline to land before returning — so every test below starts
/// from a confirmed, deterministic `Idle` reading rather than racing the
/// seed event.
#[cfg(unix)]
async fn setup_idle_pane(daemon: &common::InProcDaemon, cwd: &str, pane_id: &str) -> TestPane {
    let agent_id = daemon
        .registry
        .spawn_agent(SpawnOptions {
            command: Some("cat"),
            cwd: Some(cwd),
            env: vec![(DOT_AGENT_DECK_PANE_ID.to_string(), pane_id.to_string())],
            ..SpawnOptions::default()
        })
        .expect("spawn cat-stub pane for wait/monitored setup");

    let session_id = format!("{pane_id}-session");
    common::write_hook_line(
        &daemon.hook_path,
        &serde_json::to_string(&idle_event(pane_id, &agent_id, &session_id))
            .expect("serialize seed Idle event"),
    )
    .expect("write seed Idle event to the daemon hook socket");

    wait_for_status(
        daemon,
        pane_id,
        &SessionStatus::Idle,
        Duration::from_secs(5),
    )
    .await
    .unwrap_or_else(|e| panic!("setup_idle_pane baseline seed for {pane_id:?}: {e}"));

    TestPane {
        pane_id: pane_id.to_string(),
        agent_id,
    }
}

/// Run the REAL `dot-agent-deck wait <args>` CLI as a subprocess against
/// `daemon`'s hook socket, with `DOT_AGENT_DECK_PANE_ID` set to `pane_id` —
/// exactly as a role's own shell would invoke it. `extra_env` carries
/// per-call overrides (`wait/monitored/009`'s TTL override).
#[cfg(unix)]
async fn run_wait_cli(
    daemon: &common::InProcDaemon,
    pane_id: &str,
    args: &[&str],
    extra_env: &[(&str, &str)],
) -> CliResult {
    let hook_path = daemon.hook_path.clone();
    let pane_id = pane_id.to_string();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let extra_env: Vec<(String, String)> = extra_env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    tokio::task::spawn_blocking(move || {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_dot-agent-deck"));
        cmd.arg("wait");
        cmd.args(&args);
        cmd.env("DOT_AGENT_DECK_SOCKET", &hook_path);
        cmd.env(DOT_AGENT_DECK_PANE_ID, &pane_id);
        for (k, v) in &extra_env {
            cmd.env(k, v);
        }
        let output = cmd
            .output()
            .expect("run the `dot-agent-deck wait` CLI subprocess");
        CliResult {
            status: output.status,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    })
    .await
    .expect("wait CLI subprocess task did not panic")
}

/// The pane's current `SessionStatus`, joined by `pane_id` — `None` if no
/// session names this pane at all.
#[cfg(unix)]
async fn current_status(daemon: &common::InProcDaemon, pane_id: &str) -> Option<SessionStatus> {
    daemon
        .state
        .read()
        .await
        .sessions
        .values()
        .find(|s| s.pane_id.as_deref() == Some(pane_id))
        .map(|s| s.status.clone())
}

/// Poll until `pane_id`'s status equals `expected` or `timeout` elapses.
#[cfg(unix)]
async fn wait_for_status(
    daemon: &common::InProcDaemon,
    pane_id: &str,
    expected: &SessionStatus,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let observed = current_status(daemon, pane_id).await;
        if observed.as_ref() == Some(expected) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "pane {pane_id:?} never reached {expected:?} within {timeout:?}; last observed \
                 status = {observed:?}"
            ));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Sample `pane_id`'s status repeatedly across `hold`, asserting it equals
/// `expected` on every sample — proves persistence across multiple
/// `run_shell_activity_monitor` poll ticks (500ms), not just a single
/// instant.
#[cfg(unix)]
async fn assert_status_holds(
    daemon: &common::InProcDaemon,
    pane_id: &str,
    expected: &SessionStatus,
    hold: Duration,
    ctx: &str,
) {
    let deadline = tokio::time::Instant::now() + hold;
    loop {
        let observed = current_status(daemon, pane_id).await;
        assert_eq!(
            observed.as_ref(),
            Some(expected),
            "{ctx}: pane {pane_id:?} must hold {expected:?} for the whole {hold:?} window \
             (sampled mid-window); observed {observed:?}"
        );
        if tokio::time::Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

const PANE_001: &str = "wait-monitored-pane-001-4f8a2c";
const LABEL_001: &str = "wait-monitored-label-001-ci-check";

/// Scenario: with a single `cat`-stub pane seeded to a known `Idle` session
/// and no shell descendant ever spawned, run the real `dot-agent-deck wait
/// start <label>` CLI and confirm the pane's status flips to `Working`;
/// then run `wait done <label> --outcome success` and confirm it returns to
/// `Idle`.
#[spec("wait/monitored/001")]
#[test]
#[cfg(unix)]
fn wait_monitored_001_start_sets_working_done_clears_to_idle() {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build wait/monitored/001 runtime")
        .block_on(wait_monitored_001_start_sets_working_done_clears_to_idle_inner());
}

#[cfg(unix)]
async fn wait_monitored_001_start_sets_working_done_clears_to_idle_inner() {
    let daemon = common::spawn_inprocess_daemon().await;
    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd.path().to_string_lossy().into_owned();
    let pane = setup_idle_pane(&daemon, &cwd_str, PANE_001).await;

    let start = run_wait_cli(&daemon, &pane.pane_id, &["start", LABEL_001], &[]).await;
    assert!(
        start.status.success(),
        "wait/monitored/001: `wait start {LABEL_001}` must succeed in an otherwise-idle pane; \
         status={:?} stdout={:?} stderr={:?}",
        start.status,
        start.stdout,
        start.stderr
    );
    wait_for_status(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_secs(5),
    )
    .await
    .unwrap_or_else(|e| panic!("wait/monitored/001 (after start): {e}"));

    let done = run_wait_cli(
        &daemon,
        &pane.pane_id,
        &["done", LABEL_001, "--outcome", "success"],
        &[],
    )
    .await;
    assert!(
        done.status.success(),
        "wait/monitored/001: `wait done {LABEL_001} --outcome success` must succeed; \
         status={:?} stdout={:?} stderr={:?}",
        done.status,
        done.stdout,
        done.stderr
    );
    wait_for_status(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Idle,
        Duration::from_secs(5),
    )
    .await
    .unwrap_or_else(|e| panic!("wait/monitored/001 (after done): {e}"));

    let _ = &pane.agent_id;
    daemon.registry.shutdown_all();
}

const PANE_002: &str = "wait-monitored-pane-002-9d3b71";
const LABEL_002: &str = "wait-monitored-label-002-poll-gaps";

/// Scenario: after `wait start <label>` on a pane with no shell descendant
/// ever spawned, hold across ~4 real 500ms `run_shell_activity_monitor`
/// poll ticks with genuine gaps and no activity in between, asserting
/// `Working` on every sample — the pre-#499-reopen mechanism
/// (`descendant_shell_activity` alone) would have reverted to `Idle` by the
/// very first gap, since it has no live foreground child to observe.
/// `wait done <label> --outcome success` then clears it back to `Idle`.
#[spec("wait/monitored/002")]
#[test]
#[cfg(unix)]
fn wait_monitored_002_persists_across_polling_gaps_with_no_descendant() {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build wait/monitored/002 runtime")
        .block_on(wait_monitored_002_persists_across_polling_gaps_with_no_descendant_inner());
}

#[cfg(unix)]
async fn wait_monitored_002_persists_across_polling_gaps_with_no_descendant_inner() {
    let daemon = common::spawn_inprocess_daemon().await;
    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd.path().to_string_lossy().into_owned();
    let pane = setup_idle_pane(&daemon, &cwd_str, PANE_002).await;

    let start = run_wait_cli(&daemon, &pane.pane_id, &["start", LABEL_002], &[]).await;
    assert!(
        start.status.success(),
        "wait/monitored/002: `wait start {LABEL_002}` must succeed; status={:?} stdout={:?} \
         stderr={:?}",
        start.status,
        start.stdout,
        start.stderr
    );
    wait_for_status(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_secs(5),
    )
    .await
    .unwrap_or_else(|e| panic!("wait/monitored/002 (after start): {e}"));

    assert_status_holds(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_millis(2200),
        "wait/monitored/002",
    )
    .await;

    let done = run_wait_cli(
        &daemon,
        &pane.pane_id,
        &["done", LABEL_002, "--outcome", "success"],
        &[],
    )
    .await;
    assert!(
        done.status.success(),
        "wait/monitored/002: `wait done {LABEL_002} --outcome success` must succeed; \
         status={:?} stdout={:?} stderr={:?}",
        done.status,
        done.stdout,
        done.stderr
    );
    wait_for_status(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Idle,
        Duration::from_secs(5),
    )
    .await
    .unwrap_or_else(|e| panic!("wait/monitored/002 (after done): {e}"));

    daemon.registry.shutdown_all();
}

const PANE_003_A: &str = "wait-monitored-pane-003a-2e6f19";
const PANE_003_B: &str = "wait-monitored-pane-003b-7c4d82";
const LABEL_003: &str = "wait-monitored-label-003-attribution";

/// Scenario: two independent `cat`-stub panes, both seeded `Idle`; only
/// pane A runs `wait start <label>`. Assert pane A reads `Working` and
/// holds it across several poll ticks while pane B — untouched — holds
/// `Idle` the whole time, proving attribution is per-pane and never a
/// global "something somewhere is working" flag. `wait done` on A clears
/// only A.
#[spec("wait/monitored/003")]
#[test]
#[cfg(unix)]
fn wait_monitored_003_attribution_is_per_pane_not_global() {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build wait/monitored/003 runtime")
        .block_on(wait_monitored_003_attribution_is_per_pane_not_global_inner());
}

#[cfg(unix)]
async fn wait_monitored_003_attribution_is_per_pane_not_global_inner() {
    let daemon = common::spawn_inprocess_daemon().await;
    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd.path().to_string_lossy().into_owned();
    let pane_a = setup_idle_pane(&daemon, &cwd_str, PANE_003_A).await;
    let pane_b = setup_idle_pane(&daemon, &cwd_str, PANE_003_B).await;

    let start = run_wait_cli(&daemon, &pane_a.pane_id, &["start", LABEL_003], &[]).await;
    assert!(
        start.status.success(),
        "wait/monitored/003: `wait start {LABEL_003}` on pane A must succeed; status={:?} \
         stdout={:?} stderr={:?}",
        start.status,
        start.stdout,
        start.stderr
    );
    wait_for_status(
        &daemon,
        &pane_a.pane_id,
        &SessionStatus::Working,
        Duration::from_secs(5),
    )
    .await
    .unwrap_or_else(|e| panic!("wait/monitored/003 (pane A after start): {e}"));

    // Sample both concurrently across the same hold window: A must stay
    // Working, B must stay Idle throughout — never a global flag flipping
    // both, and never B picking up A's wait by accident.
    let hold = Duration::from_millis(1800);
    let deadline = tokio::time::Instant::now() + hold;
    loop {
        let status_a = current_status(&daemon, &pane_a.pane_id).await;
        let status_b = current_status(&daemon, &pane_b.pane_id).await;
        assert_eq!(
            status_a,
            Some(SessionStatus::Working),
            "wait/monitored/003: pane A (the one that declared the wait) must hold Working; \
             observed {status_a:?}"
        );
        assert_eq!(
            status_b,
            Some(SessionStatus::Idle),
            "wait/monitored/003: pane B (untouched) must hold Idle while pane A's wait is \
             active — attribution must be per-pane, never global; observed {status_b:?}"
        );
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    let done = run_wait_cli(
        &daemon,
        &pane_a.pane_id,
        &["done", LABEL_003, "--outcome", "success"],
        &[],
    )
    .await;
    assert!(
        done.status.success(),
        "wait/monitored/003: `wait done {LABEL_003} --outcome success` on pane A must succeed; \
         status={:?} stdout={:?} stderr={:?}",
        done.status,
        done.stdout,
        done.stderr
    );
    wait_for_status(
        &daemon,
        &pane_a.pane_id,
        &SessionStatus::Idle,
        Duration::from_secs(5),
    )
    .await
    .unwrap_or_else(|e| panic!("wait/monitored/003 (pane A after done): {e}"));

    let _ = &pane_b.agent_id;
    daemon.registry.shutdown_all();
}

const PANE_004: &str = "wait-monitored-pane-004-baseline-3a71dd";

/// Scenario: a pane with no monitored wait ever declared and no shell
/// descendant reads `Idle` and holds it — the regression guard proving the
/// new mechanism does not affect a genuinely idle pane. This test makes no
/// `wait` CLI call at all, so it is expected to already be green before
/// M3-M8 land; it is captured now as a permanent guard the implementation
/// must not break.
#[spec("wait/monitored/004")]
#[test]
#[cfg(unix)]
fn wait_monitored_004_untouched_pane_stays_idle() {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build wait/monitored/004 runtime")
        .block_on(wait_monitored_004_untouched_pane_stays_idle_inner());
}

#[cfg(unix)]
async fn wait_monitored_004_untouched_pane_stays_idle_inner() {
    let daemon = common::spawn_inprocess_daemon().await;
    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd.path().to_string_lossy().into_owned();
    let pane = setup_idle_pane(&daemon, &cwd_str, PANE_004).await;

    assert_status_holds(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Idle,
        Duration::from_millis(1500),
        "wait/monitored/004",
    )
    .await;

    let _ = &pane.agent_id;
    daemon.registry.shutdown_all();
}

/// Shared body for `wait/monitored/005`-`008`: declare a wait, confirm
/// `Working`, clear it with the given `--outcome` value, confirm it returns
/// to `Idle`. All four terminal outcomes must clear the wait — clearing
/// only on success would leave the other three wedged `Working` forever
/// (the PRD #421/#464 stale-claim hazard this PRD's Risks table names
/// directly).
#[cfg(unix)]
async fn wait_monitored_outcome_clears_working_inner(
    pane_id: &str,
    label: &str,
    outcome: &str,
    spec_id: &str,
) {
    let daemon = common::spawn_inprocess_daemon().await;
    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd.path().to_string_lossy().into_owned();
    let pane = setup_idle_pane(&daemon, &cwd_str, pane_id).await;

    let start = run_wait_cli(&daemon, &pane.pane_id, &["start", label], &[]).await;
    assert!(
        start.status.success(),
        "{spec_id}: `wait start {label}` must succeed; status={:?} stdout={:?} stderr={:?}",
        start.status,
        start.stdout,
        start.stderr
    );
    wait_for_status(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_secs(5),
    )
    .await
    .unwrap_or_else(|e| panic!("{spec_id} (after start): {e}"));

    let done = run_wait_cli(
        &daemon,
        &pane.pane_id,
        &["done", label, "--outcome", outcome],
        &[],
    )
    .await;
    assert!(
        done.status.success(),
        "{spec_id}: `wait done {label} --outcome {outcome}` must succeed; status={:?} \
         stdout={:?} stderr={:?}",
        done.status,
        done.stdout,
        done.stderr
    );
    wait_for_status(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Idle,
        Duration::from_secs(5),
    )
    .await
    .unwrap_or_else(|e| panic!("{spec_id} (after done --outcome {outcome}): {e}"));

    daemon.registry.shutdown_all();
}

const PANE_005: &str = "wait-monitored-pane-005-outcome-success";
const LABEL_005: &str = "wait-monitored-label-005-outcome-success";

/// Scenario: `wait start` then `wait done --outcome success` clears
/// `Working` back to `Idle`.
#[spec("wait/monitored/005")]
#[test]
#[cfg(unix)]
fn wait_monitored_005_outcome_success_clears_working() {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build wait/monitored/005 runtime")
        .block_on(wait_monitored_outcome_clears_working_inner(
            PANE_005,
            LABEL_005,
            "success",
            "wait/monitored/005",
        ));
}

const PANE_006: &str = "wait-monitored-pane-006-outcome-failure";
const LABEL_006: &str = "wait-monitored-label-006-outcome-failure";

/// Scenario: `wait start` then `wait done --outcome failure` clears
/// `Working` back to `Idle` — a failed external outcome must not wedge the
/// pane `Working` forever.
#[spec("wait/monitored/006")]
#[test]
#[cfg(unix)]
fn wait_monitored_006_outcome_failure_clears_working() {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build wait/monitored/006 runtime")
        .block_on(wait_monitored_outcome_clears_working_inner(
            PANE_006,
            LABEL_006,
            "failure",
            "wait/monitored/006",
        ));
}

const PANE_007: &str = "wait-monitored-pane-007-outcome-cancelled";
const LABEL_007: &str = "wait-monitored-label-007-outcome-cancelled";

/// Scenario: `wait start` then `wait done --outcome cancelled` clears
/// `Working` back to `Idle`.
#[spec("wait/monitored/007")]
#[test]
#[cfg(unix)]
fn wait_monitored_007_outcome_cancelled_clears_working() {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build wait/monitored/007 runtime")
        .block_on(wait_monitored_outcome_clears_working_inner(
            PANE_007,
            LABEL_007,
            "cancelled",
            "wait/monitored/007",
        ));
}

const PANE_008: &str = "wait-monitored-pane-008-outcome-timeout";
const LABEL_008: &str = "wait-monitored-label-008-outcome-timeout";

/// Scenario: `wait start` then `wait done --outcome timeout` clears
/// `Working` back to `Idle`.
#[spec("wait/monitored/008")]
#[test]
#[cfg(unix)]
fn wait_monitored_008_outcome_timeout_clears_working() {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build wait/monitored/008 runtime")
        .block_on(wait_monitored_outcome_clears_working_inner(
            PANE_008,
            LABEL_008,
            "timeout",
            "wait/monitored/008",
        ));
}

const PANE_009: &str = "wait-monitored-pane-009-ttl-2f91ab";
const LABEL_009: &str = "wait-monitored-label-009-ttl-selfheal";

/// Scenario: `wait start <label>` with the test-only TTL override
/// (`DOT_AGENT_DECK_WAIT_TTL_SECS=2` on the CLI subprocess's own
/// environment) flips the pane to `Working`; `wait done` is deliberately
/// never called. After sleeping past the 2s TTL plus poll-cadence margin,
/// the pane must have self-healed back to `Idle` on its own, proving the
/// monitored wait cannot wedge a pane `Working` forever when nobody clears
/// it.
#[spec("wait/monitored/009")]
#[test]
#[cfg(unix)]
fn wait_monitored_009_ttl_self_heals_without_explicit_done() {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build wait/monitored/009 runtime")
        .block_on(wait_monitored_009_ttl_self_heals_without_explicit_done_inner());
}

#[cfg(unix)]
async fn wait_monitored_009_ttl_self_heals_without_explicit_done_inner() {
    let daemon = common::spawn_inprocess_daemon().await;
    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd.path().to_string_lossy().into_owned();
    let pane = setup_idle_pane(&daemon, &cwd_str, PANE_009).await;

    let start = run_wait_cli(
        &daemon,
        &pane.pane_id,
        &["start", LABEL_009],
        &[("DOT_AGENT_DECK_WAIT_TTL_SECS", "2")],
    )
    .await;
    assert!(
        start.status.success(),
        "wait/monitored/009: `wait start {LABEL_009}` with a 2s TTL override must succeed; \
         status={:?} stdout={:?} stderr={:?}",
        start.status,
        start.stdout,
        start.stderr
    );
    wait_for_status(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_secs(5),
    )
    .await
    .unwrap_or_else(|e| panic!("wait/monitored/009 (after start): {e}"));

    // Deliberately no `wait done` call. Sleep past the 2s TTL plus enough
    // margin for the 500ms shell-activity poll cadence and a sweep tick to
    // notice, then confirm the self-heal happened with nobody clearing it
    // explicitly.
    tokio::time::sleep(Duration::from_secs(3)).await;
    wait_for_status(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Idle,
        Duration::from_secs(5),
    )
    .await
    .unwrap_or_else(|e| panic!("wait/monitored/009 (TTL self-heal): {e}"));

    daemon.registry.shutdown_all();
}
