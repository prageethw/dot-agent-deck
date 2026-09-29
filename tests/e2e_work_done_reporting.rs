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
    //
    // EVERY POSITIVE NEEDLE BELOW WAITS. The daemon composes one message and the
    // TUI renders whatever of it has arrived, so a grid sampled the instant the
    // FIRST needle appears can legitimately be mid-message — the label drawn and
    // the framed report not yet. Issue #818's sibling symptom: on a contended
    // runner this test failed at the report-frame assertion having PASSED the two
    // above it, which is that partial render and not a missing report. These were
    // instantaneous `pane_contains` checks; each is now a bounded wait, which
    // asserts exactly the same thing (the needle must become visible) without
    // pinning WHEN inside the message's own render. The budget is `load_scaled`
    // for the reason issue #709 gives: a fast box still fails fast.
    //
    // THE WINDOW IS MEASURED, not inferred. Polling from the instant the PTY
    // wait above returned, for how long each later needle stayed ABSENT: under a
    // 96-way CPU load on 16 cores, `SENTINEL` — the last thing painted — was
    // still absent for a further 27.6ms and 37.4ms in 2 of 5 runs, while
    // `DAEMON_CLAUSE` and `REPORT_FRAME_NEEDLE` were already there. Read those
    // two figures only as "a real window exists, of order tens of ms": the
    // sub-millisecond numbers the same probe reported for the other needles are
    // dominated by the cost of three sequential `snapshot_grid` calls, so this
    // measures presence and absence rather than paint latency. A single-shot read
    // evaluates its predicate roughly one snapshot after that instant, so in
    // those 2 runs it would have read `SENTINEL` as absent — and a 4-vCPU runner
    // inside a full tier starves the render loop far harder than a loaded 16-core
    // box, which is what `e2e-deterministic` hit: the captured grid showed the
    // framing painted and cut off immediately before the sentinel.
    //
    // NOT reproduced as a local red on the old code: 0 of 8 solo runs under the
    // same load, consistent with a ~2-in-5 window that only fires when the check
    // lands inside it. The justification is the measured window plus that CI
    // grid, not a local red-to-green.
    let visible_timeout = common::load_scaled(Duration::from_secs(20));
    assert!(
        wait_for_pane_string(&deck, UNSOLICITED_NEEDLE, visible_timeout),
        "the unsolicited label reached the orchestrator's PTY but never became visible in the \
         rendered orchestration surface\nFinal grid:\n{}",
        deck.snapshot_grid()
    );
    // Fork issue #513: DAEMON_CLAUSE and the untrusted-report frame both sit
    // LATER than UNSOLICITED_NEEDLE inside the one composed, one-shot
    // `write_all` payload (`compose_work_done_feedback`'s `head` then
    // `tail` in `src/state.rs`) — the daemon never writes them separately,
    // so there is no product-side staggering here. But rendering this
    // payload onto the grid is NOT atomic from this test's point of view:
    // the daemon's write reaches `cat`'s stdin as one pane-write, `cat`
    // echoes it, and a background thread reads THAT output off the PTY and
    // feeds it into the vt100 parser this file reads via
    // `deck.snapshot_grid()`) across however many `read()`s that takes —
    // under heavy CI parallelism, a fully-appeared *prefix* of the message
    // (enough for `wait_for_pane_string` above to see UNSOLICITED_NEEDLE)
    // does not guarantee the *rest* of the same message has been read and
    // parsed yet. A bare, non-retrying `pane_contains` immediately after
    // that wait was exactly this false assumption, and is what made CI run
    // 34551298486 fail at 124.638s on this exact assertion while its own
    // panic-message grid dump (a SECOND, slightly later `snapshot_grid()`
    // call) showed the content already present — proof the content was
    // still arriving, not missing. Every positive needle check here now
    // waits like UNSOLICITED_NEEDLE's already does; only the ABSENCE
    // checks below stay one-shot, since nothing here makes an absent
    // pointer/file start existing over time.
    assert!(
        wait_for_pane_string(&deck, DAEMON_CLAUSE, visible_timeout),
        "the label must identify itself as a daemon report, not as a message from a person or an \
         agent\nFinal grid:\n{}",
        deck.snapshot_grid()
    );
    assert!(
        wait_for_pane_string(&deck, REPORT_FRAME_NEEDLE, visible_timeout)
            && wait_for_pane_string(&deck, SENTINEL, visible_timeout),
        "the worker's own report must still reach the orchestrator, framed as untrusted \
         data\nFinal grid:\n{}",
        deck.snapshot_grid()
    );
    // Deliberately NOT a wait: this is a negative check, and polling for an
    // absence would pass on the first frame that has not painted it yet —
    // vacuously, and most easily on exactly the starved runner this test keeps
    // failing on. It is sound as a single read because the three waits above
    // have just proved this write finished painting.
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
// `--subject` has existed on both `delegate` and `work-done` since PRD #586
// M4 landed in PR #593. The two tests below exercise the real CLI end to
// end: `run_delegate_cli_with_subject`/`run_work_done_cli_with_subject`
// invoke the actual subprocess argument, so a regression that dropped the
// flag would fail here on a clap "unrecognized argument" runtime error
// rather than a compile error.

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
///
/// Issue #567: this subprocess is launched from the test harness's own
/// process, not `caller_pane`'s real spawned child, so it never inherits the
/// `DOT_AGENT_DECK_REGISTRATION_GENERATION` / `DOT_AGENT_DECK_DAEMON_BOOT_ID`
/// env vars a real spawn injects — same fork-#358-style gap
/// `run_work_done_cli_with_subject` below already closes for the worker side.
/// Query the daemon's own `ListAgents` for the values it assigned
/// `caller_pane`, the same technique `worker_fail_closed_identity` uses.
fn run_delegate_cli_with_subject(
    deck: &TuiDeck,
    caller_pane: &str,
    to: &str,
    task: &str,
    subject: &str,
) -> std::process::Output {
    let caller_record = common::agent_records_on(deck.attach_socket_path())
        .into_iter()
        .find(|r| r.pane_id_env.as_deref() == Some(caller_pane))
        .expect("the caller pane must still be present in ListAgents");
    let caller_generation = caller_record
        .registration_generation
        .expect("the caller pane must carry a registration_generation once registered");
    let caller_boot_id = caller_record
        .daemon_boot_id
        .expect("ListAgents must report a daemon_boot_id (fork issue #513)");
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
        .env(
            "DOT_AGENT_DECK_REGISTRATION_GENERATION",
            caller_generation.to_string(),
        )
        .env("DOT_AGENT_DECK_DAEMON_BOOT_ID", caller_boot_id)
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
#[spec("orchestration/work-done/007")]
#[test]
fn work_done_007_subject_mismatch_produces_a_visible_warning_in_the_attached_tui() {
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
        "Do the thing under test for orchestration/work-done/007.",
        DELEGATED_SUBJECT,
    );
    assert!(
        delegate_output.status.success(),
        "`delegate --subject {DELEGATED_SUBJECT}` exited {:?} — expected `--subject` to be \
         accepted by the `Delegate` CLI\nstdout: {}\nstderr: {}",
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
        "`work-done --subject {REPORTED_SUBJECT}` exited {:?} — expected `--subject` to be \
         accepted by the `WorkDone` CLI\nstdout: {}\nstderr: {}",
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
#[spec("orchestration/work-done/008")]
#[test]
fn work_done_008_matching_subjects_produce_no_mismatch_warning() {
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
        "Do the thing under test for orchestration/work-done/008.",
        SAME_SUBJECT,
    );
    assert!(
        delegate_output.status.success(),
        "`delegate --subject {SAME_SUBJECT}` exited {:?} — expected `--subject` to be \
         accepted by the `Delegate` CLI\nstdout: {}\nstderr: {}",
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
        "`work-done --subject {SAME_SUBJECT}` exited {:?} — expected `--subject` to be \
         accepted by the `WorkDone` CLI\nstdout: {}\nstderr: {}",
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
         `orchestration/work-done/007` and only this regression guard would catch it\nFinal \
         grid:\n{}",
        deck.snapshot_grid()
    );
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
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("No active sessions");
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
    let output = run_delegate_cli_with_subject(
        deck,
        orchestrator_pane,
        WORKER_ROLE,
        "Do the thing under test for status/observing.",
        "#803",
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

    let (worker_generation, worker_boot_id, _worker_cwd) =
        worker_fail_closed_identity(&deck, &worker_pane);
    let work_done_output = run_work_done_cli_with_subject(
        &deck,
        &worker_pane,
        worker_generation,
        &worker_boot_id,
        "Finished the delegated task. e2e-observing-report-5d20",
        "#803",
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

/// Scenario: Launch the real TUI and its daemon, open the two-role `orch-deck` orchestration, announce the `cat` orchestrator as an idle agent, and run the REAL `delegate` CLI from the orchestrator to the worker, which respawns the worker under a new agent id. Write the `SessionStart` the respawned worker's hook would send, so the deck replaces the worker's session: the orchestrator's card must STILL read `Observing` once the worker's card shows a live agent. Then run the REAL `work-done` CLI from the worker: the orchestrator's card must go back to `Idle`.
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

    let (worker_generation, worker_boot_id, _worker_cwd) =
        worker_fail_closed_identity(&deck, &worker_pane);
    let work_done_output = run_work_done_cli_with_subject(
        &deck,
        &worker_pane,
        worker_generation,
        &worker_boot_id,
        "Finished the delegated task. e2e-observing-report-9c41",
        "#803",
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
