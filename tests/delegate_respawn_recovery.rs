//! What a `clear = true` delegate does when its replacement worker cannot be
//! produced, or dies before it is ready — issues #584 and #606.
//!
//! Both issues land on the same seam: `dispatch_one_owned` destroys the current
//! worker (`respawn_agent_for_pane`) BEFORE it has anywhere to deliver the task
//! pointer, and then treats "no live worker at delivery time" as a terminal,
//! silent drop. The orchestrator's `delegate` has already exited 0, so it waits
//! for a `work-done` that can never arrive and nothing anywhere says why.
//!
//! * **#606** — a `StopAgent` for the worker pane is in flight, so the pane has
//!   no registry entry when the respawn looks for one. It failed with `NotFound`
//!   and the role was gone for the rest of the session.
//! * **#584** — the respawn succeeded but the replacement child died before it
//!   ever announced itself, so the identity gate refused the pointer with
//!   `NoLiveTarget` after the full readiness wait, logging one `warn!` and
//!   nothing else.
//!
//! The third test here is #584's CONTROL rather than a defect: it drives one
//! `clear = true` respawn through the daemon's dispatch spawn primitive
//! (`crate::spawn::spawn`) and another through the TUI's `StartAgent` shape, and
//! compares what the two replacements were actually launched with. #584's
//! leading hypothesis was that those two paths preserve different relaunch
//! parameters; the issue asked for that to be reproduced before it was believed,
//! and this is what does the asking.
//!
//! No LLM: the workers are `cat` / small shell stand-ins, because what is under
//! test is a daemon-side lifecycle race, a delivery decision, and a comparison
//! of launch parameters — not an agent's behaviour. The real-agent half of #584
//! lives on the dispatch path's `orchestration/dispatch/002`, whose `coder` role
//! is `clear = true` precisely so a REAL worker drives this same respawn.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use dot_agent_deck::agent_pty::{
    AgentPtyRegistry, DOT_AGENT_DECK_DAEMON_BOOT_ID, DOT_AGENT_DECK_PANE_ID,
    DOT_AGENT_DECK_REGISTRATION_GENERATION, GuardedSend, SpawnOptions, TabMembership,
};
use dot_agent_deck::event::{AgentEvent, AgentType, DelegateSignal, EventType, WorkDoneSignal};
use dot_agent_deck::state::OrchestrationIdentity;
use spec::spec;

mod common;

const ORCH_PANE: &str = "recovery-orchestrator";
const WORKER_PANE: &str = "recovery-coder";
const WORKER_ROLE: &str = "coder";
const ORCHESTRATION: &str = "recovery-orchestration";
const ORCHESTRATION_ID: &str = "recovery-instance-1";
/// Issue #960: the tab's run-identifying title, stamped on every role pane by
/// both producers (`tab.rs` for `Ctrl+n`, `spawn.rs` for a dispatch). Present
/// here rather than `None` because a `clear = true` delegate that has to
/// RE-CREATE its worker pane rebuilds that pane's membership from scratch, and
/// used to rebuild it with `display_title: None` — so the tab kept its label
/// only while some OTHER title-carrying pane was still live, and lost it
/// silently once every pane had exited or been re-created this way. Distinct
/// from `ORCHESTRATION` on purpose: a fallback to the canonical name would read
/// as a pass if the two were equal.
const DISPLAY_TITLE: &str = "recovery-orchestration · issue-960";

/// Issue #709: what the SIGTERM-ignoring stand-in prints once — and only once —
/// its `trap '' TERM` is installed, so `delegate/022` can wait for the state its
/// scenario depends on instead of guessing at how long a `sh` takes to boot.
const STUBBORN_WORKER_ARMED: &[u8] = b"STUBBORN-WORKER-ARMED";

fn config(worker_command: &str) -> String {
    format!(
        "[[orchestrations]]\nname = \"{ORCHESTRATION}\"\n\n\
         [[orchestrations.roles]]\nname = \"orchestrator\"\ncommand = \"cat\"\nstart = true\n\n\
         [[orchestrations.roles]]\nname = \"{WORKER_ROLE}\"\ncommand = \"{worker_command}\"\nclear = true\n"
    )
}

fn membership(role_index: usize, role_name: &str, is_start_role: bool, cwd: &str) -> TabMembership {
    TabMembership::Orchestration {
        name: ORCHESTRATION.to_string(),
        role_index,
        role_name: role_name.to_string(),
        is_start_role,
        orchestration_cwd: Some(cwd.to_string()),
        display_title: Some(DISPLAY_TITLE.to_string()),
        orchestration_id: Some(ORCHESTRATION_ID.to_string()),
    }
}

fn snapshot_contains(snapshot: &[u8], needle: &[u8]) -> bool {
    snapshot.windows(needle.len()).any(|w| w == needle)
}

async fn wait_for_pane_needle(
    registry: &AgentPtyRegistry,
    pane_id: &str,
    needle: &[u8],
    timeout: Duration,
) -> Vec<u8> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let snapshot = registry
            .pane_current_agent_id(pane_id)
            .and_then(|id| registry.snapshot(&id).ok())
            .unwrap_or_default();
        if snapshot_contains(&snapshot, needle) || tokio::time::Instant::now() >= deadline {
            return snapshot;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Issue #709: wait until `pane_id`'s close has OBSERVABLY entered its grace
/// window, so a delegate aimed at that window lands inside it rather than at a
/// guessed offset from when the close was asked for.
///
/// Bounded by [`common::child_boot_budget`] for the same reason the boot waits
/// are: the quantity being waited on is a freshly scheduled task getting its
/// turn, so the ceiling has to follow how contended the machine is. It returns
/// the instant the window opens, so an idle box pays nothing for the headroom.
async fn wait_for_close_in_flight<T>(
    registry: &AgentPtyRegistry,
    pane_id: &str,
    request: &tokio::task::JoinHandle<T>,
) -> bool {
    let deadline = tokio::time::Instant::now() + common::child_boot_budget();
    loop {
        if registry.pane_close_in_flight(pane_id) {
            return true;
        }
        // The `StopAgent` request returns only once the close has run to
        // completion (measured: `is_finished` flips in the same 100 ms tick that
        // `pane_close_in_flight` goes back to false), so a finished request with
        // no window ever observed means there is nothing left to wait for — the
        // request failed, or the pane was never closed at all. Ending here turns
        // that into a prompt, legible failure instead of one that spends the
        // whole budget and then reports the wrong cause.
        if request.is_finished() {
            return false;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// The pane's live agent id, once it is one this test has not seen before.
async fn wait_for_replacement_agent(
    registry: &AgentPtyRegistry,
    pane_id: &str,
    old_agent_id: &str,
    timeout: Duration,
) -> Option<String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(id) = registry.pane_current_agent_id(pane_id)
            && id != old_agent_id
        {
            return Some(id);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn session_start(pane_id: &str, agent_id: &str) -> String {
    let event = AgentEvent {
        session_id: format!("session-{agent_id}"),
        agent_type: AgentType::None,
        event_type: EventType::SessionStart,
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
    };
    serde_json::to_string(&event).expect("serialize synthetic SessionStart")
}

struct Fixture {
    daemon: common::InProcDaemon,
    _dir: tempfile::TempDir,
    cwd: String,
    orchestrator_agent_id: String,
    worker_agent_id: String,
}

async fn fixture(worker_command_in_dir: impl FnOnce(&std::path::Path) -> String) -> Fixture {
    let daemon = common::spawn_inprocess_daemon().await;
    let dir = common::race_safe_tempdir();
    let worker_command = worker_command_in_dir(dir.path());
    std::fs::write(
        dir.path().join(".dot-agent-deck.toml"),
        config(&worker_command),
    )
    .expect("write orchestration config");
    let cwd = dir.path().to_string_lossy().into_owned();

    let orchestrator_agent_id = daemon
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
            command: Some(&worker_command),
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
        cwd,
        orchestrator_agent_id,
        worker_agent_id,
    }
}

async fn delegate(fx: &Fixture, task: &str) {
    // Issue #567 (mirrors fork #358 M4's reasoning exactly): ORCH_PANE is
    // registered exactly once per fixture via `register_orchestration_role`,
    // which reserves generation `1` for a pane_id it has never seen before —
    // read the daemon's own `daemon_boot_id` back rather than hand-rolling it.
    let daemon_boot_id = fx.daemon.state.read().await.daemon_boot_id().to_string();
    let signal = DelegateSignal {
        pane_id: ORCH_PANE.to_string(),
        task: task.to_string(),
        to: vec![WORKER_ROLE.to_string()],
        timestamp: chrono::Utc::now(),
        generation: 1,
        daemon_boot_id,
        subject: None,
    };
    fx.daemon
        .state
        .read()
        .await
        .handle_delegate_with_state(
            signal,
            &fx.daemon.registry,
            &fx.daemon.event_tx,
            Some(&fx.daemon.state),
        )
        .await;
}

/// Scenario: start an orchestration whose `coder` role is `clear = true`, wait
/// for its SIGTERM-ignoring stand-in to print the marker that proves its
/// `trap '' TERM` is installed, close the worker's pane through the daemon's
/// real `StopAgent` path, and — as soon as that close is observably in flight,
/// still spending its termination grace — delegate to `coder`. The worker role must come back:
/// a live agent on its pane that physically receives the task pointer, and a
/// role registration that still routes the NEXT delegate.
#[tokio::test(flavor = "multi_thread")]
#[spec("orchestration/delegate/022")]
async fn delegate_022_delegate_during_an_in_flight_close_brings_the_role_back() {
    use std::os::unix::fs::PermissionsExt;

    // The stand-in IGNORES SIGTERM, so `close_agent` spends its full
    // `AGENT_TERMINATE_GRACE` before the child is reaped — which is the window
    // #606 is about. A plain `cat` dies on the first signal and the whole close
    // is over in well under the 200 ms the reporter measured, so it cannot
    // reproduce the race at all. `exec` keeps the ignore disposition (it is
    // inherited across `execve`) while still giving the pane something that
    // echoes what the daemon writes into it.
    let fx = fixture(|dir| {
        let script = dir.join("stubborn-worker.sh");
        // Issue #709: the marker is printed AFTER the trap and BEFORE the exec,
        // so seeing it is proof the disposition is already `SIG_IGN` — the one
        // fact this scenario cannot proceed without. `exec` carries it across
        // `execve`, so it still holds for the `cat` that replaces the shell.
        let marker = String::from_utf8_lossy(STUBBORN_WORKER_ARMED).into_owned();
        std::fs::write(
            &script,
            format!("#!/bin/sh\ntrap '' TERM\nprintf '{marker}'\nexec cat\n"),
        )
        .expect("write SIGTERM-ignoring worker stand-in");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("chmod SIGTERM-ignoring worker stand-in");
        script.to_string_lossy().into_owned()
    })
    .await;
    let client = dot_agent_deck::daemon_client::DaemonClient::new(fx.daemon.attach_path.clone());

    // Issue #709: this was a flat 400 ms sleep, and it was the load-sensitive
    // seam of the whole test. What the scenario needs is not "400 ms have
    // passed" but "the stand-in has installed its `trap '' TERM`" — and on a
    // loaded box a freshly forked `sh` has not necessarily run its first line
    // inside 400 ms. When it has not, the `StopAgent` below kills it on the
    // first signal, the close is over in milliseconds, and the test fails at the
    // in-flight precondition further down: a starvation failure wearing the
    // costume of a #606 regression. Waiting for the marker asserts the fact
    // directly, and cannot pass before it is true.
    let armed = common::wait_for_child_first_output(
        &fx.daemon.registry,
        &fx.worker_agent_id,
        STUBBORN_WORKER_ARMED,
    )
    .await;
    assert!(
        snapshot_contains(&armed, STUBBORN_WORKER_ARMED),
        "precondition: the worker stand-in never got as far as installing its `trap '' TERM`, so \
         the close below would be an ordinary fast termination rather than the grace-period \
         window #606 is about; snapshot = {:?}",
        String::from_utf8_lossy(&armed)
    );
    assert_eq!(
        fx.daemon
            .registry
            .pane_current_agent_id(WORKER_PANE)
            .as_deref(),
        Some(fx.worker_agent_id.as_str()),
        "precondition: the worker stand-in must still own its pane before the close"
    );
    // Issue #709: the assertion above says "still alive" but only ever checked
    // REGISTRATION, and the difference is the whole scenario. `close_agent`
    // spends `AGENT_TERMINATE_GRACE` only while the child is still running —
    // against an already-dead one it returns at once, the close transition opens
    // and shuts inside a few milliseconds, and the grace window this test needs
    // to deliver into never observably exists. That reads downstream as "the
    // close was not in flight", which is true and useless. Check the fact the
    // sentence always meant.
    assert!(
        fx.daemon.registry.agent_is_live(&fx.worker_agent_id),
        "precondition: the worker stand-in is registered but no longer running, so the close \
         below would finish instantly instead of spending its termination grace"
    );

    let closing_id = fx.worker_agent_id.clone();
    let closing = tokio::spawn(async move { client.stop_agent(&closing_id).await });

    // Issue #709: this was a flat `sleep(200 ms)` — the reporter's own interval
    // — followed by the assertion below, and it was the SECOND fixed deadline in
    // this test. The 200 ms was standing in for "the close has entered its grace
    // window", but `stop_agent` is driven by a spawned task and a socket round
    // trip, so on a loaded box neither had necessarily reached
    // `begin_pane_close` yet when the sleep expired. The assertion then fired
    // saying the close was not in flight — and it was right, for the opposite of
    // the reason it names: not "the close already finished" but "the close had
    // not started". Measured on this branch at load 78 on 16 cores, in a
    // full-tier run whose whole failing case took 0.98 s, so nothing had
    // overshot anything.
    //
    // `pane_close_in_flight` is exactly the state the 200 ms was approximating —
    // it is true from the moment the cleanup hold and the closing mark go up,
    // which is also the moment the pane's registry entry is gone and its role is
    // still registered: the window a `clear = true` respawn used to fail
    // `NotFound` in. Waiting for it puts the delegate inside that window by
    // construction rather than by arithmetic, and it cannot report a window that
    // has not opened as one that has closed.
    let entered_grace = wait_for_close_in_flight(&fx.daemon.registry, WORKER_PANE, &closing).await;
    assert!(
        entered_grace,
        "precondition: the close never entered its grace window, so the delegate below would be \
         an ordinary post-close delegate instead of #606's race; stop_agent finished = {}, \
         stand-in still live = {}, records = {:?}",
        closing.is_finished(),
        fx.daemon.registry.agent_is_live(&fx.worker_agent_id),
        fx.daemon.registry.agent_records()
    );
    delegate(&fx, "list the files in this directory").await;

    let replacement = wait_for_replacement_agent(
        &fx.daemon.registry,
        WORKER_PANE,
        &fx.worker_agent_id,
        Duration::from_secs(20),
    )
    .await
    .unwrap_or_else(|| {
        panic!(
            "delegating to a `clear = true` role while its pane was mid-close left the role with \
             no live agent at all — the pane is dead for the rest of the session (#606). \
             records = {:?}",
            fx.daemon.registry.agent_records()
        )
    });

    // A `cat` stand-in emits no readiness signal of its own, so stand in for the
    // agent's hook exactly as the rest of the fast delegate suite does.
    common::write_hook_line(
        &fx.daemon.hook_path,
        &session_start(WORKER_PANE, &replacement),
    )
    .expect("deliver synthetic SessionStart for the replacement worker");

    let pointer =
        common::expected_delegate_pointer(std::path::Path::new(&fx.cwd), WORKER_ROLE, WORKER_PANE);
    let snapshot = wait_for_pane_needle(
        &fx.daemon.registry,
        WORKER_PANE,
        &pointer,
        Duration::from_secs(20),
    )
    .await;
    assert!(
        snapshot_contains(&snapshot, &pointer),
        "the recovered worker never received the task pointer; snapshot = {:?}",
        String::from_utf8_lossy(&snapshot)
    );

    let _ = closing.await;

    // Issue #960's secondary path: the re-created pane's membership. There is no
    // record left to respawn from here, so the replacement is built from
    // `PaneRecreateIdentity` — which hardcoded `display_title: None`, silently
    // dropping the tab's run-identifying label from this pane. It is not
    // immediately visible, because `partition_hydrated_panes` keeps the first
    // non-`None` title it finds and the orchestrator pane still has one; the
    // label is lost once every title-carrying pane has exited or been re-created
    // this way. Asserted on the RECREATED pane specifically, since that is the
    // only one whose membership this path authors.
    let recreated_membership = fx
        .daemon
        .registry
        .agent_records()
        .into_iter()
        .find(|r| r.pane_id_env.as_deref() == Some(WORKER_PANE))
        .and_then(|r| r.tab_membership)
        .unwrap_or_else(|| {
            panic!(
                "the recovered worker pane must have an Orchestration membership; records = {:?}",
                fx.daemon.registry.agent_records()
            )
        });
    let TabMembership::Orchestration {
        display_title,
        role_index,
        orchestration_id,
        ..
    } = &recreated_membership
    else {
        panic!("the recovered worker pane left its orchestration tab: {recreated_membership:?}");
    };
    assert_eq!(
        (
            display_title.as_deref(),
            *role_index,
            orchestration_id.as_deref()
        ),
        (Some(DISPLAY_TITLE), 1, Some(ORCHESTRATION_ID)),
        "the re-created worker must rejoin its tab with the tab's own title, index and instance \
         token — a `None` title here is issue #960's secondary path, and it costs the tab its \
         label as soon as the last pane that still carries one goes away"
    );

    let state = fx.daemon.state.read().await;
    assert_eq!(
        state.pane_role_map.get(WORKER_PANE).map(String::as_str),
        Some(WORKER_ROLE),
        "the role must still route after the recovery, or the NEXT delegate is rejected with \
         `reached no worker for role(s)` — the permanent breakage #606 reports"
    );
}

/// Issue #584's promptness half: how long the orchestrator may be left in the
/// dark after its `clear = true` replacement worker dies before it is ready.
///
/// **Re-derived in issue #243, from a measurement rather than from the
/// alternative.** It was 20 s, justified in the catalog as "well under the
/// production `SESSION_START_WAIT_TIMEOUT` + readiness buffer (31 s) that the
/// pre-fix path burned" — a bound picked to be under the thing it was replacing.
/// That reasoning has expired twice over: #584 itself ended the readiness wait
/// on the replacement's PTY reaching EOF, and #243 removed the dead wait for
/// declared-no-signal agents outright, so 31 s is nobody's behaviour any more and
/// a 20 s ceiling on a ~0.1 s operation asserts approximately nothing.
///
/// **Measured on this branch: 103.1 / 103.4 / 103.9 / 104.1 ms idle, and
/// 54.4-108.4 ms across eight runs with all 16 cores saturated and a concurrent
/// full fast tier.** The figure is dominated by the fixture's own 50 ms poll
/// interval and barely moves under load, because the notice is driven by the
/// child's exit rather than by any timer.
///
/// Five seconds is ~46x the slowest figure measured here — room for a CI runner
/// an order of magnitude slower than this box and then some — while staying 6x
/// under the 30 s `SESSION_START_WAIT_TIMEOUT` a reverted EOF-driven wait would
/// cost. It is deliberately not tighter: this is an upper bound on a fast event,
/// so unlike `orchestration/delegate/010`'s lower bound it IS the load-sensitive
/// direction, and headroom is the only mitigation available.
const DEAD_REPLACEMENT_NOTICE_BUDGET: Duration = Duration::from_secs(5);

/// Scenario: start an orchestration whose `clear = true` worker refuses to start
/// while a marker file sits beside it, drop that marker once the first worker is
/// confirmed up, then delegate. The replacement dies before it can announce
/// itself, and the orchestrator must be TOLD — in its own pane, and within five
/// seconds rather than the thirty a readiness wait would cost — instead of being
/// left to wait for a `work-done` that can never arrive.
#[tokio::test(flavor = "multi_thread")]
#[spec("orchestration/delegate/023")]
async fn delegate_023_a_replacement_that_dies_is_reported_to_the_orchestrator() {
    use std::os::unix::fs::PermissionsExt;

    // The stand-in refuses to start once a `die` marker exists beside it. The
    // TEST drops that marker, after confirming the first worker is up — so
    // "the replacement dies before it is ready" is a fact the test establishes,
    // not a race it hopes for.
    let fx = fixture(|dir| {
        let script = dir.join("one-shot-worker.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nif [ -e \"$(dirname \"$0\")/die\" ]; then exit 3; fi\nexec cat\n",
        )
        .expect("write one-shot worker stand-in");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("chmod one-shot worker stand-in");
        script.to_string_lossy().into_owned()
    })
    .await;

    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        fx.daemon
            .registry
            .pane_current_agent_id(WORKER_PANE)
            .as_deref(),
        Some(fx.worker_agent_id.as_str()),
        "precondition: the first worker must be up before we make the next one fail"
    );
    std::fs::write(std::path::Path::new(&fx.cwd).join("die"), "")
        .expect("arm the stand-in's refusal to start again");

    delegate(&fx, "list the files in this directory").await;

    // The user's altitude: something visible in the orchestrator's own pane.
    let deadline = tokio::time::Instant::now() + DEAD_REPLACEMENT_NOTICE_BUDGET;
    let mut snapshot;
    loop {
        snapshot = fx
            .daemon
            .registry
            .snapshot(&fx.orchestrator_agent_id)
            .unwrap_or_default();
        let text = String::from_utf8_lossy(&snapshot);
        if text.contains("delegated worker never came up") && text.contains(WORKER_PANE) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "a `clear = true` delegate whose replacement worker died reported nothing to the \
             orchestrator within {DEAD_REPLACEMENT_NOTICE_BUDGET:?} — either the notice is gone \
             entirely, or the readiness wait no longer ends on the replacement's EOF and the \
             orchestrator is sitting through it (#584; budget re-derived in #243 against a \
             measured ~0.1 s). orchestrator pane = {:?}",
            text
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let pointer =
        common::expected_delegate_pointer(std::path::Path::new(&fx.cwd), WORKER_ROLE, WORKER_PANE);
    let worker_snapshot = fx
        .daemon
        .registry
        .pane_current_agent_id(WORKER_PANE)
        .and_then(|id| fx.daemon.registry.snapshot(&id).ok())
        .unwrap_or_default();
    assert!(
        !snapshot_contains(&worker_snapshot, &pointer),
        "nothing may be written into a pane whose agent is not live"
    );
    // PRD #249 finding B3's precedent: this notice family interpolates the
    // worker's scrubbed pane id and nothing else, so the role name — which is
    // caller-supplied config text — must not appear.
    assert!(
        !String::from_utf8_lossy(&snapshot).contains("'coder'"),
        "the notice must not interpolate the role name; snapshot = {:?}",
        String::from_utf8_lossy(&snapshot)
    );
}

// ---------------------------------------------------------------------------
// Issue #805: a delegate that never reaches its worker must not leave the
// worker owing a `work-done`.
//
// `handle_delegate` arms the outstanding-delegation record before the dispatch
// task has run at all, so every way that task can end without handing the
// worker a task pointer has to retire the record it was armed for — and only
// that one. Left armed, the worker's card reads `Idle (delegated)` and the
// orchestrator's `Observing` until the idle-worker timeout (two hours by
// default), for a task nobody received.
// ---------------------------------------------------------------------------

/// A role command that cannot be exec'd. One word with no shell
/// metacharacters is exec'd directly rather than through `$SHELL -c`, so this
/// is a spawn ERROR and not a shell that starts and then exits 127.
const MISSING_WORKER_BINARY: &str = "/nonexistent-worker-agent-deck-respawn-target";

/// The daemon's own respawn-failure notice, written into the orchestrator's
/// pane immediately before the respawn-error exit. Spelled out here rather
/// than imported so a silent rewording fails the test instead of following it.
const RESPAWN_FAILED_NEEDLE: &str = "respawn failed for role 'coder'";

/// The daemon's dead-replacement notice (`orchestration/delegate/023`).
const DEAD_REPLACEMENT_NEEDLE: &str = "delegated worker never came up";

const READINESS_BUFFER_ENV: &str = "DOT_AGENT_DECK_DELEGATE_READINESS_BUFFER_MS";

/// Re-point the `coder` role at `worker_command` for every LATER delegate. The
/// dispatch task re-reads `.dot-agent-deck.toml` on each delegate, so the
/// worker the fixture already started is untouched.
fn point_worker_role_at(fx: &Fixture, worker_command: &str) {
    std::fs::write(
        std::path::Path::new(&fx.cwd).join(".dot-agent-deck.toml"),
        config(worker_command),
    )
    .expect("rewrite the orchestration config");
}

/// Poll the orchestrator's scrollback until it holds `needle`, returning the
/// last snapshot either way so the caller can assert on (and print) it.
async fn wait_for_orchestrator_text(fx: &Fixture, needle: &str) -> String {
    let deadline = tokio::time::Instant::now() + common::load_scaled(Duration::from_secs(10));
    loop {
        let snapshot = String::from_utf8_lossy(
            &fx.daemon
                .registry
                .snapshot(&fx.orchestrator_agent_id)
                .unwrap_or_default(),
        )
        .into_owned();
        if snapshot.contains(needle) || tokio::time::Instant::now() >= deadline {
            return snapshot;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// A stand-in that starts normally until a `die` marker sits beside it, and
/// from then on exits before doing anything — the same one-shot worker
/// `orchestration/delegate/023` uses to make a replacement die on arrival.
fn write_one_shot_worker(dir: &std::path::Path) -> String {
    use std::os::unix::fs::PermissionsExt;

    let script = dir.join("one-shot-worker.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nif [ -e \"$(dirname \"$0\")/die\" ]; then exit 3; fi\nexec cat\n",
    )
    .expect("write one-shot worker stand-in");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("chmod one-shot worker stand-in");
    script.to_string_lossy().into_owned()
}

/// Pins `DOT_AGENT_DECK_DELEGATE_READINESS_BUFFER_MS` for one test and restores
/// it afterwards. nextest runs each test in its own process, so nothing else
/// reads the variable while it is set.
struct ReadinessBufferGuard {
    previous: Option<String>,
}

impl ReadinessBufferGuard {
    fn set(millis: u64) -> Self {
        let previous = std::env::var(READINESS_BUFFER_ENV).ok();
        // SAFETY: set before the fixture starts any task that reads it, in a
        // test process nextest gives this test alone.
        unsafe { std::env::set_var(READINESS_BUFFER_ENV, millis.to_string()) };
        Self { previous }
    }
}

impl Drop for ReadinessBufferGuard {
    fn drop(&mut self) {
        // SAFETY: as in `set`.
        unsafe {
            match self.previous.take() {
                Some(value) => std::env::set_var(READINESS_BUFFER_ENV, value),
                None => std::env::remove_var(READINESS_BUFFER_ENV),
            }
        }
    }
}

/// Scenario: start an orchestration with a live `cat` worker, re-point its `clear = true` role at a binary that does not exist, and delegate, so the respawn disposes of the worker and then fails to replace it. Once the orchestrator has been told the respawn failed, the worker pane must owe nothing: its outstanding delegation is retired and the daemon has broadcast that retirement.
#[tokio::test(flavor = "multi_thread")]
#[spec("orchestration/delegate/048")]
async fn delegate_048_a_failed_respawn_retires_the_delegation_it_armed() {
    let fx = fixture(|_dir: &std::path::Path| "cat".to_string()).await;
    point_worker_role_at(&fx, MISSING_WORKER_BINARY);
    let mut broadcasts = fx.daemon.event_tx.subscribe();

    delegate(&fx, "list the files in this directory").await;

    let orchestrator = wait_for_orchestrator_text(&fx, RESPAWN_FAILED_NEEDLE).await;
    assert!(
        orchestrator.contains(RESPAWN_FAILED_NEEDLE),
        "precondition: the dispatch must reach the respawn-error exit for this test to be \
         testing anything; orchestrator pane = {orchestrator:?}"
    );

    common::assert_undelivered_delegation_is_retired(
        &fx.daemon.registry,
        &mut broadcasts,
        WORKER_PANE,
        "its clear=true respawn failed",
    )
    .await;
}

/// Scenario: start an orchestration whose `clear = true` worker refuses to start once a marker file sits beside it, drop that marker after the first worker is up, and delegate, so the replacement dies before it can receive anything. Once the orchestrator has been told the worker never came up, the worker pane must owe nothing: its outstanding delegation is retired and the daemon has broadcast that retirement.
#[tokio::test(flavor = "multi_thread")]
#[spec("orchestration/delegate/049")]
async fn delegate_049_a_replacement_that_never_becomes_live_retires_the_delegation() {
    let fx = fixture(write_one_shot_worker).await;
    // The stand-in prints nothing of its own, so there is no output to wait
    // for; the same settle `orchestration/delegate/023` gives it. A first
    // worker still mid-boot when the marker lands only dies early — the
    // replacement the delegate produces dies on arrival either way.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        fx.daemon.registry.agent_is_live(&fx.worker_agent_id),
        "precondition: the first worker must be up before the next one is made to fail"
    );
    std::fs::write(std::path::Path::new(&fx.cwd).join("die"), "")
        .expect("arm the stand-in's refusal to start again");
    let mut broadcasts = fx.daemon.event_tx.subscribe();

    delegate(&fx, "list the files in this directory").await;

    let orchestrator = wait_for_orchestrator_text(&fx, DEAD_REPLACEMENT_NEEDLE).await;
    assert!(
        orchestrator.contains(DEAD_REPLACEMENT_NEEDLE),
        "precondition: the dispatch must reach the dead-replacement exit for this test to be \
         testing anything; orchestrator pane = {orchestrator:?}"
    );

    common::assert_undelivered_delegation_is_retired(
        &fx.daemon.registry,
        &mut broadcasts,
        WORKER_PANE,
        "its clear=true replacement worker never became live",
    )
    .await;
}

/// Scenario: delegate to a `clear = true` worker, announce the replacement so the dispatch enters its readiness buffer, and during that buffer close the replacement and let a different agent take the worker pane. The task pointer is bound to the replacement, so the identity gate refuses it and nothing is written into the successor; the delegation must then be retired and that retirement broadcast.
#[tokio::test(flavor = "multi_thread")]
#[spec("orchestration/delegate/050")]
async fn delegate_050_a_pointer_refused_for_the_wrong_session_retires_the_delegation() {
    // Long enough that the hand-over below lands well inside it: the swap
    // starts a quarter of the way in and takes milliseconds. Both figures grow
    // together with how contended the machine is — four seconds and one second
    // on an idle box, up to 24 s and 6 s at the scaling's ceiling, which stays
    // under the 30 s the daemon clamps this override to. Measured ONCE, so the
    // two cannot be scaled by different load readings.
    let readiness_buffer = common::load_scaled(Duration::from_millis(4000));
    let hand_over_after = readiness_buffer / 4;
    let _buffer = ReadinessBufferGuard::set(
        u64::try_from(readiness_buffer.as_millis()).expect("readiness buffer fits in u64 ms"),
    );
    let fx = fixture(|_dir: &std::path::Path| "cat".to_string()).await;
    let mut broadcasts = fx.daemon.event_tx.subscribe();

    delegate(&fx, "list the files in this directory").await;
    let replacement = wait_for_replacement_agent(
        &fx.daemon.registry,
        WORKER_PANE,
        &fx.worker_agent_id,
        common::load_scaled(Duration::from_secs(20)),
    )
    .await
    .expect("precondition: the clear=true delegate must respawn the worker");
    common::write_hook_line(
        &fx.daemon.hook_path,
        &session_start(WORKER_PANE, &replacement),
    )
    .expect("deliver synthetic SessionStart for the replacement worker");

    // The dispatch re-checks that the replacement still owns the pane as soon
    // as its readiness wait ends, and only then starts the buffer. Swapping
    // before that check would take the dead-replacement exit instead, which
    // `orchestration/delegate/049` covers; the precondition below tells the
    // two apart.
    tokio::time::sleep(hand_over_after).await;
    fx.daemon
        .registry
        .close_agent(&replacement)
        .expect("close the replacement worker mid-buffer");
    let successor = fx
        .daemon
        .registry
        .spawn_agent(SpawnOptions {
            command: Some("cat"),
            cwd: Some(&fx.cwd),
            display_name: Some(WORKER_ROLE),
            env: vec![(DOT_AGENT_DECK_PANE_ID.to_string(), WORKER_PANE.to_string())],
            tab_membership: Some(membership(1, WORKER_ROLE, false, &fx.cwd)),
            ..SpawnOptions::default()
        })
        .expect("spawn the successor onto the worker pane");
    assert_ne!(
        successor, replacement,
        "the hand-over must produce a NEW registry agent id"
    );

    assert!(
        common::wait_for_commission_release(&fx.daemon.registry, WORKER_PANE).await,
        "precondition: the dispatch never reached a no-delivery exit; watches = {:?}",
        fx.daemon.registry.delegation_watch_snapshot(WORKER_PANE)
    );
    let orchestrator = wait_for_orchestrator_text(&fx, "").await;
    assert!(
        !orchestrator.contains(DEAD_REPLACEMENT_NEEDLE),
        "precondition: the hand-over landed before the readiness buffer began, so the dispatch \
         left through the dead-replacement exit rather than through the identity gate this test \
         is about; orchestrator pane = {orchestrator:?}"
    );
    let pointer =
        common::expected_delegate_pointer(std::path::Path::new(&fx.cwd), WORKER_ROLE, WORKER_PANE);
    let successor_snapshot = fx.daemon.registry.snapshot(&successor).unwrap_or_default();
    assert!(
        !snapshot_contains(&successor_snapshot, &pointer),
        "precondition: a pointer bound to the replacement must not be written into the agent \
         that inherited its pane; successor pane = {:?}",
        String::from_utf8_lossy(&successor_snapshot)
    );

    common::assert_undelivered_delegation_is_retired(
        &fx.daemon.registry,
        &mut broadcasts,
        WORKER_PANE,
        "the identity gate refused its task pointer as WrongSession",
    )
    .await;
}

/// Scenario: hold the worker pane's dispatch lock so a delegate whose respawn is going to fail is armed but cannot run, arm a NEWER delegation on the same worker pane, then release the lock and let the older delegate fail. The older delegate's exit may retire only its own delegation: the newer one must still be armed afterwards, and no retirement may have been broadcast for the pane.
#[tokio::test(flavor = "multi_thread")]
#[spec("orchestration/delegate/051")]
async fn delegate_051_an_undelivered_delegate_leaves_a_newer_delegation_armed() {
    let fx = fixture(|_dir: &std::path::Path| "cat".to_string()).await;
    point_worker_role_at(&fx, MISSING_WORKER_BINARY);
    let mut broadcasts = fx.daemon.event_tx.subscribe();

    // Every dispatch on a pane serializes on this lock, so holding it keeps the
    // older delegate's task parked at its first statement while the newer
    // delegation is armed — the ordering this test is about, fixed by
    // construction rather than by timing.
    let dispatch_lock = fx.daemon.registry.pane_dispatch_lock(WORKER_PANE);
    let parked = dispatch_lock.lock().await;
    delegate(&fx, "list the files in this directory").await;
    assert!(
        fx.daemon
            .registry
            .delegation_watch_snapshot(WORKER_PANE)
            .outstanding_delegation
            .is_some(),
        "precondition: the older delegate must have armed its delegation"
    );
    // What `handle_delegate`'s fan-out does for the next delegate to this pane.
    let newer = fx
        .daemon
        .registry
        .arm_outstanding_delegation(
            WORKER_PANE,
            WORKER_ROLE,
            ORCH_PANE,
            &fx.orchestrator_agent_id,
            Some(&OrchestrationIdentity::Instance {
                id: ORCHESTRATION_ID.to_string(),
                name: ORCHESTRATION.to_string(),
            }),
        )
        .expect("precondition: neither pane is closing, so the newer delegation must arm");
    drop(parked);

    let orchestrator = wait_for_orchestrator_text(&fx, RESPAWN_FAILED_NEEDLE).await;
    assert!(
        orchestrator.contains(RESPAWN_FAILED_NEEDLE),
        "precondition: the older delegate must reach the respawn-error exit; orchestrator pane \
         = {orchestrator:?}"
    );
    assert!(
        common::wait_for_commission_release(&fx.daemon.registry, WORKER_PANE).await,
        "precondition: the older delegate never released its commission, so its no-delivery \
         exit has not run; watches = {:?}",
        fx.daemon.registry.delegation_watch_snapshot(WORKER_PANE)
    );

    // A negative window, spent in full: whatever the exit does to the record
    // it does in the same task as the commission release just observed.
    let seen = common::delegation_broadcasts_for(
        &mut broadcasts,
        WORKER_PANE,
        Duration::from_millis(750),
        false,
    )
    .await;
    assert_eq!(
        seen.retired, 0,
        "the older delegate never reached the worker, but the delegation left on the pane is a \
         NEWER one and no retirement may be announced for it; broadcasts = {seen:?}"
    );
    assert!(
        fx.daemon
            .registry
            .take_outstanding_delegation_if(WORKER_PANE, newer.seq)
            .is_some(),
        "the older delegate's no-delivery exit retired the NEWER delegation armed on the same \
         worker pane; retirement must be conditional on the delegation's own generation. \
         watches = {:?}",
        fx.daemon.registry.delegation_watch_snapshot(WORKER_PANE)
    );
}

/// Report `work-done` from the worker pane, as the worker's own CLI call does.
async fn work_done(fx: &Fixture) {
    // Read back rather than assumed, so the signal is the one the pane's
    // current registration would produce and cannot be refused as stale.
    let (generation, daemon_boot_id) = {
        let state = fx.daemon.state.read().await;
        (
            state
                .pane_registration_generation
                .get(WORKER_PANE)
                .copied()
                .expect("precondition: the fixture registered the worker pane"),
            state.daemon_boot_id().to_string(),
        )
    };
    fx.daemon
        .state
        .read()
        .await
        .handle_work_done(
            WorkDoneSignal {
                pane_id: WORKER_PANE.to_string(),
                task: "The delegated test task is complete.".to_string(),
                done: false,
                timestamp: chrono::Utc::now(),
                generation,
                daemon_boot_id,
                subject: None,
            },
            &fx.daemon.registry,
            Some(&fx.daemon.event_tx),
        )
        .await;
}

/// Wait until the commission ledger holds exactly `expected` entries for the
/// worker pane, returning whether that happened within the budget. Polled after
/// a delegate call has returned, a count back at what it was before that call
/// means the delegate's dispatch has run to a no-delivery exit.
async fn wait_for_outstanding_commissions(fx: &Fixture, expected: u32) -> bool {
    let deadline = tokio::time::Instant::now() + common::load_scaled(Duration::from_secs(10));
    loop {
        let outstanding = fx
            .daemon
            .registry
            .delegation_watch_snapshot(WORKER_PANE)
            .delegation_commission
            .map_or(0, |commission| commission.outstanding);
        if outstanding == expected {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Scenario: hold the worker pane's dispatch lock so a delegate whose respawn is going to fail is armed but cannot run, arm a NEWER delegation and its commission on the same worker pane, then release the lock and let the older delegate fail. The older delegation no longer counts, so the first `work-done` from the pane answers the newer one: the record must be retired and that retirement broadcast.
#[tokio::test(flavor = "multi_thread")]
#[spec("orchestration/delegate/052")]
async fn delegate_052_work_done_after_an_overtaken_undelivered_delegate_retires_the_newer_one() {
    let fx = fixture(|_dir: &std::path::Path| "cat".to_string()).await;
    point_worker_role_at(&fx, MISSING_WORKER_BINARY);
    let mut broadcasts = fx.daemon.event_tx.subscribe();

    // As in `orchestration/delegate/051`: the older delegate's task stays
    // parked at its first statement while the newer delegation is armed.
    let dispatch_lock = fx.daemon.registry.pane_dispatch_lock(WORKER_PANE);
    let parked = dispatch_lock.lock().await;
    delegate(&fx, "list the files in this directory").await;
    assert!(
        fx.daemon
            .registry
            .delegation_watch_snapshot(WORKER_PANE)
            .outstanding_delegation
            .is_some(),
        "precondition: the older delegate must have armed its delegation"
    );
    // What `handle_delegate`'s fan-out does for the next delegate to this
    // pane. Held so the newer delegation's cancellation channel stays open.
    let _newer = fx
        .daemon
        .registry
        .arm_outstanding_delegation(
            WORKER_PANE,
            WORKER_ROLE,
            ORCH_PANE,
            &fx.orchestrator_agent_id,
            Some(&OrchestrationIdentity::Instance {
                id: ORCHESTRATION_ID.to_string(),
                name: ORCHESTRATION.to_string(),
            }),
        )
        .expect("precondition: neither pane is closing, so the newer delegation must arm");
    assert!(
        fx.daemon
            .registry
            .arm_delegation_commission(WORKER_PANE, ORCH_PANE, None),
        "precondition: neither pane is closing, so the newer commission must arm"
    );
    drop(parked);

    let orchestrator = wait_for_orchestrator_text(&fx, RESPAWN_FAILED_NEEDLE).await;
    assert!(
        orchestrator.contains(RESPAWN_FAILED_NEEDLE),
        "precondition: the older delegate must reach the respawn-error exit; orchestrator pane \
         = {orchestrator:?}"
    );
    assert!(
        wait_for_outstanding_commissions(&fx, 1).await,
        "precondition: the older delegate never released its commission, so its no-delivery \
         exit has not run; watches = {:?}",
        fx.daemon.registry.delegation_watch_snapshot(WORKER_PANE)
    );
    // A negative window, spent in full: whatever the exit does to the record
    // it does in the same task as the commission release just observed.
    let seen = common::delegation_broadcasts_for(
        &mut broadcasts,
        WORKER_PANE,
        Duration::from_millis(750),
        false,
    )
    .await;
    assert!(
        seen.retired == 0
            && fx
                .daemon
                .registry
                .delegation_watch_snapshot(WORKER_PANE)
                .outstanding_delegation
                .is_some(),
        "precondition: the older delegate's exit must leave the newer delegation armed and \
         unannounced (`orchestration/delegate/051`); broadcasts = {seen:?}, watches = {:?}",
        fx.daemon.registry.delegation_watch_snapshot(WORKER_PANE)
    );

    work_done(&fx).await;

    assert!(
        fx.daemon
            .registry
            .delegation_watch_snapshot(WORKER_PANE)
            .outstanding_delegation
            .is_none(),
        "the older delegate never reached the worker, but the newer delegation still counts it \
         as owed: the first work-done was credited to the delegation that was never delivered, \
         and the one it answers stays armed until the idle-worker timeout; watches = {:?}",
        fx.daemon.registry.delegation_watch_snapshot(WORKER_PANE)
    );
    let seen = common::delegation_broadcasts_for(
        &mut broadcasts,
        WORKER_PANE,
        common::load_scaled(Duration::from_secs(3)),
        true,
    )
    .await;
    assert!(
        seen.retired > 0,
        "the work-done that answers the newer delegation must be announced as a retirement; \
         broadcasts = {seen:?}"
    );
}

/// Issue #812: arm the record an EARLIER, delivered-but-unanswered delegation
/// leaves on the worker pane, exactly as `handle_delegate`'s fan-out armed it and
/// `dispatch_one_owned`'s delivery tail then bound it to the worker that got the
/// pointer (`fx.worker_agent_id`, the agent a `clear = true` respawn is about to
/// terminate). The next delegate replaces the record and carries this generation
/// forward inside it, with `worker_agent_id` back at `None`.
fn arm_earlier_delivered_delegation(fx: &Fixture) -> u64 {
    let earlier = fx
        .daemon
        .registry
        .arm_outstanding_delegation(
            WORKER_PANE,
            WORKER_ROLE,
            ORCH_PANE,
            &fx.orchestrator_agent_id,
            Some(&OrchestrationIdentity::Instance {
                id: ORCHESTRATION_ID.to_string(),
                name: ORCHESTRATION.to_string(),
            }),
        )
        .expect("precondition: neither pane is closing, so the earlier delegation must arm");
    fx.daemon.registry.bind_delegation_worker_agent_id(
        WORKER_PANE,
        earlier.seq,
        &fx.worker_agent_id,
    );
    assert!(
        fx.daemon
            .registry
            .delegation_watch_snapshot(WORKER_PANE)
            .outstanding_delegation
            .is_some(),
        "precondition: the earlier delegation must be armed on the worker pane"
    );
    earlier.seq
}

/// Issue #812: the shared tail of the two respawn-exit tests. The delegate call
/// has returned and `broadcasts` was subscribed before it. Everything the pane
/// owed is gone once the failed delegate has left: the earlier delegation is
/// not answerable (its worker was terminated by the respawn) and, being bound to
/// nobody, the exit sweep can never drain it.
async fn assert_failed_respawn_leaves_nothing_owed(
    fx: &Fixture,
    broadcasts: &mut tokio::sync::broadcast::Receiver<dot_agent_deck::event::BroadcastMsg>,
    exit: &str,
) {
    assert!(
        common::wait_for_commission_release(&fx.daemon.registry, WORKER_PANE).await,
        "precondition: the delegate's commission was never released, so the dispatch task has \
         not reached the no-delivery exit under test ({exit}); watches = {:?}",
        fx.daemon.registry.delegation_watch_snapshot(WORKER_PANE)
    );
    let still_armed =
        common::wait_for_outstanding_delegation_to_retire(&fx.daemon.registry, WORKER_PANE).await;
    let seen = common::delegation_broadcasts_for(
        broadcasts,
        WORKER_PANE,
        common::load_scaled(Duration::from_secs(3)),
        true,
    )
    .await;
    assert!(
        still_armed.is_none(),
        "the delegate to pane {WORKER_PANE} failed ({exit}) after its respawn had terminated the \
         worker that an EARLIER delegation was delivered to, so that delegation can never be \
         answered; but its record is still armed, and it is bound to no worker so the exit sweep \
         can never drain it. The worker's card keeps reading `Idle (delegated)` and the \
         orchestrator's `Observing` for the whole worker_response_timeout, and an idle nudge \
         follows the respawn-failure notice; still armed = {still_armed:?}, broadcasts = {seen:?}"
    );
    assert!(
        seen.retired > 0,
        "the pane's outstanding delegations are gone ({exit}) but no DelegationRetired was \
         broadcast for it, so an already-attached deck keeps showing the delegation; \
         broadcasts = {seen:?}"
    );
}

/// Scenario: a `cat` worker already owes an earlier, delivered delegation that it has not answered; re-point its `clear = true` role at a binary that does not exist and delegate again, so the respawn terminates the worker and then fails. Once the orchestrator has been told the respawn failed, the worker pane must owe nothing at all: the earlier delegation can never be answered by the worker that was terminated, so its record is gone and the retirement was broadcast.
#[tokio::test(flavor = "multi_thread")]
#[spec("orchestration/delegate/053")]
async fn delegate_053_a_failed_respawn_drops_an_earlier_delegation_the_terminated_worker_owed() {
    let fx = fixture(|_dir: &std::path::Path| "cat".to_string()).await;
    let _earlier = arm_earlier_delivered_delegation(&fx);
    point_worker_role_at(&fx, MISSING_WORKER_BINARY);
    let mut broadcasts = fx.daemon.event_tx.subscribe();

    delegate(&fx, "list the files in this directory").await;

    let orchestrator = wait_for_orchestrator_text(&fx, RESPAWN_FAILED_NEEDLE).await;
    assert!(
        orchestrator.contains(RESPAWN_FAILED_NEEDLE),
        "precondition: the dispatch must reach the respawn-error exit for this test to be \
         testing anything; orchestrator pane = {orchestrator:?}"
    );

    assert_failed_respawn_leaves_nothing_owed(
        &fx,
        &mut broadcasts,
        "its clear=true respawn failed",
    )
    .await;
}

/// Scenario: a one-shot worker already owes an earlier, delivered delegation that it has not answered; make the next worker refuse to start and delegate again, so the respawn terminates the worker and the replacement dies before it is ever live. Once the orchestrator has been told the worker never came up, the worker pane must owe nothing at all: the earlier delegation's record is gone and the retirement was broadcast.
#[tokio::test(flavor = "multi_thread")]
#[spec("orchestration/delegate/054")]
async fn delegate_054_a_dead_replacement_drops_an_earlier_delegation_the_terminated_worker_owed() {
    let fx = fixture(write_one_shot_worker).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        fx.daemon.registry.agent_is_live(&fx.worker_agent_id),
        "precondition: the first worker must be up before the next one is made to fail"
    );
    let _earlier = arm_earlier_delivered_delegation(&fx);
    std::fs::write(std::path::Path::new(&fx.cwd).join("die"), "")
        .expect("arm the stand-in's refusal to start again");
    let mut broadcasts = fx.daemon.event_tx.subscribe();

    delegate(&fx, "list the files in this directory").await;

    let orchestrator = wait_for_orchestrator_text(&fx, DEAD_REPLACEMENT_NEEDLE).await;
    assert!(
        orchestrator.contains(DEAD_REPLACEMENT_NEEDLE),
        "precondition: the dispatch must reach the dead-replacement exit for this test to be \
         testing anything; orchestrator pane = {orchestrator:?}"
    );

    assert_failed_respawn_leaves_nothing_owed(
        &fx,
        &mut broadcasts,
        "its clear=true replacement worker never became live",
    )
    .await;
}

/// Arm the shape the two guard tests share: an earlier delivered delegation, a
/// delegate to a role whose respawn will fail parked on the pane's dispatch lock,
/// and a NEWER delegation armed after both. Returns the newer delegation, the
/// held lock guard's release having already happened, once the failing delegate
/// has left through the respawn-error exit.
async fn overtaken_after_an_earlier_delegation(
    fx: &Fixture,
) -> dot_agent_deck::agent_pty::ArmedDelegation {
    point_worker_role_at(fx, MISSING_WORKER_BINARY);
    let _earlier = arm_earlier_delivered_delegation(fx);
    let dispatch_lock = fx.daemon.registry.pane_dispatch_lock(WORKER_PANE);
    let parked = dispatch_lock.lock().await;
    delegate(fx, "list the files in this directory").await;
    let newer = fx
        .daemon
        .registry
        .arm_outstanding_delegation(
            WORKER_PANE,
            WORKER_ROLE,
            ORCH_PANE,
            &fx.orchestrator_agent_id,
            Some(&OrchestrationIdentity::Instance {
                id: ORCHESTRATION_ID.to_string(),
                name: ORCHESTRATION.to_string(),
            }),
        )
        .expect("precondition: neither pane is closing, so the newer delegation must arm");
    assert!(
        fx.daemon
            .registry
            .arm_delegation_commission(WORKER_PANE, ORCH_PANE, None),
        "precondition: neither pane is closing, so the newer commission must arm"
    );
    drop(parked);

    let orchestrator = wait_for_orchestrator_text(fx, RESPAWN_FAILED_NEEDLE).await;
    assert!(
        orchestrator.contains(RESPAWN_FAILED_NEEDLE),
        "precondition: the failing delegate must reach the respawn-error exit; orchestrator \
         pane = {orchestrator:?}"
    );
    assert!(
        wait_for_outstanding_commissions(fx, 1).await,
        "precondition: the failing delegate never released its commission, so its no-delivery \
         exit has not run; watches = {:?}",
        fx.daemon.registry.delegation_watch_snapshot(WORKER_PANE)
    );
    newer
}

/// Scenario: a worker owes an earlier delivered delegation, a delegate whose respawn will fail is parked on the pane's dispatch lock, and a newer delegation is armed after both; release the lock and let the delegate fail. Whatever the exit drops of the older delegations, the NEWER delegation's record must still be armed afterwards and no retirement may be announced for the pane.
#[tokio::test(flavor = "multi_thread")]
#[spec("orchestration/delegate/055")]
async fn delegate_055_a_failed_respawn_never_drops_a_newer_delegations_record() {
    let fx = fixture(|_dir: &std::path::Path| "cat".to_string()).await;
    let mut broadcasts = fx.daemon.event_tx.subscribe();

    let newer = overtaken_after_an_earlier_delegation(&fx).await;

    let seen = common::delegation_broadcasts_for(
        &mut broadcasts,
        WORKER_PANE,
        Duration::from_millis(750),
        false,
    )
    .await;
    assert_eq!(
        seen.retired, 0,
        "the failing delegate's record on the pane belongs to a NEWER delegation; no retirement \
         may be announced for it; broadcasts = {seen:?}"
    );
    assert!(
        fx.daemon
            .registry
            .take_outstanding_delegation_if(WORKER_PANE, newer.seq)
            .is_some(),
        "the failing delegate's no-delivery exit dropped the record of a NEWER delegation armed \
         on the same worker pane; dropping the older delegations owed must not touch it. \
         watches = {:?}",
        fx.daemon.registry.delegation_watch_snapshot(WORKER_PANE)
    );
}

/// Scenario: a worker owes an earlier delivered delegation, a delegate whose respawn will fail is parked on the pane's dispatch lock, and a newer delegation is armed after both; release the lock, let the delegate fail, then report one `work-done` from the worker pane. Only the newer delegation is still owed, so that single `work-done` must retire the record and the retirement must be broadcast.
#[tokio::test(flavor = "multi_thread")]
#[spec("orchestration/delegate/056")]
async fn delegate_056_after_a_failed_respawn_the_first_work_done_answers_the_newer_delegation() {
    let fx = fixture(|_dir: &std::path::Path| "cat".to_string()).await;
    let mut broadcasts = fx.daemon.event_tx.subscribe();

    let _newer = overtaken_after_an_earlier_delegation(&fx).await;
    work_done(&fx).await;

    assert!(
        fx.daemon
            .registry
            .delegation_watch_snapshot(WORKER_PANE)
            .outstanding_delegation
            .is_none(),
        "the earlier delegation was delivered to a worker the failed respawn terminated, yet it \
         is still counted as owed, so the first work-done was credited to it and the newer \
         delegation it actually answers stays armed until the idle-worker timeout; watches = {:?}",
        fx.daemon.registry.delegation_watch_snapshot(WORKER_PANE)
    );
    let seen = common::delegation_broadcasts_for(
        &mut broadcasts,
        WORKER_PANE,
        common::load_scaled(Duration::from_secs(3)),
        true,
    )
    .await;
    assert!(
        seen.retired > 0,
        "the work-done that answers the newer delegation must be announced as a retirement; \
         broadcasts = {seen:?}"
    );
}

/// Scenario: an older delegate's dispatch is still queued behind the pane's dispatch lock (its task never handed to any worker) when a newer delegate whose respawn will fail is dispatched first. After the newer delegate fails, the older generation must still be owed and watched, because if the failure was transient its dispatch will still deliver; one `work-done` then retires it and the retirement is broadcast.
#[tokio::test(flavor = "multi_thread")]
#[spec("orchestration/delegate/057")]
async fn delegate_057_a_failed_respawn_keeps_an_older_delegation_whose_dispatch_has_not_started() {
    let fx = fixture(|_dir: &std::path::Path| "cat".to_string()).await;
    point_worker_role_at(&fx, MISSING_WORKER_BINARY);
    let mut broadcasts = fx.daemon.event_tx.subscribe();
    let queued = arm_earlier_delivered_delegation(&fx);
    fx.daemon.registry.mark_delegation_dispatch_pending(queued);
    let dispatch_lock = fx.daemon.registry.pane_dispatch_lock(WORKER_PANE);
    let parked = dispatch_lock.lock().await;
    delegate(&fx, "list the files in this directory").await;
    drop(parked);

    let orchestrator = wait_for_orchestrator_text(&fx, RESPAWN_FAILED_NEEDLE).await;
    assert!(
        orchestrator.contains(RESPAWN_FAILED_NEEDLE),
        "precondition: the newer delegate must reach the respawn-error exit; orchestrator pane \
         = {orchestrator:?}"
    );

    assert!(
        fx.daemon
            .registry
            .delegation_watch_snapshot(WORKER_PANE)
            .outstanding_delegation
            .is_some(),
        "the older delegation's dispatch had not started, so its task was never given to the \
         terminated worker; the newer delegate's failed respawn dropped it and would leave its \
         task, if delivered later, unwatched; watches = {:?}",
        fx.daemon.registry.delegation_watch_snapshot(WORKER_PANE)
    );
    fx.daemon.registry.mark_delegation_dispatch_started(queued);
    work_done(&fx).await;
    assert!(
        fx.daemon
            .registry
            .delegation_watch_snapshot(WORKER_PANE)
            .outstanding_delegation
            .is_none(),
        "the one work-done must answer the one generation still owed"
    );
    let seen = common::delegation_broadcasts_for(
        &mut broadcasts,
        WORKER_PANE,
        common::load_scaled(Duration::from_secs(3)),
        true,
    )
    .await;
    assert!(
        seen.retired > 0,
        "the retirement must be announced; broadcasts = {seen:?}"
    );
}

/// Issue #706: an env-dumping worker stand-in — logs the vars that decide
/// whether a recreated worker's `work-done` can pass the daemon's
/// generation/boot-id staleness gate, then behaves like `cat`.
#[cfg(unix)]
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

/// Scenario: issue #706 — `dispatch_one_owned`'s `clear = true` recreate leg
/// (issue #606's recovery path) is driven by closing the worker's registry
/// record directly, the same deterministic technique
/// `agent_detection.rs`'s `spawn_010_declared_identity_wins_spawn_recreate_and_learning`
/// fixture uses, rather than racing an in-flight close — so the next
/// delegate's respawn attempt reliably reports `recreated: true`. The
/// recreated worker's actual environment must carry the SAME registration
/// generation and daemon boot id that `pane_registration_generation` ends up
/// holding for its pane, not just `DOT_AGENT_DECK_PANE_ID`.
#[tokio::test(flavor = "multi_thread")]
#[spec("orchestration/delegate/046")]
async fn delegate_046_recreate_leg_injects_matching_registration_generation() {
    let script_dir = common::race_safe_tempdir();
    let script_path = script_dir.path().join("env-dump-worker.sh");
    let log_path = script_dir.path().join("env-dump.log");
    write_env_dump_worker(&script_path, &log_path);
    let worker_command = script_path.to_string_lossy().into_owned();

    let fx = fixture(move |_dir: &std::path::Path| worker_command).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while recorded_launches(&log_path).is_empty() {
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
        "precondition: the worker stand-in must still own its pane before it is closed"
    );

    fx.daemon
        .registry
        .close_agent(&fx.worker_agent_id)
        .expect("remove the worker's registry record to force the recreate leg (issue #606)");

    delegate(&fx, "list the files in this directory").await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while recorded_launches(&log_path).len() < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "delegating to a role whose pane had no registry record left never recreated it \
             (issue #606); blocks = {:?}",
            recorded_launches(&log_path)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let blocks = recorded_launches(&log_path);
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

    // The `if recreated { ... }` re-registration in `dispatch_one_owned` runs
    // inline, before any readiness wait — but poll rather than assume, since
    // nothing here should depend on that ordering staying true.
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
/// (non-recreate) respawn leg of a `clear = true` delegate, exercised against
/// a worker pane whose registry record is left INTACT — the opposite of
/// `delegate_046_recreate_leg_injects_matching_registration_generation`,
/// which deliberately forces `recreated: true` by closing the record first.
/// `respawn_or_recreate_agent_for_pane`'s own doc calls this the
/// "overwhelmingly common" outcome: with a record present it never falls
/// through to the recreate branch, so simply not closing the agent (as this
/// test does) is what selects this leg deterministically. `dispatch_one_owned`
/// reserves a fresh registration generation unconditionally before EVERY
/// `clear = true` respawn attempt — and `reserve_registration_generation`
/// writes that value into `pane_registration_generation` immediately, not
/// only once confirmed — but `respawn_agent_for_pane_declared` replays the
/// PREVIOUS child's `spawn_env` verbatim and never looks at
/// `PaneRecreateIdentity::env`. On this leg (`recreated == false`),
/// `dispatch_one_owned` restores the map to the pre-reservation value
/// synchronously, under the same `pane_dispatch_lock` guard, instead of
/// confirming — so the map stays in sync with what the live worker's own
/// `work-done` will report, closing the same failure #706 fixed on the
/// recreate leg, on the far more common ordinary path too.
#[tokio::test(flavor = "multi_thread")]
#[spec("orchestration/delegate/047")]
async fn delegate_047_ordinary_respawn_leg_keeps_registration_generation_in_sync() {
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

    let orchestrator_agent_id = daemon
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
    // this daemon's boot id — the same as a REAL production spawn would
    // (`crate::spawn::spawn`'s `pane_env`, `src/spawn.rs`), which this
    // in-process fixture bypasses by calling `registry.spawn_agent` directly.
    // `"1"` is not arbitrary: it is the exact value
    // `reserve_registration_generation` computes for a pane_id it has never
    // seen before (`.or_insert(0) += 1`), which is what `register_orchestration_role`
    // below performs for `WORKER_PANE`. So the env and the map start out
    // GENUINELY in sync — the precondition an ordinary respawn is supposed to
    // preserve, and the state a real live pane is actually in before any
    // `clear = true` delegate touches it.
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
        cwd,
        orchestrator_agent_id,
        worker_agent_id,
    };

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while recorded_launches(&log_path).is_empty() {
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
         closed — which is what selects the ORDINARY respawn leg rather than delegate_046's \
         forced recreate leg"
    );

    delegate(&fx, "list the files in this directory").await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while recorded_launches(&log_path).len() < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the `clear = true` delegate against an intact registry record never produced an \
             ordinary respawn; blocks = {:?}",
            recorded_launches(&log_path)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let blocks = recorded_launches(&log_path);
    let respawned_block = blocks.last().expect("a second launch block exists");
    let field = |name: &str| -> String {
        respawned_block
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{name}=")))
            .unwrap_or_default()
            .to_string()
    };
    let injected_gen = field("gen");

    // Unlike `delegate_046`, no detached task confirms anything on this leg —
    // `dispatch_one_owned`'s reservation is synchronous and its
    // `confirm_orchestration_role` call only runs `if recreated`, which this
    // is not — so there is nothing to poll for; the map is already whatever
    // it is going to be by the time `delegate` above returns.
    let state = fx.daemon.state.read().await;
    let map_generation = state.pane_registration_generation.get(WORKER_PANE).copied();
    drop(state);

    assert_eq!(
        map_generation,
        injected_gen.parse::<u64>().ok(),
        "on the ORDINARY (non-recreate) respawn leg, `pane_registration_generation` must still \
         equal whatever generation the respawned child's actual (verbatim-replayed) env carries \
         — the eager, unconditional reservation `dispatch_one_owned` performs before EVERY \
         `clear = true` respawn attempt bumps the map regardless of which leg runs, but only the \
         recreate leg's `confirm_orchestration_role` call keeps it in sync (issue #706 fix-round \
         review B1 / audit A1); block = {respawned_block:?}, map = {map_generation:?}"
    );
}

// ---------------------------------------------------------------------------
// #584's control: the two spawn paths' respawns, side by side.
// ---------------------------------------------------------------------------

/// A recorder worker: appends everything that decides HOW it was launched to a
/// log, then behaves like a `cat` pane so the delegate's pointer is observable.
#[cfg(unix)]
fn write_recorder(path: &std::path::Path, log: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let script = format!(
        "#!/bin/sh\n\
         {{\n\
         echo \"argv0=$0 args=$*\"\n\
         echo \"cwd=$(pwd)\"\n\
         echo \"pane=$DOT_AGENT_DECK_PANE_ID\"\n\
         echo \"sock=$DOT_AGENT_DECK_SOCKET\"\n\
         echo \"shell=$SHELL\"\n\
         echo \"---\"\n\
         }} >> \"{log}\"\n\
         exec cat\n",
        log = log.display()
    );
    std::fs::write(path, script).expect("write recorder worker stand-in");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod recorder worker stand-in");
}

/// The recorder's per-invocation blocks, minus the agent id (which is expected
/// to differ — it is the whole point of a respawn).
fn recorded_launches(log: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .split("---\n")
        .map(str::trim)
        .filter(|block| !block.is_empty())
        .map(str::to_string)
        .collect()
}

struct SilentNotifier;
impl dot_agent_deck::scheduler::Notifier for SilentNotifier {
    fn notify(&self, event: dot_agent_deck::scheduler::NotifyEvent) {
        // Surfaced only on failure, via the assertions below; a spawn error here
        // would otherwise be invisible.
        eprintln!("[spawn notifier] {event:?}");
    }
}

/// Scenario: bring the same `clear = true` orchestration up twice — once through
/// the daemon's own dispatch spawn primitive (what `dot-agent-deck dispatch`,
/// the scheduler and issue-dispatch all use) and once through the `StartAgent`
/// shape the TUI's Ctrl+N path uses — then delegate to the worker on each. Both
/// replacements must be launched with the same command, cwd, pane id, hook
/// socket and shell, and both workers must physically receive the task pointer.
#[tokio::test(flavor = "multi_thread")]
#[spec("orchestration/dispatch/003")]
async fn dispatch_003_the_dispatch_and_startagent_paths_respawn_identically() {
    let daemon = common::spawn_inprocess_daemon().await;

    // --- the DISPATCH path: `crate::spawn::spawn`, exactly as the daemon's
    // `dispatch` / scheduler / issue-dispatch producers call it.
    let dispatch_dir = common::race_safe_tempdir();
    let dispatch_log = dispatch_dir.path().join("launches.log");
    let dispatch_recorder = dispatch_dir.path().join("recorder.sh");
    write_recorder(&dispatch_recorder, &dispatch_log);
    std::fs::write(
        dispatch_dir.path().join(".dot-agent-deck.toml"),
        config(&dispatch_recorder.to_string_lossy()),
    )
    .expect("write dispatched orchestration config");

    let handle = dot_agent_deck::spawn::spawn(
        dot_agent_deck::spawn::SpawnRequest {
            task_name: "parity".to_string(),
            working_dir: dispatch_dir.path().to_string_lossy().into_owned(),
            command: None,
            prompt: "coordinate the team".to_string(),
            resolved_target: None,
            compose_orchestrator_context: Some(
                dot_agent_deck::orchestrator_context::Attendance::Unattended,
            ),
            owner: None,
        },
        &daemon.registry,
        &SilentNotifier,
        Some(&daemon.event_tx),
        true,
        Some(&daemon.state),
    )
    .await
    .expect("the dispatch spawn primitive must bring the orchestration up");
    let dispatched_orchestrator = handle
        .agents
        .iter()
        .find(|a| a.role_name.as_deref() == Some("orchestrator"))
        .expect("dispatched orchestration has an orchestrator pane")
        .pane_id
        .clone();
    let dispatched_worker = handle
        .agents
        .iter()
        .find(|a| a.role_name.as_deref() == Some(WORKER_ROLE))
        .expect("dispatched orchestration has a worker pane")
        .pane_id
        .clone();
    let dispatched_worker_agent = daemon
        .registry
        .pane_current_agent_id(&dispatched_worker)
        .expect("the dispatched worker pane has a live agent");

    // --- the StartAgent path: the shape `AttachRequest::StartAgent` builds.
    let control = fixture(|dir| {
        let recorder = dir.join("recorder.sh");
        write_recorder(&recorder, &dir.join("launches.log"));
        recorder.to_string_lossy().into_owned()
    })
    .await;
    let control_log = std::path::Path::new(&control.cwd).join("launches.log");

    // Both first invocations must be on disk before either respawn, or the
    // comparison below cannot tell a respawn's block from an initial one.
    for (label, log) in [
        ("dispatch", dispatch_log.as_path()),
        ("startagent", control_log.as_path()),
    ] {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while recorded_launches(log).is_empty() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the {label} path's worker never recorded an initial launch"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    // --- delegate on each path and let each replacement announce itself, so
    // neither side pays the production readiness fallback.
    //
    // Issue #567: `dispatched_orchestrator` was registered by the REAL
    // `dot_agent_deck::spawn::spawn` primitive, so read its genuine
    // generation/boot-id back from `daemon.state` rather than hand-typing a
    // literal that could silently drift from what that primitive actually
    // reserved.
    let (dispatched_generation, dispatched_boot_id) = {
        let guard = daemon.state.read().await;
        let generation = *guard
            .pane_registration_generation
            .get(&dispatched_orchestrator)
            .expect("the dispatch spawn primitive must have reserved a generation");
        (generation, guard.daemon_boot_id().to_string())
    };
    let signal = DelegateSignal {
        pane_id: dispatched_orchestrator,
        task: "list the files in this directory".to_string(),
        to: vec![WORKER_ROLE.to_string()],
        timestamp: chrono::Utc::now(),
        generation: dispatched_generation,
        daemon_boot_id: dispatched_boot_id,
        subject: None,
    };
    daemon
        .state
        .read()
        .await
        .handle_delegate_with_state(
            signal,
            &daemon.registry,
            &daemon.event_tx,
            Some(&daemon.state),
        )
        .await;
    let dispatched_replacement = wait_for_replacement_agent(
        &daemon.registry,
        &dispatched_worker,
        &dispatched_worker_agent,
        Duration::from_secs(20),
    )
    .await
    .expect("the dispatched worker must be replaced by the clear=true delegate");
    common::write_hook_line(
        &daemon.hook_path,
        &session_start(&dispatched_worker, &dispatched_replacement),
    )
    .expect("deliver the dispatched replacement's SessionStart");

    delegate(&control, "list the files in this directory").await;
    let control_replacement = wait_for_replacement_agent(
        &control.daemon.registry,
        WORKER_PANE,
        &control.worker_agent_id,
        Duration::from_secs(20),
    )
    .await
    .expect("the StartAgent-path worker must be replaced by the clear=true delegate");
    common::write_hook_line(
        &control.daemon.hook_path,
        &session_start(WORKER_PANE, &control_replacement),
    )
    .expect("deliver the control replacement's SessionStart");

    // --- both workers actually receive the pointer.
    let dispatched_pointer =
        common::expected_delegate_pointer(dispatch_dir.path(), WORKER_ROLE, &dispatched_worker);
    let dispatched_snapshot = wait_for_pane_needle(
        &daemon.registry,
        &dispatched_worker,
        &dispatched_pointer,
        Duration::from_secs(20),
    )
    .await;
    assert!(
        snapshot_contains(&dispatched_snapshot, &dispatched_pointer),
        "the DISPATCHED orchestration's respawned worker never received the task pointer — the \
         user-visible half of #584; snapshot = {:?}",
        String::from_utf8_lossy(&dispatched_snapshot)
    );
    let control_pointer = common::expected_delegate_pointer(
        std::path::Path::new(&control.cwd),
        WORKER_ROLE,
        WORKER_PANE,
    );
    let control_snapshot = wait_for_pane_needle(
        &control.daemon.registry,
        WORKER_PANE,
        &control_pointer,
        Duration::from_secs(20),
    )
    .await;
    assert!(
        snapshot_contains(&control_snapshot, &control_pointer),
        "the CONTROL orchestration's respawned worker never received the task pointer — a broken \
         control means the harness is wrong and the dispatched result above proves nothing; \
         snapshot = {:?}",
        String::from_utf8_lossy(&control_snapshot)
    );

    // --- and the two paths' relaunch parameters agree.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while recorded_launches(&dispatch_log).len() < 2 || recorded_launches(&control_log).len() < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "one of the two paths never recorded a SECOND launch: dispatch = {:?}, control = {:?}",
            recorded_launches(&dispatch_log),
            recorded_launches(&control_log)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let dispatch_launches = recorded_launches(&dispatch_log);
    let control_launches = recorded_launches(&control_log);

    // Within a path, the respawn must reproduce the initial launch. Everything
    // that is legitimately per-pane (the recorder's own path, the pane id, the
    // cwd) is normalised away so the two paths can then be compared to each
    // other as well.
    let normalise = |block: &str, dir: &str, pane: &str| {
        block
            .replace(dir, "<CWD>")
            .replace(pane, "<PANE>")
            .replace(&daemon.hook_path.to_string_lossy().into_owned(), "<SOCK>")
            .replace(
                &control.daemon.hook_path.to_string_lossy().into_owned(),
                "<SOCK>",
            )
    };
    let dispatch_dir_str = dispatch_dir.path().to_string_lossy().into_owned();
    let dispatch_initial = normalise(&dispatch_launches[0], &dispatch_dir_str, &dispatched_worker);
    let dispatch_respawn = normalise(&dispatch_launches[1], &dispatch_dir_str, &dispatched_worker);
    let control_initial = normalise(&control_launches[0], &control.cwd, WORKER_PANE);
    let control_respawn = normalise(&control_launches[1], &control.cwd, WORKER_PANE);

    assert_eq!(
        dispatch_initial, dispatch_respawn,
        "a dispatched pane's respawn must relaunch the worker exactly as its initial spawn did"
    );
    assert_eq!(
        control_initial, control_respawn,
        "a StartAgent pane's respawn must relaunch the worker exactly as its initial spawn did"
    );
    assert_eq!(
        dispatch_respawn, control_respawn,
        "#584's leading hypothesis was that the dispatch and StartAgent paths hand the respawn \
         DIFFERENT relaunch parameters. They do not — and a future change that makes them \
         diverge is what this assertion is here to catch"
    );
}

/// A raw, no-echo `cat` on `pane_id` that prints `marker` once its termios is
/// already in raw mode. Every byte the daemon submits into the pane afterwards
/// appears exactly once in the agent's snapshot and nothing else does, which is
/// what makes "this text was NOT submitted" directly observable — the same stub
/// `idle_worker_detector.rs` and `work_done_reporting.rs` use for the same
/// reason.
fn spawn_raw_cat_observer(
    registry: &Arc<AgentPtyRegistry>,
    pane_id: &str,
    marker: &str,
    cwd: &str,
) -> String {
    let command =
        format!("stty -echo -icanon -icrnl -opost min 1 time 0 && printf {marker} && exec cat -u");
    registry
        .spawn_agent(SpawnOptions {
            command: Some(&command),
            cwd: Some(cwd),
            env: vec![
                (DOT_AGENT_DECK_PANE_ID.to_string(), pane_id.to_string()),
                ("SHELL".to_string(), "/bin/sh".to_string()),
            ],
            ..SpawnOptions::default()
        })
        .unwrap_or_else(|error| panic!("spawn raw-cat observer on {pane_id}: {error}"))
}

/// Poll `agent_id`'s scrollback until `needle` appears, or the deadline passes.
/// Returns whatever the last snapshot held so the caller can print it.
async fn wait_for_snapshot(registry: &AgentPtyRegistry, agent_id: &str, needle: &str) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot =
            String::from_utf8_lossy(&registry.snapshot(agent_id).unwrap_or_default()).into_owned();
        if snapshot.contains(needle) || tokio::time::Instant::now() >= deadline {
            return snapshot;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Scenario: An agent asks the daemon to dispatch, and its pane changes hands
/// while `handle_dispatch` does its worktree-and-spawn work — the caller is
/// closed and an unrelated agent inherits the same `DOT_AGENT_DECK_PANE_ID`.
/// Delivering the dispatch result must be refused as `WrongSession`, and none of
/// the result text may appear in the successor's scrollback even after a later
/// authorized write to that successor has demonstrably landed.
#[spec("orchestration/dispatch/006")]
#[tokio::test]
async fn dispatch_006_a_dispatch_result_is_refused_when_the_caller_pane_changed_hands() {
    common::init_test_env();
    const PANE: &str = "dispatch-result-handover-pane";
    const RESULT: &str = "DISPATCH-RESULT-MUST-NOT-REACH-THE-SUCCESSOR-2f7c";
    const BARRIER: &str = "AUTHORIZED-WRITE-AFTER-THE-REFUSAL-9b13";

    let dir = common::race_safe_tempdir();
    let cwd = dir.path().to_string_lossy().into_owned();
    let registry = Arc::new(AgentPtyRegistry::new());

    // The agent that asked for the dispatch, and whose registry id the daemon
    // captures from ONE `AgentRecord` before the slow half runs.
    let caller = spawn_raw_cat_observer(&registry, PANE, "CALLER-READY", &cwd);
    let ready = wait_for_snapshot(&registry, &caller, "CALLER-READY").await;
    assert!(
        ready.contains("CALLER-READY"),
        "precondition: the caller stub must be up before it is handed over; snapshot = {ready:?}"
    );

    // The hand-over: the caller goes away and an unrelated agent takes the pane
    // id, exactly as a recycled `DOT_AGENT_DECK_PANE_ID` is reissued.
    registry.close_agent(&caller).expect("close the caller");
    let successor = spawn_raw_cat_observer(&registry, PANE, "SUCCESSOR-READY", &cwd);
    assert_ne!(
        caller, successor,
        "the hand-over must produce a NEW registry agent id"
    );
    let ready = wait_for_snapshot(&registry, &successor, "SUCCESSOR-READY").await;
    assert!(
        ready.contains("SUCCESSOR-READY"),
        "precondition: the successor must be up and echoing, or the absence asserted below \
         proves only that its stub never started; snapshot = {ready:?}"
    );

    let outcome =
        dot_agent_deck::daemon::deliver_dispatch_result(&registry, PANE, &caller, RESULT).await;
    assert_eq!(
        outcome,
        GuardedSend::WrongSession,
        "a dispatch result bound to the caller must be refused once the caller's pane belongs \
         to somebody else"
    );

    // A barrier, not a sleep: an AUTHORIZED write to the successor that has
    // demonstrably arrived proves the pane has drained past the point where a
    // leaked result would have landed, so its absence below is a fact rather
    // than a race the test happened to win.
    let barrier = registry
        .write_and_submit_guarded(PANE, BARRIER, &successor, || async { true })
        .await
        .expect("the barrier write must reach the registry");
    assert_eq!(
        barrier,
        GuardedSend::Applied,
        "the successor owns the pane, so a write bound to IT must be applied — otherwise this \
         test proves nothing about the refusal above"
    );
    let snapshot = wait_for_snapshot(&registry, &successor, BARRIER).await;
    assert!(
        snapshot.contains(BARRIER),
        "the barrier write never reached the successor's PTY, so the absence below is untested; \
         snapshot = {snapshot:?}"
    );
    assert!(
        !snapshot.contains(RESULT),
        "the dispatch result reached a process that merely inherited the caller's pane id — the \
         successor may act on it with its own tools; snapshot = {snapshot:?}"
    );

    registry.shutdown_all();
}
