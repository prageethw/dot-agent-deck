#![cfg(feature = "e2e")]

//! PTY-attached coverage for what the orchestrator is TOLD when a worker reports
//! `work-done` (issues #448 and #433).
//!
//! The fast-tier suite (`tests/work_done_reporting.rs`) pins the daemon's
//! decisions against in-process PTYs. This one covers the boundary that suite
//! cannot see, and that this change specifically moved: the REAL `dot-agent-deck
//! work-done` binary, over the daemon's real hook socket, into the real TUI's
//! rendered orchestration surface.
//!
//! Rendering is the point, not incidental. The old feedback was one short
//! sentence; the unsolicited label is a long paragraph carrying a framed report,
//! and a long daemon-injected line lands on the vt100 grid hard-wrapped at
//! whatever column a role pane happens to be — which is exactly how
//! `scheduler/idle-worker/011` fails today. So the assertions read the PANE
//! COLUMN and squeeze whitespace out of both sides (see [`pane_column_text`]), and
//! the test proves the label genuinely reaches a user's screen rather than merely
//! reaching a PTY.

mod common;

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::Duration;

use common::{TuiDeck, commit_fixture};
use dot_agent_deck::daemon_protocol::TabMembership;
use dot_agent_deck::state::work_done_file_name;
use spec::spec;

/// The `orch-deck` fixture's non-start `cat` role — the worker whose completion
/// this test issues. (Its `orchestrator` sibling is identified by membership, not
/// by name, in [`orchestration_ids`].)
const WORKER_ROLE: &str = "worker";

/// The #448 label, spelled out here rather than imported from `src/` so a silent
/// rewording of the daemon's template fails this test instead of following it.
const UNSOLICITED_NEEDLE: &str = "you have no outstanding delegation to that worker";

/// The daemon's provenance clause — an orchestrator agent could write prose about
/// a worker, but not a verbatim self-identification as a daemon report.
const DAEMON_CLAUSE: &str = "dot-agent-deck daemon report, not a message from a person or an agent";

/// The happy-path pointer. Its ABSENCE is the assertion: nothing was delegated,
/// so no summary file was written, so there is nothing to point at. The exact
/// filename carries a per-pane digest (upstream #331 + fork #76) that depends on
/// the worker pane id discovered at test time, so the needle is built in the
/// test body via [`work_done_file_name`] rather than hardcoded here.
fn pointer_needle(worker_pane_id: &str) -> String {
    format!(
        "Read .dot-agent-deck/{} for their full report.",
        work_done_file_name(WORKER_ROLE, worker_pane_id)
    )
}

/// Opening marker of the untrusted-report frame the inlined report sits inside.
const REPORT_FRAME_NEEDLE: &str = "[UNTRUSTED-WORKER-REPORT:";

/// A token unique to this test's report, so its appearance on the grid proves the
/// daemon inlined THIS report. `[a-z0-9-]` only, so it survives the whitespace
/// collapse and the frame-breaking filter unchanged.
const SENTINEL: &str = "e2e-unsolicited-report-4b7d";

/// Drop every whitespace run, so a needle that straddles the pane's wrap column
/// still matches text that is fully on screen.
fn squeeze(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The embedded pane column's text, rows joined in order — the orchestration
/// surface as the user reads it.
///
/// Slicing the column is load-bearing, not tidiness. An orchestration tab renders
/// the role CARDS to the left of the pane on the same grid rows, so joining whole
/// rows splices card text and card borders into the middle of every wrapped pane
/// line: `…no outstanding delegat` + `┃Launch an agent to get started┃` +
/// `ion to that worker…`. A needle longer than the pane is wide then matches
/// nothing even though every character of it is on screen, which is precisely how
/// `scheduler/idle-worker/011` fails today (issue #460) — the daemon's long line
/// is plainly rendered in its dump and the assertion still cannot see it.
///
/// The pane's left border column is found from its `┌<title>` header row and is
/// constant down the box, so every row is cut at the same column and the trailing
/// border is trimmed. Char-indexed throughout: box-drawing glyphs are multibyte.
fn pane_column_text(grid: &str) -> String {
    let Some(left) = grid
        .lines()
        .find(|line| line.contains('┌'))
        .and_then(|line| line.chars().position(|c| c == '┌'))
    else {
        return String::new();
    };
    grid.lines()
        .filter_map(|line| {
            let row: Vec<char> = line.chars().collect();
            if row.len() <= left + 1 {
                return None;
            }
            // Stops at the pane's RIGHT border on content rows; on the
            // header/footer rows it yields box glyphs, which are harmless because
            // no needle contains them.
            let interior: String = row[left + 1..].iter().take_while(|c| **c != '│').collect();
            Some(interior)
        })
        .collect::<Vec<_>>()
        .join("")
}

fn pane_contains(deck: &TuiDeck, needle: &str) -> bool {
    squeeze(&pane_column_text(&deck.snapshot_grid())).contains(&squeeze(needle))
}

fn wait_for_pane_string(deck: &TuiDeck, needle: &str, timeout: Duration) -> bool {
    common::wait_until(timeout, || pane_contains(deck, needle))
}

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
/// accepted as this orchestration's start role — `orchestration/work-done/006`
/// and `007` need it to run a real delegate from the orchestrator's identity;
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

/// The orchestrator PTY's own scrollback, straight from the daemon — the bytes it
/// wrote, before any rendering is involved.
fn orchestrator_pty(deck: &TuiDeck, orchestrator_agent_id: &str) -> String {
    String::from_utf8_lossy(&common::pane_snapshot_on(
        deck.attach_socket_path(),
        orchestrator_agent_id,
    ))
    .into_owned()
}

/// Scenario: Launch the real TUI and its lazy daemon, open the two-role `orch-deck` fixture, and run the REAL `dot-agent-deck work-done` binary from the live `worker` pane without anything ever having been delegated to it — the shape of a worker a person tasked directly. The rendered orchestration surface must visibly carry the daemon's unsolicited label and the worker's own report inside its untrusted-report markers, must NOT carry the pointer to a summary file, and no `work-done-worker-<pane digest>.md` may appear on disk.
#[spec("orchestration/work-done/004")]
#[test]
fn work_done_004_unsolicited_completion_is_visibly_labelled_in_the_attached_tui() {
    let deck = TuiDeck::builder()
        .with_pty_size(120, 40)
        // Both delegation watches off: this test is about what an UNDELEGATED
        // completion renders as, and a detector firing into the same pane would
        // be noise competing for the surface under assertion.
        .with_env("DOT_AGENT_DECK_WORKER_RESPONSE_TIMEOUT_MS", "0")
        .with_env("DOT_AGENT_DECK_DELEGATE_NO_EVENT_WINDOW_MS", "0")
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("No active sessions");
    open_orchestration(&deck);
    deck.wait_for_string(WORKER_ROLE);

    let (worker_pane, orchestrator_agent, _orchestrator_pane) = orchestration_ids(&deck);
    let summary_file_name = work_done_file_name(WORKER_ROLE, &worker_pane);
    let summary_path = deck
        .workdir()
        .join(".dot-agent-deck")
        .join(&summary_file_name);
    let pointer_needle_text = pointer_needle(&worker_pane);

    // Fork issue #513: this CLI invocation is a NEW subprocess launched from
    // the test harness's own process, not the worker pane's real spawned
    // child — so it never inherited the `DOT_AGENT_DECK_REGISTRATION_GENERATION`
    // / `DOT_AGENT_DECK_DAEMON_BOOT_ID` env vars a real spawn injects
    // (`src/spawn.rs`). Without them the signal defaults to `generation: 0`
    // / `daemon_boot_id: ""`, which `handle_work_done`'s fork-#358 fail-closed
    // guard never matches, and the report is silently refused before the
    // labelling logic under test ever runs. Query the daemon's own
    // `ListAgents` for the values it just assigned this pane and set them
    // explicitly, so this subprocess constructs the same legitimate signal a
    // real spawned worker would have.
    let worker_record = common::agent_records_on(deck.attach_socket_path())
        .into_iter()
        .find(|r| r.pane_id_env.as_deref() == Some(worker_pane.as_str()))
        .expect("the worker pane must still be present in ListAgents");
    let worker_boot_id = worker_record
        .daemon_boot_id
        .expect("ListAgents must report a daemon_boot_id (fork issue #513)");
    let worker_generation = worker_record
        .registration_generation
        .expect("the worker pane must carry a registration_generation once registered as an orchestration role (fork issue #513)");

    // The REAL CLI, as the footer tells a worker to run it, against the deck's
    // own daemon. Nothing was delegated, so the daemon owes this pane nothing.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_dot-agent-deck"))
        .arg("work-done")
        .arg("--task")
        .arg(format!("A person asked me to do this. {SENTINEL}"))
        .env("DOT_AGENT_DECK_SOCKET", deck.hook_socket_path())
        .env("DOT_AGENT_DECK_PANE_ID", &worker_pane)
        .env(
            "DOT_AGENT_DECK_REGISTRATION_GENERATION",
            worker_generation.to_string(),
        )
        .env("DOT_AGENT_DECK_DAEMON_BOOT_ID", &worker_boot_id)
        .env("HOME", deck.home_dir())
        .current_dir(deck.workdir())
        .output()
        .expect("run the real `dot-agent-deck work-done` CLI");
    assert!(
        output.status.success(),
        "`work-done` exited {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // First: did the DAEMON compose and write the label into the orchestrator's
    // PTY at all? Asserted before the grid so a daemon-side failure is never
    // reported as a rendering failure.
    let wrote = common::wait_until(Duration::from_secs(20), || {
        squeeze(&orchestrator_pty(&deck, &orchestrator_agent))
            .contains(&squeeze(UNSOLICITED_NEEDLE))
    });
    assert!(
        wrote,
        "the daemon never wrote the unsolicited label into the orchestrator's PTY — an \
         uncommissioned completion still reads to the orchestrator as delegated work coming \
         back\nOrchestrator PTY:\n{}",
        orchestrator_pty(&deck, &orchestrator_agent)
    );

    // Then: does it reach the user's screen? A long daemon-injected line has to
    // survive the orchestration surface's wrapping to be worth anything.
    assert!(
        wait_for_pane_string(&deck, UNSOLICITED_NEEDLE, Duration::from_secs(20)),
        "the unsolicited label reached the orchestrator's PTY but never became visible in the \
         rendered orchestration surface\nFinal grid:\n{}",
        deck.snapshot_grid()
    );
    assert!(
        pane_contains(&deck, DAEMON_CLAUSE),
        "the label must identify itself as a daemon report, not as a message from a person or an \
         agent\nFinal grid:\n{}",
        deck.snapshot_grid()
    );
    assert!(
        pane_contains(&deck, REPORT_FRAME_NEEDLE) && pane_contains(&deck, SENTINEL),
        "the worker's own report must still reach the orchestrator, framed as untrusted \
         data\nFinal grid:\n{}",
        deck.snapshot_grid()
    );
    assert!(
        !pane_contains(&deck, &pointer_needle_text),
        "the orchestrator was pointed at a summary file that was never written — the #433 \
         defect, reached through #448's path\nFinal grid:\n{}",
        deck.snapshot_grid()
    );
    assert!(
        !summary_path.exists(),
        "an uncommissioned completion wrote {} — the role-keyed path is the record of \
         COMMISSIONED work and must not be overwritten by a report nobody asked for",
        summary_path.display()
    );
}

// --- PRD #586 M4: `--subject` echo + mismatch warning (Problem 2) ---------
//
// Neither `delegate` nor `work-done` recognizes `--subject` yet — this is the
// RED round the M4 implementation task is written against. The two tests
// below are written as if the flag already existed; until it lands, the
// first `assert!(...status.success()...)` each hits is expected to fail on a
// clap "unrecognized argument" runtime error rather than a compile error,
// since the flag is passed as a subprocess argument rather than referenced
// from Rust source.

/// Subject the orchestrator states when arming the delegation below — PRD
/// #586 M4's `--subject` flag on `dot-agent-deck delegate`.
const DELEGATED_SUBJECT: &str = "#589";

/// Subject the worker echoes back on a MISMATCHED report, deliberately
/// different from [`DELEGATED_SUBJECT`] — Problem 2's exact observed shape: a
/// worker's agent session has drifted to a stale task and reports on it
/// coherently, just under the wrong subject.
const REPORTED_SUBJECT: &str = "#544";

/// The mismatch-warning needle this test expects the daemon's notification to
/// carry when the delegated and reported subjects disagree. Spelled out here
/// rather than imported from `src/`, matching [`UNSOLICITED_NEEDLE`]'s
/// convention above: a silent rewording of the daemon's template fails this
/// test instead of following it.
const SUBJECT_MISMATCH_NEEDLE: &str = "SUBJECT MISMATCH";

/// Run the real `dot-agent-deck delegate` CLI as a subprocess against the
/// deck's own hook socket, from `caller_pane`'s identity — the caller-identity
/// technique `e2e_dispatcher_mode.rs`'s `run_delegate_to` already uses,
/// reimplemented locally because integration tests cannot import one
/// another's helpers across a compiled-test-binary boundary.
fn run_delegate_cli_with_subject(
    deck: &TuiDeck,
    caller_pane: &str,
    to: &str,
    task: &str,
    subject: &str,
) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_dot-agent-deck"))
        .arg("delegate")
        .arg("--to")
        .arg(to)
        .arg("--task")
        .arg(task)
        .arg("--subject")
        .arg(subject)
        .env("DOT_AGENT_DECK_SOCKET", deck.hook_socket_path())
        .env("DOT_AGENT_DECK_PANE_ID", caller_pane)
        .output()
        .expect("run the real `dot-agent-deck delegate` CLI")
}

/// Run the real `dot-agent-deck work-done` CLI as a subprocess, carrying the
/// fork-#358 fail-closed identity (`generation` + `daemon_boot_id`) the same
/// way `work_done_004`'s inline invocation above does, plus PRD #586 M4's
/// `--subject` flag.
fn run_work_done_cli_with_subject(
    deck: &TuiDeck,
    worker_pane: &str,
    worker_generation: u64,
    worker_boot_id: &str,
    task: &str,
    subject: &str,
) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_dot-agent-deck"))
        .arg("work-done")
        .arg("--task")
        .arg(task)
        .arg("--subject")
        .arg(subject)
        .env("DOT_AGENT_DECK_SOCKET", deck.hook_socket_path())
        .env("DOT_AGENT_DECK_PANE_ID", worker_pane)
        .env(
            "DOT_AGENT_DECK_REGISTRATION_GENERATION",
            worker_generation.to_string(),
        )
        .env("DOT_AGENT_DECK_DAEMON_BOOT_ID", worker_boot_id)
        .env("HOME", deck.home_dir())
        .current_dir(deck.workdir())
        .output()
        .expect("run the real `dot-agent-deck work-done` CLI")
}

/// Look up the currently-registered worker pane's fork-#358 identity
/// (`registration_generation` + `daemon_boot_id`) plus its real `cwd`, exactly
/// as `work_done_004`'s inline lookup above does for the identity half —
/// needed by both `006` and `007` to construct a legitimate `work-done`
/// signal for a real CLI subprocess that never inherited a real spawn's
/// environment, and by `006` alone to know where the daemon actually wrote
/// its summary file: `open_orchestration` provisions an isolated clone in a
/// sibling directory of `deck.workdir()` (`resolve_workspace_path`,
/// `src/ui.rs`), so the worker pane's real `cwd` — and therefore the base
/// `write_work_done_summary` writes under — is never `deck.workdir()` itself.
///
/// Both callers invoke this immediately after a real `delegate` CLI call, and
/// the `orch-deck` fixture sets no `clear` override on either role so it
/// defaults to `true` — a delegate RESPAWNS the worker pane, exactly the race
/// `wait_for_delegate_pointer` in `e2e_dispatcher_mode.rs` documents: the
/// agent id that existed when the delegate was sent is dead by the time the
/// respawn completes, and the registry briefly holds both the old and the new
/// agent for one pane with `ListAgents` order unspecified (fork issue #513).
/// So this re-resolves on every poll rather than caching, and only accepts a
/// record once ALL THREE fields are populated — a mid-respawn record might
/// carry some but not others.
fn worker_fail_closed_identity(deck: &TuiDeck, worker_pane: &str) -> (u64, String, PathBuf) {
    let identity = RefCell::new(None);
    let found = common::wait_until(Duration::from_secs(30), || {
        let Some(record) = common::agent_records_on(deck.attach_socket_path())
            .into_iter()
            .find(|r| r.pane_id_env.as_deref() == Some(worker_pane))
        else {
            return false;
        };
        let (Some(boot_id), Some(generation), Some(cwd)) = (
            record.daemon_boot_id,
            record.registration_generation,
            record.cwd,
        ) else {
            return false;
        };
        *identity.borrow_mut() = Some((generation, boot_id, PathBuf::from(cwd)));
        true
    });
    assert!(
        found,
        "the worker pane must eventually reappear in ListAgents with a daemon_boot_id, \
         registration_generation and cwd, post-respawn (fork issue #513)"
    );
    identity.into_inner().expect("wait_until returned true")
}

/// Scenario: Launch the real TUI and its lazy daemon, open the two-role `orch-deck` fixture, then run the REAL `dot-agent-deck delegate` CLI from the orchestrator's identity stating subject `#589`, followed by the REAL `dot-agent-deck work-done` CLI from the worker's identity echoing back a DIFFERENT subject `#544` — Problem 2's exact observed shape, a coherent report on the wrong subject. The rendered orchestration surface must visibly carry a subject-mismatch warning naming both subjects, and must still carry the ordinary completion pointer to the worker's summary file (a mismatch warning augments the notification; it does not replace or suppress it).
#[spec("orchestration/work-done/006")]
#[test]
fn work_done_006_subject_mismatch_produces_a_visible_warning_in_the_attached_tui() {
    let deck = TuiDeck::builder()
        .with_pty_size(120, 40)
        .with_env("DOT_AGENT_DECK_WORKER_RESPONSE_TIMEOUT_MS", "0")
        .with_env("DOT_AGENT_DECK_DELEGATE_NO_EVENT_WINDOW_MS", "0")
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("No active sessions");
    open_orchestration(&deck);
    deck.wait_for_string(WORKER_ROLE);

    let (worker_pane, orchestrator_agent, orchestrator_pane) = orchestration_ids(&deck);
    let summary_file_name = work_done_file_name(WORKER_ROLE, &worker_pane);
    let pointer_needle_text = pointer_needle(&worker_pane);

    let delegate_output = run_delegate_cli_with_subject(
        &deck,
        &orchestrator_pane,
        WORKER_ROLE,
        "Do the thing under test for orchestration/work-done/006.",
        DELEGATED_SUBJECT,
    );
    assert!(
        delegate_output.status.success(),
        "`delegate --subject {DELEGATED_SUBJECT}` exited {:?} — PRD #586 M4's `--subject` \
         flag does not exist on the `Delegate` CLI yet\nstdout: {}\nstderr: {}",
        delegate_output.status.code(),
        String::from_utf8_lossy(&delegate_output.stdout),
        String::from_utf8_lossy(&delegate_output.stderr)
    );

    // The worker pane's REAL cwd, not `deck.workdir()`: `open_orchestration`
    // provisions an isolated clone in a sibling directory
    // (`resolve_workspace_path`, `src/ui.rs`), and that resolved path — not
    // `deck.workdir()` — is what `write_work_done_summary` actually writes
    // under.
    let (worker_generation, worker_boot_id, worker_cwd) =
        worker_fail_closed_identity(&deck, &worker_pane);
    let summary_path = worker_cwd.join(".dot-agent-deck").join(&summary_file_name);

    const SENTINEL: &str = "e2e-subject-mismatch-report-9c31";
    let work_done_output = run_work_done_cli_with_subject(
        &deck,
        &worker_pane,
        worker_generation,
        &worker_boot_id,
        &format!("Finished the delegated task. {SENTINEL}"),
        REPORTED_SUBJECT,
    );
    assert!(
        work_done_output.status.success(),
        "`work-done --subject {REPORTED_SUBJECT}` exited {:?} — PRD #586 M4's `--subject` \
         flag does not exist on the `WorkDone` CLI yet\nstdout: {}\nstderr: {}",
        work_done_output.status.code(),
        String::from_utf8_lossy(&work_done_output.stdout),
        String::from_utf8_lossy(&work_done_output.stderr)
    );

    // First: did the DAEMON compose and write the mismatch warning into the
    // orchestrator's PTY at all? Asserted before the grid, same discipline as
    // `work_done_004` above, so a daemon-side failure is never reported as a
    // rendering failure.
    let wrote = common::wait_until(Duration::from_secs(20), || {
        let pty = squeeze(&orchestrator_pty(&deck, &orchestrator_agent));
        pty.contains(&squeeze(SUBJECT_MISMATCH_NEEDLE))
            && pty.contains(&squeeze(DELEGATED_SUBJECT))
            && pty.contains(&squeeze(REPORTED_SUBJECT))
    });
    assert!(
        wrote,
        "the daemon never wrote a subject-mismatch warning into the orchestrator's PTY for a \
         delegate stating {DELEGATED_SUBJECT} against a work-done echoing {REPORTED_SUBJECT} \
         — Problem 2's exact observed shape (a coherent report, wrong subject) reaches the \
         orchestrator with no flag at all\nOrchestrator PTY:\n{}",
        orchestrator_pty(&deck, &orchestrator_agent)
    );

    // Then: does it reach the user's screen, naming both subjects?
    assert!(
        wait_for_pane_string(&deck, SUBJECT_MISMATCH_NEEDLE, Duration::from_secs(20)),
        "the mismatch warning reached the orchestrator's PTY but never became visible in the \
         rendered orchestration surface\nFinal grid:\n{}",
        deck.snapshot_grid()
    );
    assert!(
        pane_contains(&deck, DELEGATED_SUBJECT) && pane_contains(&deck, REPORTED_SUBJECT),
        "the mismatch warning must name BOTH subjects — what was delegated and what was \
         reported — or the orchestrator cannot tell which task actually got done\nFinal \
         grid:\n{}",
        deck.snapshot_grid()
    );

    // A mismatch warning augments the notification; it must never replace or
    // suppress the ordinary report the daemon exposes as ground truth (PRD
    // #586's Decisions table: the daemon exposes ground truth, it doesn't
    // withhold it).
    assert!(
        wait_for_pane_string(&deck, &pointer_needle_text, Duration::from_secs(20)),
        "a subject mismatch must not suppress the ordinary completion pointer\nFinal grid:\n{}",
        deck.snapshot_grid()
    );
    assert!(
        std::fs::read_to_string(&summary_path)
            .map(|contents| contents.contains(SENTINEL))
            .unwrap_or(false),
        "the summary file the orchestrator was pointed at must hold THIS worker's actual \
         report content even though its subject did not match. Path: {}",
        summary_path.display()
    );
}

/// Scenario: The regression guard for `006`. Same setup, but the REAL `dot-agent-deck delegate` and `dot-agent-deck work-done` CLIs both state the SAME subject `#589`. The rendered orchestration surface must still carry the ordinary completion pointer, and must NOT carry any subject-mismatch warning — guarding against a coder implementation that fires the warning unconditionally or too eagerly.
#[spec("orchestration/work-done/007")]
#[test]
fn work_done_007_matching_subjects_produce_no_mismatch_warning() {
    let deck = TuiDeck::builder()
        .with_pty_size(120, 40)
        .with_env("DOT_AGENT_DECK_WORKER_RESPONSE_TIMEOUT_MS", "0")
        .with_env("DOT_AGENT_DECK_DELEGATE_NO_EVENT_WINDOW_MS", "0")
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("No active sessions");
    open_orchestration(&deck);
    deck.wait_for_string(WORKER_ROLE);

    let (worker_pane, _orchestrator_agent, orchestrator_pane) = orchestration_ids(&deck);
    let pointer_needle_text = pointer_needle(&worker_pane);

    const SAME_SUBJECT: &str = "#589";

    let delegate_output = run_delegate_cli_with_subject(
        &deck,
        &orchestrator_pane,
        WORKER_ROLE,
        "Do the thing under test for orchestration/work-done/007.",
        SAME_SUBJECT,
    );
    assert!(
        delegate_output.status.success(),
        "`delegate --subject {SAME_SUBJECT}` exited {:?} — PRD #586 M4's `--subject` flag \
         does not exist on the `Delegate` CLI yet\nstdout: {}\nstderr: {}",
        delegate_output.status.code(),
        String::from_utf8_lossy(&delegate_output.stdout),
        String::from_utf8_lossy(&delegate_output.stderr)
    );

    let (worker_generation, worker_boot_id, _worker_cwd) =
        worker_fail_closed_identity(&deck, &worker_pane);

    const SENTINEL: &str = "e2e-subject-match-report-71ae";
    let work_done_output = run_work_done_cli_with_subject(
        &deck,
        &worker_pane,
        worker_generation,
        &worker_boot_id,
        &format!("Finished the delegated task. {SENTINEL}"),
        SAME_SUBJECT,
    );
    assert!(
        work_done_output.status.success(),
        "`work-done --subject {SAME_SUBJECT}` exited {:?} — PRD #586 M4's `--subject` flag \
         does not exist on the `WorkDone` CLI yet\nstdout: {}\nstderr: {}",
        work_done_output.status.code(),
        String::from_utf8_lossy(&work_done_output.stdout),
        String::from_utf8_lossy(&work_done_output.stderr)
    );

    assert!(
        wait_for_pane_string(&deck, &pointer_needle_text, Duration::from_secs(20)),
        "a matching-subject completion must still receive the ordinary completion pointer\n\
         Final grid:\n{}",
        deck.snapshot_grid()
    );
    assert!(
        !pane_contains(&deck, SUBJECT_MISMATCH_NEEDLE),
        "matching subjects must NOT produce a mismatch warning — a coder implementation that \
         fires the warning unconditionally or too eagerly would still pass every assertion in \
         `orchestration/work-done/006` and only this regression guard would catch it\nFinal \
         grid:\n{}",
        deck.snapshot_grid()
    );
}
