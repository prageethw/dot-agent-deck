//! PRD #699 M2: `pane restart <role>` — a CLI-triggerable daemon verb that
//! restarts a worker role's pane on demand, riding the same unversioned
//! hook-socket `DaemonMessage` channel `Delegate`/`Dispatch`/`GetSeed`
//! already use.
//!
//! M1 (merged) gave a naturally-exited worker's `AgentRecord` a
//! `crashed == Some(true)` marker, but the only recovery path was a human
//! restarting the pane from the TUI. This file pins the daemon-side contract
//! for the new verb — `RestartRoleSignal` in, `RestartRoleResponse` out,
//! handled by `handle_restart_role_with_state` — calling the handler
//! directly the same way `tests/delegate_respawn_recovery.rs`'s `delegate()`
//! helper calls `handle_delegate_with_state`, bypassing the CLI/socket layer
//! entirely. None of `RestartRoleSignal`/`RestartRoleResponse`/
//! `handle_restart_role_with_state` exist yet, so this file does not
//! compile — that is the expected RED state; the CLI subcommand and the
//! `DaemonMessage::RestartRole` variant come with the coder delegation that
//! makes it GREEN.

#![cfg(unix)]

use std::time::Duration;

use dot_agent_deck::agent_pty::{
    AgentPtyRegistry, DOT_AGENT_DECK_DAEMON_BOOT_ID, DOT_AGENT_DECK_PANE_ID,
    DOT_AGENT_DECK_REGISTRATION_GENERATION, SpawnOptions, TabMembership,
};
use dot_agent_deck::event::{RestartRoleResponse, RestartRoleSignal};
use dot_agent_deck::state::OrchestrationIdentity;
use spec::spec;

mod common;

const ORCH_PANE: &str = "restart-orchestrator";
const WORKER_PANE: &str = "restart-coder";
const WORKER_ROLE: &str = "coder";
const UNKNOWN_ROLE: &str = "no-such-role";
const ORCHESTRATION: &str = "restart-orchestration";
const ORCHESTRATION_ID: &str = "restart-instance-1";

fn config(worker_command: &str) -> String {
    format!(
        "[[orchestrations]]\nname = \"{ORCHESTRATION}\"\n\n\
         [[orchestrations.roles]]\nname = \"orchestrator\"\ncommand = \"cat\"\nstart = true\n\n\
         [[orchestrations.roles]]\nname = \"{WORKER_ROLE}\"\ncommand = \"{worker_command}\"\n"
    )
}

fn membership(role_index: usize, role_name: &str, is_start_role: bool, cwd: &str) -> TabMembership {
    TabMembership::Orchestration {
        name: ORCHESTRATION.to_string(),
        role_index,
        role_name: role_name.to_string(),
        is_start_role,
        orchestration_cwd: Some(cwd.to_string()),
        display_title: None,
        orchestration_id: Some(ORCHESTRATION_ID.to_string()),
    }
}

struct Fixture {
    daemon: common::InProcDaemon,
    _dir: tempfile::TempDir,
    worker_agent_id: String,
}

/// Spawn an orchestrator + a worker running `worker_command`, register both
/// roles. `worker_command` decides whether the worker stays healthy for the
/// life of the test (`"cat"`) or exits on its own shortly after boot to
/// simulate a crash (a short `sleep`), mirroring the M1 precedent test's own
/// technique of letting a stand-in exit naturally rather than sending it a
/// signal, so `pump_reader`'s EOF branch is what marks it `crashed`.
async fn fixture(worker_command: &str) -> Fixture {
    let daemon = common::spawn_inprocess_daemon().await;
    let dir = common::race_safe_tempdir();
    std::fs::write(
        dir.path().join(".dot-agent-deck.toml"),
        config(worker_command),
    )
    .expect("write orchestration config");
    let cwd = dir.path().to_string_lossy().into_owned();

    daemon
        .registry
        .spawn_agent(SpawnOptions {
            command: Some("cat"),
            cwd: Some(&cwd),
            display_name: Some("orchestrator"),
            env: vec![(DOT_AGENT_DECK_PANE_ID.to_string(), ORCH_PANE.to_string())],
            tab_membership: Some(membership(0, "orchestrator", true, &cwd)),
            ..SpawnOptions::default()
        })
        .expect("spawn orchestrator stand-in");
    let worker_agent_id = daemon
        .registry
        .spawn_agent(SpawnOptions {
            command: Some(worker_command),
            cwd: Some(&cwd),
            display_name: Some(WORKER_ROLE),
            env: vec![(DOT_AGENT_DECK_PANE_ID.to_string(), WORKER_PANE.to_string())],
            tab_membership: Some(membership(1, WORKER_ROLE, false, &cwd)),
            ..SpawnOptions::default()
        })
        .expect("spawn worker stand-in");

    {
        let mut state = daemon.state.write().await;
        let identity = OrchestrationIdentity::Instance {
            id: ORCHESTRATION_ID.to_string(),
            name: ORCHESTRATION.to_string(),
        };
        state.register_orchestration_role(
            ORCH_PANE,
            "orchestrator",
            true,
            identity.clone(),
            Some(&cwd),
        );
        state.register_orchestration_role(WORKER_PANE, WORKER_ROLE, false, identity, Some(&cwd));
    }

    Fixture {
        daemon,
        _dir: dir,
        worker_agent_id,
    }
}

/// Poll until `agent_id`'s registry record is `crashed == Some(true)`, the
/// same bounded-deadline idiom every other waiter in this harness uses.
async fn wait_for_crashed(registry: &AgentPtyRegistry, agent_id: &str, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if registry
            .agent_record_any(agent_id)
            .is_some_and(|r| r.crashed == Some(true))
        {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Call the not-yet-existing `pane restart <role>` handler directly, the same
/// way `delegate_respawn_recovery.rs`'s `delegate()` helper calls
/// `handle_delegate_with_state` — bypassing the CLI/socket layer, which is
/// mechanical wiring that belongs to the coder delegation, not this RED test.
async fn restart_role(
    fx: &Fixture,
    caller_pane_id: &str,
    role: &str,
    force: bool,
) -> RestartRoleResponse {
    let signal = RestartRoleSignal {
        pane_id: caller_pane_id.to_string(),
        role: role.to_string(),
        force,
        timestamp: chrono::Utc::now(),
    };
    dot_agent_deck::state::handle_restart_role_with_state(
        signal,
        &fx.daemon.state,
        &fx.daemon.registry,
        &fx.daemon.event_tx,
    )
    .await
}

/// Scenario: a worker stand-in exits on its own shortly after boot (M1 marks
/// its record `crashed == Some(true)`), then the orchestrator pane calls the
/// new restart handler with `force: false`. The role must come back: the
/// response reports success with no error, a fresh agent id now owns the
/// worker's pane, and the role registration survives — a subsequent
/// `delegate_targets` lookup from the orchestrator still resolves `coder` to
/// the same pane.
#[tokio::test(flavor = "multi_thread")]
#[spec("pane/restart/001")]
async fn pane_restart_001_restarts_a_crashed_worker_and_role_stays_reachable() {
    let fx = fixture("sleep 0.2").await;

    let crashed = wait_for_crashed(
        &fx.daemon.registry,
        &fx.worker_agent_id,
        Duration::from_secs(5),
    )
    .await;
    assert!(
        crashed,
        "precondition: the worker stand-in never got marked crashed; record = {:?}",
        fx.daemon.registry.agent_record_any(&fx.worker_agent_id)
    );

    let response = restart_role(&fx, ORCH_PANE, WORKER_ROLE, false).await;
    assert!(
        response.restarted,
        "restarting a crashed worker without force must succeed; response = {response:?}"
    );
    assert!(
        response.error.is_none(),
        "a successful restart must carry no error; response = {response:?}"
    );

    let new_agent_id = fx
        .daemon
        .registry
        .pane_current_agent_id(WORKER_PANE)
        .expect("the worker pane must have a live agent after restart");
    assert_ne!(
        new_agent_id, fx.worker_agent_id,
        "restart must replace the crashed agent with a freshly spawned one"
    );

    let state = fx.daemon.state.read().await;
    let targets = state.delegate_targets(ORCH_PANE, &[WORKER_ROLE.to_string()]);
    assert_eq!(
        targets,
        vec![(WORKER_ROLE.to_string(), WORKER_PANE.to_string())],
        "the role registration must survive the restart so the next delegate still resolves it"
    );
}

/// Scenario: the worker is healthy (never crashed) and the orchestrator calls
/// restart without `force`. The handler must refuse: `restarted` is false, an
/// error names that the pane is not crashed, and nothing was actually
/// killed — the worker's agent id is unchanged.
#[tokio::test(flavor = "multi_thread")]
#[spec("pane/restart/002")]
async fn pane_restart_002_refuses_a_healthy_pane_without_force() {
    let fx = fixture("cat").await;

    let response = restart_role(&fx, ORCH_PANE, WORKER_ROLE, false).await;
    assert!(
        !response.restarted,
        "restarting a healthy pane without force must be refused; response = {response:?}"
    );
    assert!(
        response
            .error
            .as_deref()
            .is_some_and(|e| e.to_lowercase().contains("crash")),
        "the refusal must explain the pane is not crashed; response = {response:?}"
    );

    let agent_id = fx
        .daemon
        .registry
        .pane_current_agent_id(WORKER_PANE)
        .expect("the worker pane must still have its original agent");
    assert_eq!(
        agent_id, fx.worker_agent_id,
        "a refused restart must not touch the healthy worker's agent"
    );
}

/// Scenario: the worker is healthy, but the orchestrator calls restart with
/// `force: true`. The handler must restart it anyway: success, and a fresh
/// agent id replaces the healthy one despite it never having crashed.
#[tokio::test(flavor = "multi_thread")]
#[spec("pane/restart/003")]
async fn pane_restart_003_force_restarts_a_healthy_pane() {
    let fx = fixture("cat").await;

    let response = restart_role(&fx, ORCH_PANE, WORKER_ROLE, true).await;
    assert!(
        response.restarted,
        "force must restart a healthy pane; response = {response:?}"
    );
    assert!(
        response.error.is_none(),
        "a successful forced restart must carry no error; response = {response:?}"
    );

    let new_agent_id = fx
        .daemon
        .registry
        .pane_current_agent_id(WORKER_PANE)
        .expect("the worker pane must have a live agent after a forced restart");
    assert_ne!(
        new_agent_id, fx.worker_agent_id,
        "force must replace the healthy agent with a freshly spawned one"
    );
}

/// Scenario: the orchestrator asks to restart a role name that does not
/// exist anywhere in this orchestration's registration. The handler must
/// refuse and name the unknown role rather than silently doing nothing.
#[tokio::test(flavor = "multi_thread")]
#[spec("pane/restart/004")]
async fn pane_restart_004_refuses_an_unknown_role() {
    let fx = fixture("cat").await;

    let response = restart_role(&fx, ORCH_PANE, UNKNOWN_ROLE, false).await;
    assert!(
        !response.restarted,
        "restarting an unregistered role must be refused; response = {response:?}"
    );
    assert!(
        response
            .error
            .as_deref()
            .is_some_and(|e| e.contains(UNKNOWN_ROLE)),
        "the refusal must name the unknown role; response = {response:?}"
    );
}

/// Scenario: the caller is the WORKER's own pane, not the orchestrator.
/// Mirrors `handle_delegate_with_state`'s anti-spoofing check — only the
/// orchestrator pane of an orchestration may trigger a restart within it.
#[tokio::test(flavor = "multi_thread")]
#[spec("pane/restart/005")]
async fn pane_restart_005_refuses_from_a_non_orchestrator_pane() {
    let fx = fixture("cat").await;

    let response = restart_role(&fx, WORKER_PANE, WORKER_ROLE, false).await;
    assert!(
        !response.restarted,
        "a restart requested by a non-orchestrator pane must be refused; response = {response:?}"
    );
    assert!(
        response.error.is_some(),
        "the refusal must carry an error explaining the caller is not this orchestration's \
         orchestrator; response = {response:?}"
    );

    let agent_id = fx
        .daemon
        .registry
        .pane_current_agent_id(WORKER_PANE)
        .expect("the worker pane must still have its original agent");
    assert_eq!(
        agent_id, fx.worker_agent_id,
        "a refused restart must not touch the worker's agent"
    );
}

/// Scenario: PRD #699 fix-round coverage gap (auditor `findings-699-auditor.md`
/// "F10") — NOT a live defect: the audit's own headline verdict is that
/// daemon-side isolation ALREADY HOLDS for `pane restart` (routing goes
/// through `delegate_targets`' `OrchestrationIdentity` equality, not a bare
/// `(cwd, name)` tuple — the B2/F1 bug is TUI-tab-side only, pinned in
/// `tests/orchestration_tab_growth.rs`). This test exists purely as a
/// regression guard for that already-correct behavior, so future changes to
/// the restart routing path can't silently reintroduce cross-instance
/// bleed. Two orchestration instances share the exact same orchestration
/// `name` and `cwd` — told apart only by their PRD #140 `Instance`
/// token — each running a role named `coder`. Instance A's orchestrator
/// force-restarts `coder`; only instance A's worker may be touched.
#[tokio::test(flavor = "multi_thread")]
#[spec("pane/restart/006")]
async fn pane_restart_006_two_same_name_cwd_instances_do_not_cross_restart() {
    let daemon = common::spawn_inprocess_daemon().await;
    let dir = common::race_safe_tempdir();
    std::fs::write(dir.path().join(".dot-agent-deck.toml"), config("cat"))
        .expect("write orchestration config");
    let cwd = dir.path().to_string_lossy().into_owned();

    const ORCH_PANE_A: &str = "restart-iso-orch-a";
    const WORKER_PANE_A: &str = "restart-iso-worker-a";
    const ORCH_PANE_B: &str = "restart-iso-orch-b";
    const WORKER_PANE_B: &str = "restart-iso-worker-b";

    let worker_agent_id_a = daemon
        .registry
        .spawn_agent(SpawnOptions {
            command: Some("cat"),
            cwd: Some(&cwd),
            display_name: Some(WORKER_ROLE),
            env: vec![(
                DOT_AGENT_DECK_PANE_ID.to_string(),
                WORKER_PANE_A.to_string(),
            )],
            tab_membership: Some(membership(1, WORKER_ROLE, false, &cwd)),
            ..SpawnOptions::default()
        })
        .expect("spawn instance A's worker stand-in");
    let worker_agent_id_b = daemon
        .registry
        .spawn_agent(SpawnOptions {
            command: Some("cat"),
            cwd: Some(&cwd),
            display_name: Some(WORKER_ROLE),
            env: vec![(
                DOT_AGENT_DECK_PANE_ID.to_string(),
                WORKER_PANE_B.to_string(),
            )],
            tab_membership: Some(membership(1, WORKER_ROLE, false, &cwd)),
            ..SpawnOptions::default()
        })
        .expect("spawn instance B's worker stand-in");
    daemon
        .registry
        .spawn_agent(SpawnOptions {
            command: Some("cat"),
            cwd: Some(&cwd),
            display_name: Some("orchestrator"),
            env: vec![(DOT_AGENT_DECK_PANE_ID.to_string(), ORCH_PANE_A.to_string())],
            tab_membership: Some(membership(0, "orchestrator", true, &cwd)),
            ..SpawnOptions::default()
        })
        .expect("spawn instance A's orchestrator stand-in");
    daemon
        .registry
        .spawn_agent(SpawnOptions {
            command: Some("cat"),
            cwd: Some(&cwd),
            display_name: Some("orchestrator"),
            env: vec![(DOT_AGENT_DECK_PANE_ID.to_string(), ORCH_PANE_B.to_string())],
            tab_membership: Some(membership(0, "orchestrator", true, &cwd)),
            ..SpawnOptions::default()
        })
        .expect("spawn instance B's orchestrator stand-in");

    {
        let mut state = daemon.state.write().await;
        let identity_a = OrchestrationIdentity::Instance {
            id: "restart-iso-instance-a".to_string(),
            name: ORCHESTRATION.to_string(),
        };
        let identity_b = OrchestrationIdentity::Instance {
            id: "restart-iso-instance-b".to_string(),
            name: ORCHESTRATION.to_string(),
        };
        state.register_orchestration_role(
            ORCH_PANE_A,
            "orchestrator",
            true,
            identity_a.clone(),
            Some(&cwd),
        );
        state.register_orchestration_role(
            WORKER_PANE_A,
            WORKER_ROLE,
            false,
            identity_a,
            Some(&cwd),
        );
        state.register_orchestration_role(
            ORCH_PANE_B,
            "orchestrator",
            true,
            identity_b.clone(),
            Some(&cwd),
        );
        state.register_orchestration_role(
            WORKER_PANE_B,
            WORKER_ROLE,
            false,
            identity_b,
            Some(&cwd),
        );
    }

    let signal = RestartRoleSignal {
        pane_id: ORCH_PANE_A.to_string(),
        role: WORKER_ROLE.to_string(),
        force: true,
        timestamp: chrono::Utc::now(),
    };
    let response = dot_agent_deck::state::handle_restart_role_with_state(
        signal,
        &daemon.state,
        &daemon.registry,
        &daemon.event_tx,
    )
    .await;
    assert!(
        response.restarted,
        "instance A's force restart of its own `coder` must succeed; response = {response:?}"
    );

    let agent_id_b_after = daemon
        .registry
        .pane_current_agent_id(WORKER_PANE_B)
        .expect("instance B's worker pane must still have a live agent");
    assert_eq!(
        agent_id_b_after, worker_agent_id_b,
        "instance A's restart of its own same-named `coder` must NEVER touch instance B's \
         same-name/same-cwd worker pane; response = {response:?}"
    );

    let agent_id_a_after = daemon
        .registry
        .pane_current_agent_id(WORKER_PANE_A)
        .expect("instance A's worker pane must have a live agent after restart");
    assert_ne!(
        agent_id_a_after, worker_agent_id_a,
        "sanity: instance A's OWN worker must actually have been restarted (a no-op restart \
         would make the isolation assertion above meaningless)"
    );
}

/// Issue #706: an env-dumping worker stand-in — logs the vars that decide
/// whether a recreated worker's `work-done` can pass the daemon's
/// generation/boot-id staleness gate, then behaves like `cat`.
fn write_env_dump_worker(path: &std::path::Path, log: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let script = format!(
        "#!/bin/sh\n\
         {{\n\
         echo \"gen=$DOT_AGENT_DECK_REGISTRATION_GENERATION\"\n\
         echo \"boot=$DOT_AGENT_DECK_DAEMON_BOOT_ID\"\n\
         echo \"pane=$DOT_AGENT_DECK_PANE_ID\"\n\
         echo \"---\"\n\
         }} >> \"{log}\"\n\
         exec cat\n",
        log = log.display()
    );
    std::fs::write(path, script).expect("write env-dump worker stand-in");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod env-dump worker stand-in");
}

/// The env-dump worker's per-invocation blocks (mirrors
/// `delegate_respawn_recovery.rs`'s `recorded_launches`).
fn recorded_env_blocks(log: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .split("---\n")
        .map(str::trim)
        .filter(|block| !block.is_empty())
        .map(str::to_string)
        .collect()
}

/// Scenario: issue #706 — the worker pane's registry record is closed out
/// from under it (the same deterministic technique `agent_detection.rs`'s
/// `spawn_010_declared_identity_wins_spawn_recreate_and_learning` fixture
/// uses to force `recreated: true`, rather than racing an in-flight close),
/// then the orchestrator force-restarts the role via
/// `handle_restart_role_with_state`'s recreate branch. The recreated
/// worker's actual environment must carry the SAME registration generation
/// and daemon boot id that `pane_registration_generation` ends up holding
/// for its pane, not just `DOT_AGENT_DECK_PANE_ID`.
#[tokio::test(flavor = "multi_thread")]
#[spec("pane/restart/007")]
async fn pane_restart_007_recreate_leg_injects_matching_registration_generation() {
    let script_dir = common::race_safe_tempdir();
    let script_path = script_dir.path().join("env-dump-worker.sh");
    let log_path = script_dir.path().join("env-dump.log");
    write_env_dump_worker(&script_path, &log_path);

    let fx = fixture(&script_path.to_string_lossy()).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while recorded_env_blocks(&log_path).is_empty() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the worker stand-in never recorded its initial launch"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    fx.daemon
        .registry
        .close_agent(&fx.worker_agent_id)
        .expect("remove the worker's registry record to force the recreate leg (issue #606)");

    let response = restart_role(&fx, ORCH_PANE, WORKER_ROLE, true).await;
    assert!(
        response.restarted,
        "force-restarting a pane whose registry record is gone must still recreate it; \
         response = {response:?}"
    );

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while recorded_env_blocks(&log_path).len() < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the recreated worker never recorded a second launch; blocks = {:?}",
            recorded_env_blocks(&log_path)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let blocks = recorded_env_blocks(&log_path);
    let recreated_block = blocks.last().expect("a second launch block exists");
    let field = |name: &str| -> String {
        recreated_block
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{name}=")))
            .unwrap_or_default()
            .to_string()
    };
    let injected_gen = field("gen");
    let injected_boot = field("boot");

    // `handle_restart_role_with_state`'s `if recreated { ... }` re-registration
    // runs in a DETACHED `tokio::spawn` task, so give it a moment to land
    // before reading the map it writes.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !fx
        .daemon
        .state
        .read()
        .await
        .pane_registration_generation
        .contains_key(WORKER_PANE)
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let state = fx.daemon.state.read().await;
    let map_generation = state.pane_registration_generation.get(WORKER_PANE).copied();
    let daemon_boot_id = state.daemon_boot_id().to_string();
    drop(state);

    assert!(
        !injected_gen.is_empty() && injected_gen != "0",
        "the recreated worker's env carried no DOT_AGENT_DECK_REGISTRATION_GENERATION \
         (issue #706); block = {recreated_block:?}"
    );
    assert_eq!(
        injected_gen.parse::<u64>().ok(),
        map_generation,
        "the generation injected into the recreated worker's env must match what \
         `pane_registration_generation` ends up holding for its pane, or the worker's own \
         `work-done` fails the daemon's staleness gate (issue #706); block = \
         {recreated_block:?}, map = {map_generation:?}"
    );
    assert_eq!(
        injected_boot, daemon_boot_id,
        "the recreated worker's env must carry this daemon's own boot id (issue #706); block = \
         {recreated_block:?}"
    );
}

/// Scenario: issue #706 fix-round (reviewer B1 / auditor A1) — the ORDINARY
/// (non-recreate) respawn leg of `handle_restart_role_with_state`, exercised
/// with `--force` against a HEALTHY worker pane whose registry record is left
/// INTACT — the opposite of `pane_restart_007`, which forces `recreated:
/// true` by closing the record first. A record present means
/// `respawn_or_recreate_agent_for_pane` never falls through to the recreate
/// branch, so simply not closing the agent (as this test does, mirroring
/// `pane_restart_003`'s healthy-pane `--force` technique) selects this leg
/// deterministically. This handler reserves a fresh registration generation
/// unconditionally before EVERY respawn attempt — and
/// `reserve_registration_generation` writes that value into
/// `pane_registration_generation` immediately, not only once confirmed — but
/// the respawn itself replays the previous child's `spawn_env` verbatim and
/// never consumes the freshly reserved generation, and the detached
/// `confirm_orchestration_role` task only runs `if recreated`. So the map
/// advances while the live worker's env does not, desynchronizing
/// `pane_registration_generation` from what that worker's own `work-done`
/// will report — the same failure #706 fixed, relocated onto `pane restart
/// --force`'s ordinary (non-recovery) case.
#[tokio::test(flavor = "multi_thread")]
#[spec("pane/restart/008")]
async fn pane_restart_008_ordinary_respawn_leg_keeps_registration_generation_in_sync() {
    let daemon = common::spawn_inprocess_daemon().await;
    let initial_boot_id = daemon.state.read().await.daemon_boot_id().to_string();

    let script_dir = common::race_safe_tempdir();
    let script_path = script_dir.path().join("env-dump-worker.sh");
    let log_path = script_dir.path().join("env-dump.log");
    write_env_dump_worker(&script_path, &log_path);
    let worker_command = script_path.to_string_lossy().into_owned();

    let dir = common::race_safe_tempdir();
    std::fs::write(
        dir.path().join(".dot-agent-deck.toml"),
        config(&worker_command),
    )
    .expect("write orchestration config");
    let cwd = dir.path().to_string_lossy().into_owned();

    daemon
        .registry
        .spawn_agent(SpawnOptions {
            command: Some("cat"),
            cwd: Some(&cwd),
            display_name: Some("orchestrator"),
            env: vec![(DOT_AGENT_DECK_PANE_ID.to_string(), ORCH_PANE.to_string())],
            tab_membership: Some(membership(0, "orchestrator", true, &cwd)),
            ..SpawnOptions::default()
        })
        .expect("spawn orchestrator stand-in");

    // The initial worker's env already carries a registration generation and
    // this daemon's boot id, the same as a REAL production spawn would
    // (`crate::spawn::spawn`'s `pane_env`, `src/spawn.rs`), which this
    // in-process fixture bypasses by calling `registry.spawn_agent` directly.
    // `"1"` is not arbitrary: it is the exact value
    // `reserve_registration_generation` computes for a pane_id it has never
    // seen before (`.or_insert(0) += 1`), which is what
    // `register_orchestration_role` below performs for `WORKER_PANE`. So the
    // env and the map start out GENUINELY in sync — the precondition an
    // ordinary respawn is supposed to preserve.
    let worker_agent_id = daemon
        .registry
        .spawn_agent(SpawnOptions {
            command: Some(&worker_command),
            cwd: Some(&cwd),
            display_name: Some(WORKER_ROLE),
            env: vec![
                (DOT_AGENT_DECK_PANE_ID.to_string(), WORKER_PANE.to_string()),
                (
                    DOT_AGENT_DECK_REGISTRATION_GENERATION.to_string(),
                    "1".to_string(),
                ),
                (DOT_AGENT_DECK_DAEMON_BOOT_ID.to_string(), initial_boot_id),
            ],
            tab_membership: Some(membership(1, WORKER_ROLE, false, &cwd)),
            ..SpawnOptions::default()
        })
        .expect("spawn worker stand-in");

    {
        let mut state = daemon.state.write().await;
        let identity = OrchestrationIdentity::Instance {
            id: ORCHESTRATION_ID.to_string(),
            name: ORCHESTRATION.to_string(),
        };
        state.register_orchestration_role(
            ORCH_PANE,
            "orchestrator",
            true,
            identity.clone(),
            Some(&cwd),
        );
        state.register_orchestration_role(WORKER_PANE, WORKER_ROLE, false, identity, Some(&cwd));
    }

    let fx = Fixture {
        daemon,
        _dir: dir,
        worker_agent_id,
    };

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while recorded_env_blocks(&log_path).is_empty() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the worker stand-in never recorded its initial launch"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        fx.daemon
            .registry
            .pane_current_agent_id(WORKER_PANE)
            .as_deref(),
        Some(fx.worker_agent_id.as_str()),
        "precondition: the worker's registry record must stay INTACT through this test — never \
         closed — which is what selects the ORDINARY respawn leg rather than pane_restart_007's \
         forced recreate leg"
    );

    let response = restart_role(&fx, ORCH_PANE, WORKER_ROLE, true).await;
    assert!(
        response.restarted,
        "force-restarting a healthy pane with an intact registry record must succeed; \
         response = {response:?}"
    );
    assert!(
        response.error.is_none(),
        "a successful restart must carry no error; response = {response:?}"
    );

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while recorded_env_blocks(&log_path).len() < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "force-restarting a pane with an intact registry record never produced an ordinary \
             respawn; blocks = {:?}",
            recorded_env_blocks(&log_path)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let blocks = recorded_env_blocks(&log_path);
    let respawned_block = blocks.last().expect("a second launch block exists");
    let field = |name: &str| -> String {
        respawned_block
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{name}=")))
            .unwrap_or_default()
            .to_string()
    };
    let injected_gen = field("gen");

    // Unlike `pane_restart_007`, the confirmation this handler detaches only
    // runs `if recreated`, which this is not — so there is no detached task
    // to wait for; the map is already whatever it is going to be by the time
    // `restart_role` above returns (the reservation itself is synchronous,
    // before the spawn).
    let state = fx.daemon.state.read().await;
    let map_generation = state.pane_registration_generation.get(WORKER_PANE).copied();
    drop(state);

    assert_eq!(
        map_generation,
        injected_gen.parse::<u64>().ok(),
        "on the ORDINARY (non-recreate) respawn leg, `pane_registration_generation` must still \
         equal whatever generation the respawned child's actual (verbatim-replayed) env carries \
         — the eager, unconditional reservation this handler performs before EVERY respawn \
         attempt bumps the map regardless of which leg runs, but only the recreate leg's \
         `confirm_orchestration_role` call keeps it in sync (issue #706 fix-round review B1 / \
         audit A1); block = {respawned_block:?}, map = {map_generation:?}"
    );
}
