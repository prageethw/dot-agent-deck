#![cfg(feature = "e2e")]
// `spawn_daemon_serve` and `wait_for_pane_text_on` are Unix-only in `common`
// (real Unix-socket attach protocol), matching every other socket-driven
// e2e file in this suite.
#![cfg(unix)]
//! Fork #166 restart-gap follow-up (PR #215 review round, findings-215-restart-manual.md):
//! `session/restore/017` proves the persisted `owner` string survives a TOML
//! round trip and reaches `AgentSpawnOptions::owner` — via `CapturingPaneController`,
//! a test double whose `create_pane_with_options` records the field and returns.
//! That is the same one-hop-short gap `orchestration/identity/009`'s file doc
//! describes for the CREATE path: a capturing mock proves the value reaches
//! `AgentSpawnOptions`, nothing about whether a genuinely spawned process's own
//! environment ever carries it.
//!
//! This test closes that gap for the RESTORE path specifically. It calls
//! `TabManager::open_orchestration_tab` with the exact argument shape the
//! daemon-empty restore branch in `run_tui` uses (`creator` = the snapshot's
//! `owner.as_deref()`), against a real `dot-agent-deck daemon serve` BINARY
//! (`common::spawn_daemon_serve`) through a real `EmbeddedPaneController` —
//! the same client code the TUI uses. Both role panes' commands echo
//! `$DOT_AGENT_DECK_WORKTREE_OWNER` out of their OWN environment; this test
//! reads their real stdout back over the attach socket.
//!
//! What this does NOT cover: the private `resolve_orchestration_for_restore`
//! (config re-resolution, drift checks, the anti-tampering `project_path`
//! check) is L1-only (`session/restore/015`–`017`) since it is not `pub` and
//! this file is a separate crate. This test starts from an already-resolved
//! `OrchestrationConfig` and an owner string, exactly as `run_tui`'s restore
//! branch does the instant `resolve_orchestration_for_restore` returns `Ok`.
//! It also does not drive a real `SavedSession` save→process-exit→reload
//! cycle — `session/restore/017` covers the TOML round trip. Combined, the
//! two tests cover snapshot-write → TOML round trip → resolve → real spawn →
//! real env, with only the resolve step itself left at the mock/L1 layer.
//!
//! Tier: e2e because it spawns the real binary and two real child processes.
//! It hits NO LLM, needs no credentials, and is deterministic.

mod common;

use std::time::Duration;

use dot_agent_deck::embedded_pane::EmbeddedPaneController;
use dot_agent_deck::pane::PaneController;
use dot_agent_deck::project_config::load_project_config;
use dot_agent_deck::tab::TabManager;
use spec::spec;
use std::sync::Arc;

/// The exact creator string a live orchestration would have persisted into
/// its `OrchestrationSnapshot.owner` field (`orchestration_creator_string`'s
/// output shape) — passed here as `open_orchestration_tab`'s `creator`
/// argument, exactly as `orch_snap.owner.as_deref()` is on the restore path.
const TEST_OWNER: &str = "orchestration:worktree-owner-restore-018-fixture";

/// Scenario: Write a real `.dot-agent-deck.toml` defining an orchestration
/// with two roles whose commands each echo `$DOT_AGENT_DECK_WORKTREE_OWNER`.
/// Start the real `dot-agent-deck daemon serve` binary and attach a real
/// `EmbeddedPaneController` to it. Call `TabManager::open_orchestration_tab`
/// with the resolved config and `Some(TEST_OWNER)` as `creator` — the exact
/// call shape `run_tui`'s daemon-empty restore branch uses after
/// `resolve_orchestration_for_restore` succeeds. Read BOTH spawned role
/// panes' real stdout back over the attach socket and confirm each carries
/// the owner string — proving the identity reaches every restored role's
/// real process environment, not just the start role, and not merely a
/// mock's recorded field.
#[spec("session/restore/018")]
#[test]
fn restore_018_owner_reaches_every_restored_role_panes_real_environment() {
    let tmp = common::harness_tempdir().expect("tempdir");
    let worktree = tmp.path().join("dot-agent-deck-restore-018");
    std::fs::create_dir_all(&worktree).expect("create worktree dir");
    // `open_orchestration_tab` spawns non-start roles BEFORE the start role
    // (`src/tab.rs`'s fork #92 P1 spawn-order comment), so `worker` here is
    // actually the FIRST child forked. A bare `echo` exits within
    // milliseconds, and `AgentPtyRegistry::agent_records()` filters out
    // exited entries entirely (`agent_records_filters_exited_entries`) — a
    // process that has already exited and been filtered is indistinguishable
    // from one that was never spawned. The trailing `sleep 30` keeps each
    // child alive long enough for `wait_for_agent_where`'s poll to observe it
    // as a live registry entry — the SAME race `orchestration/identity/009`'s
    // single-role variant hits under load (fork #166 CI run 31444090426: 3/3
    // fails on `wait_for_agent_where` with a bare `echo`), not one this
    // two-role test is special in having; both need the keep-alive for the
    // identical reason.
    let toml = "[[orchestrations]]\n\
                name = \"restore-018-fixture\"\n\n\
                [[orchestrations.roles]]\n\
                name = \"orchestrator\"\n\
                command = \"echo ROLE-orchestrator-OWNER-IS:$DOT_AGENT_DECK_WORKTREE_OWNER:OWNER-END; sleep 30\"\n\
                start = true\n\n\
                [[orchestrations.roles]]\n\
                name = \"worker\"\n\
                command = \"echo ROLE-worker-OWNER-IS:$DOT_AGENT_DECK_WORKTREE_OWNER:OWNER-END; sleep 30\"\n";
    std::fs::write(worktree.join(".dot-agent-deck.toml"), toml)
        .expect("write fixture .dot-agent-deck.toml");
    let worktree_str = worktree.display().to_string();

    // The same re-resolution `resolve_orchestration_for_restore` performs
    // (minus its private drift/tamper checks, covered at L1 by
    // `session/restore/015`-`017`) — load the config back off disk exactly
    // as the restore path does.
    let cfg = load_project_config(&worktree)
        .expect("load fixture config")
        .expect("fixture config must be present");
    let orch_config = cfg
        .orchestrations
        .into_iter()
        .find(|o| o.name == "restore-018-fixture")
        .expect("fixture orchestration must resolve");

    let daemon = common::spawn_daemon_serve(None, "0");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("controller runtime");
    let controller: Arc<dyn PaneController> = Arc::new(EmbeddedPaneController::new(
        daemon.attach_socket.clone(),
        runtime.handle().clone(),
    ));
    let mut tab_manager = TabManager::new(controller);

    let (_tab_idx, role_pane_ids) = tab_manager
        .open_orchestration_tab(
            &orch_config,
            &worktree_str,
            None,
            None,
            Some(TEST_OWNER),
            (24, 80),
        )
        .expect("open_orchestration_tab over the real daemon binary's attach socket");
    assert_eq!(role_pane_ids.len(), 2, "fixture defines exactly two roles");

    for (role_name, pane_id) in [
        ("orchestrator", &role_pane_ids[0]),
        ("worker", &role_pane_ids[1]),
    ] {
        let agent = daemon
            .wait_for_agent_where(
                |r| r.pane_id_env.as_deref() == Some(pane_id.as_str()),
                Duration::from_secs(5),
            )
            .unwrap_or_else(|| panic!("daemon must register the spawned {role_name} agent"));

        let expected = format!("ROLE-{role_name}-OWNER-IS:{TEST_OWNER}:OWNER-END");
        assert!(
            common::wait_for_pane_text_on(
                &daemon.attach_socket,
                &agent.id,
                &expected,
                Duration::from_secs(5)
            ),
            "the restored {role_name} role pane's own environment must carry \
             DOT_AGENT_DECK_WORKTREE_OWNER={TEST_OWNER} -- expected {expected:?}, got scrollback \
             key: {:?}",
            common::pane_search_key_on(&daemon.attach_socket, &agent.id)
        );
    }
}
