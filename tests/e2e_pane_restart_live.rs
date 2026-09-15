#![cfg(feature = "e2e")]

//! PRD #699 fix-round (issue #782 / upstream PR #918 review): PTY-attached
//! L2 coverage for `pane restart <role>` — the coverage gap every existing
//! `pane/restart/*` entry (`tests/pane_restart.rs`) leaves open. Those tests
//! all call `handle_restart_role_with_state` directly, bypassing the
//! CLI/socket layer AND a real attached TUI entirely — they prove the
//! daemon's own registry/`delegate_targets` bookkeeping survives a restart,
//! never that an ALREADY-ATTACHED viewer keeps rendering the pane correctly
//! once the daemon quietly swaps out its agent underneath it. That is
//! fundamentally an attached-view question no handler-level test can
//! exercise — exactly why this file is a genuine L2, PTY-attached test
//! (modeled directly on `tests/e2e_pane_spawn_live.rs`'s `spawn_005`, the
//! established real-TUI-via-PTY + real daemon + real CLI-subprocess
//! technique for this same `pane <verb>` family), not another entry in
//! `tests/pane_restart.rs`.

mod common;

use std::time::Duration;

use common::{TuiDeck, commit_fixture, open_orchestration, retry_pane_restart_until_success};
use dot_agent_deck::agent_pty::TabMembership;
use spec::spec;

const CODER_ROLE: &str = "coder";

/// The mutated running config `coder` is swapped onto after it crashes: same
/// orchestration/role shape as the fixture, but `coder` now runs the
/// long-lived `cat` instead of the short-lived boot command, so the pane
/// `pane restart` produces does not race its own second self-exit before
/// this test can prove anything about it. `clear = false` keeps the later
/// `delegate` reachability check from respawning `coder` a second time.
const LONG_LIVED_CONFIG: &str = "[[orchestrations]]\n\
     name = \"restart-orch\"\n\
     \n\
     [[orchestrations.roles]]\n\
     name = \"orchestrator\"\n\
     command = \"cat\"\n\
     start = true\n\
     \n\
     [[orchestrations.roles]]\n\
     name = \"coder\"\n\
     command = \"cat\"\n\
     clear = false\n";

/// Run the real `dot-agent-deck delegate` CLI as a subprocess from
/// `caller_pane`'s identity — the same fork-#358/#567 caller-identity
/// technique `tests/e2e_work_done_reporting.rs`'s
/// `run_delegate_cli_with_subject` and `tests/e2e_dispatcher_mode.rs`'s
/// `run_delegate_to` already use, reimplemented locally since integration
/// test binaries cannot share helpers across files.
fn run_delegate_cli(
    deck: &TuiDeck,
    caller_pane: &str,
    to: &str,
    task: &str,
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

/// Scenario: Open a real orchestration tab (`pane-restart-live` fixture:
/// `orchestrator` [start] + `coder`, `coder` running a short-lived command
/// so it exits on its own shortly after boot). Mutate the RUNNING
/// orchestration's own `.dot-agent-deck.toml` (the same "operator edited the
/// config mid-session" technique `tests/e2e_pane_spawn_live.rs`'s `spawn_005`
/// uses) so `coder`'s command becomes the long-lived `cat` — the post-restart
/// respawn re-reads this file fresh, so without the swap the restarted pane
/// would just race its own second self-exit before this test could observe
/// anything. Then retry the REAL `dot-agent-deck pane restart coder` CLI
/// subprocess (no `--force` needed, since the pane genuinely crashes on its
/// own) until it succeeds, exactly as a real orchestrator agent would from
/// inside its own pane: `ListAgents`/`agent_records()` deliberately filters
/// out exited-but-not-reaped entries (`src/agent_pty.rs:8071-8082`), so
/// polling it for coder's `crashed == Some(true)` marker can never observe
/// that marker firing — the real CLI call itself, retried against the "has
/// not crashed; pass --force" refusal (`src/state.rs:8258-8266`) until
/// coder's boot command has actually exited, is the reliable signal
/// available from outside the daemon. A successful restart IS the proof the
/// precondition held; any OTHER failure is a genuine test failure, not a
/// wait condition. Once restarted, prove the pane is genuinely reachable —
/// not merely that the daemon's own registry says so — by running the REAL
/// `dot-agent-deck delegate --to coder` CLI and confirming the delegated
/// task's one-line file-pointer (`compose_delegate_prompt`'s "Read
/// .dot-agent-deck/worker-task-coder..." text, the same substring
/// `tests/e2e_pi_live.rs` already waits for to prove a delegate landed)
/// actually renders in `coder`'s pane through the STILL-ATTACHED TUI. A
/// daemon that only fixes its own bookkeeping but never gets an
/// already-attached viewer to follow the pane onto its new agent would pass
/// every handler-level `pane/restart/*` test and still fail this one.
#[spec("pane/restart/011")]
#[test]
fn restart_011_restarted_pane_stays_reachable_in_an_already_attached_tui() {
    let deck = TuiDeck::builder()
        .with_pty_size(120, 40)
        .launch_with_fixture("pane-restart-live");
    let work = deck.workdir().to_path_buf();
    // Isolated-clone provisioning needs a ref to branch from — an unborn HEAD
    // (the harness's own bare `git init`) does not provide one.
    commit_fixture(&work);
    deck.wait_for_string("No active sessions");

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent"); // form closed -> tab up, orchestrator focused
    deck.wait_for_string("[Command Mode Ctrl+D]"); // live PTY, PaneInput mode, orchestrator focused

    assert!(
        deck.wait_for_grid_string_within("coder", Duration::from_secs(10)),
        "the coder role card must be visible on the freshly-opened orchestration \
         tab before this test touches anything else.\nGrid:\n{}",
        deck.snapshot_grid()
    );

    // Read back the orchestrator's real (daemon-minted) pane id and the
    // isolated-clone cwd its role panes actually run in -- both come from the
    // daemon's own registry, never reconstructed by hand, matching
    // `spawn_005`'s technique exactly.
    let records = common::agent_records_on(deck.attach_socket_path());
    let orchestrator_record = records
        .iter()
        .find(|r| {
            matches!(
                &r.tab_membership,
                Some(TabMembership::Orchestration { role_name, is_start_role: true, .. })
                    if role_name == "orchestrator"
            )
        })
        .expect("the orchestrator role must be registered with its tab membership");
    let orchestrator_pane_id = orchestrator_record
        .pane_id_env
        .clone()
        .expect("the orchestrator role must carry a DOT_AGENT_DECK_PANE_ID");
    let orchestration_cwd = orchestrator_record
        .cwd
        .clone()
        .expect("the orchestrator role must carry its (isolated-clone) cwd");

    // Swap coder's command to a long-lived one BEFORE the restart retry loop
    // below starts: the respawn re-reads this file fresh
    // (`lookup_orchestration_role_indexed` -> `load_project_config`), so this
    // is exactly what the restarted pane runs once a restart finally
    // succeeds.
    let running_config_path = std::path::Path::new(&orchestration_cwd).join(".dot-agent-deck.toml");
    std::fs::write(&running_config_path, LONG_LIVED_CONFIG)
        .expect("swap coder's command to a long-lived one before restarting it");

    // Retry the REAL `pane restart coder` CLI call itself until it succeeds,
    // instead of polling `ListAgents` for a `crashed` marker that
    // `agent_records()` never surfaces for an exited-but-not-reaped entry —
    // see `retry_pane_restart_until_success`'s own doc comment for why. A
    // successful restart IS the proof coder's short-lived boot command had
    // already exited.
    retry_pane_restart_until_success(
        deck.hook_socket_path(),
        &orchestrator_pane_id,
        CODER_ROLE,
        Duration::from_secs(30),
    );

    deck.wait_until_quiescent();

    let delegate_output = run_delegate_cli(
        &deck,
        &orchestrator_pane_id,
        CODER_ROLE,
        "prove the restarted pane is reachable",
    );
    assert!(
        delegate_output.status.success(),
        "`delegate --to coder` must succeed against the just-restarted pane -- \
         stdout={}, stderr={}",
        String::from_utf8_lossy(&delegate_output.stdout),
        String::from_utf8_lossy(&delegate_output.stderr)
    );

    assert!(
        deck.wait_for_grid_string_within("worker-task-coder", Duration::from_secs(15)),
        "a role restarted via `pane restart` must stay genuinely reachable in an \
         ALREADY-ATTACHED TUI -- the delegated task's file-pointer text never \
         rendered in coder's pane, meaning the attached view never followed the \
         pane onto its new, restarted agent.\nGrid:\n{}",
        deck.snapshot_grid()
    );
}
