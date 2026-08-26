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
use dot_agent_deck::event::{AgentEvent, AgentType, BroadcastMsg, EventType, WaitOutcome};
use dot_agent_deck::state::{AppState, SessionStatus};
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

/// A synthetic `ShellBusy`/`ShellIdle` `AgentEvent`, shaped the way the
/// daemon's own `run_shell_activity_monitor` builds one (`AgentType::None`,
/// no tool/session metadata — mirrors `shell_event` in
/// `tests/rehydration.rs`) — used to drive the existing
/// `descendant_shell_activity` signal directly over the hook socket without
/// needing a real foreground descendant process, so composition with the new
/// monitored-wait signal can be tested deterministically.
#[cfg(unix)]
fn shell_activity_event(
    pane_id: &str,
    agent_id: &str,
    session_id: &str,
    event_type: EventType,
) -> AgentEvent {
    AgentEvent {
        session_id: session_id.to_string(),
        agent_type: AgentType::None,
        event_type,
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

/// A raw hook-shaped `AgentEvent` for driving `AppState::apply_event`
/// directly (mirrors `tests/card_supersession.rs`'s own `event` helper) —
/// used by the respawn/teardown tests below, which target `AppState`'s
/// pane-card resolution directly rather than going through the real CLI
/// subprocess.
#[cfg(unix)]
fn card_event(
    pane_id: &str,
    session_id: &str,
    agent_id: &str,
    event_type: EventType,
    tool_name: Option<&str>,
    timestamp: chrono::DateTime<chrono::Utc>,
) -> AgentEvent {
    AgentEvent {
        session_id: session_id.to_string(),
        agent_type: AgentType::ClaudeCode,
        event_type,
        tool_name: tool_name.map(str::to_string),
        tool_detail: None,
        cwd: None,
        timestamp,
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

// ---------------------------------------------------------------------------
// Round 2 (PRD #499, PR #617 round-1 reviewer/auditor findings) — closing the
// gap that let a happy-path-only test suite miss BLOCKERs 1-3 and HIGH 5.
// ---------------------------------------------------------------------------

const PANE_010: &str = "wait-monitored-pane-010-repeated-start-6e1f9a";
const LABEL_010: &str = "wait-monitored-label-010-ttl-refresh";

/// Scenario: run the real `wait start <label>` CLI twice in a row for the
/// same pane/label with no intervening `wait done` — the documented
/// TTL-refresh usage pattern (`src/main.rs`'s own doc comment: "re-running
/// `start` before a matching `done` just resets the TTL clock and
/// re-records the label"). Assert the pane is still `Working` after the
/// second call, then confirm a subsequent `wait done <label> --outcome
/// success` still reverts it to `Idle`. Reviewer BLOCKER 3: today the
/// second `start_monitored_wait` call recomputes `promoted` from the
/// CURRENT status (already `Working`), so it overwrites the stored entry
/// with `promoted: false`, and `wait done` then skips its revert branch
/// entirely — wedging the pane `Working` with no monitored wait left to
/// expire it.
#[spec("wait/monitored/010")]
#[test]
#[cfg(unix)]
fn wait_monitored_010_repeated_start_refresh_still_lets_done_clear() {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build wait/monitored/010 runtime")
        .block_on(wait_monitored_010_repeated_start_refresh_still_lets_done_clear_inner());
}

#[cfg(unix)]
async fn wait_monitored_010_repeated_start_refresh_still_lets_done_clear_inner() {
    let daemon = common::spawn_inprocess_daemon().await;
    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd.path().to_string_lossy().into_owned();
    let pane = setup_idle_pane(&daemon, &cwd_str, PANE_010).await;

    let start1 = run_wait_cli(&daemon, &pane.pane_id, &["start", LABEL_010], &[]).await;
    assert!(
        start1.status.success(),
        "wait/monitored/010: first `wait start {LABEL_010}` must succeed; status={:?} \
         stdout={:?} stderr={:?}",
        start1.status,
        start1.stdout,
        start1.stderr
    );
    wait_for_status(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_secs(5),
    )
    .await
    .unwrap_or_else(|e| panic!("wait/monitored/010 (after first start): {e}"));

    let start2 = run_wait_cli(&daemon, &pane.pane_id, &["start", LABEL_010], &[]).await;
    assert!(
        start2.status.success(),
        "wait/monitored/010: second (refreshing) `wait start {LABEL_010}` must succeed; \
         status={:?} stdout={:?} stderr={:?}",
        start2.status,
        start2.stdout,
        start2.stderr
    );
    assert_status_holds(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_millis(300),
        "wait/monitored/010 (after second/refreshing start)",
    )
    .await;

    let done = run_wait_cli(
        &daemon,
        &pane.pane_id,
        &["done", LABEL_010, "--outcome", "success"],
        &[],
    )
    .await;
    assert!(
        done.status.success(),
        "wait/monitored/010: `wait done {LABEL_010} --outcome success` must succeed; \
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
    .unwrap_or_else(|e| {
        panic!(
            "wait/monitored/010 (BLOCKER 3): after a repeated `wait start` refresh, `wait done` \
             must still revert the pane to Idle — {e}. If this times out, the second `start` \
             overwrote the monitored-wait entry with `promoted: false` (recomputed from the \
             already-Working status) and `wait done` skipped its revert branch, wedging the \
             pane Working with no wait left to expire it (reviewer BLOCKER 3)."
        )
    });

    daemon.registry.shutdown_all();
}

const PANE_011: &str = "wait-monitored-pane-011-composition-or-8b3d64";
const LABEL_011: &str = "wait-monitored-label-011-composition-or";

/// Scenario: after `wait start <label>` promotes an idle pane to `Working`,
/// inject a synthetic `ShellBusy` event (the shape `run_shell_activity_monitor`
/// itself would send for a real foreground descendant) for the SAME pane,
/// then run `wait done <label> --outcome success`. Per the PRD's composition
/// commitment ("a pane is `Working` if EITHER signal is active"), the pane
/// must STAY `Working` — the shell descendant this `ShellBusy` represents is
/// still running and never got a paired `ShellIdle`. Reviewer BLOCKER 2
/// Direction A: today `wait done` reverts unconditionally whenever its OWN
/// `promoted` bookkeeping says it should, without consulting whether a live
/// shell signal is still asserting `Working` independently — clobbering it.
#[spec("wait/monitored/011")]
#[test]
#[cfg(unix)]
fn wait_monitored_011_composition_is_or_not_mutual_clobber() {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build wait/monitored/011 runtime")
        .block_on(wait_monitored_011_composition_is_or_not_mutual_clobber_inner());
}

#[cfg(unix)]
async fn wait_monitored_011_composition_is_or_not_mutual_clobber_inner() {
    let daemon = common::spawn_inprocess_daemon().await;
    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd.path().to_string_lossy().into_owned();
    let pane = setup_idle_pane(&daemon, &cwd_str, PANE_011).await;
    let session_id = format!("{PANE_011}-session");

    let start = run_wait_cli(&daemon, &pane.pane_id, &["start", LABEL_011], &[]).await;
    assert!(
        start.status.success(),
        "wait/monitored/011: `wait start {LABEL_011}` must succeed; status={:?} stdout={:?} \
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
    .unwrap_or_else(|e| panic!("wait/monitored/011 (after start): {e}"));

    // A real foreground descendant starts on the SAME pane, the way
    // `run_shell_activity_monitor` would report it — injected directly over
    // the hook socket so this is deterministic rather than racing a real
    // spawned process.
    common::write_hook_line(
        &daemon.hook_path,
        &serde_json::to_string(&shell_activity_event(
            &pane.pane_id,
            &pane.agent_id,
            &session_id,
            EventType::ShellBusy,
        ))
        .expect("serialize synthetic ShellBusy event"),
    )
    .expect("write synthetic ShellBusy event to the daemon hook socket");
    assert_status_holds(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_millis(200),
        "wait/monitored/011 (after injected ShellBusy)",
    )
    .await;

    let done = run_wait_cli(
        &daemon,
        &pane.pane_id,
        &["done", LABEL_011, "--outcome", "success"],
        &[],
    )
    .await;
    assert!(
        done.status.success(),
        "wait/monitored/011: `wait done {LABEL_011} --outcome success` must succeed; \
         status={:?} stdout={:?} stderr={:?}",
        done.status,
        done.stdout,
        done.stderr
    );

    // The descendant this ShellBusy represents never went idle, so
    // composition (OR, not clobber) requires the pane to STAY Working.
    assert_status_holds(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_millis(500),
        "wait/monitored/011 (BLOCKER 2 Direction A): `wait done` cleared the monitored wait but \
         the pane's own ShellBusy signal is still active and never got a paired ShellIdle — OR \
         composition requires the pane to stay Working, not revert to Idle just because this \
         one signal cleared",
    )
    .await;

    daemon.registry.shutdown_all();
}

const RESPAWN_PANE_ID: &str = "wait-monitored-pane-012-respawn-4c7e91";
const RESPAWN_LABEL: &str = "wait-monitored-label-012-respawn";

/// Scenario: pin reviewer HIGH 5 directly against `AppState` (mirrors
/// `tests/card_supersession.rs`'s bare-state style, since this targets the
/// exact `pane_session_id` re-resolution `src/state.rs`'s `clear_monitored_wait`
/// does). A pane's card A is `Idle`; `start_monitored_wait` promotes it to
/// `Working`. The pane then respawns: card A is retired and a new card B is
/// created on the same pane (a fresh `SessionStart` under a different
/// session/agent id), and card B genuinely starts working via a real
/// `ToolStart` — a real agent is now running, unrelated to the old wait.
/// `clear_monitored_wait` must not apply card A's stale `promoted`
/// provenance to card B: today it re-resolves the pane's CURRENT card via
/// `pane_session_id` and reverts whatever it finds reading `Working`, which
/// clobbers card B's genuine work.
#[spec("wait/monitored/012")]
#[test]
#[cfg(unix)]
fn wait_monitored_012_promoted_provenance_does_not_follow_pane_to_new_card() {
    let t0 = chrono::Utc::now();
    let t1 = t0 + chrono::Duration::seconds(1);
    let t2 = t1 + chrono::Duration::seconds(1);

    let mut state = AppState::default();
    state.register_pane(RESPAWN_PANE_ID.to_string());

    // Card A: an idle agent.
    state.apply_event(card_event(
        RESPAWN_PANE_ID,
        "card-a-session",
        "agent-a",
        EventType::SessionStart,
        None,
        t0,
    ));
    assert_eq!(
        state.sessions["card-a-session"].status,
        SessionStatus::Idle,
        "precondition: card A starts Idle"
    );

    // A monitored wait promotes card A to Working.
    state.start_monitored_wait(
        RESPAWN_PANE_ID,
        RESPAWN_LABEL.to_string(),
        Duration::from_secs(300),
    );
    assert_eq!(
        state.sessions["card-a-session"].status,
        SessionStatus::Working,
        "precondition: `wait start` promotes card A to Working"
    );

    // The pane respawns: card A is retired, card B is created (a different
    // session/agent id claiming the same pane via SessionStart).
    state.apply_event(card_event(
        RESPAWN_PANE_ID,
        "card-b-session",
        "agent-b",
        EventType::SessionStart,
        None,
        t1,
    ));
    assert!(
        !state.sessions.contains_key("card-a-session"),
        "precondition: the respawn's SessionStart must retire card A"
    );

    // Card B genuinely starts working — a real agent, unrelated to the old
    // wait.
    state.apply_event(card_event(
        RESPAWN_PANE_ID,
        "card-b-session",
        "agent-b",
        EventType::ToolStart,
        Some("Bash"),
        t2,
    ));
    assert_eq!(
        state.sessions["card-b-session"].status,
        SessionStatus::Working,
        "precondition: card B is genuinely Working via a real ToolStart"
    );

    state.clear_monitored_wait(RESPAWN_PANE_ID, RESPAWN_LABEL, WaitOutcome::Success);

    assert_eq!(
        state.sessions["card-b-session"].status,
        SessionStatus::Working,
        "wait/monitored/012 (HIGH 5): clearing card A's wait must not revert card B's genuine, \
         real-agent Working just because `pane_session_id` re-resolved to whatever card is \
         current on the pane now — a monitored wait's promoted provenance must die with the \
         card it was recorded against, not follow the pane onto its successor"
    );
}

const TEARDOWN_PANE_ID: &str = "wait-monitored-pane-013-teardown-2a9f6c";
const TEARDOWN_LABEL: &str = "wait-monitored-label-013-teardown";

/// Scenario: pin reviewer MEDIUM 6 / auditor A4 directly against
/// `AppState`. A pane with an active monitored wait is torn down
/// (`remove_sessions_for_pane` + `unregister_pane`, the same pair
/// `src/ui.rs` calls together on a real pane close) BEFORE the wait is
/// cleared or expires. The `monitored_waits` entry must not survive the
/// teardown: today neither method touches it, so it persists in memory
/// (bounded only by the TTL sweep) rather than being cleaned up eagerly at
/// the point the pane genuinely stops existing, as both findings recommend.
#[spec("wait/monitored/013")]
#[test]
#[cfg(unix)]
fn wait_monitored_013_teardown_clears_the_panes_monitored_wait() {
    let t0 = chrono::Utc::now();

    let mut state = AppState::default();
    state.register_pane(TEARDOWN_PANE_ID.to_string());
    state.apply_event(card_event(
        TEARDOWN_PANE_ID,
        "teardown-session",
        "teardown-agent",
        EventType::SessionStart,
        None,
        t0,
    ));
    state.start_monitored_wait(
        TEARDOWN_PANE_ID,
        TEARDOWN_LABEL.to_string(),
        Duration::from_secs(300),
    );
    assert!(
        state.monitored_waits.contains_key(TEARDOWN_PANE_ID),
        "precondition: `wait start` records a monitored wait for the pane"
    );

    // Pane teardown, the same pair `src/ui.rs` calls together on a real
    // pane close.
    state.remove_sessions_for_pane(TEARDOWN_PANE_ID);
    state.unregister_pane(TEARDOWN_PANE_ID);

    assert!(
        !state.monitored_waits.contains_key(TEARDOWN_PANE_ID),
        "wait/monitored/013 (MEDIUM 6 / A4): a torn-down pane's monitored wait must not survive \
         the teardown — neither `remove_sessions_for_pane` nor `unregister_pane` currently drops \
         the `monitored_waits` entry, so it lingers in memory (bounded only by the TTL sweep) \
         rather than being cleaned up at the point the pane genuinely stops existing"
    );
}

const PANE_014: &str = "wait-monitored-pane-014-broadcast-9f4a27";
const LABEL_014: &str = "wait-monitored-label-014-broadcast";

/// Scenario: subscribe to the daemon's own broadcast stream
/// (`daemon.event_tx`, the SAME channel `src/reconnect.rs:463` reads to
/// rebuild an attached/reconnecting TUI's own `AppState`) before running
/// `wait start <label>`. Assert a `BroadcastMsg::Event` naming this pane
/// actually arrives, then apply that exact event to a fresh, independent
/// `AppState` the way a real reconnecting client would — proving the
/// promotion reaches an attached client, not just the daemon's own internal
/// state. Reviewer BLOCKER 1: today `start_monitored_wait` mutates only the
/// daemon's own `AppState` directly and sends nothing on `event_tx`, so a
/// real attached TUI's dashboard never learns the pane went `Working` at
/// all — this is CLAUDE.md rule 4's bar ("at least one test... AS A USER
/// ACTUALLY USES AND SEES IT") landing on precisely the gap round 1's nine
/// tests (all reading `AppState.sessions` directly on the daemon side)
/// could not catch.
#[spec("wait/monitored/014")]
#[test]
#[cfg(unix)]
fn wait_monitored_014_promotion_reaches_an_attached_client_via_broadcast() {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build wait/monitored/014 runtime")
        .block_on(wait_monitored_014_promotion_reaches_an_attached_client_via_broadcast_inner());
}

#[cfg(unix)]
async fn wait_monitored_014_promotion_reaches_an_attached_client_via_broadcast_inner() {
    let daemon = common::spawn_inprocess_daemon().await;
    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd.path().to_string_lossy().into_owned();
    let pane = setup_idle_pane(&daemon, &cwd_str, PANE_014).await;

    let mut events = daemon.event_tx.subscribe();

    let start = run_wait_cli(&daemon, &pane.pane_id, &["start", LABEL_014], &[]).await;
    assert!(
        start.status.success(),
        "wait/monitored/014: `wait start {LABEL_014}` must succeed; status={:?} stdout={:?} \
         stderr={:?}",
        start.status,
        start.stdout,
        start.stderr
    );
    // Precondition, matching round 1's own daemon-side read: the daemon's
    // OWN AppState does reach Working.
    wait_for_status(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_secs(5),
    )
    .await
    .unwrap_or_else(|e| panic!("wait/monitored/014 (daemon-side precondition): {e}"));

    let pane_id = pane.pane_id.clone();
    let observed = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match events.recv().await {
                Ok(BroadcastMsg::Event(event)) if event.pane_id.as_deref() == Some(&pane_id) => {
                    break Some(event);
                }
                Ok(_) => continue,
                Err(_) => break None,
            }
        }
    })
    .await
    .ok()
    .flatten();

    let event = observed.unwrap_or_else(|| {
        panic!(
            "wait/monitored/014 (BLOCKER 1): `wait start` must broadcast a `BroadcastMsg::Event` \
             naming pane {pane_id:?} on `event_tx` so an attached TUI's own AppState can learn \
             the promotion via `apply_event` (`src/reconnect.rs:463`) — the same path every \
             other status-producing signal goes through via `ingest_event`. No such event \
             arrived within 3s, meaning `start_monitored_wait` mutated only the daemon's own \
             AppState directly with nothing sent on the broadcast channel; a real attached \
             dashboard would keep showing this pane as Idle."
        )
    });

    // Apply the observed event exactly the way a real reconnecting/attached
    // TUI's own independent AppState would.
    let mut client_state = AppState::default();
    client_state.register_pane(pane_id.clone());
    client_state.apply_event(event);
    let client_status = client_state
        .sessions
        .values()
        .find(|s| s.pane_id.as_deref() == Some(pane_id.as_str()))
        .map(|s| s.status.clone());
    assert_eq!(
        client_status,
        Some(SessionStatus::Working),
        "wait/monitored/014 (BLOCKER 1): the broadcast event a client would apply must itself \
         carry the Working promotion; got {client_status:?}"
    );

    daemon.registry.shutdown_all();
}

const PANE_015: &str = "wait-monitored-pane-015-label-mismatch-5d2c83";
const LABEL_015_ACTIVE: &str = "wait-monitored-label-015-active-check";
const LABEL_015_WRONG: &str = "wait-monitored-label-015-different-check";

/// Scenario: declare a monitored wait under one label, then run `wait done`
/// naming a DIFFERENT label for the same pane. Reviewer confirmed (question
/// 4) the coder's "clear anyway with a warning" design is the right
/// trade-off, since a pane carries at most one wait — but no test currently
/// pins it (MEDIUM 9). Assert the mismatched `wait done` still exits
/// successfully and still clears the pane's one active wait back to
/// `Idle`, rather than refusing or silently no-op'ing and leaving the pane
/// wedged `Working`.
#[spec("wait/monitored/015")]
#[test]
#[cfg(unix)]
fn wait_monitored_015_label_mismatch_still_clears_the_active_wait() {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build wait/monitored/015 runtime")
        .block_on(wait_monitored_015_label_mismatch_still_clears_the_active_wait_inner());
}

#[cfg(unix)]
async fn wait_monitored_015_label_mismatch_still_clears_the_active_wait_inner() {
    let daemon = common::spawn_inprocess_daemon().await;
    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd.path().to_string_lossy().into_owned();
    let pane = setup_idle_pane(&daemon, &cwd_str, PANE_015).await;

    let start = run_wait_cli(&daemon, &pane.pane_id, &["start", LABEL_015_ACTIVE], &[]).await;
    assert!(
        start.status.success(),
        "wait/monitored/015: `wait start {LABEL_015_ACTIVE}` must succeed; status={:?} \
         stdout={:?} stderr={:?}",
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
    .unwrap_or_else(|e| panic!("wait/monitored/015 (after start): {e}"));

    let done = run_wait_cli(
        &daemon,
        &pane.pane_id,
        &["done", LABEL_015_WRONG, "--outcome", "success"],
        &[],
    )
    .await;
    assert!(
        done.status.success(),
        "wait/monitored/015 (MEDIUM 9): `wait done` naming a DIFFERENT label than the pane's \
         active wait must still exit successfully (clear-anyway-with-warning, not refuse) — \
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
    .unwrap_or_else(|e| {
        panic!(
            "wait/monitored/015 (MEDIUM 9): a mismatched-label `wait done` must still clear the \
             pane's one active monitored wait back to Idle rather than leaving it wedged \
             Working — {e}"
        )
    });

    daemon.registry.shutdown_all();
}
