//! Fork-owned `delegate` / `work-done` regressions that outlived the typed
//! prompt delivery.
//!
//! This file used to be the #187 / PR #188 guard that a delegated worker pane
//! receives a SINGLE-LINE prompt typed into its PTY, with the `work-done`
//! footer in the task FILE. Upstream replaced that typed delivery with the
//! inbox (PRD #1666) and deleted the file together with the machinery it
//! pinned; the sync (2026-11-05) kept only the tests below, which pin fork
//! contracts upstream does not have and which do not depend on a typed
//! delivery:
//!
//! * `orchestration/delegate/040` to `/043`: the `delegate` CLI's verdict says
//!   what was actually armed, and never reports success for an unknown role, a
//!   self-target, or a partly resolved target list.
//!
//! DROPPED in the second tester round (sync rulings 30 Q1 and 31):
//! `orchestration/delegate/045`, `/058` and `/035` (concurrent same-role,
//! same-cwd `work-done` reports must not clobber each other, fork #613). They
//! pinned the fork's pane-keyed, archive-on-collision report FILE names, which
//! the identity layer's drop removes: upstream's summary file is role-keyed
//! (`work-done-<role>.md`) and the report itself travels as an inbox message,
//! each message retained on its own. Upstream's `work_done_013_a_worker_report_
//! at_the_summary_path_is_kept` (`tests/work_done_reporting.rs`) pins that an
//! existing file at the summary path is kept; the long-report residual (two
//! same-role siblings in one cwd overwrite one summary file) is an upstream
//! defect to file (sync memo risk 1), and a replacement inbox-level test (two
//! same-role workers, both reports present as distinct inbox messages) is owed
//! once the tree compiles.
//!
//! Dropped with the typed delivery, not carried: the `--subject` hostile-text
//! tests that drove a typed pointer into a worker pane through the
//! `SilenceHarness` (see the sync notes for their names and why).

use dot_agent_deck::agent_pty::{DOT_AGENT_DECK_PANE_ID, SpawnOptions};
use dot_agent_deck::state::{AppState, OrchestrationIdentity};
#[cfg(unix)]
use spec::spec;

mod common;

const ORCH_PANE: &str = "orchestrator-pane";
const WORKER_PANE: &str = "worker-pane";
const WORKER_ROLE: &str = "coder";

#[cfg(unix)]
fn register_orchestration(state: &mut AppState, cwd: &str) {
    // Upstream #463 made `OrchestrationIdentity` a `{ id, name }` struct and
    // dropped the tokenless `NameCwd` variant this helper used to build; the
    // per-tab token is now what groups the panes, the cwd rides in
    // `pane_cwd_map` below.
    let orchestration = OrchestrationIdentity {
        id: "test-orchestration-0".to_string(),
        name: "test-orchestration".to_string(),
    };
    state
        .pane_role_map
        .insert(ORCH_PANE.to_string(), "orchestrator".to_string());
    state
        .pane_role_map
        .insert(WORKER_PANE.to_string(), WORKER_ROLE.to_string());
    state.orchestrator_pane_ids.insert(ORCH_PANE.to_string());
    state
        .pane_orchestration_map
        .insert(ORCH_PANE.to_string(), orchestration.clone());
    state
        .pane_orchestration_map
        .insert(WORKER_PANE.to_string(), orchestration);
    state
        .pane_cwd_map
        .insert(WORKER_PANE.to_string(), cwd.to_string());
}

/// An in-process daemon whose hook-provenance policy is `warn`, so the real
/// `delegate` CLI these tests launch from the harness (which has no per-spawn
/// capability token of its own) is judged on `DOT_AGENT_DECK_PANE_ID` alone —
/// upstream's `TuiDeckBuilder::impersonating_pane_signals`, for an in-process
/// daemon. The policy is read from this process's environment when the daemon
/// starts.
#[cfg(unix)]
async fn spawn_daemon_accepting_harness_cli() -> common::InProcDaemon {
    // SAFETY: nextest runs each test of this binary in its own process, so no
    // sibling test can observe or race this mutation.
    unsafe {
        std::env::set_var(
            dot_agent_deck::hook_provenance::DOT_AGENT_DECK_HOOK_PROVENANCE,
            "warn",
        );
    }
    common::spawn_inprocess_daemon().await
}

#[cfg(unix)]
struct CliDelegateResult {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

/// Run the real `dot-agent-deck delegate` CLI as a subprocess against
/// `hook_path`, exactly as a role's shell would from inside a pane —
/// `DOT_AGENT_DECK_PANE_ID` is the only thing that identifies the caller to
/// the daemon.
///
/// This subprocess is launched from the test harness's own process, not the
/// orchestrator pane's real spawned child, so it carries no per-spawn capability
/// token; the daemon under test is started by
/// [`spawn_daemon_accepting_harness_cli`], whose hook-provenance policy is
/// `warn` (the upstream mechanism for a test that impersonates a pane).
#[cfg(unix)]
async fn run_delegate_cli(
    hook_path: &std::path::Path,
    pane_id: &str,
    to: &str,
    task: &str,
) -> CliDelegateResult {
    let hook_path = hook_path.to_path_buf();
    let pane_id = pane_id.to_string();
    let to = to.to_string();
    let task = task.to_string();
    tokio::task::spawn_blocking(move || {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_dot-agent-deck"))
            .arg("delegate")
            .arg("--to")
            .arg(&to)
            .arg("--task")
            .arg(&task)
            .env(DOT_AGENT_DECK_PANE_ID, &pane_id)
            .env("DOT_AGENT_DECK_SOCKET", &hook_path)
            .output()
            .expect("run the real `dot-agent-deck delegate` CLI as a subprocess");
        CliDelegateResult {
            status: output.status,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    })
    .await
    .expect("delegate CLI subprocess task did not panic")
}

/// Same as [`run_delegate_cli`] but with MULTIPLE `--to` flags, needed for
/// `orchestration/delegate/042`'s duplicate-role scenario — a single `--to`
/// cannot reproduce a `signal.to` containing a repeated role name.
#[cfg(unix)]
async fn run_delegate_cli_multi(
    hook_path: &std::path::Path,
    pane_id: &str,
    to: &[&str],
    task: &str,
) -> CliDelegateResult {
    let hook_path = hook_path.to_path_buf();
    let pane_id = pane_id.to_string();
    let to: Vec<String> = to.iter().map(|s| s.to_string()).collect();
    let task = task.to_string();
    tokio::task::spawn_blocking(move || {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_dot-agent-deck"));
        cmd.arg("delegate");
        for role in &to {
            cmd.arg("--to").arg(role);
        }
        cmd.arg("--task")
            .arg(&task)
            .env(DOT_AGENT_DECK_PANE_ID, &pane_id)
            .env("DOT_AGENT_DECK_SOCKET", &hook_path);
        let output = cmd
            .output()
            .expect("run the real `dot-agent-deck delegate` CLI as a subprocess");
        CliDelegateResult {
            status: output.status,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    })
    .await
    .expect("delegate CLI subprocess task did not panic")
}

// --- fork #92: `delegate` confirms a delegation it never made -------------
//
// P3 follow-up to `orchestration/delegate/016`-`018`: none of that trio
// proves the success text corresponds to a target that actually RESOLVED.
// `delegate_targets` (state.rs:2922) deliberately drops missing roles and
// excludes the orchestrator's own pane before `handle_delegate` ever fans
// out any dispatch or arms any idle-worker watch — but `handle_delegate`
// still replies `DelegateResponse::accepted(signal.to)` at state.rs:3133,
// echoing the REQUESTED roles instead of the ones that resolved. So a
// delegate that armed nothing is confirmed exactly like one that did,
// which can induce the duplicate-delegation retry upstream #330 exists to
// stop. Same real-CLI-subprocess-against-a-real-daemon-socket technique as
// `016`-`018`; same discipline of not pinning exit-code value, message
// wording, or `DelegateResponse` field shape — only what the caller can
// observe.

/// Scenario: Register a real orchestrator pane and a `cat`-stub `coder` worker in the same orchestration, then run the REAL `dot-agent-deck delegate` CLI from the orchestrator pane targeting a role with no registered pane at all. `delegate_targets` silently drops the unresolved role — no dispatch queued, no worker watch armed — but today's reply still echoes `signal.to`, so the CLI reports success. Assert the CLI does not report success for a delegation that armed nothing (fork #92 P1).
#[spec("orchestration/delegate/040")]
#[test]
#[cfg(unix)]
fn delegate_040_unknown_role_does_not_report_success() {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build unknown-role-confirmation-truthfulness runtime")
        .block_on(delegate_040_unknown_role_does_not_report_success_inner());
}

#[cfg(unix)]
async fn delegate_040_unknown_role_does_not_report_success_inner() {
    let daemon = spawn_daemon_accepting_harness_cli().await;
    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd.path().to_string_lossy().into_owned();

    // A resolvable `coder` worker exists — this is a perfectly valid
    // orchestrator pane in a live orchestration, not an empty daemon. The
    // delegate below targets a DIFFERENT role that has no pane at all, so
    // `delegate_targets` (state.rs:2941) drops it and nothing is armed for
    // this specific request.
    daemon
        .registry
        .spawn_agent(SpawnOptions {
            command: Some("cat"),
            cwd: Some(&cwd_str),
            env: vec![(DOT_AGENT_DECK_PANE_ID.to_string(), WORKER_PANE.to_string())],
            ..SpawnOptions::default()
        })
        .expect("spawn worker stub");
    {
        let mut state = daemon.state.write().await;
        register_orchestration(&mut state, &cwd_str);
    }

    let result = run_delegate_cli(
        &daemon.hook_path,
        ORCH_PANE,
        "nonexistent-role",
        "Escalate to a role that does not exist.",
    )
    .await;

    assert!(
        !result.status.success(),
        "a delegate that resolved to zero targets must not report success — no dispatch was \
         queued and no worker watch was armed, so an exit-0 confirmation here is exactly the \
         false positive that can induce the duplicate-delegation retry #330 exists to prevent \
         (fork #92 P1); got status={:?} stdout={:?} stderr={:?}",
        result.status,
        result.stdout,
        result.stderr
    );

    daemon.registry.shutdown_all();
}

/// Scenario: Register a real orchestrator pane and a `cat`-stub `coder` worker in the same orchestration, then run the REAL `dot-agent-deck delegate` CLI from the orchestrator pane targeting its OWN role name (`orchestrator`). `delegate_targets` (state.rs:2936) deliberately excludes any pane in `orchestrator_pane_ids` from role resolution, so this resolves to nothing even though a pane with that exact role name exists — and today's reply still echoes `signal.to`, reporting success. Assert the CLI does not report success for a self-targeted delegation (fork #92 P1).
#[spec("orchestration/delegate/041")]
#[test]
#[cfg(unix)]
fn delegate_041_self_target_does_not_report_success() {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build self-target-confirmation-truthfulness runtime")
        .block_on(delegate_041_self_target_does_not_report_success_inner());
}

#[cfg(unix)]
async fn delegate_041_self_target_does_not_report_success_inner() {
    let daemon = spawn_daemon_accepting_harness_cli().await;
    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd.path().to_string_lossy().into_owned();

    daemon
        .registry
        .spawn_agent(SpawnOptions {
            command: Some("cat"),
            cwd: Some(&cwd_str),
            env: vec![(DOT_AGENT_DECK_PANE_ID.to_string(), WORKER_PANE.to_string())],
            ..SpawnOptions::default()
        })
        .expect("spawn worker stub");
    {
        let mut state = daemon.state.write().await;
        register_orchestration(&mut state, &cwd_str);
    }

    // `register_orchestration` gives ORCH_PANE the role "orchestrator" —
    // delegating back to that same role name resolves to nothing, because
    // `delegate_targets` excludes any pane in `orchestrator_pane_ids`
    // regardless of whether its role name matches the request.
    //
    let result = run_delegate_cli(
        &daemon.hook_path,
        ORCH_PANE,
        "orchestrator",
        "Escalate to my own role.",
    )
    .await;

    assert!(
        !result.status.success(),
        "a delegate that resolved to zero targets (self-targeting the orchestrator's own role) \
         must not report success, for the same reason as the unresolved-role case (fork #92 \
         P1); got status={:?} stdout={:?} stderr={:?}",
        result.status,
        result.stdout,
        result.stderr
    );

    daemon.registry.shutdown_all();
}

/// Scenario: Register a real orchestrator pane and a single `cat`-stub `coder` worker, then run the REAL `dot-agent-deck delegate` CLI from the orchestrator pane with `--to coder --to coder`. `delegate_targets` de-duplicates the repeated role before dispatch, so exactly one worker pane is armed — but today's reply still echoes `signal.to` verbatim, naming `coder` twice. Assert the confirmation names the role no more times than panes were actually armed (fork #92 P2).
#[spec("orchestration/delegate/042")]
#[test]
#[cfg(unix)]
fn delegate_042_duplicate_role_reports_what_was_armed() {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build duplicate-role-confirmation-truthfulness runtime")
        .block_on(delegate_042_duplicate_role_reports_what_was_armed_inner());
}

#[cfg(unix)]
async fn delegate_042_duplicate_role_reports_what_was_armed_inner() {
    let daemon = spawn_daemon_accepting_harness_cli().await;
    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd.path().to_string_lossy().into_owned();

    daemon
        .registry
        .spawn_agent(SpawnOptions {
            command: Some("cat"),
            cwd: Some(&cwd_str),
            env: vec![(DOT_AGENT_DECK_PANE_ID.to_string(), WORKER_PANE.to_string())],
            ..SpawnOptions::default()
        })
        .expect("spawn worker stub");
    {
        let mut state = daemon.state.write().await;
        register_orchestration(&mut state, &cwd_str);
    }

    let result = run_delegate_cli_multi(
        &daemon.hook_path,
        ORCH_PANE,
        &[WORKER_ROLE, WORKER_ROLE],
        "Escalate with a duplicated target role.",
    )
    .await;

    assert!(
        result.status.success(),
        "exactly one worker pane exists for the duplicated role and de-duplication happens \
         before dispatch, so this delegate must still succeed; got status={:?} stdout={:?} \
         stderr={:?}",
        result.status,
        result.stdout,
        result.stderr
    );
    let armed_role_mentions = result.stdout.matches(WORKER_ROLE).count();
    assert_eq!(
        armed_role_mentions, 1,
        "`delegate_targets` de-duplicates the repeated role before dispatch, so exactly one \
         worker pane was armed — the confirmation must name the role once, not once per \
         request, or the caller cannot tell a duplicate report from a second worker actually \
         having been armed (fork #92 P2); got status={:?} stdout={:?} stderr={:?}",
        result.status, result.stdout, result.stderr
    );

    daemon.registry.shutdown_all();
}

/// Scenario: Register a real orchestrator pane and a single `cat`-stub `coder` worker, then run the REAL `dot-agent-deck delegate` CLI from the orchestrator pane with `--to coder --to nonexistent-role` — one requested role resolves to a real pane, its sibling does not. The fork #92 partial-resolution decision requires the call to succeed (something really did arm, so failing it would make a retry-on-non-zero orchestrator double-delegate the role that DID arm — upstream #330's harm) while naming BOTH the armed role and the unresolved one, distinguishably. Today `handle_delegate` replies `DelegateResponse::accepted(signal.to)` unconditionally, so `main.rs` folds both roles into one undifferentiated, comma-joined echo of the raw request — the same silent-drop failure `031`/`032` pin, wearing the partial-resolution shape. Assert the call succeeds, both roles are named somewhere observable, and they are not simply reported as one plain comma-joined pair (fork #92 partial-resolution decision).
#[spec("orchestration/delegate/043")]
#[test]
#[cfg(unix)]
fn delegate_043_partial_resolution_names_both_armed_and_unresolved() {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build partial-resolution-confirmation runtime")
        .block_on(delegate_043_partial_resolution_names_both_armed_and_unresolved_inner());
}

#[cfg(unix)]
async fn delegate_043_partial_resolution_names_both_armed_and_unresolved_inner() {
    let daemon = spawn_daemon_accepting_harness_cli().await;
    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd.path().to_string_lossy().into_owned();

    daemon
        .registry
        .spawn_agent(SpawnOptions {
            command: Some("cat"),
            cwd: Some(&cwd_str),
            env: vec![(DOT_AGENT_DECK_PANE_ID.to_string(), WORKER_PANE.to_string())],
            ..SpawnOptions::default()
        })
        .expect("spawn worker stub");
    {
        let mut state = daemon.state.write().await;
        register_orchestration(&mut state, &cwd_str);
    }

    const UNRESOLVED_ROLE: &str = "nonexistent-role";

    let result = run_delegate_cli_multi(
        &daemon.hook_path,
        ORCH_PANE,
        &[WORKER_ROLE, UNRESOLVED_ROLE],
        "Escalate to one real role and one that does not exist.",
    )
    .await;

    assert!(
        result.status.success(),
        "one requested role really did resolve and arm a worker — failing the whole call would \
         make an orchestrator that retries on non-zero double-delegate the role that DID arm, \
         precisely upstream #330's harm (fork #92 partial-resolution decision); got \
         status={:?} stdout={:?} stderr={:?}",
        result.status,
        result.stdout,
        result.stderr
    );

    let combined = format!("{}{}", result.stdout, result.stderr);
    assert!(
        combined.contains(WORKER_ROLE),
        "the role that actually armed a worker must be named in the confirmation, or the \
         caller has no way to know anything happened at all; stdout={:?} stderr={:?}",
        result.stdout,
        result.stderr
    );
    assert!(
        combined.contains(UNRESOLVED_ROLE),
        "the role that resolved to nothing must be named too, somewhere observable, or a \
         caller cannot tell this apart from a delegate where both roles armed (fork #92 \
         partial-resolution decision); stdout={:?} stderr={:?}",
        result.stdout,
        result.stderr
    );

    // The bug this pins: `handle_delegate` (state.rs:3133) replies
    // `DelegateResponse::accepted(signal.to)` unconditionally, so
    // `main.rs`'s `println!("Delegated to {}.", resp.roles.join(", "))`
    // folds BOTH the armed and the unresolved role into one
    // undifferentiated, comma-joined list — literally
    // `signal.to.join(", ")` verbatim, in request order. Deliberately NOT
    // pinning phrasing, stream, or ordering beyond this single check: the
    // two roles must not appear as plain comma-joined neighbors in the raw
    // request order, in EITHER order — that is the specific shape of
    // today's bug, not a proxy for "the fix must look a certain way".
    let naive_echo_forward = format!("{WORKER_ROLE}, {UNRESOLVED_ROLE}");
    let naive_echo_reverse = format!("{UNRESOLVED_ROLE}, {WORKER_ROLE}");
    assert!(
        !combined.contains(&naive_echo_forward) && !combined.contains(&naive_echo_reverse),
        "the armed role and the unresolved role must not be reported as a single \
         undifferentiated comma-joined list — that reads exactly like a delegate where both \
         roles genuinely armed, the same silent-drop failure `031`/`045` exist to stop, now in \
         the partial-resolution shape (fork #92); got stdout={:?} stderr={:?}",
        result.stdout,
        result.stderr
    );

    daemon.registry.shutdown_all();
}
