#![cfg(all(feature = "e2e", unix))]

mod common;
#[path = "support/inbox_doorbell.rs"]
mod inbox;
#[path = "support/inbox_view.rs"]
mod inbox_view;
#[path = "support/inbox_route.rs"]
mod route;

use inbox::Endpoint;
use serde_json::{Value, json};
use spec::spec;
use std::io::{BufRead, Write};
use std::path::Path;

/// Scenario: Run the real work-done CLI from an uncommissioned synthetic worker. One bound Untrusted report is fetched and rendered in the orchestrator pane, labelled as a note that completes no task, with no commissioned summary file.
#[spec("orchestration/work-done/004")]
#[test]
fn work_done_004_unsolicited_completion_is_visibly_labelled_in_the_attached_tui() {
    route::worker_report(4);
}

/// Scenario: Send an unsolicited worker report larger than the inline budget through the real CLI. The orchestrator fetches its framed head and saved-copy path from the inbox; that saved file holds the entire Untrusted-fenced report.
#[spec("orchestration/work-done/009")]
#[test]
fn work_done_009_a_cut_unsolicited_report_names_its_saved_full_copy_in_the_attached_tui() {
    route::worker_report(9);
}

/// Scenario: Delegate a task and pass a worker-owned summary file to the real work-done CLI. The source survives byte for byte, a separately framed report copy is saved, and one bound report is fetched and rendered in the orchestrator pane.
#[spec("orchestration/work-done/012")]
#[test]
fn work_done_012_task_file_at_the_summary_path_survives_in_the_attached_tui() {
    route::worker_report(12);
}

// PRD #1666 part 3, round 5: the real CLI's task-scoped `work-done`, against
// the real daemon (`/033`–`/036`, `/040`) and against a stub
// (`inbox/protocol/029`).

/// The directory the daemon's work-done seams read, under its HOME
/// (`DOT_AGENT_DECK_TEST_WORK_DONE_PAUSE_DIR`).
const SEAMS: &str = "work-done-seams";

/// A daemon whose work-done seams the test can pause and fail.
fn seamed_daemon() -> common::DaemonProc {
    inbox::daemon_unbound(&[("DOT_AGENT_DECK_TEST_WORK_DONE_PAUSE_DIR", SEAMS)])
}

fn seam(daemon: &impl Endpoint, file: &str) -> std::path::PathBuf {
    let dir = daemon.home().join(SEAMS);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(file)
}

/// A team whose worker has fetched and acknowledged one delegated task; the
/// task's id, as the worker's instructions name it.
fn delegated(daemon: &impl Endpoint) -> (inbox::Pane, inbox::Pane, String) {
    let (orch, worker) = route::team(daemon, false);
    let id = route::delegate(daemon, &orch, route::TASK, false);
    let block = route::fetched(daemon, &worker, &id, route::TASK, "the orchestrator");
    let task_id = footer_task_id(daemon, &block);
    assert_eq!(task_id, id, "the footer names the delegated message's id");
    (orch, worker, task_id)
}

/// The id after `--task-id ` in the delegated message, or else in the task
/// file the deck wrote for the worker.
fn footer_task_id(daemon: &impl Endpoint, block: &str) -> String {
    let find = |text: &str| {
        text.split("--task-id ")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .map(|id| id.trim_matches(['`', '\'', '"']).to_string())
    };
    find(block)
        .or_else(|| {
            std::fs::read_to_string(route::task_file(daemon))
                .ok()
                .and_then(|text| find(&text))
        })
        .unwrap_or_else(|| panic!("no `--task-id` in the delegated task: {block}"))
}

fn work_done(daemon: &impl Endpoint, worker: &inbox::Pane, args: &[&str]) -> std::process::Output {
    let mut argv = vec!["work-done"];
    argv.extend_from_slice(args);
    route::cli_output(daemon, worker, daemon.cwd(), &argv)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A plain delegate to the worker is refused because it still owes a task.
fn refused_as_busy(daemon: &impl Endpoint, orch: &inbox::Pane) {
    let mut req = inbox::request(orch, "delegate");
    req["task"] = json!("FOLLOW-UP-WHILE-OWED");
    req["to"] = json!(["coder"]);
    req["timestamp"] = json!(chrono::Utc::now());
    let reply = inbox::hook(daemon, req);
    assert!(
        reply["busy"]
            .as_array()
            .is_some_and(|busy| busy.iter().any(|w| w["role"] == "coder")),
        "the worker still owes its task, so a plain delegate is refused as busy: {reply}"
    );
}

/// Scenario: Delegate a task to a synthetic worker, which fetches it and runs the real `work-done` CLI with the `--task-id` its instructions name. The CLI prints that it reported that task, the orchestrator's inbox lists one report carrying that task id, and a plain follow-up delegate to the same worker is accepted.
#[spec("orchestration/work-done/033")]
#[test]
fn work_done_033_the_real_cli_reports_the_task_its_instructions_name() {
    let daemon = inbox::daemon_unbound(&[]);
    let (orch, worker, id) = delegated(&daemon);
    let out = work_done(
        &daemon,
        &worker,
        &["--task-id", &id, "--task", route::REPORT],
    );
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        format!("Reported task {id} to the orchestrator.\n")
    );
    let reports = route::count(&daemon, &orch, 1);
    assert_eq!(reports[0]["provenance"]["kind"], "task", "{}", reports[0]);
    assert_eq!(reports[0]["provenance"]["task_id"], id.as_str());
    route::delegate(&daemon, &orch, "FOLLOW-UP-AFTER-THE-REPORT", false);
}

/// Scenario: Delegate a task, then run the real `work-done` CLI without `--task-id`. The CLI says the report went to the orchestrator as a note that completes no task, the orchestrator's copy is labelled that way, and a plain follow-up delegate to the same worker is refused as busy because the task is still owed.
#[spec("orchestration/work-done/034")]
#[test]
fn work_done_034_a_report_without_a_task_id_is_a_note_and_the_task_stays_owed() {
    let daemon = inbox::daemon_unbound(&[]);
    let (orch, worker, _) = delegated(&daemon);
    let out = work_done(&daemon, &worker, &["--task", route::REPORT]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("as a note that completes no task"),
        "{}",
        text(&out.stdout)
    );
    let reports = route::count(&daemon, &orch, 1);
    assert_eq!(
        reports[0]["provenance"]["kind"], "uncorrelated",
        "{}",
        reports[0]
    );
    let block = inbox::fetch(&daemon, &orch, true).to_string();
    assert!(block.contains(route::NOTE_LABEL), "{block}");
    refused_as_busy(&daemon, &orch);
}

/// Scenario: Run the same real `work-done --task-id` command twice, as a worker retrying after a lost answer would. The orchestrator receives exactly one report, and the second run exits successfully saying the task was already reported and nothing new was sent.
#[spec("orchestration/work-done/035")]
#[test]
fn work_done_035_a_repeated_report_is_answered_once_and_delivered_once() {
    let daemon = inbox::daemon_unbound(&[]);
    let (orch, worker, id) = delegated(&daemon);
    let args = ["--task-id", id.as_str(), "--task", route::REPORT];
    let first = work_done(&daemon, &worker, &args);
    assert!(first.status.success(), "stderr: {}", text(&first.stderr));
    route::count(&daemon, &orch, 1);
    let second = work_done(&daemon, &worker, &args);
    assert!(second.status.success(), "stderr: {}", text(&second.stderr));
    assert_eq!(
        text(&second.stdout),
        format!("Task {id} was already reported; nothing new was sent.\n")
    );
    // The repeat was answered after the first report was listed, so a second
    // copy would already be listed too.
    route::count(&daemon, &orch, 1);
}

/// Scenario: With the deck's report save made to fail through its test seam, run the real `work-done --task-id`. The CLI exits 1 with the save-failed sentence, the orchestrator receives nothing, and a plain follow-up delegate is refused as busy because the task is still open; the same command run again once the fault is spent reports the task.
#[spec("orchestration/work-done/036")]
#[test]
fn work_done_036_a_failed_save_reports_nothing_and_keeps_the_task_open() {
    let daemon = seamed_daemon();
    let (orch, worker, id) = delegated(&daemon);
    std::fs::write(seam(&daemon, "work-done-fail-save.fault"), b"").unwrap();
    let args = ["--task-id", id.as_str(), "--task", route::REPORT];
    let failed = work_done(&daemon, &worker, &args);
    assert_eq!(
        failed.status.code(),
        Some(1),
        "stdout: {}",
        text(&failed.stdout)
    );
    assert!(
        text(&failed.stderr).contains("the deck could not save this report"),
        "{}",
        text(&failed.stderr)
    );
    assert!(text(&failed.stdout).is_empty(), "{}", text(&failed.stdout));
    assert!(route::messages(&daemon, &orch).is_empty());
    refused_as_busy(&daemon, &orch);
    let retried = work_done(&daemon, &worker, &args);
    assert!(
        retried.status.success(),
        "stderr: {}",
        text(&retried.stderr)
    );
    route::count(&daemon, &orch, 1);
}

fn wait_for(path: &Path) {
    assert!(
        common::wait_until(common::load_scaled(inbox::WAIT), || path.exists()),
        "the daemon never reached {}",
        path.display()
    );
}

/// Scenario: Pause the deck after it saved a worker's report and again once the report is committed, then run the real `work-done --task-id`. While paused the CLI has printed nothing and is still running, and at the second pause the orchestrator's inbox already lists the report with that task id; released, the CLI prints one `Reported task …` line. For a second task, a save failure injected while the save is paused makes the CLI exit 1 with the save-failed sentence only.
#[spec("orchestration/work-done/040")]
#[test]
fn work_done_040_the_answer_follows_the_saved_and_committed_report() {
    let daemon = seamed_daemon();
    let (orch, worker, id) = delegated(&daemon);
    std::fs::write(seam(&daemon, "work-done-saved.armed"), b"").unwrap();
    std::fs::write(seam(&daemon, "work-done-committed.armed"), b"").unwrap();
    let mut child = route::cli_command(
        &daemon,
        &worker,
        daemon.cwd(),
        &["work-done", "--task-id", &id, "--task", route::REPORT],
    )
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();

    wait_for(&seam(&daemon, "work-done-saved.reached"));
    assert!(
        child.try_wait().unwrap().is_none(),
        "the CLI answered before the commit"
    );
    std::fs::write(seam(&daemon, "work-done-saved.release"), b"").unwrap();
    wait_for(&seam(&daemon, "work-done-committed.reached"));
    assert!(
        child.try_wait().unwrap().is_none(),
        "the CLI answered before the daemon wrote its answer"
    );
    let listed = route::messages(&daemon, &orch);
    assert_eq!(listed.len(), 1, "committed before the answer: {listed:?}");
    assert_eq!(listed[0]["provenance"]["task_id"], id.as_str());
    std::fs::write(seam(&daemon, "work-done-committed.release"), b"").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        format!("Reported task {id} to the orchestrator.\n")
    );
    assert!(text(&out.stderr).is_empty(), "{}", text(&out.stderr));

    // A second task, whose save fails while paused.
    let second = route::delegate(&daemon, &orch, "ROUTE-SECOND-TASK", false);
    route::fetched(
        &daemon,
        &worker,
        &second,
        "ROUTE-SECOND-TASK",
        "the orchestrator",
    );
    std::fs::write(seam(&daemon, "work-done-saved.armed"), b"").unwrap();
    let _ = std::fs::remove_file(seam(&daemon, "work-done-saved.reached"));
    let _ = std::fs::remove_file(seam(&daemon, "work-done-saved.release"));
    let child = route::cli_command(
        &daemon,
        &worker,
        daemon.cwd(),
        &["work-done", "--task-id", &second, "--task", "SECOND-REPORT"],
    )
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
    wait_for(&seam(&daemon, "work-done-saved.reached"));
    std::fs::write(seam(&daemon, "work-done-fail-save.fault"), b"").unwrap();
    std::fs::write(seam(&daemon, "work-done-saved.release"), b"").unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stdout).is_empty(), "{}", text(&out.stdout));
    let stderr = text(&out.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(
        stderr.contains("the deck could not save this report"),
        "{stderr}"
    );
    assert_eq!(
        route::messages(&daemon, &orch).len(),
        1,
        "nothing new was posted"
    );
}

/// The real CLI run against a stub hook socket at `socket`.
fn cli_against(socket: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(common::contained_deck_binary())
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("DOT_AGENT_DECK_SOCKET", socket)
        .env("DOT_AGENT_DECK_PANE_ID", "stub-pane")
        .output()
        .unwrap()
}

fn read_line(reader: &mut impl BufRead) -> Option<String> {
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(n) if n > 0 => Some(line),
        _ => None,
    }
}

/// Scenario: Run the real `work-done --task-id` binary against a stub hook socket. A stub that reads the capability question and never answers gets no report, and the CLI exits 1 saying the daemon did not confirm it supports task reports; a stub that confirms version 1 receives the report on that same connection, opens no second one, and its accepted answer makes the CLI print `Reported task …` and exit 0.
#[spec("inbox/protocol/029")]
#[test]
fn protocol_029_the_real_cli_asks_first_and_reports_on_the_confirming_connection() {
    let dir = common::harness_tempdir().unwrap();
    let args = ["work-done", "--task-id", "m-029", "--task", "STUB-REPORT"];

    // A daemon that never answers the question.
    let socket = dir.path().join("silent.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let stub = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = std::io::BufReader::new(stream);
        std::iter::from_fn(|| read_line(&mut reader)).collect::<Vec<_>>()
    });
    let out = cli_against(&socket, dir.path(), &args);
    let lines = stub.join().unwrap();
    assert_eq!(out.status.code(), Some(1), "stdout: {}", text(&out.stdout));
    assert!(
        text(&out.stderr).contains("did not confirm"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(lines.len(), 1, "only the question: {lines:?}");
    assert!(lines[0].contains("\"hook_capabilities\""), "{lines:?}");

    // A daemon that confirms.
    let socket = dir.path().join("confirming.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let stub = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut writer = stream.try_clone().unwrap();
        let mut reader = std::io::BufReader::new(stream);
        let question = read_line(&mut reader).unwrap();
        writer
            .write_all(b"{\"task_scoped_completion\":1}\n")
            .unwrap();
        let report = read_line(&mut reader).unwrap();
        writer
            .write_all(
                b"{\"disposition\":\"accepted\",\"task_id\":\"m-029\",\"destination\":\"inbox\"}\n",
            )
            .unwrap();
        let rest: Vec<String> = std::iter::from_fn(|| read_line(&mut reader)).collect();
        listener.set_nonblocking(true).unwrap();
        let second = listener.accept().is_ok();
        (question, report, rest, second)
    });
    let out = cli_against(&socket, dir.path(), &args);
    let (question, report, rest, second) = stub.join().unwrap();
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "Reported task m-029 to the orchestrator.\n"
    );
    assert!(question.contains("\"hook_capabilities\""), "{question}");
    let report: Value = serde_json::from_str(&report).unwrap();
    assert_eq!(report["message_type"], "work_done_task", "{report}");
    assert_eq!(
        report["mode"],
        json!({"kind":"task","task_id":"m-029"}),
        "{report}"
    );
    assert_eq!(report["report"], "STUB-REPORT");
    assert!(rest.is_empty(), "nothing after the report: {rest:?}");
    assert!(!second, "the CLI opened a second connection");
}

// ---------------------------------------------------------------------------
// SYNC-MERGE 2026-11-05 (rulings 30/32/34): everything below is the FORK's
// delegation-status e2e block (`status/observing/009,010,018,022,023`,
// #755/#803/#817), RE-POINTED at upstream's mechanism: the real `delegate` and
// `work-done` CLIs run with the deck built `.impersonating_pane_signals()` (a
// harness-launched CLI has no per-spawn capability token), `work-done` names the
// delegated message by `--task-id` (read from the worker's inbox, because
// upstream retires a delegation only for a task-scoped report; see
// `work_done_034` above), and daemon notices are read from the orchestrator's
// inbox, where the deck now posts them, not from its PTY. The fork's
// `--subject` flag, registration-generation / boot-id env and the subject
// mismatch assertions are gone with the identity layer, and the PTY-text helpers
// they used (`squeeze`, `pane_column_text`, `pane_contains`,
// `wait_for_pane_string`) went with them. The pure card-rendering helpers are
// unchanged. Needs the carried #755/#803 `DelegationArmed/Retired`
// broadcasts and the `Idle (delegated)` / `Observing` card statuses.
// ---------------------------------------------------------------------------

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::Duration;

use common::{TuiDeck, commit_fixture};
use dot_agent_deck::daemon_protocol::TabMembership;

/// The `orch-deck` fixture's non-start `cat` role.
const WORKER_ROLE: &str = "worker";

/// The production new-pane flow: `Ctrl+n` → confirm dir → Right selects the
/// `[Orch: demo-orch]` chip → Enter → Enter. This is the only path that registers
/// the daemon-side role maps `handle_work_done` routes on.
fn open_orchestration(deck: &TuiDeck) {
    // Isolated-clone provisioning needs a ref to branch from — an unborn
    // HEAD (the harness's own bare `git init`) does not provide one.
    commit_fixture(deck.workdir());
    deck.send_keys(b"\x0e");
    deck.send_keys(b" ");
    deck.wait_for_string("No mode");
    deck.send_keys(b"\x1b[C");
    deck.send_keys(b"\r");
    deck.send_keys(b"\r");
}

/// The worker's `pane_id_env` (what the CLI must report as) plus the
/// ORCHESTRATOR's registry agent id (what the daemon's PTY snapshot is keyed
/// on) plus the ORCHESTRATOR's own `pane_id_env` (what a `dot-agent-deck
/// delegate` subprocess must present via `DOT_AGENT_DECK_PANE_ID` to be
/// accepted as this orchestration's start role — `orchestration/work-done/007`
/// and `008` need it to run a real delegate from the orchestrator's identity;
/// `004` and `005` ignore it).
///
/// All three are needed because this test asserts multiple times over: once
/// that the daemon WROTE the feedback into the orchestrator's PTY, and once
/// that the TUI RENDERED it. Splitting those is what makes a failure
/// diagnosable — a daemon that never composed the line and a line that never
/// reached the grid look identical on the grid alone.
fn orchestration_ids(deck: &TuiDeck) -> (String, String, String) {
    let ids = RefCell::new(None);
    let ready = common::wait_until(Duration::from_secs(10), || {
        let records = common::agent_records_on(deck.attach_socket_path());
        let worker = records
            .iter()
            .find_map(|record| match &record.tab_membership {
                Some(TabMembership::Orchestration { role_name, .. })
                    if role_name == WORKER_ROLE =>
                {
                    record.pane_id_env.clone()
                }
                _ => None,
            });
        let orchestrator = records.iter().find_map(|record| {
            matches!(
                &record.tab_membership,
                Some(TabMembership::Orchestration {
                    is_start_role: true,
                    ..
                })
            )
            .then(|| (record.id.clone(), record.pane_id_env.clone()))
        });
        if let (Some(worker), Some((orchestrator_agent, Some(orchestrator_pane)))) =
            (worker, orchestrator)
        {
            *ids.borrow_mut() = Some((worker, orchestrator_agent, orchestrator_pane));
            return true;
        }
        false
    });
    assert!(
        ready,
        "the orchestration's role panes were not registered within 10s; records = {:?}",
        common::agent_records_on(deck.attach_socket_path())
    );
    ids.into_inner()
        .expect("the ready poll stores all three ids")
}

/// Every notice the deck has posted to `orchestrator_pane`'s inbox, joined: the
/// daemon no longer types its feedback into the orchestrator's PTY, it posts it
/// (PRD #1666), so this is where "the orchestrator was told ..." is observed.
fn orchestrator_inbox_text(deck: &TuiDeck, orchestrator_pane: &str) -> String {
    inbox_view::messages(deck.attach_socket_path(), orchestrator_pane)
        .into_iter()
        .filter_map(|message| message.text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Run the real `dot-agent-deck delegate` CLI as a subprocess against the
/// deck's own hook socket, from `caller_pane`'s identity (the technique
/// `e2e_dispatcher_mode.rs`'s `run_delegate_to` uses, reimplemented locally
/// because integration tests cannot import one another's helpers). The
/// subprocess is launched from the harness, not from `caller_pane`'s own child,
/// so it carries no per-spawn capability token: the deck is built
/// `.impersonating_pane_signals()` (see [`launch_deck_with_orchestration`]) so
/// the daemon judges the call on its pane id alone.
fn run_delegate_cli(
    deck: &TuiDeck,
    caller_pane: &str,
    to: &str,
    task: &str,
) -> std::process::Output {
    std::process::Command::new(common::contained_deck_binary())
        .arg("delegate")
        .arg("--to")
        .arg(to)
        .arg("--task")
        .arg(task)
        .env("DOT_AGENT_DECK_SOCKET", deck.hook_socket_path())
        .env("DOT_AGENT_DECK_PANE_ID", caller_pane)
        .output()
        .expect("run the real `dot-agent-deck delegate` CLI")
}

/// Run the real `dot-agent-deck work-done` CLI as a subprocess for the task
/// `task_id` (the id of the delegated message, see [`delegated_task_id`]).
/// Upstream retires a delegation only for a task-scoped report; a report with
/// no task id is a note and the task stays owed.
fn run_work_done_cli(
    deck: &TuiDeck,
    worker_pane: &str,
    task_id: &str,
    report: &str,
) -> std::process::Output {
    // A task report is accepted only from the agent the daemon can attribute it
    // to (hook provenance, upstream PRD #1077): the CLI presents the agent id and
    // the capability the worker's own command exported into its directory.
    let worker = common::agent_records_on(deck.attach_socket_path())
        .into_iter()
        .find(|record| record.pane_id_env.as_deref() == Some(worker_pane))
        .unwrap_or_else(|| panic!("no agent record for the worker pane {worker_pane}"));
    let worker_dir = PathBuf::from(
        worker
            .cwd
            .as_deref()
            .expect("the worker's working directory"),
    );
    let capability = tokio::runtime::Runtime::new()
        .expect("capability reader runtime")
        .block_on(common::recorded_hook_capability(&worker_dir, &worker.id))
        .expect("the worker exported its own capability");
    std::process::Command::new(common::contained_deck_binary())
        .arg("work-done")
        .arg("--task-id")
        .arg(task_id)
        .arg("--task")
        .arg(report)
        .env("DOT_AGENT_DECK_SOCKET", deck.hook_socket_path())
        .env("DOT_AGENT_DECK_PANE_ID", worker_pane)
        .env("DOT_AGENT_DECK_AGENT_ID", &worker.id)
        .env("DOT_AGENT_DECK_PANE_CAPABILITY", &capability)
        .env("HOME", deck.home_dir())
        .current_dir(deck.workdir())
        .output()
        .expect("run the real `dot-agent-deck work-done` CLI")
}

/// The id of the task the orchestrator delegated to `worker_pane`: the id of
/// the message in that pane's inbox. Re-resolved on every poll because the
/// `orch-deck` fixture sets no `clear` override, so it defaults to `true` and a
/// delegate RESPAWNS the worker pane; the message is bound to the replacement.
fn delegated_task_id(deck: &TuiDeck, worker_pane: &str) -> String {
    let id = RefCell::new(None);
    let found = common::wait_until(common::load_scaled(Duration::from_secs(30)), || {
        match inbox_view::messages(deck.attach_socket_path(), worker_pane)
            .into_iter()
            .next()
        {
            Some(message) => {
                *id.borrow_mut() = Some(message.id.to_string());
                true
            }
            None => false,
        }
    });
    assert!(
        found,
        "the delegated task never reached the worker pane's inbox; inbox = {:?}",
        inbox_view::messages(deck.attach_socket_path(), worker_pane)
    );
    id.into_inner().expect("wait_until returned true")
}

// --- Issue #803: an orchestrator with a delegation outstanding reads ---------
// --- `Observing` on its own card ---------------------------------------------
//
// The tests below drive the whole live path with stand-in `cat` roles and the
// REAL `delegate` / `work-done` CLIs: daemon arms the delegation, pushes it to
// the attached deck (or serves it to a reattaching one), and the deck draws the
// ORCHESTRATOR's card. No LLM is involved. A `cat` stand-in never emits a hook
// event, so where a test needs one (an agent announcing itself) it writes the
// line a real agent's hook would send to the daemon's hook socket.

/// The label an orchestrator's card must carry while a delegation it issued is
/// outstanding. Spelled out here rather than imported from `src/`, matching
/// this file's convention, so a silent rewording fails the test.
const OBSERVING_LABEL: &str = "Observing";

/// The label an idle worker's card must carry while its pane still owes a
/// `work-done` (issue #755).
const DELEGATED_IDLE_LABEL: &str = "Idle (delegated)";

/// Leading fragment of the start role's name as the sidebar card draws it on
/// its first body row. A fragment, not the whole name, so a narrow sidebar
/// that ellipsizes the name (`orchestrat…`) is still matched.
const ORCHESTRATOR_CARD_NEEDLE: &str = "orchestrat";

/// What a role's card reads before any agent has announced itself on its pane.
const NO_AGENT_LABEL: &str = "No agent";

/// Ceiling on a role's placeholder card appearing on the deck, before
/// [`common::load_scaled`] widens it. The orchestration is already open and
/// its panes are already spawned when this is waited on, so it bounds a few
/// frames of rendering, and a deck that never draws the card fails the test
/// in well under a minute even at the largest load factor.
const CARD_APPEARS_BASE: Duration = Duration::from_secs(8);

/// Ceiling on a second deck booting, attaching to the running daemon and
/// drawing its first hydrated card, before [`common::load_scaled`] widens it.
const REATTACH_BASE: Duration = Duration::from_secs(15);

/// Ceiling on the deck redrawing a card after a hook event or a daemon push
/// it is already subscribed to, before [`common::load_scaled`] widens it.
const CARD_UPDATES_BASE: Duration = Duration::from_secs(5);

/// The top-border row of the sidebar card whose name starts with `needle` —
/// the row that carries its status badge — or `None` while no such card is
/// drawn.
///
/// A card is a box whose first body row OPENS with the role name, directly
/// after the box's own left border (PRD fork#405 moved the name off the title
/// onto that row, unpadded). The focused role's embedded PANE is also a box on
/// the same grid rows, and its content can mention the role anywhere (a
/// worker's task pointer carries the `…-orchestrator-1` clone path), so
/// "contains" is not enough: the name must be the first thing on the row. A
/// box whose top border names the role is that role's pane and is skipped
/// too. Only a card's status row is ever returned.
///
/// `common::card_identity_row` is a second card locator with different matching
/// rules (whole role name, number-badge title, returns a row) — keep the two in step.
fn role_card_status_row(grid: &str, needle: &str) -> Option<String> {
    let lines: Vec<Vec<char>> = grid.lines().map(|line| line.chars().collect()).collect();
    lines.iter().enumerate().find_map(|(row, chars)| {
        let body = lines.get(row + 1)?;
        common::BORDER_WEIGHTS.iter().find_map(|weight| {
            chars
                .iter()
                .enumerate()
                .filter(|(_, ch)| **ch == weight.top_left)
                .find_map(|(start, _)| {
                    let end = chars
                        .iter()
                        .enumerate()
                        .skip(start + 1)
                        .find_map(|(index, ch)| (*ch == weight.top_right).then_some(index))?;
                    let title: String = chars[start..=end].iter().collect();
                    if title.contains(needle) {
                        return None;
                    }
                    let body_span = body.get(start..=end)?;
                    let (left_border, inner) = body_span.split_first()?;
                    let inner: String = inner.iter().collect();
                    (*left_border == weight.vertical && inner.starts_with(needle)).then_some(title)
                })
        })
    })
}

fn orchestrator_card_status_row(grid: &str) -> Option<String> {
    role_card_status_row(grid, ORCHESTRATOR_CARD_NEEDLE)
}

/// Wait until the orchestrator's card is drawn and its status row satisfies
/// `pred`. Returns whether that happened within `timeout`.
fn wait_for_orchestrator_card(
    deck: &TuiDeck,
    timeout: Duration,
    pred: impl Fn(&str) -> bool,
) -> bool {
    deck.wait_for_grid_predicate_within(timeout, |grid| {
        orchestrator_card_status_row(grid).is_some_and(|row| pred(&row))
    })
}

/// Wait until the card of the role whose name starts with `needle` is drawn
/// and its status row satisfies `pred`. Returns whether that happened within
/// `timeout`.
fn wait_for_role_card(
    deck: &TuiDeck,
    needle: &str,
    timeout: Duration,
    pred: impl Fn(&str) -> bool,
) -> bool {
    deck.wait_for_grid_predicate_within(timeout, |grid| {
        role_card_status_row(grid, needle).is_some_and(|row| pred(&row))
    })
}

fn describe_orchestrator_card(deck: &TuiDeck) -> String {
    let grid = deck.snapshot_grid();
    format!(
        "orchestrator card status row: {:?}\nworker card status row: {:?}\nGrid:\n{grid}",
        orchestrator_card_status_row(&grid),
        role_card_status_row(&grid, WORKER_ROLE)
    )
}

/// Whether the daemon currently holds an outstanding delegation on
/// `worker_pane` issued by `orchestrator_pane` — the daemon-side fact the
/// deck's presentation is derived from.
fn daemon_has_delegation(deck: &TuiDeck, worker_pane: &str, orchestrator_pane: &str) -> bool {
    common::agent_records_on(deck.attach_socket_path())
        .into_iter()
        .filter(|r| r.pane_id_env.as_deref() == Some(worker_pane))
        .any(|r| {
            r.outstanding_delegation
                .is_some_and(|d| d.orchestrator_pane_id == orchestrator_pane)
        })
}

/// A deck with the `orch-deck` orchestration open, the idle-worker watch armed
/// with a timeout far longer than the test (so a delegation stays outstanding
/// until `work-done` retires it) and the silent-worker notice off (so nothing
/// else writes into the panes under observation).
fn launch_deck_with_orchestration() -> TuiDeck {
    let deck = TuiDeck::builder()
        .with_pty_size(160, 45)
        .with_env("DOT_AGENT_DECK_WORKER_RESPONSE_TIMEOUT_MS", "600000")
        .with_env("DOT_AGENT_DECK_DELEGATE_NO_EVENT_WINDOW_MS", "0")
        .impersonating_pane_signals()
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("No active agents");
    // `work-done --task-id` is a TASK report, and the daemon takes one only from
    // an agent it can attribute it to: the capability it put in that agent's own
    // environment. The `cat` worker therefore exports it (`run_work_done_cli`
    // presents it), and the config is rewritten HERE, before `open_orchestration`
    // commits it into the isolated clone every respawn re-reads it from.
    std::fs::write(
        deck.workdir().join(".dot-agent-deck.toml"),
        orchestration_config_with_worker_command(&common::capability_export_command("cat")),
    )
    .expect("give the worker role a capability-exporting command");
    open_orchestration(&deck);
    deck.wait_for_string(WORKER_ROLE);
    deck
}

/// The `SessionStart` line a real agent's hook sends when the agent
/// `agent_id` starts conversation `session_id` on `pane_id`, with the working
/// directory a real hook always reports.
fn session_start_hook_line(
    deck: &TuiDeck,
    session_id: &str,
    pane_id: &str,
    agent_id: &str,
) -> String {
    serde_json::json!({
        "session_id": session_id,
        "agent_type": "claude_code",
        "event_type": "session_start",
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "cwd": deck.workdir().to_string_lossy(),
        "pane_id": pane_id,
        "agent_id": agent_id,
    })
    .to_string()
}

/// Make the `cat` orchestrator a live, idle agent as far as the deck is
/// concerned, by writing the `SessionStart` a real agent's hook would send for
/// that pane, then wait for its card to read `Idle`.
///
/// Without this the stand-in's card reads `No agent`, which is not a status
/// the `Observing` presentation applies to.
///
/// The hook is written only once the deck has DRAWN the orchestrator's
/// placeholder card. The daemon registers the role panes before the deck
/// creates their cards, so a hook sent as soon as the daemon knows the pane
/// can reach the deck first and leave the pane with two cards, the
/// placeholder among them; every wait here is short, so a setup that goes
/// wrong fails quickly and names the step.
fn announce_idle_orchestrator(deck: &TuiDeck, orchestrator_pane: &str, orchestrator_agent: &str) {
    assert!(
        wait_for_orchestrator_card(deck, common::load_scaled(CARD_APPEARS_BASE), |row| {
            row.contains(NO_AGENT_LABEL)
        }),
        "setup: the deck never drew the orchestrator's placeholder card reading \
         {NO_AGENT_LABEL:?}, so its SessionStart hook was not sent\n{}",
        describe_orchestrator_card(deck)
    );
    common::write_hook_line(
        deck.hook_socket_path(),
        &session_start_hook_line(
            deck,
            "observing-orchestrator-session",
            orchestrator_pane,
            orchestrator_agent,
        ),
    )
    .expect("write the orchestrator's SessionStart hook");
    assert!(
        wait_for_orchestrator_card(deck, common::load_scaled(CARD_UPDATES_BASE), |row| {
            row.contains("Idle")
        }),
        "setup: the orchestrator's card never read `Idle` after its SessionStart hook, so \
         nothing below can be attributed to delegation\n{}",
        describe_orchestrator_card(deck)
    );
}

/// The registry id of the agent the daemon currently runs on `pane`, if any.
fn agent_id_on_pane(deck: &TuiDeck, pane: &str) -> Option<String> {
    common::agent_records_on(deck.attach_socket_path())
        .into_iter()
        .find(|record| record.pane_id_env.as_deref() == Some(pane))
        .map(|record| record.id)
}

/// Run the real `delegate` CLI from the orchestrator to the worker and wait
/// until the daemon holds the resulting outstanding delegation.
fn delegate_and_wait_until_armed(deck: &TuiDeck, orchestrator_pane: &str, worker_pane: &str) {
    let output = run_delegate_cli(
        deck,
        orchestrator_pane,
        WORKER_ROLE,
        "Do the thing under test for status/observing.",
    );
    assert!(
        output.status.success(),
        "setup: `delegate` exited {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        common::wait_until(common::load_scaled(Duration::from_secs(30)), || {
            daemon_has_delegation(deck, worker_pane, orchestrator_pane)
        }),
        "setup: the daemon never recorded an outstanding delegation on the worker pane issued \
         by the orchestrator pane; records = {:?}",
        common::agent_records_on(deck.attach_socket_path())
    );
}

/// Scenario: Launch the real TUI and its daemon, open the two-role `orch-deck` orchestration, and announce the `cat` orchestrator as an idle agent so its card reads `Idle`. Run the REAL `delegate` CLI from the orchestrator to the worker: with no reconnect, the orchestrator's own card must flip to `Observing`. Then run the REAL `work-done` CLI from the worker: the same card must go back to `Idle`.
#[spec("status/observing/009")]
#[test]
fn observing_009_attached_deck_flips_orchestrator_card_on_delegate_and_work_done() {
    let deck = launch_deck_with_orchestration();
    let (worker_pane, orchestrator_agent, orchestrator_pane) = orchestration_ids(&deck);
    announce_idle_orchestrator(&deck, &orchestrator_pane, &orchestrator_agent);

    delegate_and_wait_until_armed(&deck, &orchestrator_pane, &worker_pane);

    let visible_timeout = common::load_scaled(CARD_UPDATES_BASE);
    assert!(
        wait_for_orchestrator_card(&deck, visible_timeout, |row| {
            row.contains(OBSERVING_LABEL) && !row.contains("Idle")
        }),
        "the orchestrator delegated to the worker and the daemon holds that delegation as \
         outstanding, but the orchestrator's own card never read {OBSERVING_LABEL:?} on the \
         already-attached deck\n{}",
        describe_orchestrator_card(&deck)
    );

    let task_id = delegated_task_id(&deck, &worker_pane);
    let work_done_output = run_work_done_cli(
        &deck,
        &worker_pane,
        &task_id,
        "Finished the delegated task. e2e-observing-report-5d20",
    );
    assert!(
        work_done_output.status.success(),
        "`work-done` exited {:?}\nstdout: {}\nstderr: {}",
        work_done_output.status.code(),
        String::from_utf8_lossy(&work_done_output.stdout),
        String::from_utf8_lossy(&work_done_output.stderr)
    );
    assert!(
        common::wait_until(common::load_scaled(Duration::from_secs(30)), || {
            !daemon_has_delegation(&deck, &worker_pane, &orchestrator_pane)
        }),
        "the daemon never retired the delegation after the worker's work-done; records = {:?}",
        common::agent_records_on(deck.attach_socket_path())
    );
    assert!(
        wait_for_orchestrator_card(&deck, visible_timeout, |row| {
            row.contains("Idle") && !row.contains(OBSERVING_LABEL)
        }),
        "the worker reported work-done and the daemon retired the delegation, but the \
         orchestrator's card did not go back to `Idle`\n{}",
        describe_orchestrator_card(&deck)
    );
}

/// Scenario: Launch the real TUI and its daemon, open the two-role `orch-deck` orchestration, announce the `cat` orchestrator as an idle agent, and run the REAL `delegate` CLI from the orchestrator to the worker, which never reports back. Attach a second, fresh TUI to the same still-running daemon: the orchestrator's card it hydrates must read `Observing`, not `Idle`.
#[spec("status/observing/010")]
#[test]
fn observing_010_reattached_deck_hydrates_orchestrator_card_as_observing() {
    let deck = launch_deck_with_orchestration();
    let (worker_pane, orchestrator_agent, orchestrator_pane) = orchestration_ids(&deck);
    announce_idle_orchestrator(&deck, &orchestrator_pane, &orchestrator_agent);
    delegate_and_wait_until_armed(&deck, &orchestrator_pane, &worker_pane);

    // A fresh client against the SAME daemon. `deck` stays alive throughout:
    // it owns the tempdir the orchestration's role panes run in.
    let reattached = TuiDeck::builder()
        .with_pty_size(160, 45)
        .with_env(
            "DOT_AGENT_DECK_ATTACH_SOCKET",
            deck.attach_socket_path().to_string_lossy().into_owned(),
        )
        .with_env(
            "DOT_AGENT_DECK_SOCKET",
            deck.hook_socket_path().to_string_lossy().into_owned(),
        )
        .without_success_recording()
        .launch_with_fixture("minimal");

    assert!(
        wait_for_orchestrator_card(&reattached, common::load_scaled(REATTACH_BASE), |row| {
            row.contains("Idle") || row.contains(OBSERVING_LABEL)
        }),
        "setup: the reattached deck never drew the orchestrator's card with a live status, so \
         nothing can be said about how it hydrated\n{}",
        describe_orchestrator_card(&reattached)
    );
    assert!(
        daemon_has_delegation(&deck, &worker_pane, &orchestrator_pane),
        "setup: the delegation must still be outstanding when the second deck hydrates"
    );
    assert!(
        wait_for_orchestrator_card(&reattached, common::load_scaled(CARD_UPDATES_BASE), |row| {
            row.contains(OBSERVING_LABEL) && !row.contains("Idle")
        }),
        "a deck attaching while the orchestrator's delegation is outstanding must hydrate the \
         orchestrator's card as {OBSERVING_LABEL:?}\n{}",
        describe_orchestrator_card(&reattached)
    );
}

/// Scenario: Launch the real TUI and its daemon, open the two-role `orch-deck` orchestration, announce the `cat` orchestrator as an idle agent, and run the REAL `delegate` CLI from the orchestrator to the worker, which respawns the worker under a new agent id. Write the `SessionStart` the respawned worker's hook would send, so the deck replaces the worker's session: the orchestrator's card must STILL read `Observing` and the worker's card must read `Idle (delegated)`. Then run the REAL `work-done` CLI from the worker: both cards must go back to plain `Idle`.
#[spec("status/observing/018")]
#[test]
fn observing_018_orchestrator_keeps_observing_after_the_respawned_worker_announces_itself() {
    let deck = launch_deck_with_orchestration();
    let (worker_pane, orchestrator_agent, orchestrator_pane) = orchestration_ids(&deck);
    announce_idle_orchestrator(&deck, &orchestrator_pane, &orchestrator_agent);
    assert!(
        wait_for_role_card(
            &deck,
            WORKER_ROLE,
            common::load_scaled(CARD_APPEARS_BASE),
            |row| row.contains(NO_AGENT_LABEL)
        ),
        "setup: the deck never drew the worker's placeholder card reading {NO_AGENT_LABEL:?}\n{}",
        describe_orchestrator_card(&deck)
    );
    let first_worker_agent = agent_id_on_pane(&deck, &worker_pane)
        .expect("setup: the daemon runs an agent on the worker pane before the delegate");

    delegate_and_wait_until_armed(&deck, &orchestrator_pane, &worker_pane);
    let visible_timeout = common::load_scaled(CARD_UPDATES_BASE);
    assert!(
        wait_for_orchestrator_card(&deck, visible_timeout, |row| {
            row.contains(OBSERVING_LABEL) && !row.contains("Idle")
        }),
        "setup: the orchestrator's card never read {OBSERVING_LABEL:?} after the delegate, so \
         nothing can be said about whether it KEEPS reading it\n{}",
        describe_orchestrator_card(&deck)
    );

    // The delegate's `clear = true` respawn puts a NEW agent on the worker
    // pane. Its id is what the respawned agent's own hook would report.
    let respawned_worker_agent = RefCell::new(None);
    assert!(
        common::wait_until(common::load_scaled(Duration::from_secs(30)), || {
            match agent_id_on_pane(&deck, &worker_pane) {
                Some(id) if id != first_worker_agent => {
                    *respawned_worker_agent.borrow_mut() = Some(id);
                    true
                }
                _ => false,
            }
        }),
        "setup: the delegate never respawned the worker under a new agent id (it was \
         {first_worker_agent:?}); records = {:?}",
        common::agent_records_on(deck.attach_socket_path())
    );
    let respawned_worker_agent = respawned_worker_agent
        .into_inner()
        .expect("the wait stores the respawned worker's agent id");

    common::write_hook_line(
        deck.hook_socket_path(),
        &session_start_hook_line(
            &deck,
            "observing-respawned-worker-session",
            &worker_pane,
            &respawned_worker_agent,
        ),
    )
    .expect("write the respawned worker's SessionStart hook");
    assert!(
        wait_for_role_card(&deck, WORKER_ROLE, visible_timeout, |row| {
            row.contains("Idle") && !row.contains(NO_AGENT_LABEL)
        }),
        "setup: the worker's card never showed a live agent after the respawned worker's \
         SessionStart hook, so the deck has not replaced the worker's session yet\n{}",
        describe_orchestrator_card(&deck)
    );
    assert!(
        daemon_has_delegation(&deck, &worker_pane, &orchestrator_pane),
        "setup: the delegation must still be outstanding on the daemon after the respawned \
         worker announced itself"
    );

    assert!(
        wait_for_orchestrator_card(&deck, visible_timeout, |row| {
            row.contains(OBSERVING_LABEL) && !row.contains("Idle")
        }),
        "the respawned worker announced itself and the daemon still holds the delegation as \
         outstanding, but the orchestrator's card stopped reading {OBSERVING_LABEL:?}\n{}",
        describe_orchestrator_card(&deck)
    );
    assert!(
        wait_for_role_card(&deck, WORKER_ROLE, visible_timeout, |row| {
            row.contains(DELEGATED_IDLE_LABEL) && !row.contains(OBSERVING_LABEL)
        }),
        "the respawned worker announced itself and the daemon still holds the delegation as \
         outstanding, but the worker's card does not read {DELEGATED_IDLE_LABEL:?}\n{}",
        describe_orchestrator_card(&deck)
    );

    let task_id = delegated_task_id(&deck, &worker_pane);
    let work_done_output = run_work_done_cli(
        &deck,
        &worker_pane,
        &task_id,
        "Finished the delegated task. e2e-observing-report-9c41",
    );
    assert!(
        work_done_output.status.success(),
        "`work-done` exited {:?}\nstdout: {}\nstderr: {}",
        work_done_output.status.code(),
        String::from_utf8_lossy(&work_done_output.stdout),
        String::from_utf8_lossy(&work_done_output.stderr)
    );
    assert!(
        common::wait_until(common::load_scaled(Duration::from_secs(30)), || {
            !daemon_has_delegation(&deck, &worker_pane, &orchestrator_pane)
        }),
        "the daemon never retired the delegation after the worker's work-done; records = {:?}",
        common::agent_records_on(deck.attach_socket_path())
    );
    assert!(
        wait_for_orchestrator_card(&deck, visible_timeout, |row| {
            row.contains("Idle") && !row.contains(OBSERVING_LABEL)
        }),
        "the worker reported work-done and the daemon retired the delegation, but the \
         orchestrator's card did not go back to `Idle`\n{}",
        describe_orchestrator_card(&deck)
    );
    assert!(
        wait_for_role_card(&deck, WORKER_ROLE, visible_timeout, |row| {
            row.contains("Idle") && !row.contains("delegated") && !row.contains(OBSERVING_LABEL)
        }),
        "the worker reported work-done and the daemon retired the delegation, but the \
         worker's card did not go back to plain `Idle`\n{}",
        describe_orchestrator_card(&deck)
    );
}

// ---------------------------------------------------------------------------
// Issue #805: a delegate that never reaches its worker, as the attached deck
// shows it. The daemon arms the delegation before the dispatch has run, so a
// dispatch that ends without handing the worker a task pointer has to retire
// it again, or the two cards below keep advertising work nobody received until
// the idle-worker timeout.
// ---------------------------------------------------------------------------

/// A role command that cannot be exec'd. One word with no shell
/// metacharacters is exec'd directly rather than through `$SHELL -c`, so this
/// is a spawn ERROR and not a shell that starts and then exits 127.
const MISSING_WORKER_BINARY: &str = "/nonexistent-worker-agent-deck-respawn-target";

/// The daemon's respawn-failure notice, posted to the orchestrator's inbox
/// immediately before the dispatch gives up (`compose_respawn_failed_notice`,
/// `src/state.rs`). Spelled out here rather than imported, matching this file's
/// convention. The notice names the worker's PANE, not its role, inside the
/// declared-untrusted pane-id frame, so the stable part to wait for is the
/// notice's own headline, the one `delegate_respawn_recovery` and
/// `work_done_reporting` already use; the older "respawn failed for role
/// 'worker'" wording no longer exists in the daemon.
const RESPAWN_FAILED_NEEDLE: &str = "delegated worker respawn failed";

/// The daemon's notice for a replacement worker that died before it was ready.
const DEAD_REPLACEMENT_NEEDLE: &str = "delegated worker never came up";

/// How long both cards and the daemon must keep agreeing that nothing is
/// outstanding before an undelivered delegate counts as settled.
const CARDS_SETTLED_FOR: Duration = Duration::from_secs(2);

/// The `orch-deck` orchestration with its worker role re-pointed at
/// `worker_command`. The role sets no `clear`, so it keeps the default and a
/// delegate respawns the worker with this command.
fn orchestration_config_with_worker_command(worker_command: &str) -> String {
    format!(
        "[[orchestrations]]\nname = \"demo-orch\"\n\n\
         [[orchestrations.roles]]\nname = \"orchestrator\"\ncommand = \"cat\"\nstart = true\n\n\
         [[orchestrations.roles]]\nname = \"{WORKER_ROLE}\"\ncommand = {worker_command:?}\n"
    )
}

/// Every directory a role pane of the open orchestration runs in. The
/// dispatch re-reads `.dot-agent-deck.toml` from the worker's own directory on
/// each delegate, and that directory is the orchestration's isolated clone
/// rather than `deck.workdir()`.
fn orchestration_role_dirs(deck: &TuiDeck) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = common::agent_records_on(deck.attach_socket_path())
        .into_iter()
        .filter(|record| {
            matches!(
                record.tab_membership,
                Some(TabMembership::Orchestration { .. })
            )
        })
        .filter_map(|record| record.cwd.map(PathBuf::from))
        .collect();
    dirs.sort();
    dirs.dedup();
    dirs
}

/// Open the orchestration with both roles announced as idle agents, re-point
/// the worker role at the command `worker_command` builds, run the REAL
/// `delegate` CLI, and wait until the daemon has told the orchestrator that the
/// delegate did not reach a worker (`exit_needle` in the orchestrator's pane).
/// Then require both cards and the daemon to settle on a non-delegated state
/// within the deck's normal redraw window.
fn assert_undelivered_delegate_leaves_no_delegated_cards(
    worker_command: impl FnOnce(&std::path::Path) -> String,
    exit_needle: &str,
    exit: &str,
) {
    let deck = launch_deck_with_orchestration();
    let (worker_pane, orchestrator_agent, orchestrator_pane) = orchestration_ids(&deck);
    announce_idle_orchestrator(&deck, &orchestrator_pane, &orchestrator_agent);
    assert!(
        wait_for_role_card(
            &deck,
            WORKER_ROLE,
            common::load_scaled(CARD_APPEARS_BASE),
            |row| row.contains(NO_AGENT_LABEL)
        ),
        "setup: the deck never drew the worker's placeholder card reading {NO_AGENT_LABEL:?}\n{}",
        describe_orchestrator_card(&deck)
    );
    let worker_agent = agent_id_on_pane(&deck, &worker_pane)
        .expect("setup: the daemon runs an agent on the worker pane before the delegate");
    common::write_hook_line(
        deck.hook_socket_path(),
        &session_start_hook_line(
            &deck,
            "undelivered-worker-session",
            &worker_pane,
            &worker_agent,
        ),
    )
    .expect("write the worker's SessionStart hook");
    assert!(
        wait_for_role_card(
            &deck,
            WORKER_ROLE,
            common::load_scaled(CARD_UPDATES_BASE),
            |row| row.contains("Idle")
        ),
        "setup: the worker's card never read `Idle` after its SessionStart hook\n{}",
        describe_orchestrator_card(&deck)
    );

    let role_dirs = orchestration_role_dirs(&deck);
    assert!(
        !role_dirs.is_empty(),
        "setup: no role pane of the orchestration reports a working directory; records = {:?}",
        common::agent_records_on(deck.attach_socket_path())
    );
    let command = worker_command(&role_dirs[0]);
    for dir in &role_dirs {
        std::fs::write(
            dir.join(".dot-agent-deck.toml"),
            orchestration_config_with_worker_command(&command),
        )
        .expect("re-point the worker role");
    }

    let output = run_delegate_cli(
        &deck,
        &orchestrator_pane,
        WORKER_ROLE,
        "Do the thing under test for an undelivered delegate.",
    );
    assert!(
        output.status.success(),
        "setup: `delegate` exited {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        common::wait_until(common::load_scaled(Duration::from_secs(30)), || {
            orchestrator_inbox_text(&deck, &orchestrator_pane).contains(exit_needle)
        }),
        "setup: the daemon never told the orchestrator that the delegate did not reach a worker \
         ({exit}), so the dispatch did not take the exit under test; orchestrator inbox = {:?}",
        orchestrator_inbox_text(&deck, &orchestrator_pane)
    );

    // The delegation was announced to the deck before the dispatch ran, so a
    // card that merely PASSES THROUGH a non-delegated state proves nothing:
    // the deck may not have drawn the announcement yet. What is required is
    // that both cards and the daemon agree on "nothing outstanding" and keep
    // agreeing for `CARDS_SETTLED_FOR`, within the deck's normal redraw window.
    let orchestrator_settled = |row: &str| row.contains("Idle") && !row.contains(OBSERVING_LABEL);
    let worker_settled = |row: &str| !row.contains("delegated");
    let settled_since = std::cell::Cell::new(None::<std::time::Instant>);
    let settled = common::wait_until(
        common::load_scaled(CARD_UPDATES_BASE) + CARDS_SETTLED_FOR,
        || {
            let grid = deck.snapshot_grid();
            let nothing_outstanding = orchestrator_card_status_row(&grid)
                .as_deref()
                .is_some_and(orchestrator_settled)
                && role_card_status_row(&grid, WORKER_ROLE)
                    .as_deref()
                    .is_none_or(worker_settled)
                && !daemon_has_delegation(&deck, &worker_pane, &orchestrator_pane);
            if !nothing_outstanding {
                settled_since.set(None);
                return false;
            }
            let since = settled_since.get().unwrap_or_else(|| {
                let now = std::time::Instant::now();
                settled_since.set(Some(now));
                now
            });
            since.elapsed() >= CARDS_SETTLED_FOR
        },
    );

    // Named one by one, so a failure says WHICH surface still shows the
    // delegation rather than only that something does.
    let grid = deck.snapshot_grid();
    let orchestrator_row = orchestrator_card_status_row(&grid);
    assert!(
        orchestrator_row
            .as_deref()
            .is_some_and(orchestrator_settled),
        "the delegate never reached a worker ({exit}), but the orchestrator's card still reads \
         as if it were waiting on one: {orchestrator_row:?}; daemon records = {:?}\n{}",
        common::agent_records_on(deck.attach_socket_path()),
        describe_orchestrator_card(&deck)
    );
    let worker_row = role_card_status_row(&grid, WORKER_ROLE);
    assert!(
        worker_row.as_deref().is_none_or(worker_settled),
        "the delegate never reached a worker ({exit}), but the worker's card still reads as if \
         it owed a work-done: {worker_row:?}\n{}",
        describe_orchestrator_card(&deck)
    );
    assert!(
        !daemon_has_delegation(&deck, &worker_pane, &orchestrator_pane),
        "the delegate never reached a worker ({exit}), but the daemon still holds its \
         delegation as outstanding; records = {:?}",
        common::agent_records_on(deck.attach_socket_path())
    );
    assert!(
        settled,
        "the delegate never reached a worker ({exit}); the cards and the daemon each read as \
         settled just now, but never all at once for {CARDS_SETTLED_FOR:?} within the redraw \
         window\n{}",
        describe_orchestrator_card(&deck)
    );
}

/// Scenario: Launch the real TUI and its daemon, open the two-role `orch-deck` orchestration with both roles announced as idle agents, re-point the worker role at a binary that does not exist, and run the REAL `delegate` CLI so the worker's respawn fails. Once the orchestrator's pane shows the respawn-failure notice, the orchestrator's card must read `Idle` rather than `Observing` and the worker's card must not read `Idle (delegated)`.
#[spec("status/observing/022")]
#[test]
fn observing_022_a_failed_respawn_leaves_neither_card_reading_delegated() {
    assert_undelivered_delegate_leaves_no_delegated_cards(
        |_dir| MISSING_WORKER_BINARY.to_string(),
        RESPAWN_FAILED_NEEDLE,
        "its respawn failed",
    );
}

/// Scenario: Launch the real TUI and its daemon, open the two-role `orch-deck` orchestration with both roles announced as idle agents, re-point the worker role at a script that exits at once, and run the REAL `delegate` CLI so the replacement worker dies before it can receive anything. Once the orchestrator's pane shows the worker-never-came-up notice, the orchestrator's card must read `Idle` rather than `Observing` and the worker's card must not read `Idle (delegated)`.
#[spec("status/observing/023")]
#[test]
fn observing_023_a_replacement_that_never_becomes_live_leaves_neither_card_reading_delegated() {
    assert_undelivered_delegate_leaves_no_delegated_cards(
        |dir| {
            use std::os::unix::fs::PermissionsExt;

            let script = dir.join("dies-on-arrival.sh");
            std::fs::write(&script, "#!/bin/sh\nexit 3\n").expect("write the dying stand-in");
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                .expect("chmod the dying stand-in");
            script.to_string_lossy().into_owned()
        },
        DEAD_REPLACEMENT_NEEDLE,
        "its replacement worker never became live",
    );
}
