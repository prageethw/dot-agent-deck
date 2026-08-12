#![cfg(feature = "e2e")]

//! PTY-attached coverage for the daemon idle-worker detector. The synthetic
//! case opens the `orch-deck` fixture's live `cat` role panes and injects a
//! Delegate over its hook socket; the real-agent case restores an orchestration
//! whose interactive Claude Haiku orchestrator delegates to a silent worker.
//! Both must render the daemon's idle prompt in the orchestrator surface.

mod common;

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::time::Duration;

use common::TuiDeck;
use dot_agent_deck::config;
use dot_agent_deck::daemon_protocol::TabMembership;
use dot_agent_deck::event::{DaemonMessage, DelegateSignal};
use spec::spec;

/// Observation budget for `idle_worker_011`'s two grid waits, deliberately
/// held at the **pre-PR-#82 value of 20s** rather than following
/// `common::OBSERVATION_BUDGET` (30s).
///
/// Fork issue #81's last open item is characterising this test: is it runner
/// starvation, or a real defect somewhere in the hook-ingestion → role-lookup
/// → timer → daemon-delivery → render pipeline? That question can only be
/// answered from a failure captured *with a diagnostic attached*, and the two
/// changes landed in the wrong order — PR #82 widened these waits 20s → 30s
/// (against the issue's own review, which said not to widen before
/// instrumenting), and PR #116 added [`idle_timeout_diagnostics`] only
/// afterwards. The result was a detector aimed at a signal that had already
/// been turned down, leaving the item open on evidence that may never arrive.
///
/// Narrowing back restores the original failure rate now that the diagnostic
/// exists on both timeout paths. This is cheap to run as an experiment
/// because the `e2e` job is `continue-on-error: true`, so a recurrence is
/// non-blocking noise rather than a merge gate.
///
/// **Resolving this constant is the point, not keeping it.** If a run fires
/// with a diagnostic dump, fix what it names and delete this in favour of
/// `OBSERVATION_BUDGET`. If a stretch of runs stays green, that is evidence
/// too — 20s was never the problem — and it should go the same way. It is
/// scoped to this one test on purpose: `OBSERVATION_BUDGET` is shared by 134
/// sites and none of them are under investigation.
const IDLE_DETECTION_BUDGET: Duration = Duration::from_secs(20);

const REAL_ORCHESTRATION_NAME: &str = "idle-worker-real";
const REAL_ORCHESTRATOR_MODEL: &str = "claude-haiku-4-5-20251001";
const REAL_WORKER_ROLE: &str = "worker";

/// The daemon-authored opening clause of `compose_idle_worker_prompt`.
///
/// The bare `has not responded` this used to match was **not** proof of
/// provenance: a real orchestrator can write those words itself while
/// explaining why it is waiting on a worker, so the needle could be satisfied
/// by the very model whose input it is supposed to be verifying. The
/// parenthetical is the anchor — the prompt declares itself a daemon report
/// and explicitly not a message from a person or an agent, which a model
/// narrating its own state has no reason to emit verbatim.
const IDLE_DAEMON_CLAUSE: &str = "has not responded with work-done (dot-agent-deck daemon report, \
                                  not a message from a person or an agent)";

/// The second daemon-specific anchor: the role name is wrapped in unforgeable
/// untrusted-data markers (PRD #126 M1 audit finding 1), so matching the
/// WRAPPED form proves both that the daemon composed the line and that it
/// framed the role as data.
fn idle_role_label(role: &str) -> String {
    format!("[UNTRUSTED-ROLE-LABEL: {role} :END-UNTRUSTED-ROLE-LABEL]")
}

/// Wrap-tolerant wait for `needle` inside the orchestration PANE COLUMN.
///
/// Every needle goes through [`common::squeeze_wrapped_text`] because the idle
/// prompt is ONE long line: the pane wraps it at whatever column the pane happens
/// to sit at, and a needle straddling that column is absent from the row-joined
/// snapshot even though every character of it is on screen. Only
/// [`IDLE_DAEMON_CLAUSE`] *opens* the line and would be safe to match raw; the
/// untrusted-role label sits deep in the text and can land anywhere.
///
/// NOT a wrap-tolerant whole-grid search: this crops to the pane column via
/// [`common::orchestration_pane_column`] and so inherits both of that
/// function's preconditions — the fixture's `start = true` role must be named
/// literally `orchestrator`, and its pane must render as an EXPANDED box. A
/// needle rendered in the SIDEBAR, or anywhere while the orchestrator's pane is
/// collapsed, will never be found here however long the timeout.
///
/// Returns which of the two distinct failures happened rather than one
/// undifferentiated `false` (review of #465, S4). Collapsing them was a real
/// diagnosability regression against the whole-grid search this replaced, which
/// had no anchor to lose: a collapsed pane draws no corner glyph, so the crop
/// returns `None` on every poll and the operator was told the idle prompt never
/// arrived when it may have been rendering perfectly. It bites hardest in
/// `idle_worker_012`, which is credential-gated and so never runs in CI.
fn wait_for_wrapped_pane_string(
    deck: &TuiDeck,
    needle: &str,
    timeout: Duration,
) -> Result<(), String> {
    let squeezed = common::squeeze_wrapped_text(needle);
    // `wait_until` takes a `Fn`, so the "did the anchor EVER appear" flag needs
    // interior mutability. Tracking it across the whole poll loop beats
    // re-checking once after the timeout: it distinguishes a pane that was
    // never expanded at all from one that merely happened to be collapsed on
    // the final frame.
    let anchor_seen = Cell::new(false);
    let found = common::wait_until(timeout, || {
        let Some(pane) = common::orchestration_pane_column(&deck.snapshot_grid()) else {
            return false;
        };
        anchor_seen.set(true);
        common::squeeze_wrapped_text(&pane).contains(&squeezed)
    });

    if found {
        Ok(())
    } else if anchor_seen.get() {
        Err(format!(
            "the orchestrator's expanded pane box WAS located, but {needle:?} never appeared \
             inside its column within {timeout:?}"
        ))
    } else {
        Err(format!(
            "the orchestrator's expanded pane box never rendered within {timeout:?}, so the \
             pane column could not be located and {needle:?} was never actually searched for \
             — the pane is collapsed or the start role is not named \"orchestrator\", neither \
             of which says anything about whether the prompt arrived"
        ))
    }
}

/// Fork issue #81: timeout-path diagnostic for `idle_worker_011`. Reads the
/// daemon's ground-truth PTY scrollback for both role panes directly over
/// the attach socket (`AttachRequest::Snapshot`, via
/// `common::pane_search_key_on`) — independent of whatever
/// `wait_for_wrapped_grid_string` happened to have scrolled into the polled
/// vt100 viewport — so a failure can tell apart:
///   - the delegate never reaching dispatch (no pointer text ever written to
///     the worker's own scrollback: the hook was dropped, the pane/role
///     gate in `AppState::handle_delegate` rejected it, or the fan-out write
///     itself failed);
///   - the delegate reaching the worker but the detector never firing (the
///     daemon-report clause never appears in the orchestrator's scrollback
///     AT ALL, not just in the current viewport: the idle timer or its
///     delivery write is broken);
///   - the detector firing with the wrong content (the clause is present in
///     scrollback but the role label is missing: a data-shape bug, not a
///     timing one);
///   - the detector firing correctly in scrollback but never showing up in
///     the polled viewport (a render/scroll lag under CI contention, not a
///     pipeline defect).
///
/// Read-only: issues no keystrokes and asserts nothing itself, so it cannot
/// perturb the budget or the outcome it is reporting on.
fn idle_timeout_diagnostics(deck: &TuiDeck, waited_for: &str) -> String {
    let socket = deck.attach_socket_path();
    let records = common::agent_records_on(socket);
    let orchestrator_id = records.iter().find_map(|r| match &r.tab_membership {
        Some(TabMembership::Orchestration {
            role_name,
            is_start_role: true,
            ..
        }) if role_name == "orchestrator" => Some(r.id.clone()),
        _ => None,
    });
    let worker_id = records.iter().find_map(|r| match &r.tab_membership {
        Some(TabMembership::Orchestration { role_name, .. }) if role_name == "worker" => {
            Some(r.id.clone())
        }
        _ => None,
    });

    let worker_scrollback = worker_id
        .as_deref()
        .map(|id| common::pane_search_key_on(socket, id))
        .unwrap_or_default();
    let orchestrator_scrollback = orchestrator_id
        .as_deref()
        .map(|id| common::pane_search_key_on(socket, id))
        .unwrap_or_default();

    let delegate_pointer_key =
        common::search_key("Read .dot-agent-deck/worker-task-worker.md for your task.");
    let delegate_accepted = worker_scrollback.contains(&delegate_pointer_key);
    let daemon_clause_written =
        orchestrator_scrollback.contains(&common::search_key(IDLE_DAEMON_CLAUSE));
    let role_label_written =
        orchestrator_scrollback.contains(&common::search_key(&idle_role_label("worker")));

    let grid = deck.snapshot_grid();
    let waited_for_in_viewport =
        common::squeeze_wrapped_text(&grid).contains(&common::squeeze_wrapped_text(waited_for));

    format!(
        "idle_worker_011 timeout diagnostics (fork #81):\n\
         - worker pane registered with the daemon: {worker_registered}\n\
         - orchestrator pane registered with the daemon: {orchestrator_registered}\n\
         - delegate accepted & dispatched to the worker (its file-pointer text \
           was ever written to the worker pane's own scrollback): {delegate_accepted}\n\
         - idle detector fired (the daemon-report clause was ever written to \
           the orchestrator pane's own scrollback, at any point): {daemon_clause_written}\n\
         - idle prompt ever carried the worker role label in that scrollback: \
           {role_label_written}\n\
         - what this wait needed ever appeared in the polled viewport at \
           timeout: {waited_for_in_viewport}\n\
         Final polled viewport:\n{grid}",
        worker_registered = worker_id.is_some(),
        orchestrator_registered = orchestrator_id.is_some(),
    )
}

fn path_with_binary_dir() -> String {
    let bin = env!("CARGO_BIN_EXE_dot-agent-deck");
    let bin_dir = Path::new(bin)
        .parent()
        .expect("test binary has a parent directory");
    format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

fn real_agent_orchestration_config(orchestrator_command: &str) -> String {
    format!(
        "[[orchestrations]]\n\
         name = \"{REAL_ORCHESTRATION_NAME}\"\n\n\
         [[orchestrations.roles]]\n\
         name = \"orchestrator\"\n\
         command = \"{orchestrator_command}\"\n\
         start = true\n\n\
         [[orchestrations.roles]]\n\
         name = \"{REAL_WORKER_ROLE}\"\n\
         command = \"cat\"\n\
         clear = false\n"
    )
}

fn real_agent_orchestration_session(
    project_dir: &str,
    orchestrator_command: &str,
    directive: &str,
) -> String {
    let session = config::SavedSession {
        panes: vec![config::SavedPane {
            dir: project_dir.to_string(),
            name: "orchestrator".to_string(),
            command: orchestrator_command.to_string(),
            mode: None,
            orchestration: Some(config::OrchestrationSnapshot {
                version: 1,
                roles: vec!["orchestrator".to_string(), REAL_WORKER_ROLE.to_string()],
                start_role_index: 0,
                orchestrator_prompt: directive.to_string(),
                config_name: REAL_ORCHESTRATION_NAME.to_string(),
                project_path: project_dir.to_string(),
                started_role_indices: vec![0],
                display_title: None,
                owner: None,
            }),
        }],
        last_command: None,
    };
    toml::to_string_pretty(&session).expect("serialize real-agent orchestration session")
}

fn open_orchestration(deck: &TuiDeck) {
    deck.send_keys(b"\x0e"); // Ctrl+n -> directory picker
    deck.send_keys(b" "); // confirm current dir -> new-pane form
    deck.wait_for_string("No mode");
    deck.send_keys(b"\x1b[C"); // select [Orch: demo-orch]
    deck.send_keys(b"\r"); // Mode -> Name
    deck.send_keys(b"\r"); // submit (Command is hidden)
}

fn orchestration_panes(deck: &TuiDeck) -> (String, String) {
    let panes = RefCell::new(None);
    let ready = common::wait_until(Duration::from_secs(10), || {
        let records = common::agent_records_on(deck.attach_socket_path());
        let orchestrator = records
            .iter()
            .find_map(|record| match &record.tab_membership {
                Some(TabMembership::Orchestration {
                    role_name,
                    is_start_role: true,
                    ..
                }) if role_name == "orchestrator" => record.pane_id_env.clone(),
                _ => None,
            });
        let worker = records
            .iter()
            .find_map(|record| match &record.tab_membership {
                Some(TabMembership::Orchestration { role_name, .. }) if role_name == "worker" => {
                    record.pane_id_env.clone()
                }
                _ => None,
            });
        if let (Some(orchestrator), Some(worker)) = (orchestrator, worker) {
            *panes.borrow_mut() = Some((orchestrator, worker));
            return true;
        }
        false
    });
    assert!(
        ready,
        "orchestration role panes were not registered within 10s; records = {:?}",
        common::agent_records_on(deck.attach_socket_path())
    );
    panes
        .into_inner()
        .expect("ready role-pane poll stores both pane ids")
}

/// Scenario: Launch the real TUI and its lazy daemon with a tiny worker-response timeout, open the two-role `orch-deck` fixture, and inject a Delegate from the orchestrator pane to the live `cat` worker over the hook socket. The worker never sends work-done, so the rendered orchestration surface must visibly carry the daemon-report clause and the worker role inside its untrusted-role-label markers after the timeout.
#[spec("scheduler/idle-worker/011")]
#[test]
fn idle_worker_011_silent_worker_prompt_is_visible_in_attached_tui() {
    let deck = TuiDeck::builder()
        .with_pty_size(120, 40)
        .with_env("DOT_AGENT_DECK_WORKER_RESPONSE_TIMEOUT_MS", "1500")
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("No active sessions");
    open_orchestration(&deck);
    deck.wait_for_string("worker");

    let (orchestrator_pane, _worker_pane) = orchestration_panes(&deck);
    let message = DaemonMessage::Delegate(DelegateSignal {
        pane_id: orchestrator_pane,
        task: "Remain silent so the idle detector can surface its prompt.".to_string(),
        to: vec!["worker".to_string()],
        timestamp: chrono::Utc::now(),
    });
    let line = serde_json::to_string(&message).expect("serialize Delegate hook message");
    common::write_hook_line(deck.hook_socket_path(), &line)
        .expect("inject Delegate over hook socket");

    wait_for_wrapped_pane_string(&deck, IDLE_DAEMON_CLAUSE, IDLE_DETECTION_BUDGET).unwrap_or_else(
        |why| {
            panic!(
                "the daemon-authored idle prompt never became visible in the attached \
                 orchestration pane: {why}\nFinal grid:\n{}\n{}",
                deck.snapshot_grid(),
                idle_timeout_diagnostics(&deck, IDLE_DAEMON_CLAUSE)
            )
        },
    );
    wait_for_wrapped_pane_string(&deck, &idle_role_label("worker"), IDLE_DETECTION_BUDGET)
        .unwrap_or_else(|why| {
            panic!(
                "the idle prompt did not carry the silent role inside its untrusted-role-label \
                 markers: {why}\nFinal grid:\n{}\n{}",
                deck.snapshot_grid(),
                idle_timeout_diagnostics(&deck, &idle_role_label("worker"))
            )
        });
}

/// Scenario: Restore a two-role orchestration whose real interactive Claude Haiku orchestrator is directed to delegate through the `dot-agent-deck` CLI to a `cat` worker that never sends work-done. After the short detector timeout, the attached TUI must visibly render the daemon's self-identifying report clause and the worker role wrapped in its untrusted-role-label markers in the live orchestration pane.
#[spec("scheduler/idle-worker/012")]
#[test]
fn idle_worker_012_real_orchestrator_visibly_receives_idle_nudge() {
    skip_unless!(common::check_claude_available());

    let orchestration_root = common::harness_tempdir().expect("orchestration root tempdir");
    let project_dir = orchestration_root.path().join("project");
    std::fs::create_dir_all(&project_dir).expect("create orchestration project directory");
    let project_dir = project_dir
        .canonicalize()
        .expect("canonicalize orchestration project directory");
    let project_str = project_dir
        .to_str()
        .expect("orchestration project directory is UTF-8")
        .to_string();
    let _ = std::process::Command::new("git")
        .arg("init")
        .arg("--quiet")
        .current_dir(&project_dir)
        .status();

    let orchestrator_command =
        format!("claude --model {REAL_ORCHESTRATOR_MODEL} --allowedTools Bash");
    let directive = format!(
        "You are the orchestrator in a dot-agent-deck orchestration. Use the Bash tool to run \
         this exact command once: dot-agent-deck delegate --to {REAL_WORKER_ROLE} --task \
         'Remain silent and do not send work-done.' Do not do the worker task yourself and do \
         not run work-done. After the delegate command succeeds, say that you are waiting for \
         the worker, then stop."
    );

    std::fs::write(
        project_dir.join(".dot-agent-deck.toml"),
        real_agent_orchestration_config(&orchestrator_command),
    )
    .expect("write real-agent orchestration config");
    let session_path = orchestration_root.path().join("session.toml");
    std::fs::write(
        &session_path,
        real_agent_orchestration_session(&project_str, &orchestrator_command, &directive),
    )
    .expect("write real-agent orchestration session");

    let deck = TuiDeck::builder()
        .with_pty_size(200, 50)
        .with_imported_claude_credentials()
        .with_claude_project_trust(project_str.clone())
        .with_env("PATH", path_with_binary_dir())
        .with_env(
            "DOT_AGENT_DECK_SESSION",
            session_path.to_str().expect("session path is UTF-8"),
        )
        .with_env("DOT_AGENT_DECK_WORKER_RESPONSE_TIMEOUT_MS", "10000")
        .launch_with_fixture("minimal");

    assert!(
        deck.wait_for_grid_string_within(REAL_ORCHESTRATION_NAME, Duration::from_secs(45)),
        "the restored real-agent orchestration never surfaced within 45s\nFinal grid:\n{}",
        deck.snapshot_grid()
    );

    // The daemon writes this file when it dispatches a delegate, so its
    // existence proves AT LEAST ONE delegate reached the daemon. It cannot
    // prove "exactly one": a repeated delegate overwrites the same path and
    // nothing counts invocations.
    let worker_task = project_dir
        .join(".dot-agent-deck")
        .join(format!("worker-task-{REAL_WORKER_ROLE}.md"));
    assert!(
        common::wait_for_path(&worker_task, Duration::from_secs(120)),
        "the real Claude orchestrator never delegated to {REAL_WORKER_ROLE:?}; expected the \
         daemon to create {worker_task:?}\nFinal grid:\n{}",
        deck.snapshot_grid()
    );

    wait_for_wrapped_pane_string(&deck, IDLE_DAEMON_CLAUSE, Duration::from_secs(60))
        .unwrap_or_else(|why| {
            panic!(
                "the real orchestrator delegated, but the daemon-authored idle nudge never \
                 became visible in the attached orchestration pane: {why}\nFinal grid:\n{}",
                deck.snapshot_grid()
            )
        });
    wait_for_wrapped_pane_string(
        &deck,
        &idle_role_label(REAL_WORKER_ROLE),
        Duration::from_secs(30),
    )
    .unwrap_or_else(|why| {
        panic!(
            "the visible nudge did not carry the silent role inside the daemon's \
             untrusted-role-label markers, so it was not provably the daemon's own report: \
             {why}\nFinal grid:\n{}",
            deck.snapshot_grid()
        )
    });
}
