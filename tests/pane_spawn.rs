//! PRD #699 M3: `pane spawn <role>` — a CLI-triggerable daemon verb that
//! spawns a role that is declared in `.dot-agent-deck.toml` but was never
//! spawned into this running orchestration instance (e.g. the operator added
//! a role to the config mid-session; today only a full tab restart picks up
//! a new role).
//!
//! Rides the same unversioned hook-socket `DaemonMessage` channel
//! `Delegate`/`RestartRole` already use. Unlike M2's
//! `handle_restart_role_with_state` (a method called through an
//! already-held READ guard, deferring its one write-lock need into a
//! detached task), EVERY successful spawn here needs
//! `register_orchestration_role` on the main path — deferring it would let
//! the response claim `spawned: true` before `delegate` could actually
//! reach the role. So `handle_spawn_role_with_state` is a plain async free
//! function taking the `SharedState` handle directly and managing its own
//! short-lived lock acquisitions, not an `AppState` method. None of
//! `SpawnRoleSignal`/`SpawnRoleResponse`/`handle_spawn_role_with_state`
//! exist yet, so this file does not compile — that is the expected RED
//! state; the CLI subcommand and the `DaemonMessage::SpawnRole` variant come
//! with the coder delegation that makes it GREEN.

#![cfg(unix)]

use std::time::Duration;

use dot_agent_deck::agent_pty::{DOT_AGENT_DECK_PANE_ID, SpawnOptions, TabMembership};
use dot_agent_deck::event::{SpawnRoleResponse, SpawnRoleSignal};
use dot_agent_deck::state::{OrchestrationIdentity, handle_spawn_role_with_state};
use spec::spec;

mod common;

const ORCH_PANE: &str = "spawn-orchestrator";
const CODER_PANE: &str = "spawn-coder";
const CODER_ROLE: &str = "coder";
const REVIEWER_ROLE: &str = "reviewer";
const UNKNOWN_ROLE: &str = "no-such-role";
const ORCHESTRATION: &str = "spawn-orchestration";
const ORCHESTRATION_ID: &str = "spawn-instance-1";

fn config() -> String {
    format!(
        "[[orchestrations]]\nname = \"{ORCHESTRATION}\"\n\n\
         [[orchestrations.roles]]\nname = \"orchestrator\"\ncommand = \"cat\"\nstart = true\n\n\
         [[orchestrations.roles]]\nname = \"{CODER_ROLE}\"\ncommand = \"cat\"\n\n\
         [[orchestrations.roles]]\nname = \"{REVIEWER_ROLE}\"\ncommand = \"cat\"\n"
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
    coder_agent_id: String,
}

/// Spawn an orchestrator + a `coder` worker, register both roles. The
/// `.dot-agent-deck.toml` also declares a third role, `reviewer`, that is
/// deliberately never spawned or registered — the "configured but unspawned
/// role" M3 targets.
async fn fixture() -> Fixture {
    let daemon = common::spawn_inprocess_daemon().await;
    let dir = common::race_safe_tempdir();
    std::fs::write(dir.path().join(".dot-agent-deck.toml"), config())
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
    let coder_agent_id = daemon
        .registry
        .spawn_agent(SpawnOptions {
            command: Some("cat"),
            cwd: Some(&cwd),
            display_name: Some(CODER_ROLE),
            env: vec![(DOT_AGENT_DECK_PANE_ID.to_string(), CODER_PANE.to_string())],
            tab_membership: Some(membership(1, CODER_ROLE, false, &cwd)),
            ..SpawnOptions::default()
        })
        .expect("spawn coder stand-in");

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
        state.register_orchestration_role(CODER_PANE, CODER_ROLE, false, identity, Some(&cwd));
    }

    Fixture {
        daemon,
        _dir: dir,
        coder_agent_id,
    }
}

/// Call the not-yet-existing `pane spawn <role>` handler directly, the same
/// way `tests/pane_restart.rs`'s `restart_role()` helper calls
/// `handle_restart_role_with_state` — bypassing the CLI/socket layer, which
/// is mechanical wiring that belongs to the coder delegation, not this RED
/// test.
///
/// Unlike `restart_role()`, this does NOT go through a pre-held read guard:
/// `handle_spawn_role_with_state` is a free function that takes the
/// `SharedState` handle itself, because it must be free to take its own
/// short-lived write lock on the main path (see module docs).
async fn spawn_role(fx: &Fixture, caller_pane_id: &str, role: &str) -> SpawnRoleResponse {
    let signal = SpawnRoleSignal {
        pane_id: caller_pane_id.to_string(),
        role: role.to_string(),
        timestamp: chrono::Utc::now(),
    };
    handle_spawn_role_with_state(
        signal,
        &fx.daemon.state,
        &fx.daemon.registry,
        &fx.daemon.event_tx,
    )
    .await
}

/// Scenario: a role declared in `.dot-agent-deck.toml` (`reviewer`) was never
/// spawned into this orchestration instance. The orchestrator pane asks the
/// daemon to spawn it. The response must report success with no error, and
/// the role must actually be reachable afterward: `delegate_targets` from
/// the orchestrator resolved nothing for `reviewer` before the call, and
/// resolves exactly one pane for it after.
#[tokio::test(flavor = "multi_thread")]
#[spec("pane/spawn/001")]
async fn pane_spawn_001_spawns_a_configured_but_unspawned_role_and_it_becomes_reachable() {
    let fx = fixture().await;

    let before = fx
        .daemon
        .state
        .read()
        .await
        .delegate_targets(ORCH_PANE, &[REVIEWER_ROLE.to_string()]);
    assert!(
        before.is_empty(),
        "precondition: reviewer must not be reachable before it is spawned; targets = {before:?}"
    );

    let response = spawn_role(&fx, ORCH_PANE, REVIEWER_ROLE).await;
    assert!(
        response.spawned,
        "spawning a configured-but-unspawned role must succeed; response = {response:?}"
    );
    assert!(
        response.error.is_none(),
        "a successful spawn must carry no error; response = {response:?}"
    );

    let after = fx
        .daemon
        .state
        .read()
        .await
        .delegate_targets(ORCH_PANE, &[REVIEWER_ROLE.to_string()]);
    assert_eq!(
        after.len(),
        1,
        "after spawning, delegate_targets must resolve exactly one pane for reviewer; \
         targets = {after:?}"
    );
    assert_eq!(
        after[0].0, REVIEWER_ROLE,
        "the resolved target must be for the reviewer role; targets = {after:?}"
    );
    let reviewer_pane_id = after[0].1.clone();
    assert!(
        fx.daemon
            .registry
            .pane_current_agent_id(&reviewer_pane_id)
            .is_some(),
        "the pane delegate_targets names for reviewer must have a live agent; pane = {reviewer_pane_id}"
    );
}

/// Scenario: the caller asks to spawn `coder`, a role that is already live
/// in this orchestration instance. The handler must refuse rather than spawn
/// a second agent for a role that already has one: `spawned` is false, an
/// error names that the role is already running, and nothing about the
/// existing coder pane/agent changes.
#[tokio::test(flavor = "multi_thread")]
#[spec("pane/spawn/002")]
async fn pane_spawn_002_refuses_a_role_already_live_in_this_instance() {
    let fx = fixture().await;

    let records_before = fx.daemon.registry.agent_records().len();

    let response = spawn_role(&fx, ORCH_PANE, CODER_ROLE).await;
    assert!(
        !response.spawned,
        "spawning a role already live in this instance must be refused; response = {response:?}"
    );
    assert!(
        response.error.as_deref().is_some_and(
            |e| e.to_lowercase().contains("already") && e.to_lowercase().contains(CODER_ROLE)
        ),
        "the refusal must explain the role is already running; response = {response:?}"
    );

    let agent_id = fx
        .daemon
        .registry
        .pane_current_agent_id(CODER_PANE)
        .expect("coder's pane must still have its original agent");
    assert_eq!(
        agent_id, fx.coder_agent_id,
        "a refused spawn must not touch the already-live coder's agent"
    );
    assert_eq!(
        fx.daemon.registry.agent_records().len(),
        records_before,
        "a refused spawn must not create any new agent record"
    );
}

/// Scenario: the caller asks to spawn a role name that appears nowhere in
/// `.dot-agent-deck.toml`. The handler must refuse and name the unknown role
/// rather than silently doing nothing.
#[tokio::test(flavor = "multi_thread")]
#[spec("pane/spawn/003")]
async fn pane_spawn_003_refuses_a_role_not_in_config() {
    let fx = fixture().await;

    let response = spawn_role(&fx, ORCH_PANE, UNKNOWN_ROLE).await;
    assert!(
        !response.spawned,
        "spawning an unconfigured role must be refused; response = {response:?}"
    );
    assert!(
        response
            .error
            .as_deref()
            .is_some_and(|e| e.contains(UNKNOWN_ROLE)),
        "the refusal must name the unknown role; response = {response:?}"
    );
}

/// Scenario: the caller is the CODER's own pane, not the orchestrator.
/// Mirrors `pane_restart_005`'s anti-spoofing shape — only the orchestrator
/// pane of an orchestration may trigger a spawn within it.
#[tokio::test(flavor = "multi_thread")]
#[spec("pane/spawn/004")]
async fn pane_spawn_004_refuses_from_a_non_orchestrator_pane() {
    let fx = fixture().await;

    let response = spawn_role(&fx, CODER_PANE, REVIEWER_ROLE).await;
    assert!(
        !response.spawned,
        "a spawn requested by a non-orchestrator pane must be refused; response = {response:?}"
    );
    assert!(
        response.error.is_some(),
        "the refusal must carry an error explaining the caller is not this orchestration's \
         orchestrator; response = {response:?}"
    );

    let after = fx
        .daemon
        .state
        .read()
        .await
        .delegate_targets(ORCH_PANE, &[REVIEWER_ROLE.to_string()]);
    assert!(
        after.is_empty(),
        "a refused spawn must not make reviewer reachable; targets = {after:?}"
    );
}

/// Scenario: PRD #699 fix-round B1 (`findings-699-reviewer.md` "B1" —
/// corroborated by `findings-699-auditor.md`). `handle_spawn_role_with_state`'s
/// `SpawnOptions.env` overlay carries only `DOT_AGENT_DECK_PANE_ID`; it must
/// ALSO carry `DOT_AGENT_DECK_REGISTRATION_GENERATION` and
/// `DOT_AGENT_DECK_DAEMON_BOOT_ID` — the same pair `crate::spawn::pane_env`
/// injects on the batch-spawn path — because a real worker's `work-done`
/// reads exactly those two variables (`src/main.rs`'s
/// `read_registration_context`) to build the compound key
/// `handle_work_done`'s staleness gate checks. Rather than driving the whole
/// work-done round trip (which would need to hardcode a generation/boot-id
/// value the fix hasn't decided yet), spawn `reviewer` with a role command
/// that dumps its OWN environment to its PTY and assert the daemon's actual
/// child process really carries both variable names — the same observable
/// end-state a real `read_registration_context()` call inside that child
/// depends on. RED today: only `DOT_AGENT_DECK_PANE_ID=` appears in the
/// dump.
#[tokio::test(flavor = "multi_thread")]
#[spec("pane/spawn/006")]
async fn pane_spawn_006_injects_registration_generation_and_daemon_boot_id_into_spawned_env() {
    let daemon = common::spawn_inprocess_daemon().await;
    let dir = common::race_safe_tempdir();
    std::fs::write(
        dir.path().join(".dot-agent-deck.toml"),
        format!(
            "[[orchestrations]]\nname = \"{ORCHESTRATION}\"\n\n\
             [[orchestrations.roles]]\nname = \"orchestrator\"\ncommand = \"cat\"\nstart = true\n\n\
             [[orchestrations.roles]]\nname = \"{REVIEWER_ROLE}\"\ncommand = \"env; exec cat\"\n"
        ),
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
    {
        let mut state = daemon.state.write().await;
        state.register_orchestration_role(
            ORCH_PANE,
            "orchestrator",
            true,
            OrchestrationIdentity::Instance {
                id: ORCHESTRATION_ID.to_string(),
                name: ORCHESTRATION.to_string(),
            },
            Some(&cwd),
        );
    }

    let signal = SpawnRoleSignal {
        pane_id: ORCH_PANE.to_string(),
        role: REVIEWER_ROLE.to_string(),
        timestamp: chrono::Utc::now(),
    };
    let response =
        handle_spawn_role_with_state(signal, &daemon.state, &daemon.registry, &daemon.event_tx)
            .await;
    assert!(
        response.spawned,
        "precondition: spawning reviewer must succeed; response = {response:?}"
    );

    let targets = daemon
        .state
        .read()
        .await
        .delegate_targets(ORCH_PANE, &[REVIEWER_ROLE.to_string()]);
    assert_eq!(
        targets.len(),
        1,
        "precondition: reviewer must resolve to exactly one pane after spawning; \
         targets = {targets:?}"
    );
    let reviewer_pane_id = targets[0].1.clone();
    let agent_id = daemon
        .registry
        .pane_current_agent_id(&reviewer_pane_id)
        .expect("the newly-spawned reviewer pane must have a live agent");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let snapshot = loop {
        let snap =
            String::from_utf8_lossy(&daemon.registry.snapshot(&agent_id).unwrap_or_default())
                .into_owned();
        if snap.contains("DOT_AGENT_DECK_DAEMON_BOOT_ID") || tokio::time::Instant::now() >= deadline
        {
            break snap;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    assert!(
        snapshot.contains("DOT_AGENT_DECK_REGISTRATION_GENERATION="),
        "a real worker's `work-done` reads DOT_AGENT_DECK_REGISTRATION_GENERATION straight \
         from its own environment (src/main.rs's read_registration_context) — the spawned \
         reviewer's actual `env` dump must contain it, or every work-done it ever sends is \
         refused as stale (B1). snapshot = {snapshot:?}"
    );
    assert!(
        snapshot.contains("DOT_AGENT_DECK_DAEMON_BOOT_ID="),
        "same as above for DOT_AGENT_DECK_DAEMON_BOOT_ID — both halves of handle_work_done's \
         compound staleness key must be present in the child's real environment. \
         snapshot = {snapshot:?}"
    );
}

/// Scenario: PRD #699 fix-round B3 (`findings-699-reviewer.md` "B3" /
/// `findings-699-auditor.md` "F11"). `handle_spawn_role_with_state`'s
/// "already live" check reuses `delegate_targets`, which deliberately
/// EXCLUDES orchestrator panes (so restart can refuse `pane restart
/// <start-role>`) — reused here it means naming the orchestration's own
/// start role always resolves empty, so the "already running" refusal never
/// fires. The orchestrator pane asks to spawn its OWN role. The handler
/// must refuse explicitly rather than launching a second orchestrator-command
/// pane registered as a worker. RED today: the call succeeds.
#[tokio::test(flavor = "multi_thread")]
#[spec("pane/spawn/007")]
async fn pane_spawn_007_refuses_to_spawn_its_own_start_role() {
    let fx = fixture().await;
    let records_before = fx.daemon.registry.agent_records().len();

    let response = spawn_role(&fx, ORCH_PANE, "orchestrator").await;

    assert!(
        !response.spawned,
        "spawning the orchestration's own start role must be refused; response = {response:?}"
    );
    assert!(
        response
            .error
            .as_deref()
            .is_some_and(|e| e.contains("orchestrator")
                && (e.to_lowercase().contains("start") || e.to_lowercase().contains("own"))),
        "the refusal must explain the named role is the orchestration's own start role; \
         response = {response:?}"
    );
    assert_eq!(
        fx.daemon.registry.agent_records().len(),
        records_before,
        "a refused spawn of the start role must not create a second orchestrator-command agent"
    );
}

/// Scenario: PRD #699 fix-round M6 (`findings-699-reviewer.md` "M6") / F3
/// (`findings-699-auditor.md`). `handle_spawn_role_with_state` resolves its
/// "already live" check under a READ guard that is dropped BEFORE
/// `registry.spawn_agent`, then re-takes a fresh WRITE guard only to
/// register — nothing serializes two concurrent spawns of the SAME
/// not-yet-live role in that window. Fire two concurrent spawns of
/// `reviewer` at once and assert the weaker of the two invariants the fix
/// might produce (per the orchestrator's own task notes, either shape is an
/// acceptable fix): exactly ONE live pane ends up registered for `reviewer`
/// once both calls resolve — never two. RED today: both calls observe the
/// role as not-yet-live, both spawn, and `delegate_targets` resolves TWO
/// panes for one role afterward.
#[tokio::test(flavor = "multi_thread")]
#[spec("pane/spawn/008")]
async fn pane_spawn_008_concurrent_spawns_of_the_same_role_never_leave_two_live_panes() {
    let fx = fixture().await;

    let signal_a = SpawnRoleSignal {
        pane_id: ORCH_PANE.to_string(),
        role: REVIEWER_ROLE.to_string(),
        timestamp: chrono::Utc::now(),
    };
    let signal_b = signal_a.clone();

    // `tokio::spawn` onto the multi-thread runtime's own worker threads,
    // NOT `tokio::join!` on two unspawned futures polled on this one task —
    // `join!` only interleaves at `.await` points on a single task, and
    // `handle_spawn_role_with_state`'s "already live" check → `spawn_agent`
    // window is a plain synchronous span with no `.await` inside it, so a
    // same-task `join!` could never actually overlap the two calls there.
    // Real concurrent OS threads are what actually races the TOCTOU window.
    let state_a = fx.daemon.state.clone();
    let registry_a = fx.daemon.registry.clone();
    let event_tx_a = fx.daemon.event_tx.clone();
    let handle_a = tokio::spawn(async move {
        handle_spawn_role_with_state(signal_a, &state_a, &registry_a, &event_tx_a).await
    });
    let state_b = fx.daemon.state.clone();
    let registry_b = fx.daemon.registry.clone();
    let event_tx_b = fx.daemon.event_tx.clone();
    let handle_b = tokio::spawn(async move {
        handle_spawn_role_with_state(signal_b, &state_b, &registry_b, &event_tx_b).await
    });
    let (response_a, response_b) =
        tokio::try_join!(handle_a, handle_b).expect("neither concurrent spawn task should panic");

    let targets = fx
        .daemon
        .state
        .read()
        .await
        .delegate_targets(ORCH_PANE, &[REVIEWER_ROLE.to_string()]);
    assert_eq!(
        targets.len(),
        1,
        "two concurrent `pane spawn reviewer` calls must never leave more than one live pane \
         registered for the role — response_a = {response_a:?}, response_b = {response_b:?}, \
         targets = {targets:?}"
    );
}
