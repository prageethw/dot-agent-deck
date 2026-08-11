#![cfg(feature = "e2e")]
// `spawn_daemon_serve` and `wait_for_pane_text_on` are Unix-only in `common`
// (real Unix-socket attach protocol), matching every other socket-driven
// e2e file in this suite.
#![cfg(unix)]
//! PR #215 fixup (reviewer F2 / auditor H1): `orchestration/identity/008`
//! asserts against `CapturingPaneController`, a test double whose
//! `create_pane_with_options` records `opts.owner` and returns — it proves
//! the value reaches `AgentSpawnOptions` and nothing about whether anything
//! reads it. That gap is exactly why CI stayed green while
//! `EmbeddedPaneController::create_stream_pane` silently dropped `opts.owner`
//! on the floor (the interactive/restore path never carried
//! `DOT_AGENT_DECK_WORKTREE_OWNER` at all).
//!
//! This test reaches the real seam instead: the actual `dot-agent-deck
//! daemon serve` BINARY, spawned as a real subprocess (`common::spawn_daemon_serve`,
//! the same harness `e2e_reconnect_agent_type.rs` uses), and a real
//! `EmbeddedPaneController` attached to it over its real Unix socket — the
//! same client code the TUI uses. `create_pane_with_options` spawns a
//! genuine `portable_pty` child process — not a mock. The child's own
//! command reads `$DOT_AGENT_DECK_WORKTREE_OWNER` out of *its own*
//! environment and echoes it to stdout, which this test reads back over the
//! attach socket. If `create_stream_pane` ever again forwards `opts.owner`
//! nowhere, the child's environment genuinely lacks the variable, `echo`
//! prints an empty value, and the assertion fails — a mock cannot produce
//! that failure because a mock has no environment to fail to populate.
//!
//! Tier: e2e because it spawns the real binary and a real child process. It
//! hits NO LLM, needs no credentials, and is deterministic.

mod common;

use std::time::Duration;

use dot_agent_deck::embedded_pane::EmbeddedPaneController;
use dot_agent_deck::pane::{AgentSpawnOptions, PaneController};
use spec::spec;

/// The exact creator string a live orchestration would have stamped into
/// its worktree marker (`orchestration_creator_string`'s output shape) —
/// used here as `AgentSpawnOptions::owner`, the same field
/// `create_pane_with_options`'s production callers in `src/ui.rs`/`src/tab.rs`
/// populate.
const TEST_OWNER: &str = "orchestration:worktree-owner-env-009-fixture";

/// Scenario: Start the real `dot-agent-deck daemon serve` binary and attach
/// a real `EmbeddedPaneController` to it, exactly as the TUI does. Call
/// `create_pane_with_options` with `AgentSpawnOptions::owner` set (the same
/// field the orchestration create/restore paths populate) and a shell
/// command that echoes `$DOT_AGENT_DECK_WORKTREE_OWNER` back out. Read the
/// spawned process's actual stdout back over the attach socket and confirm
/// it contains the owner string — proving the value reached a genuinely
/// spawned child's environment, not merely a mock's recorded field.
#[spec("orchestration/identity/009")]
#[test]
fn identity_009_owner_reaches_a_genuinely_spawned_process_environment() {
    let daemon = common::spawn_daemon_serve(None, "0");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("controller runtime");
    let controller =
        EmbeddedPaneController::new(daemon.attach_socket.clone(), runtime.handle().clone());

    let (pane_id, _resolved) = controller
        .create_pane_with_options(
            // A bare `echo` completes and exits within milliseconds, and
            // `AgentPtyRegistry::agent_records()` filters out already-exited
            // entries (fork #166 CI run 31444090426: 3/3 fails on
            // `wait_for_agent_where` below under load). Trailing `sleep 30`
            // keeps the child registered long enough for the poll to observe
            // it, matching `restore_018`'s fix for the same race.
            Some("echo OWNER-IS:$DOT_AGENT_DECK_WORKTREE_OWNER:OWNER-END; sleep 30"),
            None,
            AgentSpawnOptions {
                owner: Some(TEST_OWNER.to_string()),
                ..AgentSpawnOptions::default()
            },
        )
        .expect("create_pane_with_options over the real daemon binary's attach socket");

    let agent = daemon
        .wait_for_agent_where(
            |r| r.pane_id_env.as_deref() == Some(pane_id.as_str()),
            Duration::from_secs(5),
        )
        .expect("daemon must register the spawned agent for this pane");

    let expected = format!("OWNER-IS:{TEST_OWNER}:OWNER-END");
    assert!(
        common::wait_for_pane_text_on(
            &daemon.attach_socket,
            &agent.id,
            &expected,
            Duration::from_secs(5)
        ),
        "the spawned process's own environment must carry \
         DOT_AGENT_DECK_WORKTREE_OWNER={TEST_OWNER} -- expected {expected:?} in the real \
         daemon-spawned process's output, got scrollback key: {:?}",
        common::pane_search_key_on(&daemon.attach_socket, &agent.id)
    );
}
