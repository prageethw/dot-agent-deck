#![cfg(feature = "e2e")]

//! L2 end-to-end coverage for upstream issue #423: an orchestration's start
//! role has its remit delivered exactly once, as a seed prompt at spawn, and
//! never re-asserted — so role adherence decays with session length, worst
//! exactly when an orchestration has run long enough to compact. This file
//! pins the fix: a `Compacting` event on the orchestrator start-role pane
//! re-delivers the `.dot-agent-deck/orchestrator-context.md` pointer, scoped
//! to the start role only, through the SAME readiness-gating and delivery-
//! confirmation discipline the spawn-time seed already uses
//! (`deliver_orchestrator_prompt`, `src/ui.rs`).
//!
//! Uses the `remit-reassert-orchestration` fixture (`orchestrator` start
//! role running a synthetic script written by each test into the fixture's
//! workdir, `worker` a plain `cat` stub) rather than the shared `orch-deck`/
//! `send-result-orchestration` fixtures — this needs BOTH a role that tees
//! its own stdin to a log file (to count re-deliveries, mirroring
//! `tests/e2e_pane_send_result.rs::pane_input_007`'s `orchestrator-prompt.log`
//! technique) AND a script capable of toggling its own declared liveness
//! live -> history-only -> live on cue from the test driver (needed only by
//! `orchestration/remit/003`; `001`/`002` simply never trigger that phase).
//!
//! `orchestration/remit/003` deliberately asserts only on the RENDERED GRID
//! feedback string and the delivery-log line count — both pre-existing,
//! stable observables `deliver_orchestrator_prompt` already produces for a
//! `HistoryOnly` `SendResult` today — never on a Rust symbol or enum variant
//! from the concurrently in-flight `fix/424-seed-delivery-confirmation`
//! branch, so this file stays correct regardless of which internal helper
//! #424 lands.
//!
//! Decision 6: gated behind the `e2e` feature so `cargo test-fast` never
//! compiles it.

mod common;

use std::time::Duration;

use common::{TuiDeck, commit_fixture, open_orchestration, write_executable};
use dot_agent_deck::event::{
    AgentEvent, AgentType, CLEAR_SESSION_START_METADATA_KEY, CLEAR_SESSION_START_METADATA_VALUE,
    EventType,
};
use spec::spec;

const DELIVERED_POINTER: &str = "Read .dot-agent-deck/orchestrator-context.md";

/// The synthetic orchestrator role script: a BACKGROUNDED subshell declares
/// the role live immediately (fast-path readiness for the spawn-time remit
/// pointer) and — only if the test writes the corresponding control file
/// into the workdir — later declares history-only and then live again;
/// meanwhile the FOREGROUND script body reads and logs every line delivered
/// to its real stdin, however many arrive, to `orchestrator-prompt.log`, and
/// — issue #424 — reports each one back over the hook socket as a
/// `user_prompt`-carrying `thinking` event, the only evidence
/// `prompt_submission_evidence` (`src/ui.rs`) accepts as CONFIRMATION that a
/// written prompt was actually submitted rather than merely landed on the
/// PTY. Without that confirmation every delivery against this fixture — the
/// spawn-time seed included — is stuck permanently PROVISIONAL. Before fork
/// #194, `MAX_PAYLOAD_SUBMISSIONS` (`src/prompt_delivery.rs`) fired one
/// automatic replacement write ~500ms later regardless of whether a real
/// re-assertion was ever requested; `orchestration/remit/002` and `_003`
/// pinned behaviour that only started once that spurious second write
/// stopped happening. As of fork #194 the constant is 1, so attempt 2 only
/// probes submission — no payload bytes, so no second log line — and the
/// spurious-write wait these tests were built around no longer occurs.
/// Confirming does not touch the log: only the `read` loop's own
/// `printf` line appends to `orchestrator-prompt.log`, so the tests' log
/// substring counts still measure exactly what was delivered to the pane.
///
/// The background/foreground split is load-bearing, not stylistic: a
/// non-interactive POSIX shell reassigns an ASYNCHRONOUS (`&`) job's stdin to
/// `/dev/null` unless that job never touches stdin at all, so an earlier
/// version of this script that ran `cat >> orchestrator-prompt.log &` in the
/// background silently read nothing — the delivered pointer landed on the
/// real PTY (visible on the rendered grid) but never reached the log,
/// producing a false RED against this file's own precondition assertion
/// instead of the feature under test (caught reading PR #177's first CI run:
/// all three tests failed at the identical precondition line with the
/// pointer plainly visible in the failure's `Final grid` dump). The
/// `emit_target` and `confirm_submission` subshells below never read stdin,
/// so backgrounding or forking them is unaffected; the `read` loop stays in
/// the foreground, so it keeps the real PTY stdin. Mirrors the `emit_target`
/// helper `tests/e2e_pane_send_result.rs::pane_input_007` uses for the
/// identical raw hook-socket `session_start` technique.
const ORCHESTRATOR_REMIT_SCRIPT: &str = r#"#!/bin/sh
emit_target() {
    WRITABLE="$1" python3 - <<'PY'
import datetime
import json
import os
import socket

pane = os.environ["DOT_AGENT_DECK_PANE_ID"]
payload = {
    "session_id": "remit-reassert-boot-session",
    "agent_type": "codex",
    "event_type": "session_start",
    "timestamp": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "pane_id": pane,
    "agent_id": os.environ.get("DOT_AGENT_DECK_AGENT_ID"),
    "live_target": {
        "kind": "pty" if os.environ["WRITABLE"] == "live" else "process",
        "writable": os.environ["WRITABLE"],
    },
}
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(os.environ["DOT_AGENT_DECK_SOCKET"])
s.sendall((json.dumps(payload) + "\n").encode())
s.close()
PY
}

confirm_submission() {
    SUBMITTED="$1" python3 - <<'PY'
import datetime
import json
import os
import socket

pane = os.environ["DOT_AGENT_DECK_PANE_ID"]
payload = {
    "session_id": "remit-reassert-boot-session",
    "agent_type": "codex",
    "event_type": "thinking",
    "timestamp": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "pane_id": pane,
    "agent_id": os.environ.get("DOT_AGENT_DECK_AGENT_ID"),
    "user_prompt": os.environ["SUBMITTED"],
}
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(os.environ["DOT_AGENT_DECK_SOCKET"])
s.sendall((json.dumps(payload) + "\n").encode())
s.close()
PY
}

(
    emit_target live
    touch initial-live-emitted

    while [ ! -f go-history-only ]; do sleep 0.05; done
    emit_target history-only
    touch history-only-emitted

    while [ ! -f go-live-again ]; do sleep 0.05; done
    emit_target live
    touch relive-emitted
) &

while IFS= read -r line; do
    printf '%s\n' "$line" >> orchestrator-prompt.log
    confirm_submission "$line"
done
"#;

// `write_executable` and `open_orchestration` (the latter behaves
// identically here — with no `[[modes]]` defined the Mode chip row is
// `[No mode] [Orch: remit-reassert] [schedule]`, so ONE Right selects the
// orchestration, and selecting it hides the Command field so a second Enter
// submits the form) are shared with `tests/e2e_orchestration_newpane.rs` via
// `tests/common/mod.rs` — the two files' private copies were flagged by
// SonarCloud's new-code duplication gate (issue #521 fix round). See
// `common::open_orchestration`'s doc comment for why this pair,
// specifically, is shared rather than following this suite's usual per-file
// convention.

/// The fixture's full daemon registry record for `role`. Mirrors
/// `tests/e2e_orchestration_focus.rs::role_agent_record`.
fn role_agent_record(
    socket: &std::path::Path,
    role: &str,
) -> dot_agent_deck::agent_pty::AgentRecord {
    common::agent_records_on(socket)
        .into_iter()
        .find(|r| {
            matches!(
                &r.tab_membership,
                Some(dot_agent_deck::agent_pty::TabMembership::Orchestration { role_name, .. })
                    if role_name == role
            )
        })
        .unwrap_or_else(|| {
            panic!("remit-reassert-orchestration fixture's {role} role pane must be registered with the daemon")
        })
}

/// Inject a synthetic `Compacting` `AgentEvent` for the given pane/agent
/// identity over the deck's hook socket, and block until the daemon's own
/// `ListAgents`/live-status join reports `SessionStatus::Compacting` for
/// that pane — proof the daemon's state (not just the wire) reflects the
/// change before the caller starts asserting on anything driven by it.
/// Mirrors `tests/e2e_orchestration_focus.rs::inject_role_status`,
/// specialized to the one event type this file needs.
#[cfg(unix)]
fn inject_compacting(
    deck: &TuiDeck,
    socket: &std::path::Path,
    pane_id: &str,
    agent_id: &str,
    session_id: &str,
) {
    let event = AgentEvent {
        session_id: session_id.to_string(),
        agent_type: AgentType::Codex,
        event_type: EventType::Compacting,
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
    let line = serde_json::to_string(&event).expect("serialize synthetic Compacting AgentEvent");
    common::write_hook_line(deck.hook_socket_path(), &line)
        .expect("inject synthetic Compacting AgentEvent over hook socket");

    let applied = common::wait_until(Duration::from_secs(10), || {
        common::agent_records_on(socket).into_iter().any(|r| {
            r.pane_id_env.as_deref() == Some(pane_id)
                && r.live.as_ref().map(|s| &s.status)
                    == Some(&dot_agent_deck::state::SessionStatus::Compacting)
        })
    });
    assert!(
        applied,
        "the daemon's own ListAgents/live-status join never reported Compacting \
         for pane {pane_id} (agent_id {agent_id}) within 10s — the hook socket write \
         was accepted, but AppState::apply_event may have rejected it or applied it \
         to the wrong session.",
    );
}

/// PRD #655: inject a synthetic `/clear`-shaped `SessionStart` `AgentEvent`
/// for the given pane/agent identity over the deck's hook socket, and block
/// until the daemon's own `ListAgents`/live-status join reports
/// `SessionStatus::Idle` for that pane — proof the daemon's state (not just
/// the wire) reflects the change before the caller starts asserting on
/// anything driven by it. Mirrors [`inject_compacting`] above, specialized to
/// the `/clear` trigger this PRD adds: `AppState::apply_event`'s
/// `EventType::SessionStart` arm unconditionally sets `session.status =
/// SessionStatus::Idle` (`src/state.rs`), which is the one observable,
/// already-existing state transition available to poll on — there is no
/// persisted "clear was observed" status the way `Compacting` is a status in
/// its own right, since a `SessionStart` is a point-in-time event (PRD #655's
/// Solution Overview, point 2). The event carries no `live_target`, exactly
/// like `inject_compacting` above, and the same-agent reuse guard in
/// `AppState::apply_event` keeps this update on the SAME session card
/// (keyed by pane/agent identity, not by `session_id`) rather than spawning a
/// second one, so a differing synthetic `session_id` per call is safe here
/// too.
///
/// The `metadata` map carries [`CLEAR_SESSION_START_METADATA_KEY`] /
/// [`CLEAR_SESSION_START_METADATA_VALUE`] (`dot_agent_deck::event`) — the
/// real production constants M1 (`src/hook.rs`) forwards
/// `ClaudeCodeHookInput.source == "clear"` into.
///
/// `agent_type` is a caller-supplied parameter, not a hardcoded value: PRD
/// #655's stated scope is Claude Code only, so `orchestration_remit_004`/
/// `_005` pass [`AgentType::ClaudeCode`] to exercise the trigger within
/// scope, while `orchestration_remit_006` deliberately passes a non-Claude
/// `agent_type` to prove events outside that scope are ignored.
#[cfg(unix)]
fn inject_clear_session_start(
    deck: &TuiDeck,
    socket: &std::path::Path,
    pane_id: &str,
    agent_id: &str,
    session_id: &str,
    agent_type: AgentType,
) {
    let mut metadata = std::collections::HashMap::new();
    metadata.insert(
        CLEAR_SESSION_START_METADATA_KEY.to_string(),
        CLEAR_SESSION_START_METADATA_VALUE.to_string(),
    );
    let event = AgentEvent {
        session_id: session_id.to_string(),
        agent_type,
        event_type: EventType::SessionStart,
        tool_name: None,
        tool_detail: None,
        cwd: None,
        timestamp: chrono::Utc::now(),
        user_prompt: None,
        metadata,
        pane_id: Some(pane_id.to_string()),
        agent_id: Some(agent_id.to_string()),
        agent_version: None,
        schema_version: None,
        live_target: None,
        model: None,
    };
    let line = serde_json::to_string(&event)
        .expect("serialize synthetic clear-originated SessionStart AgentEvent");
    common::write_hook_line(deck.hook_socket_path(), &line)
        .expect("inject synthetic clear-originated SessionStart AgentEvent over hook socket");

    let applied = common::wait_until(Duration::from_secs(10), || {
        common::agent_records_on(socket).into_iter().any(|r| {
            r.pane_id_env.as_deref() == Some(pane_id)
                && r.live.as_ref().map(|s| &s.status)
                    == Some(&dot_agent_deck::state::SessionStatus::Idle)
        })
    });
    assert!(
        applied,
        "the daemon's own ListAgents/live-status join never reported Idle for pane \
         {pane_id} (agent_id {agent_id}) within 10s after injecting a synthetic \
         clear-originated SessionStart — the hook socket write was accepted, but \
         AppState::apply_event may have rejected it or applied it to the wrong session.",
    );
}

/// Open the orchestration, write and launch the orchestrator's synthetic
/// script, and confirm the spawn-time remit pointer lands once. Returns the
/// daemon socket path, the start role's `(pane_id, agent_id)`, the log path
/// every test in this file asserts delivery counts against, and the role's
/// own cwd (the isolated clone, per PRD fork#544 M2b — NOT
/// `deck.workdir()`) for tests that need to write further trigger files
/// there themselves.
fn open_and_confirm_initial_delivery(
    deck: &TuiDeck,
) -> (
    std::path::PathBuf,
    String,
    String,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    deck.wait_for_string("No active sessions");
    write_executable(
        &deck.workdir().join("orchestrator-remit.sh"),
        ORCHESTRATOR_REMIT_SCRIPT,
    );

    // Isolated-clone provisioning needs a ref to branch from — an unborn
    // HEAD (the harness's own bare `git init`) does not provide one. `git
    // clone` only carries COMMITTED content into the isolated clone, so the
    // orchestrator's `./orchestrator-remit.sh` role command (just written
    // above) must be committed too, or its spawn fails inside the clone
    // with the script missing.
    commit_fixture(deck.workdir());
    common::run_git(deck.workdir(), &["add", "orchestrator-remit.sh"]);
    common::run_git(
        deck.workdir(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "remit script",
        ],
    );
    open_orchestration(deck);
    deck.wait_for_absence("New Agent");

    let socket = deck.attach_socket_path().to_path_buf();
    let record = role_agent_record(&socket, "orchestrator");
    let pane_id = record
        .pane_id_env
        .clone()
        .expect("orchestrator role pane must have a DOT_AGENT_DECK_PANE_ID recorded");
    let agent_id = record.id.clone();

    // PRD fork#544 M2b: isolation is unconditional, so the orchestrator
    // role's script runs (and writes this log) inside its own isolated
    // clone, not `deck.workdir()` — read the daemon's own record of the
    // role pane's cwd rather than assuming it's the fixture's source dir.
    let role_cwd = record
        .cwd
        .clone()
        .expect("orchestrator role pane must have a recorded cwd");
    let role_cwd = std::path::PathBuf::from(role_cwd);
    let log = role_cwd.join("orchestrator-prompt.log");
    let initial_delivered =
        common::wait_for_file_substr_count(&log, DELIVERED_POINTER, 1, Duration::from_secs(10));
    assert!(
        initial_delivered,
        "precondition failed: the spawn-time orchestrator prompt never reached the \
         start role's pane within 10s\nFinal grid:\n{}",
        deck.snapshot_grid()
    );

    (socket, pane_id, agent_id, log, role_cwd)
}

/// Scenario: Open a real orchestration tab and let the start role's
/// spawn-time remit pointer deliver once, then inject a `Compacting` event
/// for that SAME start-role pane. The pointer must reach the pane's stdin a
/// second time — the orchestrator's remit re-asserting itself on compaction
/// (issue #423), rather than only ever being delivered at spawn.
#[spec("orchestration/remit/001")]
#[test]
#[cfg(unix)]
fn orchestration_remit_001_start_role_compaction_reasserts_remit() {
    let deck = TuiDeck::launch_with_fixture("remit-reassert-orchestration");
    let (socket, pane_id, agent_id, log, _role_cwd) = open_and_confirm_initial_delivery(&deck);

    inject_compacting(
        &deck,
        &socket,
        &pane_id,
        &agent_id,
        &format!("{agent_id}-remit001-session"),
    );

    let reasserted =
        common::wait_for_file_substr_count(&log, DELIVERED_POINTER, 2, Duration::from_secs(10));
    assert!(
        reasserted,
        "a Compacting event on the orchestrator start-role pane must re-deliver the \
         `{DELIVERED_POINTER}` remit pointer a second time (issue #423); the log only \
         shows it once within 10s.\nFinal grid:\n{}",
        deck.snapshot_grid()
    );
}

/// Scenario: In the same orchestration, `Compacting` fires first on the
/// non-start `worker` role's pane — this must NOT re-deliver the remit
/// pointer to the start role. Then, as a positive control proving this is a
/// genuine scoping guard and not just an unimplemented feature vacuously
/// passing the negative check, `Compacting` fires on the orchestrator start
/// role itself, which MUST re-deliver. The guard against re-assertion
/// leaking into every pane of an orchestration (issue #423's stated scope:
/// the orchestrator start role only).
#[spec("orchestration/remit/002")]
#[test]
#[cfg(unix)]
fn orchestration_remit_002_non_start_role_compaction_reasserts_nothing() {
    let deck = TuiDeck::launch_with_fixture("remit-reassert-orchestration");
    let (socket, orch_pane_id, orch_agent_id, log, _role_cwd) =
        open_and_confirm_initial_delivery(&deck);

    let worker_record = role_agent_record(&socket, "worker");
    let worker_pane_id = worker_record
        .pane_id_env
        .clone()
        .expect("worker role pane must have a DOT_AGENT_DECK_PANE_ID recorded");
    let worker_agent_id = worker_record.id.clone();

    inject_compacting(
        &deck,
        &socket,
        &worker_pane_id,
        &worker_agent_id,
        &format!("{worker_agent_id}-remit002-worker-session"),
    );

    let leaked_to_worker =
        common::wait_for_file_substr_count(&log, DELIVERED_POINTER, 2, Duration::from_millis(900));
    assert!(
        !leaked_to_worker,
        "a Compacting event on the non-start `worker` role's pane must not re-assert \
         the orchestrator's remit; the start role's delivery log reached a second \
         `{DELIVERED_POINTER}` line anyway.\nFinal grid:\n{}",
        deck.snapshot_grid()
    );

    inject_compacting(
        &deck,
        &socket,
        &orch_pane_id,
        &orch_agent_id,
        &format!("{orch_agent_id}-remit002-orch-session"),
    );
    let reasserted_on_start_role =
        common::wait_for_file_substr_count(&log, DELIVERED_POINTER, 2, Duration::from_secs(10));
    assert!(
        reasserted_on_start_role,
        "control failed: a Compacting event on the orchestrator START role must still \
         re-deliver the remit pointer in this same orchestration — the negative check \
         above is only meaningful if this positive control also passes.\nFinal grid:\n{}",
        deck.snapshot_grid()
    );
}

/// Scenario: The one issue #423 cares most about. `Compacting` fires on the
/// start role while its pane currently declares itself history-only (not
/// writable). The re-assertion must NOT write blindly — the pointer must
/// stay undelivered, with the same `History-only session cannot accept live
/// input` feedback the spawn-time seed already surfaces for a non-applied
/// `SendResult` — until the SAME pane later declares itself live again, at
/// which point the deferred re-assertion must complete. Proves re-assertion
/// goes through the seed's own readiness-gating and delivery-confirmation
/// discipline rather than a direct, unconfirmed write (the exact bug class
/// issue #424 exists to fix, reintroduced inside this feature would be
/// worse: a stray unsubmitted line on a mid-session pane is even easier to
/// miss than at spawn).
#[spec("orchestration/remit/003")]
#[test]
#[cfg(unix)]
fn orchestration_remit_003_reassertion_waits_for_confirmed_delivery() {
    let deck = TuiDeck::launch_with_fixture("remit-reassert-orchestration");
    let (socket, pane_id, agent_id, log, role_cwd) = open_and_confirm_initial_delivery(&deck);

    // PRD fork#544 M2b: isolation is unconditional, so the orchestrator
    // role's script polls for these trigger files (and writes its own
    // marker) inside its own isolated clone, not `deck.workdir()` — use the
    // role's own recorded cwd, exactly as `log` above already does.
    std::fs::write(role_cwd.join("go-history-only"), "")
        .expect("trigger the fixture script's history-only phase");
    assert!(
        common::wait_until(Duration::from_secs(5), || {
            role_cwd.join("history-only-emitted").exists()
        }),
        "the fixture script never emitted its history-only session_start within 5s"
    );

    // Reuse the fixture's own boot session id here, unlike `_001`/`_002`'s
    // synthetic per-call ids: a real `Compacting` hook carries the agent's
    // own session id (its `PreCompact` originates from that agent's own
    // process), so a differing synthetic id models an event shape that does
    // not occur in production. `_003` is the only test in this file whose
    // flow (history-only -> live-again) makes the resulting id-overwrite
    // observable, which is why only this call needs the boot id.
    inject_compacting(
        &deck,
        &socket,
        &pane_id,
        &agent_id,
        "remit-reassert-boot-session",
    );

    let wrote_blindly =
        common::wait_for_file_substr_count(&log, DELIVERED_POINTER, 2, Duration::from_millis(900));
    assert!(
        !wrote_blindly,
        "a Compacting-triggered re-assertion must not write to a history-only pane \
         before delivery is confirmed; the pointer reached the log a second time while \
         the pane was still history-only.\nFinal grid:\n{}",
        deck.snapshot_grid()
    );

    let feedback = deck.wait_for_grid_string_within(
        "History-only session cannot accept live input",
        Duration::from_secs(5),
    );

    std::fs::write(role_cwd.join("go-live-again"), "")
        .expect("trigger the fixture script's return-to-live phase");
    let delivered_once_live =
        common::wait_for_file_substr_count(&log, DELIVERED_POINTER, 2, Duration::from_secs(10));

    assert!(
        feedback,
        "a deferred re-assertion attempt against a history-only pane must surface the \
         same visible feedback the spawn-time seed uses for a non-applied SendResult\n\
         Final grid:\n{}",
        deck.snapshot_grid()
    );
    assert!(
        delivered_once_live,
        "once the start-role pane reports itself live again, the deferred \
         re-assertion must complete and deliver the pointer a second time\n\
         Final grid:\n{}",
        deck.snapshot_grid()
    );
}

/// Scenario: PRD #655. Open a real orchestration tab and let the start
/// role's spawn-time remit pointer deliver once, then inject a synthetic
/// `SessionStart` for that SAME start-role pane carrying the
/// `/clear`-originated marker (`CLEAR_SESSION_START_METADATA_KEY` /
/// `CLEAR_SESSION_START_METADATA_VALUE`). The pointer must reach the pane's
/// stdin a second time — the orchestrator's remit re-asserting itself on
/// `/clear`, exactly as issue #423 already made it re-assert on compaction,
/// via the same reused delivery machinery (PRD #655's Decisions section).
#[spec("orchestration/remit/004")]
#[test]
#[cfg(unix)]
fn orchestration_remit_004_start_role_clear_reasserts_remit() {
    let deck = TuiDeck::launch_with_fixture("remit-reassert-orchestration");
    let (socket, pane_id, agent_id, log, _role_cwd) = open_and_confirm_initial_delivery(&deck);

    inject_clear_session_start(
        &deck,
        &socket,
        &pane_id,
        &agent_id,
        &format!("{agent_id}-remit004-session"),
        AgentType::ClaudeCode,
    );

    let reasserted =
        common::wait_for_file_substr_count(&log, DELIVERED_POINTER, 2, Duration::from_secs(10));
    assert!(
        reasserted,
        "a `/clear`-originated SessionStart event on the orchestrator start-role pane \
         must re-deliver the `{DELIVERED_POINTER}` remit pointer a second time (PRD \
         #655); the log only shows it once within 10s.\nFinal grid:\n{}",
        deck.snapshot_grid()
    );

    // PRD #655 review round, finding F2b: pin non-repetition, not just
    // arrival — a single `/clear`-originated `SessionStart` event must
    // deliver the pointer exactly once more, never repeatedly. Mirrors the
    // "stays put" shape `orchestration_remit_002`/`_005` already use for
    // their negative leak checks (`!wait_for_file_substr_count(..., short
    // bound)`), applied here to the count staying AT 2 rather than never
    // reaching 2.
    let repeated_beyond_two =
        common::wait_for_file_substr_count(&log, DELIVERED_POINTER, 3, Duration::from_millis(900));
    assert!(
        !repeated_beyond_two,
        "a single `/clear`-originated SessionStart event must not re-deliver the remit \
         pointer more than once; the log reached a third `{DELIVERED_POINTER}` line \
         within a bounded wait after the second.\nFinal grid:\n{}",
        deck.snapshot_grid()
    );
}

/// Scenario: PRD #655. In the same orchestration, a `/clear`-originated
/// `SessionStart` fires first on the non-start `worker` role's pane — this
/// must NOT re-deliver the remit pointer to the start role. Then, as a
/// positive control proving this is a genuine scoping guard and not just an
/// unimplemented feature vacuously passing the negative check, the same
/// `/clear`-originated `SessionStart` fires on the orchestrator start role
/// itself, which MUST re-deliver. Mirrors `orchestration_remit_002`'s exact
/// pattern for the compaction trigger, extended to this PRD's `/clear`
/// trigger: the guard against re-assertion leaking into every pane of an
/// orchestration applies identically to both triggers (issue #423's stated
/// scope, reused unchanged by PRD #655).
#[spec("orchestration/remit/005")]
#[test]
#[cfg(unix)]
fn orchestration_remit_005_non_start_role_clear_reasserts_nothing() {
    let deck = TuiDeck::launch_with_fixture("remit-reassert-orchestration");
    let (socket, orch_pane_id, orch_agent_id, log, _role_cwd) =
        open_and_confirm_initial_delivery(&deck);

    let worker_record = role_agent_record(&socket, "worker");
    let worker_pane_id = worker_record
        .pane_id_env
        .clone()
        .expect("worker role pane must have a DOT_AGENT_DECK_PANE_ID recorded");
    let worker_agent_id = worker_record.id.clone();

    inject_clear_session_start(
        &deck,
        &socket,
        &worker_pane_id,
        &worker_agent_id,
        &format!("{worker_agent_id}-remit005-worker-session"),
        AgentType::ClaudeCode,
    );

    let leaked_to_worker =
        common::wait_for_file_substr_count(&log, DELIVERED_POINTER, 2, Duration::from_millis(900));
    assert!(
        !leaked_to_worker,
        "a `/clear`-originated SessionStart event on the non-start `worker` role's pane \
         must not re-assert the orchestrator's remit; the start role's delivery log \
         reached a second `{DELIVERED_POINTER}` line anyway.\nFinal grid:\n{}",
        deck.snapshot_grid()
    );

    inject_clear_session_start(
        &deck,
        &socket,
        &orch_pane_id,
        &orch_agent_id,
        &format!("{orch_agent_id}-remit005-orch-session"),
        AgentType::ClaudeCode,
    );
    let reasserted_on_start_role =
        common::wait_for_file_substr_count(&log, DELIVERED_POINTER, 2, Duration::from_secs(10));
    assert!(
        reasserted_on_start_role,
        "control failed: a `/clear`-originated SessionStart event on the orchestrator \
         START role must still re-deliver the remit pointer in this same orchestration \
         — the negative check above is only meaningful if this positive control also \
         passes.\nFinal grid:\n{}",
        deck.snapshot_grid()
    );
}

/// Scenario: PRD #655 review round, finding F4. In the same orchestration, a
/// `/clear`-originated `SessionStart` fires on the orchestrator START role's
/// own pane, but stamped with a non-Claude-Code `agent_type` — this must NOT
/// re-deliver the remit pointer, since the PRD's stated scope is Claude Code
/// only (`AgentType::ClaudeCode`). Then, as a positive control proving this
/// is a genuine scope guard and not just an unimplemented feature vacuously
/// passing the negative check, the same `/clear`-originated `SessionStart`
/// fires again on the SAME pane, this time stamped `AgentType::ClaudeCode`,
/// which MUST re-deliver. Mirrors `orchestration_remit_002`'s pattern of
/// pairing a negative check with a positive control, but proves the
/// agent-type axis rather than the pane-identity axis `_002`/`_005` already
/// cover.
///
/// Expected RED right now: the Claude-Code-only scope gate this test pins
/// does not exist yet (it's the coder's next delegation in this same fix
/// round) — the daemon applies the injected event (status still transitions
/// to `Idle`, per `inject_clear_session_start`'s own precondition wait) but
/// today's re-assertion trigger doesn't check `agent_type` at all, so the
/// pointer is wrongly re-delivered anyway.
#[spec("orchestration/remit/006")]
#[test]
#[cfg(unix)]
fn orchestration_remit_006_non_claude_agent_type_clear_reasserts_nothing() {
    let deck = TuiDeck::launch_with_fixture("remit-reassert-orchestration");
    let (socket, pane_id, agent_id, log, _role_cwd) = open_and_confirm_initial_delivery(&deck);

    inject_clear_session_start(
        &deck,
        &socket,
        &pane_id,
        &agent_id,
        &format!("{agent_id}-remit006-non-claude-session"),
        AgentType::Codex,
    );

    let leaked_for_non_claude_agent_type =
        common::wait_for_file_substr_count(&log, DELIVERED_POINTER, 2, Duration::from_millis(900));
    assert!(
        !leaked_for_non_claude_agent_type,
        "a `/clear`-originated SessionStart event stamped with a non-Claude-Code \
         `agent_type` must not re-assert the orchestrator's remit (PRD #655's stated \
         scope is Claude Code only); the start role's delivery log reached a second \
         `{DELIVERED_POINTER}` line anyway.\nFinal grid:\n{}",
        deck.snapshot_grid()
    );

    inject_clear_session_start(
        &deck,
        &socket,
        &pane_id,
        &agent_id,
        &format!("{agent_id}-remit006-claude-session"),
        AgentType::ClaudeCode,
    );
    let reasserted_for_claude_code =
        common::wait_for_file_substr_count(&log, DELIVERED_POINTER, 2, Duration::from_secs(10));
    assert!(
        reasserted_for_claude_code,
        "control failed: a `/clear`-originated SessionStart event stamped \
         `AgentType::ClaudeCode` on the orchestrator START role's own pane must still \
         re-deliver the remit pointer — the negative check above is only meaningful if \
         this positive control also passes.\nFinal grid:\n{}",
        deck.snapshot_grid()
    );
}
