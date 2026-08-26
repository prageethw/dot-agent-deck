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

/// Shared `AgentEvent` construction for every synthetic-event helper below —
/// all four differ only in `agent_type`, `tool_name` and `timestamp`, with
/// every other field fixed at the same "no tool/session metadata" baseline.
/// Factored out (PRD #499 round 7, SonarCloud duplication) so each helper is
/// a one-line call rather than repeating this 14-field struct literal.
#[cfg(unix)]
fn base_agent_event(
    pane_id: &str,
    agent_id: &str,
    session_id: &str,
    event_type: EventType,
    agent_type: AgentType,
    tool_name: Option<&str>,
    timestamp: chrono::DateTime<chrono::Utc>,
) -> AgentEvent {
    AgentEvent {
        session_id: session_id.to_string(),
        agent_type,
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

/// A synthetic `Idle` `AgentEvent` naming `pane_id`/`agent_id` — the raw
/// shape a real agent's Stop hook would send, used here purely to seed a
/// known `SessionState` at a deterministic `Idle` baseline (`apply_event`'s
/// `EventType::Idle` arm sets `SessionStatus::Idle` unconditionally,
/// `src/state.rs`).
#[cfg(unix)]
fn idle_event(pane_id: &str, agent_id: &str, session_id: &str) -> AgentEvent {
    base_agent_event(
        pane_id,
        agent_id,
        session_id,
        EventType::Idle,
        AgentType::Pi,
        None,
        chrono::Utc::now(),
    )
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
    base_agent_event(
        pane_id,
        agent_id,
        session_id,
        event_type,
        AgentType::None,
        None,
        chrono::Utc::now(),
    )
}

/// A synthetic `ToolStart`/`ToolEnd` `AgentEvent`, shaped the way a real
/// agent hook would send one over the hook socket — used by the BLOCKER H
/// wedge tests (`019`-`021`) to drive a REAL, agent-emitted assertion of
/// `Working` ahead of the wait/shell composition under test, as opposed to
/// `MonitoredWaitStart`'s own synthetic promotion.
#[cfg(unix)]
fn tool_event(
    pane_id: &str,
    agent_id: &str,
    session_id: &str,
    event_type: EventType,
    tool_name: Option<&str>,
) -> AgentEvent {
    base_agent_event(
        pane_id,
        agent_id,
        session_id,
        event_type,
        AgentType::ClaudeCode,
        tool_name,
        chrono::Utc::now(),
    )
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
    base_agent_event(
        pane_id,
        agent_id,
        session_id,
        event_type,
        AgentType::ClaudeCode,
        tool_name,
        timestamp,
    )
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

/// Assert a `run_wait_cli` result exited successfully, panicking with
/// `message` plus the process's status/stdout/stderr — the exact `assert!`
/// shape this file repeats at nearly every `run_wait_cli` call site,
/// factored out here (PRD #499 round 7, SonarCloud duplication) so each call
/// site states only what is unique to it: the message.
#[cfg(unix)]
fn assert_wait_cli_succeeded(result: &CliResult, message: &str) {
    assert!(
        result.status.success(),
        "{message}; status={:?} stdout={:?} stderr={:?}",
        result.status,
        result.stdout,
        result.stderr
    );
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

/// `wait_for_status`, panicking with `{message}: {error}` on timeout — the
/// `wait_for_status(...).await.unwrap_or_else(|e| panic!("{message}: {e}"))`
/// shape repeated at most call sites in this file, factored out here (PRD
/// #499 round 7, SonarCloud duplication) so each call site states only its
/// own message. A handful of call sites build a longer, finding-specific
/// explanation that doesn't fit this `{message}: {error}` shape (the error
/// isn't simply appended at the end) and are left calling `wait_for_status`
/// directly rather than being forced through this helper.
#[cfg(unix)]
async fn assert_reaches_status(
    daemon: &common::InProcDaemon,
    pane_id: &str,
    expected: &SessionStatus,
    timeout: Duration,
    message: &str,
) {
    wait_for_status(daemon, pane_id, expected, timeout)
        .await
        .unwrap_or_else(|e| panic!("{message}: {e}"));
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

/// Build a fresh multi-threaded Tokio runtime and block on `fut` — the
/// `tokio::runtime::Builder::new_multi_thread()...block_on(...)` shape every
/// `#[test]` wrapper in this file repeats verbatim around its own `_inner`
/// future, factored out here (PRD #499 round 7, SonarCloud duplication) so
/// each `#[test]` fn is a one-line call naming its own spec id and future.
#[cfg(unix)]
fn run_async_test<F: std::future::Future<Output = ()>>(spec_id: &str, fut: F) {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap_or_else(|e| panic!("build {spec_id} runtime: {e}"))
        .block_on(fut);
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
    run_async_test(
        "wait/monitored/001",
        wait_monitored_001_start_sets_working_done_clears_to_idle_inner(),
    );
}

#[cfg(unix)]
async fn wait_monitored_001_start_sets_working_done_clears_to_idle_inner() {
    let daemon = common::spawn_inprocess_daemon().await;
    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd.path().to_string_lossy().into_owned();
    let pane = setup_idle_pane(&daemon, &cwd_str, PANE_001).await;

    let start = run_wait_cli(&daemon, &pane.pane_id, &["start", LABEL_001], &[]).await;
    assert_wait_cli_succeeded(
        &start,
        &format!(
            "wait/monitored/001: `wait start {LABEL_001}` must succeed in an otherwise-idle pane"
        ),
    );
    assert_reaches_status(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_secs(5),
        "wait/monitored/001 (after start)",
    )
    .await;

    let done = run_wait_cli(
        &daemon,
        &pane.pane_id,
        &["done", LABEL_001, "--outcome", "success"],
        &[],
    )
    .await;
    assert_wait_cli_succeeded(
        &done,
        &format!("wait/monitored/001: `wait done {LABEL_001} --outcome success` must succeed"),
    );
    assert_reaches_status(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Idle,
        Duration::from_secs(5),
        "wait/monitored/001 (after done)",
    )
    .await;

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
    run_async_test(
        "wait/monitored/002",
        wait_monitored_002_persists_across_polling_gaps_with_no_descendant_inner(),
    );
}

#[cfg(unix)]
async fn wait_monitored_002_persists_across_polling_gaps_with_no_descendant_inner() {
    let daemon = common::spawn_inprocess_daemon().await;
    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd.path().to_string_lossy().into_owned();
    let pane = setup_idle_pane(&daemon, &cwd_str, PANE_002).await;

    let start = run_wait_cli(&daemon, &pane.pane_id, &["start", LABEL_002], &[]).await;
    assert_wait_cli_succeeded(
        &start,
        &format!("wait/monitored/002: `wait start {LABEL_002}` must succeed"),
    );
    assert_reaches_status(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_secs(5),
        "wait/monitored/002 (after start)",
    )
    .await;

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
    assert_wait_cli_succeeded(
        &done,
        &format!("wait/monitored/002: `wait done {LABEL_002} --outcome success` must succeed"),
    );
    assert_reaches_status(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Idle,
        Duration::from_secs(5),
        "wait/monitored/002 (after done)",
    )
    .await;

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
    run_async_test(
        "wait/monitored/003",
        wait_monitored_003_attribution_is_per_pane_not_global_inner(),
    );
}

#[cfg(unix)]
async fn wait_monitored_003_attribution_is_per_pane_not_global_inner() {
    let daemon = common::spawn_inprocess_daemon().await;
    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd.path().to_string_lossy().into_owned();
    let pane_a = setup_idle_pane(&daemon, &cwd_str, PANE_003_A).await;
    let pane_b = setup_idle_pane(&daemon, &cwd_str, PANE_003_B).await;

    let start = run_wait_cli(&daemon, &pane_a.pane_id, &["start", LABEL_003], &[]).await;
    assert_wait_cli_succeeded(
        &start,
        &format!("wait/monitored/003: `wait start {LABEL_003}` on pane A must succeed"),
    );
    assert_reaches_status(
        &daemon,
        &pane_a.pane_id,
        &SessionStatus::Working,
        Duration::from_secs(5),
        "wait/monitored/003 (pane A after start)",
    )
    .await;

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
    assert_wait_cli_succeeded(
        &done,
        &format!(
            "wait/monitored/003: `wait done {LABEL_003} --outcome success` on pane A must succeed"
        ),
    );
    assert_reaches_status(
        &daemon,
        &pane_a.pane_id,
        &SessionStatus::Idle,
        Duration::from_secs(5),
        "wait/monitored/003 (pane A after done)",
    )
    .await;

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
    run_async_test(
        "wait/monitored/004",
        wait_monitored_004_untouched_pane_stays_idle_inner(),
    );
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
    assert_wait_cli_succeeded(
        &start,
        &format!("{spec_id}: `wait start {label}` must succeed"),
    );
    assert_reaches_status(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_secs(5),
        &format!("{spec_id} (after start)"),
    )
    .await;

    let done = run_wait_cli(
        &daemon,
        &pane.pane_id,
        &["done", label, "--outcome", outcome],
        &[],
    )
    .await;
    assert_wait_cli_succeeded(
        &done,
        &format!("{spec_id}: `wait done {label} --outcome {outcome}` must succeed"),
    );
    assert_reaches_status(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Idle,
        Duration::from_secs(5),
        &format!("{spec_id} (after done --outcome {outcome})"),
    )
    .await;

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
    run_async_test(
        "wait/monitored/005",
        wait_monitored_outcome_clears_working_inner(
            PANE_005,
            LABEL_005,
            "success",
            "wait/monitored/005",
        ),
    );
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
    run_async_test(
        "wait/monitored/006",
        wait_monitored_outcome_clears_working_inner(
            PANE_006,
            LABEL_006,
            "failure",
            "wait/monitored/006",
        ),
    );
}

const PANE_007: &str = "wait-monitored-pane-007-outcome-cancelled";
const LABEL_007: &str = "wait-monitored-label-007-outcome-cancelled";

/// Scenario: `wait start` then `wait done --outcome cancelled` clears
/// `Working` back to `Idle`.
#[spec("wait/monitored/007")]
#[test]
#[cfg(unix)]
fn wait_monitored_007_outcome_cancelled_clears_working() {
    run_async_test(
        "wait/monitored/007",
        wait_monitored_outcome_clears_working_inner(
            PANE_007,
            LABEL_007,
            "cancelled",
            "wait/monitored/007",
        ),
    );
}

const PANE_008: &str = "wait-monitored-pane-008-outcome-timeout";
const LABEL_008: &str = "wait-monitored-label-008-outcome-timeout";

/// Scenario: `wait start` then `wait done --outcome timeout` clears
/// `Working` back to `Idle`.
#[spec("wait/monitored/008")]
#[test]
#[cfg(unix)]
fn wait_monitored_008_outcome_timeout_clears_working() {
    run_async_test(
        "wait/monitored/008",
        wait_monitored_outcome_clears_working_inner(
            PANE_008,
            LABEL_008,
            "timeout",
            "wait/monitored/008",
        ),
    );
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
    run_async_test(
        "wait/monitored/009",
        wait_monitored_009_ttl_self_heals_without_explicit_done_inner(),
    );
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
    assert_wait_cli_succeeded(
        &start,
        &format!(
            "wait/monitored/009: `wait start {LABEL_009}` with a 2s TTL override must succeed"
        ),
    );
    assert_reaches_status(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_secs(5),
        "wait/monitored/009 (after start)",
    )
    .await;

    // Deliberately no `wait done` call. Sleep past the 2s TTL plus enough
    // margin for the 500ms shell-activity poll cadence and a sweep tick to
    // notice, then confirm the self-heal happened with nobody clearing it
    // explicitly.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_reaches_status(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Idle,
        Duration::from_secs(5),
        "wait/monitored/009 (TTL self-heal)",
    )
    .await;

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
    run_async_test(
        "wait/monitored/010",
        wait_monitored_010_repeated_start_refresh_still_lets_done_clear_inner(),
    );
}

#[cfg(unix)]
async fn wait_monitored_010_repeated_start_refresh_still_lets_done_clear_inner() {
    let daemon = common::spawn_inprocess_daemon().await;
    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd.path().to_string_lossy().into_owned();
    let pane = setup_idle_pane(&daemon, &cwd_str, PANE_010).await;

    let start1 = run_wait_cli(&daemon, &pane.pane_id, &["start", LABEL_010], &[]).await;
    assert_wait_cli_succeeded(
        &start1,
        &format!("wait/monitored/010: first `wait start {LABEL_010}` must succeed"),
    );
    assert_reaches_status(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_secs(5),
        "wait/monitored/010 (after first start)",
    )
    .await;

    let start2 = run_wait_cli(&daemon, &pane.pane_id, &["start", LABEL_010], &[]).await;
    assert_wait_cli_succeeded(
        &start2,
        &format!("wait/monitored/010: second (refreshing) `wait start {LABEL_010}` must succeed"),
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
    assert_wait_cli_succeeded(
        &done,
        &format!("wait/monitored/010: `wait done {LABEL_010} --outcome success` must succeed"),
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
    run_async_test(
        "wait/monitored/011",
        wait_monitored_011_composition_is_or_not_mutual_clobber_inner(),
    );
}

#[cfg(unix)]
async fn wait_monitored_011_composition_is_or_not_mutual_clobber_inner() {
    let daemon = common::spawn_inprocess_daemon().await;
    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd.path().to_string_lossy().into_owned();
    let pane = setup_idle_pane(&daemon, &cwd_str, PANE_011).await;
    let session_id = format!("{PANE_011}-session");

    let start = run_wait_cli(&daemon, &pane.pane_id, &["start", LABEL_011], &[]).await;
    assert_wait_cli_succeeded(
        &start,
        &format!("wait/monitored/011: `wait start {LABEL_011}` must succeed"),
    );
    assert_reaches_status(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_secs(5),
        "wait/monitored/011 (after start)",
    )
    .await;

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
    assert_wait_cli_succeeded(
        &done,
        &format!("wait/monitored/011: `wait done {LABEL_011} --outcome success` must succeed"),
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
/// teardown: `remove_sessions_for_pane` provably drops it — it takes every
/// card the pane could have carried down with it (round 3, auditor B3) —
/// while `unregister_pane` ALONE deliberately does not, since that method's
/// card can survive it (see `wait/monitored/022`, the sibling regression
/// guard auditor C2 asked for).
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
         the teardown — `remove_sessions_for_pane` provably takes every card the pane could \
         have carried down with it, including the one this wait was recorded against, so the \
         `monitored_waits` entry must not remain once both teardown methods have run"
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
    run_async_test(
        "wait/monitored/014",
        wait_monitored_014_promotion_reaches_an_attached_client_via_broadcast_inner(),
    );
}

#[cfg(unix)]
async fn wait_monitored_014_promotion_reaches_an_attached_client_via_broadcast_inner() {
    let daemon = common::spawn_inprocess_daemon().await;
    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd.path().to_string_lossy().into_owned();
    let pane = setup_idle_pane(&daemon, &cwd_str, PANE_014).await;

    let mut events = daemon.event_tx.subscribe();

    let start = run_wait_cli(&daemon, &pane.pane_id, &["start", LABEL_014], &[]).await;
    assert_wait_cli_succeeded(
        &start,
        &format!("wait/monitored/014: `wait start {LABEL_014}` must succeed"),
    );
    // Precondition, matching round 1's own daemon-side read: the daemon's
    // OWN AppState does reach Working.
    assert_reaches_status(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_secs(5),
        "wait/monitored/014 (daemon-side precondition)",
    )
    .await;

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
    run_async_test(
        "wait/monitored/015",
        wait_monitored_015_label_mismatch_still_clears_the_active_wait_inner(),
    );
}

#[cfg(unix)]
async fn wait_monitored_015_label_mismatch_still_clears_the_active_wait_inner() {
    let daemon = common::spawn_inprocess_daemon().await;
    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd.path().to_string_lossy().into_owned();
    let pane = setup_idle_pane(&daemon, &cwd_str, PANE_015).await;

    let start = run_wait_cli(&daemon, &pane.pane_id, &["start", LABEL_015_ACTIVE], &[]).await;
    assert_wait_cli_succeeded(
        &start,
        &format!("wait/monitored/015: `wait start {LABEL_015_ACTIVE}` must succeed"),
    );
    assert_reaches_status(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_secs(5),
        "wait/monitored/015 (after start)",
    )
    .await;

    let done = run_wait_cli(
        &daemon,
        &pane.pane_id,
        &["done", LABEL_015_WRONG, "--outcome", "success"],
        &[],
    )
    .await;
    assert_wait_cli_succeeded(
        &done,
        "wait/monitored/015 (MEDIUM 9): `wait done` naming a DIFFERENT label than the pane's \
         active wait must still exit successfully (clear-anyway-with-warning, not refuse)",
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

// ---------------------------------------------------------------------------
// Round 3 (PRD #499, PR #617 round-3 reviewer/auditor findings) — closing the
// test-coverage gap that let BLOCKER A / auditor B1 (daemon-only composition
// state, invisible to an attached client) ship undetected by any of the
// nine round-1/round-2 tests above, all of which read `AppState` directly on
// the daemon side. Round 3 moved the composition state off the daemon-only
// `AppState::monitored_waits` map onto three new `SessionState`/
// `SessionSnapshot` fields (`monitored_wait_active`, `wait_synthetic_working`,
// `shell_descendant_busy`) set/cleared inside `apply_event` itself, so every
// consumer of the broadcast event stream converges identically. These three
// tests pin that convergence directly, plus the two untested directions
// (respawn-suppression, same-card real-activity protection) coder's own
// report named as still needing coverage.
//
// Round 5 added a fourth replicated composition field,
// `wait_deferred_revert`. Round 6 (MEDIUM J) joins it to the convergence
// tuple below, which round 5 left out — a future daemon-only write to that
// field would otherwise pass this check silently.
// ---------------------------------------------------------------------------

/// The `(status, monitored_wait_active, wait_synthetic_working,
/// shell_descendant_busy, wait_deferred_revert)` tuple for `pane_id` inside
/// `state` — `None` if no session names this pane at all. Shared by
/// [`daemon_composition_state`] and [`client_composition_state`] (PRD #499
/// round 7, SonarCloud duplication), which differ only in where `state`
/// comes from — the daemon's own live `AppState` behind a lock, versus an
/// independent client `AppState` built by replaying broadcast events.
///
/// MEDIUM J (PRD #499 round 6): `wait_deferred_revert` was added in round 5
/// but never joined this tuple, so a future daemon-only write to that field
/// — precisely the defect this convergence check exists to catch — would
/// pass silently. Extended here to close that gap.
#[cfg(unix)]
fn composition_state_tuple(
    state: &AppState,
    pane_id: &str,
) -> Option<(SessionStatus, bool, bool, bool, bool)> {
    state
        .sessions
        .values()
        .find(|s| s.pane_id.as_deref() == Some(pane_id))
        .map(|s| {
            (
                s.status.clone(),
                s.monitored_wait_active,
                s.wait_synthetic_working,
                s.shell_descendant_busy,
                s.wait_deferred_revert,
            )
        })
}

/// [`composition_state_tuple`] read directly off the DAEMON's own live
/// `AppState`.
#[cfg(unix)]
async fn daemon_composition_state(
    daemon: &common::InProcDaemon,
    pane_id: &str,
) -> Option<(SessionStatus, bool, bool, bool, bool)> {
    let state = daemon.state.read().await;
    composition_state_tuple(&state, pane_id)
}

/// [`composition_state_tuple`] read off an independent, freshly-constructed
/// CLIENT `AppState` that has only ever seen events replayed onto it via
/// `apply_event` — never read from the daemon's own state directly. This is
/// the read that would have caught round 2's BLOCKER A/B1: a daemon-only
/// `AppState::monitored_waits` map gave the two answers no reason to agree.
#[cfg(unix)]
fn client_composition_state(
    client_state: &AppState,
    pane_id: &str,
) -> Option<(SessionStatus, bool, bool, bool, bool)> {
    composition_state_tuple(client_state, pane_id)
}

/// Await the next broadcast event naming `pane_id` on `events` — the same
/// filter `wait/monitored/014` applies inline, factored out here so the two
/// scenarios in `wait/monitored/016` don't repeat it.
#[cfg(unix)]
async fn recv_matching_event(
    events: &mut tokio::sync::broadcast::Receiver<BroadcastMsg>,
    pane_id: &str,
    timeout: Duration,
) -> Option<AgentEvent> {
    tokio::time::timeout(timeout, async {
        loop {
            match events.recv().await {
                Ok(BroadcastMsg::Event(event)) if event.pane_id.as_deref() == Some(pane_id) => {
                    break Some(event);
                }
                Ok(_) => continue,
                Err(_) => break None,
            }
        }
    })
    .await
    .ok()
    .flatten()
}

const PANE_016_A: &str = "wait-monitored-pane-016a-replay-idle-7d2f83";
const LABEL_016_A: &str = "wait-monitored-label-016a-replay-idle";
const PANE_016_B: &str = "wait-monitored-pane-016b-replay-shell-9e4c17";
const LABEL_016_B: &str = "wait-monitored-label-016b-replay-shell";

/// Scenario: subscribe to the daemon's broadcast stream and replay every
/// captured event onto an independent, freshly-constructed client `AppState`
/// (the same mechanic `wait/monitored/014` uses for the bare promotion
/// event, extended here past just the promotion) across two full
/// compositions on two independent panes: (A) `wait start` followed by a
/// real `Idle` while the wait is still outstanding — the PRD's own
/// motivating scenario, an agent ending its turn with an external dependency
/// still unresolved — and (B) `wait start`, an injected `ShellBusy`, then
/// `wait done` while the shell signal is still live (OR composition,
/// Direction A). After every event, assert the client's
/// independently-replayed `(status, monitored_wait_active,
/// wait_synthetic_working, shell_descendant_busy, wait_deferred_revert)`
/// tuple is identical to the daemon's own — this is the test that would have
/// caught round 2's BLOCKER A/B1: a daemon-only `AppState::monitored_waits`
/// map meant an attached client never learned a wait was live at all, so its
/// own `apply_event` had nothing to suppress the real `Idle` with and it
/// read `Idle` while the daemon read `Working`.
#[spec("wait/monitored/016")]
#[test]
#[cfg(unix)]
fn wait_monitored_016_daemon_and_client_converge_across_replayed_events() {
    run_async_test(
        "wait/monitored/016",
        wait_monitored_016_daemon_and_client_converge_across_replayed_events_inner(),
    );
}

#[cfg(unix)]
async fn wait_monitored_016_daemon_and_client_converge_across_replayed_events_inner() {
    let daemon = common::spawn_inprocess_daemon().await;
    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd.path().to_string_lossy().into_owned();

    // --- Case A: MonitoredWaitStart, then a real Idle while the wait holds. ---
    let pane_a = setup_idle_pane(&daemon, &cwd_str, PANE_016_A).await;
    let session_a = format!("{PANE_016_A}-session");
    let mut events_a = daemon.event_tx.subscribe();
    let mut client_a = AppState::default();
    client_a.register_pane(pane_a.pane_id.clone());

    let start_a = run_wait_cli(&daemon, &pane_a.pane_id, &["start", LABEL_016_A], &[]).await;
    assert_wait_cli_succeeded(
        &start_a,
        &format!("wait/monitored/016 (case A): `wait start {LABEL_016_A}` must succeed"),
    );
    let start_event = recv_matching_event(&mut events_a, &pane_a.pane_id, Duration::from_secs(3))
        .await
        .unwrap_or_else(|| {
            panic!(
                "wait/monitored/016 (case A, BLOCKER 1 regression): `wait start` must broadcast \
                 an event naming pane {:?} — none arrived within 3s",
                pane_a.pane_id
            )
        });
    client_a.apply_event(start_event);
    assert_reaches_status(
        &daemon,
        &pane_a.pane_id,
        &SessionStatus::Working,
        Duration::from_secs(5),
        "wait/monitored/016 (case A, after wait start)",
    )
    .await;
    assert_eq!(
        client_composition_state(&client_a, &pane_a.pane_id),
        daemon_composition_state(&daemon, &pane_a.pane_id).await,
        "wait/monitored/016 (case A, BLOCKER A/B1): after MonitoredWaitStart, the client's \
         independently-replayed composition state must already match the daemon's"
    );

    // A real Stop-hook Idle arrives while the wait is still outstanding.
    // Round 3 (Direction C): the daemon suppresses the status write because
    // `monitored_wait_active` is set on THIS card; a client that applied the
    // identical MonitoredWaitStart event above must suppress it identically.
    common::write_hook_line(
        &daemon.hook_path,
        &serde_json::to_string(&idle_event(&pane_a.pane_id, &pane_a.agent_id, &session_a))
            .expect("serialize real Idle event"),
    )
    .expect("write real Idle event to the daemon hook socket");
    let idle_event_seen =
        recv_matching_event(&mut events_a, &pane_a.pane_id, Duration::from_secs(3))
            .await
            .unwrap_or_else(|| {
                panic!(
                    "wait/monitored/016 (case A): the real Idle event itself must still \
                     broadcast (composition suppresses the STATUS write, not the broadcast) — \
                     none arrived within 3s"
                )
            });
    client_a.apply_event(idle_event_seen);
    // Daemon-side precondition, matching round 1's own read: the real Idle
    // must NOT have reverted the pane while the wait is outstanding.
    assert_status_holds(
        &daemon,
        &pane_a.pane_id,
        &SessionStatus::Working,
        Duration::from_millis(200),
        "wait/monitored/016 (case A, daemon precondition)",
    )
    .await;
    assert_eq!(
        client_composition_state(&client_a, &pane_a.pane_id).map(|(status, ..)| status),
        Some(SessionStatus::Working),
        "wait/monitored/016 (case A, BLOCKER A/B1 — the original bug): the client's own \
         independent AppState must read Working, NOT Idle, after applying the same real Idle \
         event the daemon applied — it has monitored_wait_active set from having applied the \
         identical MonitoredWaitStart event above, so Direction C suppresses the write exactly \
         as it does on the daemon"
    );
    assert_eq!(
        client_composition_state(&client_a, &pane_a.pane_id),
        daemon_composition_state(&daemon, &pane_a.pane_id).await,
        "wait/monitored/016 (case A, BLOCKER A/B1): the client and daemon composition state \
         must still agree after the real Idle"
    );

    let done_a = run_wait_cli(
        &daemon,
        &pane_a.pane_id,
        &["done", LABEL_016_A, "--outcome", "success"],
        &[],
    )
    .await;
    assert_wait_cli_succeeded(
        &done_a,
        &format!(
            "wait/monitored/016 (case A): `wait done {LABEL_016_A} --outcome success` must succeed"
        ),
    );
    let done_event_a = recv_matching_event(&mut events_a, &pane_a.pane_id, Duration::from_secs(3))
        .await
        .unwrap_or_else(|| {
            panic!("wait/monitored/016 (case A): `wait done` must also broadcast an event")
        });
    client_a.apply_event(done_event_a);
    assert_reaches_status(
        &daemon,
        &pane_a.pane_id,
        &SessionStatus::Idle,
        Duration::from_secs(5),
        "wait/monitored/016 (case A, after wait done)",
    )
    .await;
    assert_eq!(
        client_composition_state(&client_a, &pane_a.pane_id),
        daemon_composition_state(&daemon, &pane_a.pane_id).await,
        "wait/monitored/016 (case A, BLOCKER A/B1): the client and daemon composition state \
         must converge back to the cleared baseline together"
    );

    // --- Case B: MonitoredWaitStart, an injected ShellBusy, then wait done
    // while the shell signal is still live (Direction A, mirroring
    // `wait/monitored/011` daemon-side, now with client convergence). ---
    let pane_b = setup_idle_pane(&daemon, &cwd_str, PANE_016_B).await;
    let session_b = format!("{PANE_016_B}-session");
    let mut events_b = daemon.event_tx.subscribe();
    let mut client_b = AppState::default();
    client_b.register_pane(pane_b.pane_id.clone());

    let start_b = run_wait_cli(&daemon, &pane_b.pane_id, &["start", LABEL_016_B], &[]).await;
    assert_wait_cli_succeeded(
        &start_b,
        &format!("wait/monitored/016 (case B): `wait start {LABEL_016_B}` must succeed"),
    );
    let start_event_b = recv_matching_event(&mut events_b, &pane_b.pane_id, Duration::from_secs(3))
        .await
        .unwrap_or_else(|| panic!("wait/monitored/016 (case B): `wait start` must broadcast"));
    client_b.apply_event(start_event_b);
    assert_reaches_status(
        &daemon,
        &pane_b.pane_id,
        &SessionStatus::Working,
        Duration::from_secs(5),
        "wait/monitored/016 (case B, after wait start)",
    )
    .await;
    assert_eq!(
        client_composition_state(&client_b, &pane_b.pane_id),
        daemon_composition_state(&daemon, &pane_b.pane_id).await,
        "wait/monitored/016 (case B): client/daemon must converge after MonitoredWaitStart"
    );

    common::write_hook_line(
        &daemon.hook_path,
        &serde_json::to_string(&shell_activity_event(
            &pane_b.pane_id,
            &pane_b.agent_id,
            &session_b,
            EventType::ShellBusy,
        ))
        .expect("serialize synthetic ShellBusy event"),
    )
    .expect("write synthetic ShellBusy event to the daemon hook socket");
    let shell_busy_event =
        recv_matching_event(&mut events_b, &pane_b.pane_id, Duration::from_secs(3))
            .await
            .unwrap_or_else(|| panic!("wait/monitored/016 (case B): ShellBusy must broadcast"));
    client_b.apply_event(shell_busy_event);
    assert_status_holds(
        &daemon,
        &pane_b.pane_id,
        &SessionStatus::Working,
        Duration::from_millis(200),
        "wait/monitored/016 (case B, after injected ShellBusy)",
    )
    .await;
    assert_eq!(
        client_composition_state(&client_b, &pane_b.pane_id),
        daemon_composition_state(&daemon, &pane_b.pane_id).await,
        "wait/monitored/016 (case B): client/daemon must converge after the injected ShellBusy \
         too (shell_descendant_busy set unconditionally on both sides)"
    );

    let done_b = run_wait_cli(
        &daemon,
        &pane_b.pane_id,
        &["done", LABEL_016_B, "--outcome", "success"],
        &[],
    )
    .await;
    assert_wait_cli_succeeded(
        &done_b,
        &format!(
            "wait/monitored/016 (case B): `wait done {LABEL_016_B} --outcome success` must succeed"
        ),
    );
    let done_event_b = recv_matching_event(&mut events_b, &pane_b.pane_id, Duration::from_secs(3))
        .await
        .unwrap_or_else(|| panic!("wait/monitored/016 (case B): `wait done` must broadcast"));
    client_b.apply_event(done_event_b);
    // The ShellBusy signal never got a paired ShellIdle, so OR composition
    // requires BOTH sides to stay Working — mirrors `011`'s daemon-only
    // assertion, now checked on the independently-replayed client too.
    assert_status_holds(
        &daemon,
        &pane_b.pane_id,
        &SessionStatus::Working,
        Duration::from_millis(300),
        "wait/monitored/016 (case B, after wait done, daemon precondition)",
    )
    .await;
    assert_eq!(
        client_composition_state(&client_b, &pane_b.pane_id).map(|(status, ..)| status),
        Some(SessionStatus::Working),
        "wait/monitored/016 (case B, BLOCKER A/B1): the client must also stay Working — the \
         still-live ShellBusy it independently replayed keeps `shell_descendant_busy` set, \
         which blocks the revert exactly as it does on the daemon"
    );
    assert_eq!(
        client_composition_state(&client_b, &pane_b.pane_id),
        daemon_composition_state(&daemon, &pane_b.pane_id).await,
        "wait/monitored/016 (case B): client and daemon composition state must converge after \
         wait done too"
    );

    daemon.registry.shutdown_all();
}

const RESPAWN2_PANE_ID: &str = "wait-monitored-pane-017-respawn-idle-3f8b52";
const RESPAWN2_LABEL: &str = "wait-monitored-label-017-respawn-idle";

/// Scenario: pin the untested direction of reviewer HIGH C directly against
/// `AppState`, sibling to `wait/monitored/012`'s bare-state respawn fixture.
/// Card A gets a monitored wait declared and it is deliberately NEVER
/// cleared before the pane respawns — the real-world shape the mechanism
/// exists for. The pane respawns to card B, which does genuine work via a
/// real `ToolStart`, then genuinely finishes via a real `Idle`. Assert card
/// B's `Idle` takes effect — it must NOT stay wedged `Working` because of
/// the stale wait still recorded (in `AppState::monitored_waits`, keyed by
/// pane, never cleared) against the now-retired card A. `012` only proves
/// `wait done` doesn't clobber card B's `ToolStart`-driven `Working`; this
/// is the opposite direction — a genuinely idle card B must be allowed to
/// go idle.
#[spec("wait/monitored/017")]
#[test]
#[cfg(unix)]
fn wait_monitored_017_stale_wait_on_retired_card_does_not_wedge_new_card_idle() {
    let t0 = chrono::Utc::now();
    let t1 = t0 + chrono::Duration::seconds(1);
    let t2 = t1 + chrono::Duration::seconds(1);
    let t3 = t2 + chrono::Duration::seconds(1);

    let mut state = AppState::default();
    state.register_pane(RESPAWN2_PANE_ID.to_string());

    // Card A: an idle agent.
    state.apply_event(card_event(
        RESPAWN2_PANE_ID,
        "card-a2-session",
        "agent-a2",
        EventType::SessionStart,
        None,
        t0,
    ));
    assert_eq!(
        state.sessions["card-a2-session"].status,
        SessionStatus::Idle,
        "precondition: card A starts Idle"
    );

    // A monitored wait promotes card A to Working — and is deliberately
    // never cleared, mirroring the real-world shape: the pane respawns
    // before anyone calls `wait done`.
    state.start_monitored_wait(
        RESPAWN2_PANE_ID,
        RESPAWN2_LABEL.to_string(),
        Duration::from_secs(300),
    );
    assert_eq!(
        state.sessions["card-a2-session"].status,
        SessionStatus::Working,
        "precondition: `wait start` promotes card A to Working"
    );

    // The pane respawns: card A is retired, card B is created. The stale
    // `monitored_waits` entry (keyed by pane, pointing at card A) survives
    // this — a respawn is not a teardown, so `013`'s eager-cleanup fix does
    // not apply here.
    state.apply_event(card_event(
        RESPAWN2_PANE_ID,
        "card-b2-session",
        "agent-b2",
        EventType::SessionStart,
        None,
        t1,
    ));
    assert!(
        !state.sessions.contains_key("card-a2-session"),
        "precondition: the respawn's SessionStart must retire card A"
    );
    assert!(
        state.monitored_waits.contains_key(RESPAWN2_PANE_ID),
        "precondition: the stale wait against card A is still recorded on the pane — nobody \
         ever called `wait done` for it, so the daemon-only bookkeeping map still names it"
    );

    // Card B genuinely starts working via a real ToolStart, unrelated to the
    // old wait.
    state.apply_event(card_event(
        RESPAWN2_PANE_ID,
        "card-b2-session",
        "agent-b2",
        EventType::ToolStart,
        Some("Bash"),
        t2,
    ));
    assert_eq!(
        state.sessions["card-b2-session"].status,
        SessionStatus::Working,
        "precondition: card B is genuinely Working via a real ToolStart"
    );

    // Card B genuinely finishes — a real Stop-hook Idle, nothing to do with
    // the stale wait against its predecessor.
    state.apply_event(card_event(
        RESPAWN2_PANE_ID,
        "card-b2-session",
        "agent-b2",
        EventType::Idle,
        None,
        t3,
    ));

    assert_eq!(
        state.sessions["card-b2-session"].status,
        SessionStatus::Idle,
        "wait/monitored/017 (HIGH C, untested direction): card B's own real Idle must take \
         effect — it must not stay wedged Working because of a stale wait recorded (in \
         `AppState::monitored_waits`) against the retired card A. `monitored_wait_active` lives \
         on `SessionState` per-card, and card B never had a MonitoredWaitStart applied to it, \
         so it must read false regardless of what the daemon-only pane-keyed `monitored_waits` \
         map still says about the pane"
    );
}

const SAME_CARD_PANE_ID: &str = "wait-monitored-pane-018-same-card-clobber-6a9d31";
const SAME_CARD_LABEL: &str = "wait-monitored-label-018-same-card-clobber";

/// Scenario: pin the untested same-card direction of reviewer HIGH B/B2
/// directly against `AppState`, sibling to `wait/monitored/012`'s bare-state
/// style. A single card: `wait start` promotes it Idle -> Working, then a
/// real `ToolStart` re-asserts `Working` on the SAME card — the exact
/// scenario round 3's own `MonitoredWaitDone` doc comment names ("a real
/// ToolStart asserted after the wait started"). `wait done` follows. Assert
/// the real Working survives — `wait done` must not revert it just because
/// a monitored wait was once involved. `012` only pins the CROSS-card
/// version (a respawn to a different card); this is the SAME-card case the
/// task specifically calls out as untested.
#[spec("wait/monitored/018")]
#[test]
#[cfg(unix)]
fn wait_monitored_018_same_card_real_tool_start_survives_wait_done() {
    let t0 = chrono::Utc::now();
    let t1 = t0 + chrono::Duration::seconds(1);

    let mut state = AppState::default();
    state.register_pane(SAME_CARD_PANE_ID.to_string());

    state.apply_event(card_event(
        SAME_CARD_PANE_ID,
        "same-card-session",
        "same-card-agent",
        EventType::SessionStart,
        None,
        t0,
    ));
    assert_eq!(
        state.sessions["same-card-session"].status,
        SessionStatus::Idle,
        "precondition: the card starts Idle"
    );

    state.start_monitored_wait(
        SAME_CARD_PANE_ID,
        SAME_CARD_LABEL.to_string(),
        Duration::from_secs(300),
    );
    assert_eq!(
        state.sessions["same-card-session"].status,
        SessionStatus::Working,
        "precondition: `wait start` promotes the card to Working"
    );
    assert!(
        state.sessions["same-card-session"].wait_synthetic_working,
        "precondition: the wait's own promotion is what asserted this Working"
    );

    // A real ToolStart re-asserts Working on the SAME card — round 3's own
    // motivating case for switching the revert guard from
    // `!shell_synthetic_working` to `wait_synthetic_working`.
    state.apply_event(card_event(
        SAME_CARD_PANE_ID,
        "same-card-session",
        "same-card-agent",
        EventType::ToolStart,
        Some("Bash"),
        t1,
    ));
    assert_eq!(
        state.sessions["same-card-session"].status,
        SessionStatus::Working,
        "precondition: the real ToolStart re-asserts Working"
    );
    assert!(
        !state.sessions["same-card-session"].wait_synthetic_working,
        "precondition: a real event other than ShellBusy/MonitoredWaitStart/MonitoredWaitDone \
         clears the wait's own provenance marker — the wait no longer owns this Working"
    );

    state.clear_monitored_wait(SAME_CARD_PANE_ID, SAME_CARD_LABEL, WaitOutcome::Success);

    assert_eq!(
        state.sessions["same-card-session"].status,
        SessionStatus::Working,
        "wait/monitored/018 (HIGH B/B2, untested same-card direction): a real ToolStart's \
         Working, re-asserted on the SAME card after the wait started, must survive `wait \
         done` — reverting because a monitored wait was merely once involved (round 2's \
         `!shell_synthetic_working` guard) would clobber genuine, real-agent activity that has \
         nothing to do with the wait any more"
    );
}

const PANE_019: &str = "wait-monitored-pane-019-headline-tool-52e9f4";
const LABEL_019: &str = "wait-monitored-label-019-headline-tool";

/// Scenario: run the PRD's own headline flow verbatim — an agent runs `wait
/// start` AS A TOOL CALL, so the card is already `Working` from a real
/// `ToolStart` (not `Idle`/`Unknown`) by the time `MonitoredWaitStart`
/// lands; the matching `ToolEnd` follows, then the agent's own Stop-hook
/// `Idle` arrives and is correctly suppressed while the wait is outstanding
/// (Direction C), and finally `wait done` is called. Reviewer BLOCKER H
/// wedge 1: `wait_synthetic_working` only gets set inside
/// `MonitoredWaitStart`'s `promotable` branch, which never fires here — so
/// `wait done` declines to revert and the pane is wedged `Working` forever,
/// with the agent's own real `Idle` already swallowed by the suppression.
#[spec("wait/monitored/019")]
#[test]
#[cfg(unix)]
fn wait_monitored_019_wait_declared_on_working_card_can_still_be_reverted() {
    run_async_test(
        "wait/monitored/019",
        wait_monitored_019_wait_declared_on_working_card_can_still_be_reverted_inner(),
    );
}

#[cfg(unix)]
async fn wait_monitored_019_wait_declared_on_working_card_can_still_be_reverted_inner() {
    let daemon = common::spawn_inprocess_daemon().await;
    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd.path().to_string_lossy().into_owned();
    let pane = setup_idle_pane(&daemon, &cwd_str, PANE_019).await;
    let session_id = format!("{PANE_019}-session");

    // 1. ToolStart (the Bash call running `wait start`) — a REAL agent
    // event, asserts Working.
    common::write_hook_line(
        &daemon.hook_path,
        &serde_json::to_string(&tool_event(
            &pane.pane_id,
            &pane.agent_id,
            &session_id,
            EventType::ToolStart,
            Some("Bash"),
        ))
        .expect("serialize ToolStart event"),
    )
    .expect("write ToolStart event to the daemon hook socket");
    assert_reaches_status(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_secs(5),
        "wait/monitored/019 (after ToolStart)",
    )
    .await;

    // 2. ToolEnd — declines (status != WaitingForInput); no visible change.
    common::write_hook_line(
        &daemon.hook_path,
        &serde_json::to_string(&tool_event(
            &pane.pane_id,
            &pane.agent_id,
            &session_id,
            EventType::ToolEnd,
            None,
        ))
        .expect("serialize ToolEnd event"),
    )
    .expect("write ToolEnd event to the daemon hook socket");
    assert_status_holds(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_millis(200),
        "wait/monitored/019 (after ToolEnd)",
    )
    .await;

    // 3. `wait start` — MonitoredWaitStart lands on an already-Working card
    // (not Idle/Unknown), so it declines to promote and
    // `wait_synthetic_working` is never set.
    let start = run_wait_cli(&daemon, &pane.pane_id, &["start", LABEL_019], &[]).await;
    assert_wait_cli_succeeded(
        &start,
        &format!("wait/monitored/019: `wait start {LABEL_019}` must succeed"),
    );
    assert_status_holds(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_millis(200),
        "wait/monitored/019 (after wait start, declined promotion since already Working)",
    )
    .await;

    // 4. The agent's own Stop-hook Idle arrives while the wait is
    // outstanding — correctly suppressed (Direction C), swallowed for good.
    common::write_hook_line(
        &daemon.hook_path,
        &serde_json::to_string(&idle_event(&pane.pane_id, &pane.agent_id, &session_id))
            .expect("serialize real Idle event"),
    )
    .expect("write real Idle event to the daemon hook socket");
    assert_status_holds(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_millis(200),
        "wait/monitored/019 (after suppressed Idle, Direction C)",
    )
    .await;

    // 5. `wait done` — the wait is now the ONLY live signal that was ever
    // standing (the agent's own Idle is already gone), so the pane must
    // revert. BLOCKER H wedge 1: it never does, because
    // `wait_synthetic_working` was never set at step 3.
    let done = run_wait_cli(
        &daemon,
        &pane.pane_id,
        &["done", LABEL_019, "--outcome", "success"],
        &[],
    )
    .await;
    assert_wait_cli_succeeded(
        &done,
        &format!("wait/monitored/019: `wait done {LABEL_019} --outcome success` must succeed"),
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
            "wait/monitored/019 (BLOCKER H wedge 1, the PRD's own headline flow): pane must \
             revert to Idle after `wait done` once the wait was the last live signal standing — \
             the agent's own real Idle was already suppressed and swallowed at step 4 — but the \
             pane stays wedged Working forever: {e}"
        )
    });

    daemon.registry.shutdown_all();
}

const PANE_020: &str = "wait-monitored-pane-020-direction-a-tail-6d1c83";
const LABEL_020: &str = "wait-monitored-label-020-direction-a-tail";

/// Scenario: extend `wait/monitored/011`'s own sequence past the point it
/// stops — `wait start` promotes an idle pane to `Working`, an injected
/// `ShellBusy` holds it up (declines to promote since already `Working`),
/// `wait done` declines because `shell_descendant_busy` is still set
/// (Direction A, same precondition `011` asserts) — and THEN, unlike `011`,
/// the paired `ShellIdle` arrives. Reviewer BLOCKER H wedge 2:
/// `MonitoredWaitDone` unconditionally clears `wait_synthetic_working` on
/// its way out even when it declined, so by the time `ShellIdle` arrives
/// neither marker was ever transferred to shell — `was_holding` reads
/// `shell_synthetic_working == false` (never set, since `ShellBusy` only
/// promotes a stale/no-opinion status) and declines too, wedging the pane
/// `Working` forever.
#[spec("wait/monitored/020")]
#[test]
#[cfg(unix)]
fn wait_monitored_020_direction_a_tail_shell_idle_still_reverts() {
    run_async_test(
        "wait/monitored/020",
        wait_monitored_020_direction_a_tail_shell_idle_still_reverts_inner(),
    );
}

#[cfg(unix)]
async fn wait_monitored_020_direction_a_tail_shell_idle_still_reverts_inner() {
    let daemon = common::spawn_inprocess_daemon().await;
    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd.path().to_string_lossy().into_owned();
    let pane = setup_idle_pane(&daemon, &cwd_str, PANE_020).await;
    let session_id = format!("{PANE_020}-session");

    // 1. `wait start` on an Idle pane — MonitoredWaitStart promotes:
    // status = Working, wait_synthetic_working = true.
    let start = run_wait_cli(&daemon, &pane.pane_id, &["start", LABEL_020], &[]).await;
    assert_wait_cli_succeeded(
        &start,
        &format!("wait/monitored/020: `wait start {LABEL_020}` must succeed"),
    );
    assert_reaches_status(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_secs(5),
        "wait/monitored/020 (after wait start)",
    )
    .await;

    // 2. Injected ShellBusy — declines to promote (already Working);
    // shell_descendant_busy = true unconditionally, shell_synthetic_working
    // stays false (this mechanism did not cause the current Working).
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
        "wait/monitored/020 (after injected ShellBusy)",
    )
    .await;

    // 3. `wait done` — declines because shell_descendant_busy still holds
    // the card up (Direction A, correct) — but unconditionally clears
    // wait_synthetic_working on the way out, same as `011` stops at.
    let done = run_wait_cli(
        &daemon,
        &pane.pane_id,
        &["done", LABEL_020, "--outcome", "success"],
        &[],
    )
    .await;
    assert_wait_cli_succeeded(
        &done,
        &format!("wait/monitored/020: `wait done {LABEL_020} --outcome success` must succeed"),
    );
    assert_status_holds(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_millis(500),
        "wait/monitored/020 (after wait done, Direction A precondition — matches `011`)",
    )
    .await;

    // 4. The paired ShellIdle arrives — the event `011` never sends. The
    // descendant it represents has genuinely finished, so OR composition
    // requires the pane to revert now that neither signal is live. BLOCKER
    // H wedge 2: `was_holding` reads shell_synthetic_working == false (it
    // was never set at step 2), so this also declines, and the pane is
    // wedged Working forever with all four markers false.
    common::write_hook_line(
        &daemon.hook_path,
        &serde_json::to_string(&shell_activity_event(
            &pane.pane_id,
            &pane.agent_id,
            &session_id,
            EventType::ShellIdle,
        ))
        .expect("serialize synthetic ShellIdle event"),
    )
    .expect("write synthetic ShellIdle event to the daemon hook socket");
    wait_for_status(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Idle,
        Duration::from_secs(5),
    )
    .await
    .unwrap_or_else(|e| {
        panic!(
            "wait/monitored/020 (BLOCKER H wedge 2, Direction A's tail — extends `011` past the \
             ShellIdle it stops before): the pane must revert to Idle once the shell descendant \
             `011` leaves busy has genuinely gone idle and the wait already cleared — but \
             `MonitoredWaitDone`'s unconditional clear of `wait_synthetic_working` at step 3 \
             left nothing for `ShellIdle` to find still holding the card, so it declines too and \
             the pane stays wedged Working forever: {e}"
        )
    });

    daemon.registry.shutdown_all();
}

const PANE_021: &str = "wait-monitored-pane-021-direction-b-tail-9a4e27";
const LABEL_021: &str = "wait-monitored-label-021-direction-b-tail";

/// Scenario: the mirror of `wait/monitored/020` — `ShellBusy` promotes an
/// idle pane to `Working` first (`shell_synthetic_working = true`), `wait
/// start` lands on the already-Working card and declines to promote
/// (`wait_synthetic_working` stays false), then the paired `ShellIdle`
/// arrives while the wait is still outstanding and correctly declines to
/// revert (Direction B) — but reviewer BLOCKER H wedge 3: `ShellIdle`
/// unconditionally clears `shell_synthetic_working` on its way out
/// regardless of whether it actually reverted, so by the time `wait done`
/// runs neither marker was ever transferred to the wait, and it declines
/// too, wedging the pane `Working` forever.
#[spec("wait/monitored/021")]
#[test]
#[cfg(unix)]
fn wait_monitored_021_direction_b_tail_wait_done_still_reverts() {
    run_async_test(
        "wait/monitored/021",
        wait_monitored_021_direction_b_tail_wait_done_still_reverts_inner(),
    );
}

#[cfg(unix)]
async fn wait_monitored_021_direction_b_tail_wait_done_still_reverts_inner() {
    let daemon = common::spawn_inprocess_daemon().await;
    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd.path().to_string_lossy().into_owned();
    let pane = setup_idle_pane(&daemon, &cwd_str, PANE_021).await;
    let session_id = format!("{PANE_021}-session");

    // 1. Injected ShellBusy on the Idle pane — promotes: status = Working,
    // shell_synthetic_working = true, shell_descendant_busy = true.
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
    assert_reaches_status(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_secs(5),
        "wait/monitored/021 (after injected ShellBusy)",
    )
    .await;

    // 2. `wait start` — MonitoredWaitStart lands on an already-Working card
    // (not Idle/Unknown), so it declines to promote and
    // wait_synthetic_working is never set.
    let start = run_wait_cli(&daemon, &pane.pane_id, &["start", LABEL_021], &[]).await;
    assert_wait_cli_succeeded(
        &start,
        &format!("wait/monitored/021: `wait start {LABEL_021}` must succeed"),
    );
    assert_status_holds(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_millis(200),
        "wait/monitored/021 (after wait start, declined promotion since already Working)",
    )
    .await;

    // 3. The paired ShellIdle arrives while the wait is still outstanding —
    // was_holding is true (shell_synthetic_working was set at step 1), but
    // monitored_wait_active suppresses the revert (Direction B, correct) —
    // yet it still unconditionally clears shell_synthetic_working AND
    // shell_descendant_busy on the way out.
    common::write_hook_line(
        &daemon.hook_path,
        &serde_json::to_string(&shell_activity_event(
            &pane.pane_id,
            &pane.agent_id,
            &session_id,
            EventType::ShellIdle,
        ))
        .expect("serialize synthetic ShellIdle event"),
    )
    .expect("write synthetic ShellIdle event to the daemon hook socket");
    assert_status_holds(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_millis(200),
        "wait/monitored/021 (after suppressed ShellIdle, Direction B)",
    )
    .await;

    // 4. `wait done` — the wait is now the ONLY live signal that was ever
    // standing (shell's own claim was cleared at step 3 despite declining
    // to revert). BLOCKER H wedge 3: it never reverts, because
    // wait_synthetic_working was never set at step 2.
    let done = run_wait_cli(
        &daemon,
        &pane.pane_id,
        &["done", LABEL_021, "--outcome", "success"],
        &[],
    )
    .await;
    assert_wait_cli_succeeded(
        &done,
        &format!("wait/monitored/021: `wait done {LABEL_021} --outcome success` must succeed"),
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
            "wait/monitored/021 (BLOCKER H wedge 3, Direction B's tail — the mirror of wedge 2): \
             the pane must revert to Idle after `wait done` once the wait was the last live \
             signal standing — but ShellIdle's unconditional clear of shell_synthetic_working at \
             step 3 left nothing for `wait done` to find, so it declines too and the pane stays \
             wedged Working forever: {e}"
        )
    });

    daemon.registry.shutdown_all();
}

const UNREGISTER_ONLY_PANE_ID: &str = "wait-monitored-pane-022-unregister-only-7c2b48";
const UNREGISTER_ONLY_LABEL: &str = "wait-monitored-label-022-unregister-only";

/// Scenario: pin auditor C2 directly against `AppState`, sibling to `013`.
/// A pane with an active monitored wait is torn down with `unregister_pane`
/// ALONE — never `remove_sessions_for_pane` — the shape the daemon's own
/// `StopAgent` handler actually uses, where the card can survive the call.
/// Round 3's B3 fix (the eager `monitored_waits.remove` was pulled OUT of
/// `unregister_pane` specifically because this method's card can survive
/// it) is pinned by no existing test: `013` calls both teardown methods
/// together, so it passes whether or not `unregister_pane` alone still
/// drops the entry. Assert the card AND the `monitored_waits` entry both
/// survive `unregister_pane` alone, so a future regression re-adding the
/// eager removal there fails this test rather than going green.
#[spec("wait/monitored/022")]
#[test]
#[cfg(unix)]
fn wait_monitored_022_unregister_pane_alone_does_not_drop_the_wait() {
    let t0 = chrono::Utc::now();

    let mut state = AppState::default();
    state.register_pane(UNREGISTER_ONLY_PANE_ID.to_string());
    state.apply_event(card_event(
        UNREGISTER_ONLY_PANE_ID,
        "unregister-only-session",
        "unregister-only-agent",
        EventType::SessionStart,
        None,
        t0,
    ));
    state.start_monitored_wait(
        UNREGISTER_ONLY_PANE_ID,
        UNREGISTER_ONLY_LABEL.to_string(),
        Duration::from_secs(300),
    );
    assert!(
        state.monitored_waits.contains_key(UNREGISTER_ONLY_PANE_ID),
        "precondition: `wait start` records a monitored wait for the pane"
    );
    assert!(
        state.sessions.contains_key("unregister-only-session"),
        "precondition: the card exists"
    );

    // `unregister_pane` ALONE — not `remove_sessions_for_pane` — the shape
    // that can leave the card standing.
    state.unregister_pane(UNREGISTER_ONLY_PANE_ID);

    assert!(
        state.sessions.contains_key("unregister-only-session"),
        "precondition check: `unregister_pane` alone must not touch `sessions` — if this fails, \
         the test fixture itself is wrong, not the assertion below"
    );
    assert!(
        state.monitored_waits.contains_key(UNREGISTER_ONLY_PANE_ID),
        "wait/monitored/022 (auditor C2): `unregister_pane` ALONE must NOT eagerly drop the \
         pane's `monitored_waits` entry — the card it was recorded against can survive this \
         call (it is not paired with `remove_sessions_for_pane` at every call site, including \
         the daemon's own `StopAgent` handler), and dropping the entry here removes the ONLY \
         thing that could still heal that surviving card via the TTL sweep. This regressed once \
         already inside this PR (added round 2, removed round 3) and was pinned by no test — \
         `013` calls both teardown methods together and passes either way"
    );
}

const PANE_023: &str = "wait-monitored-pane-023-direction-a-deferred-tail-b7e29d";
const LABEL_023: &str = "wait-monitored-label-023-direction-a-deferred-tail";

/// Scenario: the PRD's own headline flow (an agent runs `wait start` as a
/// tool call, so the card is `Working` from a real `ToolStart`) plus a
/// background shell descendant that is still busy when `wait done` lands.
/// Reviewer BLOCKER I wedge 4a: `MonitoredWaitDone`'s Direction-A decline
/// (it declines because `shell_descendant_busy` is live, same as `020`'s
/// precondition) unconditionally clears `wait_deferred_revert` — the marker
/// the agent's own suppressed `Idle` set — without transferring it to shell,
/// so the paired `ShellIdle` that follows finds nothing still holding the
/// card and also declines. Same failure class as `019`/`020`/`021`, but via
/// the `wait_deferred_revert` marker those three never exercise together
/// with a live `shell_descendant_busy` at the moment `wait done` runs.
#[spec("wait/monitored/023")]
#[test]
#[cfg(unix)]
fn wait_monitored_023_direction_a_deferred_revert_tail_still_reverts() {
    run_async_test(
        "wait/monitored/023",
        wait_monitored_023_direction_a_deferred_revert_tail_still_reverts_inner(),
    );
}

#[cfg(unix)]
async fn wait_monitored_023_direction_a_deferred_revert_tail_still_reverts_inner() {
    let daemon = common::spawn_inprocess_daemon().await;
    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd.path().to_string_lossy().into_owned();
    let pane = setup_idle_pane(&daemon, &cwd_str, PANE_023).await;
    let session_id = format!("{PANE_023}-session");

    // 1. ToolStart (the Bash call running `wait start`) — a REAL agent
    // event, asserts Working; the trailing block clears all four markers.
    common::write_hook_line(
        &daemon.hook_path,
        &serde_json::to_string(&tool_event(
            &pane.pane_id,
            &pane.agent_id,
            &session_id,
            EventType::ToolStart,
            Some("Bash"),
        ))
        .expect("serialize ToolStart event"),
    )
    .expect("write ToolStart event to the daemon hook socket");
    assert_reaches_status(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_secs(5),
        "wait/monitored/023 (after ToolStart)",
    )
    .await;

    // 2. ToolEnd — declines (status != WaitingForInput); no visible change.
    common::write_hook_line(
        &daemon.hook_path,
        &serde_json::to_string(&tool_event(
            &pane.pane_id,
            &pane.agent_id,
            &session_id,
            EventType::ToolEnd,
            None,
        ))
        .expect("serialize ToolEnd event"),
    )
    .expect("write ToolEnd event to the daemon hook socket");
    assert_status_holds(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_millis(200),
        "wait/monitored/023 (after ToolEnd)",
    )
    .await;

    // 3. `wait start` — MonitoredWaitStart lands on an already-Working card
    // (not Idle/Unknown), so it declines to promote and
    // wait_synthetic_working is never set.
    let start = run_wait_cli(&daemon, &pane.pane_id, &["start", LABEL_023], &[]).await;
    assert_wait_cli_succeeded(
        &start,
        &format!("wait/monitored/023: `wait start {LABEL_023}` must succeed"),
    );
    assert_status_holds(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_millis(200),
        "wait/monitored/023 (after wait start, declined promotion since already Working)",
    )
    .await;

    // 4. The background shell watcher observes the descendant is busy —
    // declines to promote (already Working); shell_descendant_busy = true
    // unconditionally, shell_synthetic_working stays false (this mechanism
    // did not cause the current Working, so it has no claim yet).
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
        "wait/monitored/023 (after injected ShellBusy)",
    )
    .await;

    // 5. The agent's own Stop-hook Idle arrives while the wait is still
    // outstanding — suppressed (Direction C, correct), and because this
    // Working was never the wait's own promotion, the suppression records
    // wait_deferred_revert = true rather than wait_synthetic_working.
    common::write_hook_line(
        &daemon.hook_path,
        &serde_json::to_string(&idle_event(&pane.pane_id, &pane.agent_id, &session_id))
            .expect("serialize real Idle event"),
    )
    .expect("write real Idle event to the daemon hook socket");
    assert_status_holds(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_millis(200),
        "wait/monitored/023 (after suppressed Idle, Direction C, wait_deferred_revert set)",
    )
    .await;

    // 6. `wait done` — declines because shell_descendant_busy still holds
    // the card up (Direction A, correct) — but BLOCKER I:
    // MonitoredWaitDone unconditionally clears wait_deferred_revert on the
    // way out without transferring it to shell, exactly as `020` showed for
    // wait_synthetic_working.
    let done = run_wait_cli(
        &daemon,
        &pane.pane_id,
        &["done", LABEL_023, "--outcome", "success"],
        &[],
    )
    .await;
    assert_wait_cli_succeeded(
        &done,
        &format!("wait/monitored/023: `wait done {LABEL_023} --outcome success` must succeed"),
    );
    assert_status_holds(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Working,
        Duration::from_millis(500),
        "wait/monitored/023 (after wait done, Direction A precondition)",
    )
    .await;

    // 7. The paired ShellIdle arrives — the descendant it represents has
    // genuinely finished, so OR composition requires the pane to revert now
    // that neither signal is live. BLOCKER I: was_holding reads
    // shell_synthetic_working == false (never set — step 6 never
    // transferred wait_deferred_revert to it), so this also declines, and
    // the pane is wedged Working forever with all four markers false.
    common::write_hook_line(
        &daemon.hook_path,
        &serde_json::to_string(&shell_activity_event(
            &pane.pane_id,
            &pane.agent_id,
            &session_id,
            EventType::ShellIdle,
        ))
        .expect("serialize synthetic ShellIdle event"),
    )
    .expect("write synthetic ShellIdle event to the daemon hook socket");
    wait_for_status(
        &daemon,
        &pane.pane_id,
        &SessionStatus::Idle,
        Duration::from_secs(5),
    )
    .await
    .unwrap_or_else(|e| {
        panic!(
            "wait/monitored/023 (BLOCKER I wedge 4a, the PRD's headline flow plus a live shell \
             descendant): the pane must revert to Idle once the wait was the last live signal \
             standing — the agent's own real Idle was already suppressed into \
             wait_deferred_revert at step 5, and the shell descendant genuinely went idle at \
             step 7 — but `MonitoredWaitDone`'s Direction-A decline at step 6 unconditionally \
             cleared wait_deferred_revert without transferring it to shell, so nothing was left \
             for ShellIdle to find still holding the card, and the pane stays wedged Working \
             forever: {e}"
        )
    });

    daemon.registry.shutdown_all();
}
