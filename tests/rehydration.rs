// PRD #42 M8: the mock attach servers here bind Unix-domain sockets
// (tokio::net::UnixListener/UnixStream) and set 0o600 socket perms via
// PermissionsExt, so this suite is Unix-only at the source level.
// `#![cfg(unix)]` makes the crate empty on Windows so the cross-platform test
// build compiles; on Unix every test still runs exactly as before. A named-pipe
// port of this harness for Windows is tracked by #164 (M10).
#![cfg(unix)]
//! PRD #76 M2.x — TUI session-list rehydration on bootstrap.
//!
//! The bug: in external-daemon mode the TUI never queried the daemon for
//! existing agents on startup, so an ssh-reconnect via `dot-agent-deck
//! connect` showed "No active sessions" even though the daemon had live
//! agents from the previous TUI session. `DaemonClient::list_agents` had
//! zero production callers.
//!
//! These tests pin the new bootstrap step in
//! [`EmbeddedPaneController::hydrate_from_daemon`]:
//!   - happy path: every `list_agents` id ends up as a stream-backed pane
//!     and STREAM_OUT bytes from each agent reach the corresponding vt100
//!     parser (the daemon-replayed scrollback snapshot, then live bytes);
//!   - empty list: hydrate returns no panes and does not error;
//!   - `list_agents` failure: hydrate logs at debug and returns no panes
//!     (the user can retry by reconnecting);
//!   - race: an agent terminates between `list_agents` and `attach` — the
//!     missing one is skipped, the rest still attach.

#![cfg(unix)]

// Issue #322. This crate is fast-tier and deliberately does NOT link
// `tests/common/mod.rs`; pulling the PTY harness in would duplicate its ~530
// executions here. But its eight scratch dirs each hold an `attach.sock`, so
// they were the largest single source of live `/tmp/.tmp*` directories during a
// recorded `cargo test-e2e` — sampling attributed most of the 49 observed to
// this file. Sharing the ~40-line crate-internal resolver by path costs one
// module and two extra test executions instead. See
// `docs/develop/e2e-temp-dirs.md`.
#[path = "../src/test_temp.rs"]
mod test_temp;
// Issue #668: same shape, same reason — the wrapped-child lifetime bound this
// file's registry spawns and in-process daemon need, in a file small enough to
// include on its own instead of pulling in the harness. `common::init_test_env`
// calls the same `arm()`.
#[path = "common/child_lifetime_bound.rs"]
mod child_lifetime_bound;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tempfile::TempDir;
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinHandle;

use chrono::Utc;
use dot_agent_deck::agent_pty::{
    AgentPtyRegistry, AgentRecord, DOT_AGENT_DECK_AGENT_ID, DOT_AGENT_DECK_PANE_ID, SpawnOptions,
    TabMembership,
};
use dot_agent_deck::daemon::{Daemon, run_daemon_with};
use dot_agent_deck::daemon_client::{DaemonClient, StartAgentOptions};
use dot_agent_deck::daemon_protocol::{
    AttachRequest, AttachResponse, KIND_EVENT, KIND_REQ, KIND_RESP, KIND_STREAM_END,
    bind_attach_listener, read_frame, serve_attach, serve_attach_with_counter, write_frame,
};
use dot_agent_deck::embedded_pane::EmbeddedPaneController;
use dot_agent_deck::event::{AgentEvent, AgentType, BroadcastMsg, EventType, Writable};
use dot_agent_deck::reconnect::{HydrationGate, run_event_subscriber};
use dot_agent_deck::state::{
    ActiveTool, AppState, OrchestrationIdentity, SessionSnapshot, SessionState, SessionStatus,
    SharedState,
};
use dot_agent_deck::ui::{
    dead_slot_pane_id, fill_dead_slots_with_placeholders, is_dead_slot_pane_id,
    partition_hydrated_panes, resolve_orch_config_for_hydration,
};
use spec::spec;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::AtomicUsize;

// `bind_attach_listener` flips the process-global umask while binding; share
// the lock with the other M-series tests so concurrent tempdir creation
// can't inherit a 0o600 dir during that window.
static HARNESS_BIND_LOCK: Mutex<()> = Mutex::new(());

struct Server {
    _dir: TempDir,
    path: PathBuf,
    registry: Arc<AgentPtyRegistry>,
    handle: JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

const CLI_REHYDRATION_PANE: &str = "live-cli-rehydrate-pane-53d9a7";

/// Full in-process daemon used only by `session/live/011`: unlike this file's
/// attach-only servers, it owns both the hook socket consumed by the real
/// `agent-event` CLI and the attach socket consumed by a fresh TUI hydrate.
struct AgentEventDaemon {
    _dir: TempDir,
    hook_path: PathBuf,
    attach_path: PathBuf,
    registry: Arc<AgentPtyRegistry>,
    event_tx: tokio::sync::broadcast::Sender<BroadcastMsg>,
    handle: JoinHandle<()>,
}

impl Drop for AgentEventDaemon {
    fn drop(&mut self) {
        self.handle.abort();
        self.registry.shutdown_all();
    }
}

async fn start_agent_event_daemon() -> AgentEventDaemon {
    // Issue #668: this harness runs a real daemon in-process and hands its
    // registry to the test, so arm the wrapped-child lifetime bound before
    // anything it spawns exists.
    child_lifetime_bound::arm();

    let dir = test_temp::tempdir().expect("allocate real-agent-event daemon tempdir");
    let hook_path = dir.path().join("hook.sock");
    let attach_path = dir.path().join("attach.sock");
    let state: SharedState = Arc::new(tokio::sync::RwLock::new(AppState::default()));
    let daemon = Daemon::with_attach(state, attach_path.clone())
        .with_idle_shutdown(None)
        .with_lock_dir_override(Some(dir.path().join("locks")));
    let registry = daemon.pty_registry.clone();
    let event_tx = daemon.event_tx.clone();
    let hook_for_task = hook_path.clone();
    let handle = tokio::spawn(async move {
        let _ = run_daemon_with(&hook_for_task, daemon).await;
    });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let hook_ready = tokio::net::UnixStream::connect(&hook_path).await.is_ok();
        let attach_ready = tokio::net::UnixStream::connect(&attach_path).await.is_ok();
        if hook_ready && attach_ready {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "full in-process daemon sockets were not accepting connections within 5s: hook={} attach={}",
            hook_path.display(),
            attach_path.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    AgentEventDaemon {
        _dir: dir,
        hook_path,
        attach_path,
        registry,
        event_tx,
        handle,
    }
}

/// Run the real lifecycle CLI with exactly the pane/agent identity a daemon
/// injects, then await its daemon broadcast so hydration cannot race ingestion.
async fn run_real_agent_event(
    daemon: &AgentEventDaemon,
    pane_id: &str,
    agent_id: &str,
    cwd: &Path,
) -> AgentEvent {
    let mut events = daemon.event_tx.subscribe();
    let hook_path = daemon.hook_path.clone();
    let pane_id_owned = pane_id.to_string();
    let agent_id_owned = agent_id.to_string();
    let cwd = cwd.to_path_buf();
    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new(env!("CARGO_BIN_EXE_dot-agent-deck"))
            .arg("agent-event")
            .arg("--type")
            .arg("running")
            .current_dir(&cwd)
            .env_clear()
            .env("HOME", &cwd)
            .env("DOT_AGENT_DECK_SOCKET", &hook_path)
            .env(DOT_AGENT_DECK_PANE_ID, &pane_id_owned)
            .env(DOT_AGENT_DECK_AGENT_ID, &agent_id_owned)
            .output()
            .expect("run real agent-event CLI for reconnect")
    })
    .await
    .expect("agent-event subprocess task did not panic");
    assert!(
        output.status.success(),
        "real `agent-event --type running` failed: status={:?} stdout={:?} stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await {
                Ok(BroadcastMsg::Event(event))
                    if event.pane_id.as_deref() == Some(pane_id)
                        && event.agent_id.as_deref() == Some(agent_id) =>
                {
                    break event;
                }
                Ok(_) => continue,
                Err(error) => panic!("daemon event broadcast closed before CLI event: {error}"),
            }
        }
    })
    .await
    .expect("daemon did not broadcast the real CLI event within 5s")
}

async fn start_real_server() -> Server {
    // Issue #668: the arming point for the twelve tests that reach a registry
    // through this harness.
    child_lifetime_bound::arm();

    let registry = Arc::new(AgentPtyRegistry::new());
    let (dir, path, listener) = {
        let _g = HARNESS_BIND_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = test_temp::tempdir().unwrap();
        let path = dir.path().join("attach.sock");
        let listener = bind_attach_listener(&path).expect("bind attach listener");
        (dir, path, listener)
    };
    let registry_for_task = registry.clone();
    let (event_tx, _) = tokio::sync::broadcast::channel(16);
    let handle = tokio::spawn(async move {
        let _ = serve_attach(listener, registry_for_task, event_tx).await;
    });
    Server {
        _dir: dir,
        path,
        registry,
        handle,
    }
}

/// PRD #162: variant of [`start_real_server`] that serves with the daemon's
/// real, caller-supplied `state: SharedState` via `serve_attach_with_counter`
/// instead of `serve_attach`'s empty dummy state. This is the production path
/// the `ListAgents` handler reads to attach the live `SessionSnapshot`. The
/// caller owns the `registry` (so it can pre-spawn agents into it) and the
/// `state` (so it can populate `AppState.sessions` via `apply_event`). Returns
/// the tempdir (keep it alive — drop removes the socket) and the join handle
/// (abort it at teardown).
async fn start_server_with_state(
    registry: Arc<AgentPtyRegistry>,
    state: SharedState,
) -> (TempDir, PathBuf, JoinHandle<()>) {
    let (dir, path, listener) = {
        let _g = HARNESS_BIND_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = test_temp::tempdir().unwrap();
        let path = dir.path().join("attach.sock");
        let listener = bind_attach_listener(&path).expect("bind attach listener");
        (dir, path, listener)
    };
    let (event_tx, _rx) = tokio::sync::broadcast::channel(16);
    let client_count = Arc::new(AtomicUsize::new(0));
    let scheduler = Arc::new(dot_agent_deck::scheduler::Scheduler::with_stderr_notifier());
    let reuse = dot_agent_deck::spawn::new_reuse_registry();
    // PRD #120 added a 9th `worktree_registry` arg to `serve_attach_with_counter`;
    // this rehydration harness doesn't exercise issue dispatch, so it passes the
    // same empty stand-in the non-orchestration callers use.
    let worktrees = dot_agent_deck::issue_dispatch_run::new_worktree_registry();
    let registry_for_task = registry.clone();
    let handle = tokio::spawn(async move {
        let _ = serve_attach_with_counter(
            listener,
            registry_for_task,
            event_tx,
            client_count,
            state,
            None,
            scheduler,
            reuse,
            worktrees,
            dot_agent_deck::daemon::noop_start_agent_registration_hook(),
        )
        .await;
    });
    (dir, path, handle)
}

/// PRD #162: drive an `AppState` session via the same `apply_event` path the
/// daemon uses for hook events, until the session is `Working` with an active
/// tool, `tool_count > 0`, an event-derived `agent_type` of ClaudeCode, and a
/// recorded first/last prompt. Mirrors the status/transition apply_event flow:
/// SessionStart → Thinking(prompt) → ToolStart(Read) → ToolEnd → ToolStart(Edit).
fn drive_session_to_working(state: &mut AppState, session_id: &str, pane_id: &str, agent_id: &str) {
    let mk = |event_type: EventType,
              tool_name: Option<&str>,
              tool_detail: Option<&str>,
              user_prompt: Option<&str>| AgentEvent {
        session_id: session_id.to_string(),
        agent_type: AgentType::ClaudeCode,
        event_type,
        tool_name: tool_name.map(str::to_string),
        tool_detail: tool_detail.map(str::to_string),
        cwd: None,
        timestamp: Utc::now(),
        user_prompt: user_prompt.map(str::to_string),
        metadata: HashMap::new(),
        pane_id: Some(pane_id.to_string()),
        agent_id: Some(agent_id.to_string()),
        agent_version: None,
        schema_version: None,
        live_target: None,
        model: None,
    };
    state.apply_event(mk(EventType::SessionStart, None, None, None));
    state.apply_event(mk(
        EventType::Thinking,
        None,
        None,
        Some("build the feature"),
    ));
    state.apply_event(mk(
        EventType::ToolStart,
        Some("Read"),
        Some("src/main.rs"),
        None,
    ));
    state.apply_event(mk(EventType::ToolEnd, None, None, None));
    state.apply_event(mk(
        EventType::ToolStart,
        Some("Edit"),
        Some("src/lib.rs"),
        None,
    ));
}

/// PRD #162: serve the *empty dummy-state* `serve_attach` path on a
/// caller-owned registry (so the same spawned agent can be queried over both
/// the populated and the dummy path). This is the older-daemon / test-harness
/// shape whose `ListAgents` must yield `live == None`.
async fn start_dummy_server_on(
    registry: Arc<AgentPtyRegistry>,
) -> (TempDir, PathBuf, JoinHandle<()>) {
    let (dir, path, listener) = {
        let _g = HARNESS_BIND_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = test_temp::tempdir().unwrap();
        let path = dir.path().join("attach.sock");
        let listener = bind_attach_listener(&path).expect("bind attach listener");
        (dir, path, listener)
    };
    let (event_tx, _rx) = tokio::sync::broadcast::channel(16);
    let registry_for_task = registry.clone();
    let handle = tokio::spawn(async move {
        let _ = serve_attach(listener, registry_for_task, event_tx).await;
    });
    (dir, path, handle)
}

/// PRD #162: hand-build a `SessionState` for the newest-wins join test
/// (session/live/003). Bypasses `apply_event` on purpose so two sessions can
/// coexist on the same `agent_id` + `pane_id` (the `/clear`-restart stale-entry
/// case `apply_event`'s reuse guard would otherwise collapse).
fn make_session(
    session_id: &str,
    pane_id: &str,
    agent_id: &str,
    status: SessionStatus,
    last_prompt: &str,
    last_activity: chrono::DateTime<Utc>,
) -> SessionState {
    SessionState {
        session_id: session_id.to_string(),
        agent_type: AgentType::ClaudeCode,
        cwd: None,
        status,
        active_tool: None,
        started_at: last_activity,
        last_activity,
        recent_events: VecDeque::new(),
        tool_count: 0,
        last_user_prompt: Some(last_prompt.to_string()),
        first_prompts: vec![last_prompt.to_string()],
        pane_id: Some(pane_id.to_string()),
        agent_id: Some(agent_id.to_string()),
        display_name: None,
        pending_permission_tool: None,
        shell_synthetic_working: false,
        monitored_wait_active: false,
        wait_synthetic_working: false,
        shell_descendant_busy: false,
        wait_deferred_revert: false,
        model: None,
        expects_agent_report: false,
    }
}

async fn wait_for<F: FnMut() -> bool>(timeout: Duration, interval: Duration, mut pred: F) -> bool {
    let start = tokio::time::Instant::now();
    while tokio::time::Instant::now() - start < timeout {
        if pred() {
            return true;
        }
        tokio::time::sleep(interval).await;
    }
    pred()
}

fn screen_contains(ctrl: &EmbeddedPaneController, pane_id: &str, needle: &str) -> bool {
    let Some(screen) = ctrl.get_screen(pane_id) else {
        return false;
    };
    let parser = screen.lock().unwrap();
    parser.screen().contents().contains(needle)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hydrate_creates_panes_for_existing_agents() {
    // Spawn three agents via StartAgent — each writes a unique marker — then
    // build a fresh controller and hydrate. Every agent id must surface as a
    // pane, and the daemon-replayed scrollback snapshot must put each marker
    // into the corresponding vt100 parser. This is the regression check for
    // the bug: before the fix, the controller would have been empty here.
    let server = start_real_server().await;
    let client = DaemonClient::new(server.path.clone());

    let mut started_ids = Vec::new();
    for i in 0..3 {
        let id = client
            .start_agent(StartAgentOptions {
                command: Some(format!("sh -c 'echo HYDRATE_MARKER_{i}; sleep 30'")),
                ..Default::default()
            })
            .await
            .expect("start_agent should succeed");
        started_ids.push(id);
    }

    // Give the daemon a moment to drain each agent's first stdout chunk into
    // its scrollback ring so the snapshot replayed on attach contains the
    // marker. Without this the `attach` could land before the agent has
    // emitted its echo, and the parser assertion below would race.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let ctrl = Arc::new(EmbeddedPaneController::new(
        server.path.clone(),
        tokio::runtime::Handle::current(),
    ));

    // hydrate_from_daemon block_on's the daemon client; run on a blocking
    // thread so the runtime keeps polling the in-process server.
    let hydrated = {
        let ctrl = ctrl.clone();
        tokio::task::spawn_blocking(move || ctrl.hydrate_from_daemon())
            .await
            .unwrap()
    };

    assert_eq!(
        hydrated.len(),
        started_ids.len(),
        "every started agent should be hydrated as a pane"
    );

    let hydrated_ids: Vec<String> = hydrated.iter().map(|h| h.agent_id.clone()).collect();
    for id in &started_ids {
        assert!(
            hydrated_ids.contains(id),
            "agent id {id} missing from hydrated set {hydrated_ids:?}"
        );
    }

    // Each pane should receive its corresponding marker via the daemon's
    // scrollback snapshot. Tie the assertion to the agent id so a mis-paired
    // wiring (e.g. all panes attached to the same agent) would be caught.
    for h in &hydrated {
        let idx = started_ids
            .iter()
            .position(|id| id == &h.agent_id)
            .expect("hydrated agent_id must be one we started");
        let needle = format!("HYDRATE_MARKER_{idx}");
        let ctrl_for_wait = ctrl.clone();
        let pane_for_wait = h.pane_id.clone();
        let needle_for_wait = needle.clone();
        let saw = wait_for(
            Duration::from_secs(5),
            Duration::from_millis(50),
            move || screen_contains(&ctrl_for_wait, &pane_for_wait, &needle_for_wait),
        )
        .await;
        assert!(
            saw,
            "expected marker '{needle}' to reach pane {} via STREAM_OUT scrollback replay",
            h.pane_id
        );
    }

    // Cleanup so the test doesn't leak the `sleep 30` children.
    drop(ctrl);
    for id in &started_ids {
        let _ = server.registry.close_agent(id);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hydrate_returns_empty_when_no_agents_exist() {
    // Empty `list_agents` result: dashboard should fall through to its
    // normal "No active sessions..." view. The hydrate call must not error
    // and must not create any panes.
    let server = start_real_server().await;
    let ctrl = Arc::new(EmbeddedPaneController::new(
        server.path.clone(),
        tokio::runtime::Handle::current(),
    ));

    let hydrated = {
        let ctrl = ctrl.clone();
        tokio::task::spawn_blocking(move || ctrl.hydrate_from_daemon())
            .await
            .unwrap()
    };

    assert!(
        hydrated.is_empty(),
        "no agents → no hydrated panes; got {hydrated:?}"
    );
    assert!(
        ctrl.pane_ids().is_empty(),
        "no panes should have been registered"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hydrate_treats_list_agents_failure_as_empty() {
    // No daemon running at the configured path: list_agents will fail with
    // ECONNREFUSED / ENOENT. The TUI must not error out — log and treat as
    // empty so the user can reconnect.
    let dir = test_temp::tempdir().unwrap();
    let missing = dir.path().join("does-not-exist.sock");

    let ctrl = Arc::new(EmbeddedPaneController::new(
        missing,
        tokio::runtime::Handle::current(),
    ));

    let hydrated = {
        let ctrl = ctrl.clone();
        tokio::task::spawn_blocking(move || ctrl.hydrate_from_daemon())
            .await
            .unwrap()
    };

    assert!(
        hydrated.is_empty(),
        "list_agents failure must surface as empty hydration; got {hydrated:?}"
    );
    assert!(
        ctrl.pane_ids().is_empty(),
        "no panes should have been registered on list_agents failure"
    );
}

// ---------------------------------------------------------------------------
// Mock daemon for the "agent disappears between list and attach" test.
// ---------------------------------------------------------------------------

/// Minimal mock that returns two agent ids on `ListAgents` but rejects the
/// `AttachStream` for one of them. Mirrors the real daemon's response shape
/// so the controller's `attach` call surfaces a typed `Server` error rather
/// than a malformed-frame error.
async fn run_partial_attach_server(listener: UnixListener) {
    loop {
        let (mut stream, _) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => return,
        };
        tokio::spawn(async move {
            let req = match read_frame(&mut stream).await {
                Ok(Some((KIND_REQ, payload))) => {
                    match serde_json::from_slice::<AttachRequest>(&payload) {
                        Ok(r) => r,
                        Err(_) => return,
                    }
                }
                _ => return,
            };
            match req {
                AttachRequest::ListAgents => {
                    let resp = AttachResponse {
                        ok: true,
                        agents: Some(vec!["agent-alive".to_string(), "agent-gone".to_string()]),
                        ..Default::default()
                    };
                    let _ = write_resp(&mut stream, &resp).await;
                }
                AttachRequest::AttachStream { id } => {
                    if id == "agent-gone" {
                        // Simulate the race: the agent terminated between
                        // ListAgents and AttachStream. The real daemon
                        // returns a typed error here.
                        let resp = AttachResponse::err("agent not found");
                        let _ = write_resp(&mut stream, &resp).await;
                        return;
                    }
                    // For the surviving agent, ack the attach and keep the
                    // connection open so the controller's reader can park
                    // on `read_frame` indefinitely (the test does not need
                    // STREAM_OUT bytes — only that the pane is wired up).
                    let _ = write_resp(&mut stream, &AttachResponse::ok()).await;
                    loop {
                        match read_frame(&mut stream).await {
                            Ok(None) | Err(_) => break,
                            Ok(Some(_)) => continue,
                        }
                    }
                }
                _ => {
                    let _ = write_resp(&mut stream, &AttachResponse::ok()).await;
                }
            }
        });
    }
}

async fn write_resp(s: &mut UnixStream, resp: &AttachResponse) -> std::io::Result<()> {
    let payload = serde_json::to_vec(resp).expect("AttachResponse must serialize");
    write_frame(s, KIND_RESP, &payload).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hydrate_preserves_pane_id_from_agent_env() {
    // Regression for the hook-routing bug: before this fix the hydrator
    // allocated a *fresh* local pane id, so hook events emitted by the
    // rehydrated agent (which still carry the original DOT_AGENT_DECK_PANE_ID
    // in its env) were silently dropped by `AppState::apply_event`. The fix
    // captures the spawn-time env on the daemon side and threads it through
    // `AttachResponse::agent_records`; rehydration must reuse that exact id.
    //
    // PRD #365 M2: the daemon now mints `pane_id` itself and ignores
    // whatever a client proposes, so the value this test asserts survives
    // hydration is no longer the client's literal proposal — it's whatever
    // the daemon actually recorded for the spawn (`daemon/pane-id/001`).
    // The property under test is unchanged (hydration must reuse the
    // daemon's real record, not a fresh `allocate_id()`); only how the test
    // discovers "the daemon's real record" changes — via `list_agents()`
    // after spawning, instead of assuming the proposed literal round-trips.
    let server = start_real_server().await;
    let client = DaemonClient::new(server.path.clone());

    let agent_id = client
        .start_agent(StartAgentOptions {
            command: Some("sh -c 'sleep 30'".into()),
            env: vec![(
                "DOT_AGENT_DECK_PANE_ID".to_string(),
                "pane-from-env-7".to_string(),
            )],
            ..Default::default()
        })
        .await
        .expect("start_agent should succeed");

    let records = client
        .list_agents()
        .await
        .expect("list_agents should succeed");
    let daemon_pane_id = records
        .iter()
        .find(|r| r.id == agent_id)
        .expect("just-spawned agent should be in list")
        .pane_id_env
        .clone()
        .expect("daemon must have minted a pane_id_env for this spawn");

    let ctrl = Arc::new(EmbeddedPaneController::new(
        server.path.clone(),
        tokio::runtime::Handle::current(),
    ));

    let hydrated = {
        let ctrl = ctrl.clone();
        tokio::task::spawn_blocking(move || ctrl.hydrate_from_daemon())
            .await
            .unwrap()
    };

    assert_eq!(hydrated.len(), 1, "single agent should hydrate as one pane");
    assert_eq!(
        hydrated[0].pane_id, daemon_pane_id,
        "hydrated pane must reuse the daemon's recorded pane_id, not allocate_id()"
    );
    assert_eq!(hydrated[0].agent_id, agent_id);

    drop(ctrl);
    let _ = server.registry.close_agent(&agent_id);
}

// ---------------------------------------------------------------------------
// Mock daemon for the "older daemon (no agent_records)" test.
// ---------------------------------------------------------------------------

/// Mock that mimics an older daemon: replies to ListAgents with the legacy
/// `agents` field only (no `agent_records`). Verifies that a newer client
/// stays forward-compatible — pane hydrates with an allocated id, no panic.
async fn run_legacy_list_server(listener: UnixListener) {
    loop {
        let (mut stream, _) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => return,
        };
        tokio::spawn(async move {
            let req = match read_frame(&mut stream).await {
                Ok(Some((KIND_REQ, payload))) => {
                    match serde_json::from_slice::<AttachRequest>(&payload) {
                        Ok(r) => r,
                        Err(_) => return,
                    }
                }
                _ => return,
            };
            match req {
                AttachRequest::ListAgents => {
                    // Older daemons only knew about `agents`. The newer
                    // `agent_records` field must be left at None to model
                    // the legacy wire shape exactly.
                    let resp = AttachResponse {
                        ok: true,
                        agents: Some(vec!["legacy-agent".to_string()]),
                        agent_records: None,
                        ..Default::default()
                    };
                    let _ = write_resp(&mut stream, &resp).await;
                }
                AttachRequest::AttachStream { .. } => {
                    let _ = write_resp(&mut stream, &AttachResponse::ok()).await;
                    loop {
                        match read_frame(&mut stream).await {
                            Ok(None) | Err(_) => break,
                            Ok(Some(_)) => continue,
                        }
                    }
                }
                _ => {
                    let _ = write_resp(&mut stream, &AttachResponse::ok()).await;
                }
            }
        });
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hydrate_falls_back_to_allocated_id_for_legacy_daemon() {
    // Backward-compat: a daemon predating this fix returns `agents: Some(..)`
    // with `agent_records: None`. The hydrator must still produce a pane
    // (with a freshly-allocated id, since the daemon doesn't know the
    // original env) without panicking — losing hook routing on reconnect
    // matches the pre-fix behavior, but startup must not regress.
    let (dir, path, listener) = {
        let _g = HARNESS_BIND_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = test_temp::tempdir().unwrap();
        let path = dir.path().join("attach.sock");
        let listener = UnixListener::bind(&path).expect("bind mock attach socket");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        (dir, path, listener)
    };
    let server_handle = tokio::spawn(async move {
        run_legacy_list_server(listener).await;
    });

    let ctrl = Arc::new(EmbeddedPaneController::new(
        path,
        tokio::runtime::Handle::current(),
    ));

    let hydrated = {
        let ctrl = ctrl.clone();
        tokio::task::spawn_blocking(move || ctrl.hydrate_from_daemon())
            .await
            .unwrap()
    };

    assert_eq!(
        hydrated.len(),
        1,
        "legacy daemon listing must still hydrate one pane; got {hydrated:?}"
    );
    assert_eq!(hydrated[0].agent_id, "legacy-agent");
    // PRD #365 M2: allocate_id() is retired — the fallback now mints a
    // local placeholder with the same scheme the daemon itself uses
    // (agent_pty::mint_pane_id, "pane-" prefixed), not the agent id.
    assert_ne!(
        hydrated[0].pane_id, hydrated[0].agent_id,
        "legacy fallback must synthesize its own pane id, not reuse the agent id"
    );
    assert!(
        hydrated[0].pane_id.starts_with("pane-"),
        "legacy fallback should mint via agent_pty::mint_pane_id() — got {:?}",
        hydrated[0].pane_id
    );

    drop(ctrl);
    server_handle.abort();
    drop(dir);
}

// ---------------------------------------------------------------------------
// Mock daemon for the "list_agents hangs past timeout" test.
// ---------------------------------------------------------------------------

/// Mock that accepts the ListAgents REQ and never replies. Used to verify
/// the hydration list-call timeout path: the controller must give up and
/// return an empty hydration rather than blocking TUI startup forever.
async fn run_silent_list_server(listener: UnixListener) {
    loop {
        let (mut stream, _) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => return,
        };
        tokio::spawn(async move {
            // Read the REQ but never write a RESP. Hold the stream so the
            // client doesn't see an EOF either — purely a hang.
            let _ = read_frame(&mut stream).await;
            std::future::pending::<()>().await;
        });
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hydrate_treats_list_agents_timeout_as_empty() {
    // A daemon that accepts the connection but never answers must not pin
    // TUI startup. The HYDRATE_LIST_TIMEOUT bound in `hydrate_from_daemon`
    // gives up and the controller proceeds with an empty pane set so the
    // user can see the dashboard and reconnect.
    let (dir, path, listener) = {
        let _g = HARNESS_BIND_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = test_temp::tempdir().unwrap();
        let path = dir.path().join("attach.sock");
        let listener = UnixListener::bind(&path).expect("bind mock attach socket");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        (dir, path, listener)
    };
    let server_handle = tokio::spawn(async move {
        run_silent_list_server(listener).await;
    });

    let ctrl = Arc::new(EmbeddedPaneController::new(
        path,
        tokio::runtime::Handle::current(),
    ));

    // Outer guard: even if the timeout regressed, this catches the regression
    // before the test runner's global timeout. HYDRATE_LIST_TIMEOUT is 5s; we
    // give 10s headroom for slow CI.
    let hydrated_result = tokio::time::timeout(Duration::from_secs(10), {
        let ctrl = ctrl.clone();
        tokio::task::spawn_blocking(move || ctrl.hydrate_from_daemon())
    })
    .await
    .expect("hydrate_from_daemon must not hang past HYDRATE_LIST_TIMEOUT")
    .expect("blocking task should not panic");

    assert!(
        hydrated_result.is_empty(),
        "list_agents timeout must surface as empty hydration; got {hydrated_result:?}"
    );

    drop(ctrl);
    server_handle.abort();
    drop(dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hydrate_skips_agent_that_disappears_between_list_and_attach() {
    // Race coverage: ListAgents reports two ids; AttachStream succeeds for
    // one and fails for the other. The hydrator must skip the failing one
    // and continue with the rest — a single missing agent must not sink the
    // whole rehydration.
    let (dir, path, listener) = {
        let _g = HARNESS_BIND_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = test_temp::tempdir().unwrap();
        let path = dir.path().join("attach.sock");
        let listener = UnixListener::bind(&path).expect("bind mock attach socket");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        (dir, path, listener)
    };
    let server_handle = tokio::spawn(async move {
        run_partial_attach_server(listener).await;
    });

    let ctrl = Arc::new(EmbeddedPaneController::new(
        path,
        tokio::runtime::Handle::current(),
    ));

    let hydrated = {
        let ctrl = ctrl.clone();
        tokio::task::spawn_blocking(move || ctrl.hydrate_from_daemon())
            .await
            .unwrap()
    };

    assert_eq!(
        hydrated.len(),
        1,
        "the surviving agent should hydrate; the disappeared one should be skipped — got {hydrated:?}"
    );
    assert_eq!(
        hydrated[0].agent_id, "agent-alive",
        "the kept pane should be the agent that successfully attached"
    );

    drop(ctrl);
    server_handle.abort();
    drop(dir);
}

// ---------------------------------------------------------------------------
// PRD #76 M2.x rehydration FIXUP-2: pane_id_env capture/hydrate validation.
// ---------------------------------------------------------------------------
// These three tests pin the defense-in-depth scrub for caller-supplied
// DOT_AGENT_DECK_PANE_ID values. Without it a buggy/hostile same-user peer
// reaching the attach socket can poison the daemon's stored copy (echoed
// via `agent_records`) or, in the duplicate case, get one rehydrated pane
// to silently overwrite another in `wire_stream_pane`'s HashMap.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hydrate_drops_oversize_pane_id_env_at_capture() {
    // 200-char pane id: comfortably above PANE_ID_ENV_MAX_LEN (64). The
    // daemon must store None for this agent's record, and the TUI must
    // hydrate with a freshly-minted local placeholder id rather than the
    // poison value — otherwise a near-MAX_FRAME_LEN value could push the
    // cumulative `list_agents` response past the frame cap and break
    // hydration for *every* agent on reconnect.
    //
    // PRD #365 M2: spawn via `AgentPtyRegistry::spawn_agent` directly rather
    // than the wire `StartAgent` path (`client.start_agent`). The daemon's
    // `StartAgent` handler now unconditionally strips whatever
    // `DOT_AGENT_DECK_PANE_ID` a client proposes and injects its own minted
    // value, so a wire-proposed oversize value never reaches
    // `spawn_agent`'s own scrub (`is_valid_pane_id_env`) at all anymore —
    // that scrub is the thing this test pins, and it is still live,
    // reachable production code (`spawn.rs`'s headless fire path and
    // `respawn_agent_for_pane` both call `spawn_agent` directly), just not
    // through the wire. `list_agents()` below still goes over the wire —
    // only the spawn is bypassed.
    let server = start_real_server().await;
    let client = DaemonClient::new(server.path.clone());

    let oversize: String = "a".repeat(200);
    let agent_id = server
        .registry
        .spawn_agent(SpawnOptions {
            command: Some("sh -c 'sleep 30'"),
            env: vec![(DOT_AGENT_DECK_PANE_ID.to_string(), oversize.clone())],
            ..SpawnOptions::default()
        })
        .expect("direct registry spawn_agent should succeed");

    // Daemon-side: list_agents must report pane_id_env = None for this id.
    let records = client
        .list_agents()
        .await
        .expect("list_agents should succeed");
    let record = records
        .iter()
        .find(|r| r.id == agent_id)
        .expect("just-spawned agent should be in list");
    assert!(
        record.pane_id_env.is_none(),
        "daemon must scrub oversize pane_id_env; got {:?}",
        record.pane_id_env
    );

    // Client-side: hydrate must produce a locally-minted placeholder pane id.
    let ctrl = Arc::new(EmbeddedPaneController::new(
        server.path.clone(),
        tokio::runtime::Handle::current(),
    ));
    let hydrated = {
        let ctrl = ctrl.clone();
        tokio::task::spawn_blocking(move || ctrl.hydrate_from_daemon())
            .await
            .unwrap()
    };
    assert_eq!(hydrated.len(), 1);
    assert_eq!(hydrated[0].agent_id, agent_id);
    // PRD #365 M2: allocate_id() is retired — the fallback now mints via
    // agent_pty::mint_pane_id(), the same scheme the daemon itself uses.
    assert!(
        hydrated[0].pane_id.starts_with("pane-"),
        "oversize pane_id_env must fall back to agent_pty::mint_pane_id() — got {:?}",
        hydrated[0].pane_id
    );
    assert_ne!(
        hydrated[0].pane_id, oversize,
        "the poison value must not surface as a pane id"
    );

    drop(ctrl);
    let _ = server.registry.close_agent(&agent_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hydrate_drops_control_char_pane_id_env_at_capture() {
    // ANSI escape embedded in the pane id: anything outside [a-zA-Z0-9_-]
    // is rejected by `is_valid_pane_id_env`. The daemon stores None and
    // the client hydrates with a fresh, locally-minted placeholder id —
    // keeps debug-log output free of injected color codes if anything ever
    // prints a stored value.
    //
    // PRD #365 M2: spawn via `AgentPtyRegistry::spawn_agent` directly — see
    // the comment on `hydrate_drops_oversize_pane_id_env_at_capture` above
    // for why the wire `StartAgent` path can no longer reach this scrub.
    let server = start_real_server().await;
    let client = DaemonClient::new(server.path.clone());

    let poison = "pane\x1b[31mctl";
    let agent_id = server
        .registry
        .spawn_agent(SpawnOptions {
            command: Some("sh -c 'sleep 30'"),
            env: vec![(DOT_AGENT_DECK_PANE_ID.to_string(), poison.to_string())],
            ..SpawnOptions::default()
        })
        .expect("direct registry spawn_agent should succeed");

    let records = client
        .list_agents()
        .await
        .expect("list_agents should succeed");
    let record = records
        .iter()
        .find(|r| r.id == agent_id)
        .expect("just-spawned agent should be in list");
    assert!(
        record.pane_id_env.is_none(),
        "daemon must scrub control-char pane_id_env; got {:?}",
        record.pane_id_env
    );

    let ctrl = Arc::new(EmbeddedPaneController::new(
        server.path.clone(),
        tokio::runtime::Handle::current(),
    ));
    let hydrated = {
        let ctrl = ctrl.clone();
        tokio::task::spawn_blocking(move || ctrl.hydrate_from_daemon())
            .await
            .unwrap()
    };
    assert_eq!(hydrated.len(), 1);
    // PRD #365 M2: allocate_id() is retired — the fallback now mints via
    // agent_pty::mint_pane_id(), the same scheme the daemon itself uses.
    assert!(
        hydrated[0].pane_id.starts_with("pane-"),
        "control-char pane_id_env must fall back to agent_pty::mint_pane_id() — got {:?}",
        hydrated[0].pane_id
    );
    assert_ne!(
        hydrated[0].pane_id, poison,
        "the poison value must not surface as a pane id"
    );

    drop(ctrl);
    let _ = server.registry.close_agent(&agent_id);
}

// `duplicate_pane_id_env_is_rejected_at_spawn_time` was retired here on
// PRD #365 M2. It used to spawn two agents over the WIRE `StartAgent` path
// with the identical client-proposed `DOT_AGENT_DECK_PANE_ID` and assert the
// second was rejected as a duplicate. That premise is now categorically
// impossible: the daemon mints its own `pane_id` on every `StartAgent` call
// and ignores whatever the client proposes (`src/daemon_protocol.rs`'s
// `handle_connection`), so two wire spawns can never even attempt to share
// one — they always get distinct daemon-minted ids, which is exactly what
// `daemon/pane-id/001` (`tests/e2e_pane_id_daemon_authoritative.rs`) now
// pins at the same wire-protocol layer this test used to operate at.
//
// `spawn_agent`'s own duplicate-`pane_id_env` rejection (`agent_pty.rs`,
// `AgentPtyError::DuplicatePaneId`) is explicitly kept as a backstop per the
// PRD's M1 Decisions ("now defends against a daemon-minting bug instead of
// a client-minting race") — it is NOT dead code, and IS still reachable by
// bypassing the wire and calling `AgentPtyRegistry::spawn_agent` directly
// (a real, still-live production entry point: `spawn.rs`'s headless fire
// path and `respawn_agent_for_pane` both go through it). But that exact
// construction and assertion already exist, byte-for-byte, as
// `registry_rejects_duplicate_pane_id_env` in `src/agent_pty.rs`'s own test
// module — which predates this PRD and was written specifically to pin
// `spawn_agent`'s guard directly. Reconstructing the same scenario here via
// the same bypass would be pure duplication of that unit test with zero
// additional coverage, just at a less natural layer (this file's harness is
// built around `DaemonClient`/the wire protocol, not raw registry access).
//
// So: no wire-level construction exists anymore (the invariant it pinned is
// gone, replaced by `daemon/pane-id/001`'s uniqueness guarantee), and no
// bypass-level construction is needed (already covered by
// `registry_rejects_duplicate_pane_id_env`). Retiring beats keeping a
// redundant regression pin.

// ---------------------------------------------------------------------------
// PRD #76 M2.13 fixup F2 — full hydration-path agent_type plumbing.
// ---------------------------------------------------------------------------
// Wire-format tests pin `StartAgent.agent_type` and `AgentRecord.agent_type`
// round-trips in isolation; the placeholder-seeding unit tests pin the
// `AppState` side. This test exercises the *full* path end-to-end so a future
// refactor that breaks any single link (StartAgent → daemon registry →
// AgentRecord → hydrate_from_daemon → HydratedPane → insert_placeholder_session)
// is caught before the dashboard goes back to rendering "No agent" on reconnect.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hydrate_preserves_agent_type_end_to_end() {
    // Spawn with an explicit `StartAgentOptions.agent_type = Some(ClaudeCode)`,
    // hydrate, then thread the hydrated value through `insert_placeholder_session`
    // exactly the way `ui.rs` does. The placeholder must end up with
    // `agent_type == ClaudeCode`, not `AgentType::None`.
    let server = start_real_server().await;
    let client = DaemonClient::new(server.path.clone());

    let agent_id = client
        .start_agent(StartAgentOptions {
            command: Some("sh -c 'sleep 30'".into()),
            agent_type: Some(AgentType::ClaudeCode),
            ..Default::default()
        })
        .await
        .expect("start_agent should succeed");

    let ctrl = Arc::new(EmbeddedPaneController::new(
        server.path.clone(),
        tokio::runtime::Handle::current(),
    ));
    let hydrated = {
        let ctrl = ctrl.clone();
        tokio::task::spawn_blocking(move || ctrl.hydrate_from_daemon())
            .await
            .unwrap()
    };

    assert_eq!(hydrated.len(), 1, "single agent should hydrate as one pane");
    let h = &hydrated[0];
    assert_eq!(h.agent_id, agent_id);
    assert_eq!(
        h.agent_type,
        Some(AgentType::ClaudeCode),
        "hydrated pane must carry the StartAgent-supplied agent_type, \
         not None — got {:?}",
        h.agent_type
    );

    // Mirror `ui.rs` hydration: register the pane and seed the placeholder
    // with `h.agent_type`. Without M2.13 wiring this collapses to None.
    let mut state = AppState::default();
    state.register_pane(h.pane_id.clone());
    state.insert_placeholder_session(
        h.pane_id.clone(),
        h.cwd.clone(),
        h.agent_type.clone(),
        Some(h.agent_id.clone()),
    );

    let session = state
        .sessions
        .values()
        .find(|s| s.pane_id.as_deref() == Some(h.pane_id.as_str()))
        .expect("placeholder session must exist for the hydrated pane");
    assert_eq!(
        session.agent_type,
        AgentType::ClaudeCode,
        "placeholder session must inherit the daemon-recorded agent_type — \
         got {:?}",
        session.agent_type
    );

    drop(ctrl);
    let _ = server.registry.close_agent(&agent_id);
}

// PRD #76 M2.13: the wire field is an enum; a serde rename or variant
// addition that breaks `OpenCode` round-trip would slip past the
// ClaudeCode-only end-to-end test above. Re-run the same hydration chain
// with `OpenCode` so any single-variant regression in `AgentRecord.agent_type`
// (or downstream plumbing) fails loudly on its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hydrate_preserves_agent_type_end_to_end_opencode() {
    let server = start_real_server().await;
    let client = DaemonClient::new(server.path.clone());

    let agent_id = client
        .start_agent(StartAgentOptions {
            command: Some("sh -c 'sleep 30'".into()),
            agent_type: Some(AgentType::OpenCode),
            ..Default::default()
        })
        .await
        .expect("start_agent should succeed");

    let ctrl = Arc::new(EmbeddedPaneController::new(
        server.path.clone(),
        tokio::runtime::Handle::current(),
    ));
    let hydrated = {
        let ctrl = ctrl.clone();
        tokio::task::spawn_blocking(move || ctrl.hydrate_from_daemon())
            .await
            .unwrap()
    };

    assert_eq!(hydrated.len(), 1);
    let h = &hydrated[0];
    assert_eq!(h.agent_id, agent_id);
    assert_eq!(
        h.agent_type,
        Some(AgentType::OpenCode),
        "hydrated pane must carry the OpenCode agent_type, not None — got {:?}",
        h.agent_type
    );

    let mut state = AppState::default();
    state.register_pane(h.pane_id.clone());
    state.insert_placeholder_session(
        h.pane_id.clone(),
        h.cwd.clone(),
        h.agent_type.clone(),
        Some(h.agent_id.clone()),
    );

    let session = state
        .sessions
        .values()
        .find(|s| s.pane_id.as_deref() == Some(h.pane_id.as_str()))
        .expect("placeholder session must exist for the hydrated pane");
    assert_eq!(
        session.agent_type,
        AgentType::OpenCode,
        "placeholder session must inherit the daemon-recorded OpenCode \
         agent_type — got {:?}",
        session.agent_type
    );

    drop(ctrl);
    let _ = server.registry.close_agent(&agent_id);
}

/// Symptom 2 regression
/// (`.dot-agent-deck/agent-card-lifecycle-bugs.md`): a role whose daemon
/// agent has died (e.g., a `clear = false` `release` agent that runs
/// through its workflow and exits cleanly) is absent from
/// `agent_records()` on reconnect. The TUI's hydration partition
/// therefore receives sparse role slots — the bucket has one fewer
/// entry than the orchestration config declares. Pre-fix the missing
/// role's slot disappeared from the rebuilt orchestration tab
/// entirely.
///
/// This test pins the fix end-to-end at the integration boundary the
/// real reconnect path uses:
///   1. Spawn 5 orchestration role agents through `DaemonClient`,
///      mirroring the production `.dot-agent-deck.toml` layout
///      (orchestrator + coder + reviewer + auditor + release).
///   2. Kill the LAST agent (`release`-equivalent) the same way a
///      `clear = false` agent exiting cleanly would die — its
///      registry entry is pruned, `list_agents` no longer reports
///      it.
///   3. Hydrate. Verify `hydrate_from_daemon` returns 4 panes (the
///      live ones) — that's the underlying state the fix has to cope
///      with.
///   4. Build a `Vec<Option<String>>` of length 5 the way the
///      hydration loop in `ui.rs` does (one `Some(pane_id)` per
///      hydrated role slot, the dead role's slot is `None`).
///   5. Call `fill_dead_slots_with_placeholders` and assert every
///      slot now carries a non-empty id, the dead one is the
///      deterministic synthetic id, and a placeholder session has
///      been seeded so the orchestration tab's card filter
///      (`pane_id ∈ role_pane_ids`) finds it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dead_role_stays_visible_on_reconnect_as_placeholder_card() {
    let server = start_real_server().await;
    let client = DaemonClient::new(server.path.clone());

    let orchestration_name = "tdd-cycle";
    let cwd = server._dir.path().to_string_lossy().into_owned();
    let role_names = ["orchestrator", "coder", "reviewer", "auditor", "release"];
    let mut spawned_ids: Vec<String> = Vec::new();
    for (role_index, role_name) in role_names.iter().enumerate() {
        let pane_env = format!("pane-{role_name}");
        let id = client
            .start_agent(StartAgentOptions {
                command: Some("sh -c 'sleep 30'".to_string()),
                cwd: Some(cwd.clone()),
                display_name: Some((*role_name).to_string()),
                env: vec![("DOT_AGENT_DECK_PANE_ID".to_string(), pane_env)],
                tab_membership: Some(TabMembership::Orchestration {
                    name: orchestration_name.to_string(),
                    role_index,
                    role_name: (*role_name).to_string(),
                    is_start_role: role_index == 0,
                    orchestration_cwd: Some(cwd.clone()),
                    display_title: None,
                    orchestration_id: None,
                }),
                ..Default::default()
            })
            .await
            .expect("start_agent should succeed");
        spawned_ids.push(id);
    }

    // Kill the LAST role (`release`-equivalent). Matches the
    // production failure mode: an agent that exits cleanly (or that
    // the user explicitly closed) is pruned from `agent_records()`
    // and disappears from `list_agents`.
    let release_id = spawned_ids.last().unwrap().clone();
    server
        .registry
        .close_agent(&release_id)
        .expect("close_agent for release should succeed");

    // Hydrate. Confirms the precondition the fix has to cope with:
    // only 4 panes are returned even though the orchestration
    // declares 5.
    let ctrl = Arc::new(EmbeddedPaneController::new(
        server.path.clone(),
        tokio::runtime::Handle::current(),
    ));
    let hydrated = {
        let ctrl = ctrl.clone();
        tokio::task::spawn_blocking(move || ctrl.hydrate_from_daemon())
            .await
            .unwrap()
    };
    assert_eq!(
        hydrated.len(),
        4,
        "release agent was closed, so only 4 of 5 should hydrate; \
         got {hydrated:?}"
    );

    // Build `role_pane_ids` the way the hydration loop does. We map
    // each surviving role into its slot by role_index.
    let mut role_pane_ids: Vec<Option<String>> = vec![None; role_names.len()];
    for h in &hydrated {
        if let Some(TabMembership::Orchestration { role_index, .. }) = &h.tab_membership {
            role_pane_ids[*role_index] = Some(h.pane_id.clone());
        }
    }
    assert!(
        role_pane_ids[4].is_none(),
        "role_index 4 must be the dead slot"
    );

    // Apply the fix: every dead slot gets a synthetic id and a
    // placeholder session is seeded so the orchestration tab keeps
    // the role's card visible.
    let mut state = AppState::default();
    // Seed placeholders for the live hydrated panes too — mirrors the
    // hydration loop's normal path so the post-fix AppState looks like
    // the real run_tui's would.
    for h in &hydrated {
        state.register_pane(h.pane_id.clone());
        state.insert_placeholder_session(
            h.pane_id.clone(),
            h.cwd.clone(),
            h.agent_type.clone(),
            Some(h.agent_id.clone()),
        );
    }
    // Token-less spawn above → the LEGACY `(name, cwd)` routing identity, which
    // is what namespaces the synthetic dead-slot id (PRD #140 review).
    let legacy_identity = OrchestrationIdentity::NameCwd {
        name: orchestration_name.to_string(),
        cwd: cwd.clone(),
    };
    fill_dead_slots_with_placeholders(&mut role_pane_ids, &legacy_identity, &cwd, &mut state);

    // Every role slot is now filled.
    assert!(
        role_pane_ids.iter().all(Option::is_some),
        "all 5 role slots must have a pane id after the dead-slot fill; \
         got {role_pane_ids:?}"
    );
    let dead_id = role_pane_ids[4].as_deref().unwrap();
    assert_eq!(
        dead_id,
        dead_slot_pane_id(&legacy_identity, 4),
        "dead slot id must be the deterministic synthetic"
    );
    assert!(is_dead_slot_pane_id(dead_id));

    // The placeholder session backing the dead slot exists, has the
    // 'No agent' shape, and would be picked up by the orchestration
    // tab's card-filter (`pane_id ∈ role_pane_ids`).
    let dead_session = state
        .sessions
        .values()
        .find(|s| s.pane_id.as_deref() == Some(dead_id))
        .expect("dead-slot placeholder session must exist in AppState");
    assert_eq!(dead_session.agent_type, AgentType::None);

    // And we end up with one session per role — five total, not
    // four. Pre-fix this would have been four.
    let cards_per_pane: Vec<&str> = role_pane_ids
        .iter()
        .filter_map(|p| p.as_deref())
        .filter(|pid| {
            state
                .sessions
                .values()
                .any(|s| s.pane_id.as_deref() == Some(*pid))
        })
        .collect();
    assert_eq!(
        cards_per_pane.len(),
        5,
        "exactly one card must exist per role slot; got {cards_per_pane:?}"
    );

    drop(ctrl);
    // Clean up surviving agents so the test doesn't leak the `sleep 30`
    // children.
    for id in &spawned_ids[..spawned_ids.len() - 1] {
        let _ = server.registry.close_agent(id);
    }
}

// ---------------------------------------------------------------------------
// PRD #140 M3.1 — detach/reattach of two same-`(name, cwd)` orchestration tabs.
// ---------------------------------------------------------------------------
// Synthetic tier on purpose: the assertion is about a HYDRATION ROUND TRIP
// (does the daemon echo carry the per-tab token far enough for the TUI to
// rebuild two tabs?), not about anything an LLM does. Real agents would add
// cost and flake without touching the code under test. The PTY e2e
// (`orchestration/route/001`) covers the live two-tab case with real agents but
// never detaches, which is exactly the gap this fills.
//
// The path driven here is the production one end to end: `start_agent` stores
// `TabMembership` on the daemon's `AgentRecord` → `hydrate_from_daemon` reads it
// back over the attach socket via `ListAgents` (through
// `validate_tab_membership`) → `partition_hydrated_panes` buckets by
// `OrchestrationIdentity` → `resolve_orch_config_for_hydration` /
// `synthesize_from_bucket_metadata` rebuilds each bucket's config →
// `open_orchestration_tab_with_existing_role_panes` builds the tab.
//
// Written as a sync `#[test]` driving an explicit runtime for the same reason as
// `restore_007` above: the linkage-check scanner only recognises a plain `fn`
// after a `#[spec(...)]`.

/// Scenario: Spawn two orchestration tabs' worth of role agents
/// (`orchestrator` + `coder` each) on a warm daemon with byte-identical
/// orchestration `name` and `orchestration_cwd`, told apart only by their
/// per-tab `orchestration_id`, plus a third token-less pair standing in for a
/// pre-#140 client, then detach and reattach by hydrating a fresh controller.
/// Asserts the reattach rebuilds the two tokened pairs as TWO distinct
/// orchestration tabs with disjoint role panes (each keeping its own routing
/// group) while the token-less pair still merges into ONE tab, and that a dead
/// role slot in each tokened tab mints its own placeholder card instead of the
/// two tabs aliasing one.
#[spec("orchestration/route/002")]
#[test]
fn route_002_reattach_rebuilds_two_same_cwd_orchestration_tabs() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build multi-thread runtime");
    rt.block_on(route_002_reattach_rebuilds_two_same_cwd_orchestration_tabs_inner());
}

async fn route_002_reattach_rebuilds_two_same_cwd_orchestration_tabs_inner() {
    let server = start_real_server().await;
    let client = DaemonClient::new(server.path.clone());

    // ONE orchestration name, ONE directory — the ambiguity the PRD is about.
    let orchestration_name = "route-iso";
    let cwd = server._dir.path().to_string_lossy().into_owned();
    let role_names = ["orchestrator", "coder"];
    // Tab A and Tab B carry distinct per-tab tokens; the third pair carries
    // none, standing in for a client that predates PRD #140.
    let tabs: [(&str, Option<&str>); 3] = [
        ("a", Some("orch-inst-aaaa1111")),
        ("b", Some("orch-inst-bbbb2222")),
        ("legacy", None),
    ];

    // PRD #365 M2: the daemon now mints `pane_id` itself and ignores
    // whatever a client proposes, so `DOT_AGENT_DECK_PANE_ID` below is no
    // longer what ends up registered — it exists only to give
    // `display_name` a matching label. Every assertion in this test that
    // used to identify a pane by its proposed literal (`pane-a-coder`, the
    // `starts_with("pane-a-")` checks, ...) now looks the real
    // daemon-minted value up in `pane_id_of` (keyed by `"{tab_tag}-{role}"`,
    // populated via `list_agents()` once every agent has spawned) instead
    // of assuming the literal round-trips.
    let mut spawned_ids: Vec<String> = Vec::new();
    let mut agent_id_of: HashMap<String, String> = HashMap::new();
    for (tab_tag, orchestration_id) in tabs {
        for (role_index, role_name) in role_names.iter().enumerate() {
            let id = client
                .start_agent(StartAgentOptions {
                    command: Some("sh -c 'sleep 30'".to_string()),
                    cwd: Some(cwd.clone()),
                    display_name: Some(format!("{tab_tag}-{role_name}")),
                    env: vec![(
                        "DOT_AGENT_DECK_PANE_ID".to_string(),
                        format!("pane-{tab_tag}-{role_name}"),
                    )],
                    tab_membership: Some(TabMembership::Orchestration {
                        name: orchestration_name.to_string(),
                        role_index,
                        role_name: (*role_name).to_string(),
                        is_start_role: role_index == 0,
                        orchestration_cwd: Some(cwd.clone()),
                        display_title: None,
                        orchestration_id: orchestration_id.map(str::to_string),
                    }),
                    ..Default::default()
                })
                .await
                .expect("start_agent should succeed");
            agent_id_of.insert(format!("{tab_tag}-{role_name}"), id.clone());
            spawned_ids.push(id);
        }
    }
    let records = client
        .list_agents()
        .await
        .expect("list_agents should succeed");
    let pane_id_of: HashMap<String, String> = agent_id_of
        .iter()
        .map(|(key, agent_id)| {
            let pane_id = records
                .iter()
                .find(|r| &r.id == agent_id)
                .and_then(|r| r.pane_id_env.clone())
                .unwrap_or_else(|| panic!("agent {agent_id} ({key}) missing a daemon pane_id_env"));
            (key.clone(), pane_id)
        })
        .collect();
    let pane_id_for = |tab_tag: &str, role_name: &str| -> &str {
        pane_id_of
            .get(&format!("{tab_tag}-{role_name}"))
            .unwrap_or_else(|| panic!("no daemon pane_id recorded for {tab_tag}-{role_name}"))
    };

    // ---- Detach + reattach: a FRESH controller hydrating from the warm daemon.
    let ctrl = Arc::new(EmbeddedPaneController::new(
        server.path.clone(),
        tokio::runtime::Handle::current(),
    ));
    let hydrated = {
        let ctrl = ctrl.clone();
        tokio::task::spawn_blocking(move || ctrl.hydrate_from_daemon())
            .await
            .unwrap()
    };
    assert_eq!(
        hydrated.len(),
        6,
        "all six role panes across the three tabs should hydrate; got {hydrated:?}"
    );

    // The token survived the daemon echo + `validate_tab_membership` on every
    // pane — the precondition for the partition to tell the tabs apart at all.
    for h in &hydrated {
        let Some(TabMembership::Orchestration {
            orchestration_id, ..
        }) = &h.tab_membership
        else {
            panic!("hydrated pane lost its Orchestration tab membership: {h:?}");
        };
        let expected = if [pane_id_for("a", "coder"), pane_id_for("a", "orchestrator")]
            .contains(&h.pane_id.as_str())
        {
            Some("orch-inst-aaaa1111".to_string())
        } else if [pane_id_for("b", "coder"), pane_id_for("b", "orchestrator")]
            .contains(&h.pane_id.as_str())
        {
            Some("orch-inst-bbbb2222".to_string())
        } else {
            None
        };
        assert_eq!(
            *orchestration_id, expected,
            "pane {} must round-trip its own orchestration_id",
            h.pane_id
        );
    }

    // ---- Partition: the reattach's tab-reconstruction decision.
    let partition = partition_hydrated_panes(&hydrated);
    assert!(
        partition.dashboard_pane_ids.is_empty(),
        "no orchestration role pane should fall through to the dashboard; got {:?}",
        partition.dashboard_pane_ids
    );
    assert_eq!(
        partition.orchestration_buckets.len(),
        3,
        "two tokened tabs must rebuild as TWO buckets (not one merged bucket of \
         four panes) and the token-less pair as ONE; got {:?}",
        partition
            .orchestration_buckets
            .iter()
            .map(|b| (b.orchestration_id.clone(), b.role_slots.len()))
            .collect::<Vec<_>>()
    );

    let bucket_for = |token: Option<&str>| {
        partition
            .orchestration_buckets
            .iter()
            .find(|b| b.orchestration_id.as_deref() == token)
            .unwrap_or_else(|| panic!("no bucket for orchestration_id {token:?}"))
    };
    let bucket_a = bucket_for(Some("orch-inst-aaaa1111"));
    let bucket_b = bucket_for(Some("orch-inst-bbbb2222"));
    let bucket_legacy = bucket_for(None);

    for (label, bucket) in [("A", bucket_a), ("B", bucket_b), ("legacy", bucket_legacy)] {
        // Same name, same cwd across all three — the identity is doing the work,
        // not the tuple.
        assert_eq!(bucket.orchestration_name, orchestration_name);
        assert_eq!(bucket.cwd, cwd);
        assert_eq!(
            bucket.role_slots.len(),
            2,
            "tab {label} must keep BOTH of its own role panes; got {:?}",
            bucket.role_slots
        );
    }

    // Each bucket owns its own panes — the merge failure mode (one tab holding
    // all four tokened panes while the other is orphaned) is excluded.
    let panes_of = |bucket: &dot_agent_deck::ui::OrchestrationHydrationBucket| {
        let mut ids: Vec<String> = bucket
            .role_slots
            .iter()
            .map(|s| s.pane_id.clone())
            .collect();
        ids.sort();
        ids
    };
    let expected_panes_of = |tab_tag: &str| {
        let mut ids = vec![
            pane_id_for(tab_tag, "coder").to_string(),
            pane_id_for(tab_tag, "orchestrator").to_string(),
        ];
        ids.sort();
        ids
    };
    assert_eq!(panes_of(bucket_a), expected_panes_of("a"));
    assert_eq!(panes_of(bucket_b), expected_panes_of("b"));
    assert_eq!(panes_of(bucket_legacy), expected_panes_of("legacy"));

    // The routing group each rebuilt tab retains: distinct for the two tokened
    // tabs, the legacy `(name, cwd)` fallback for the token-less one.
    assert_ne!(
        bucket_a.identity(),
        bucket_b.identity(),
        "the two tokened tabs must remain distinct routing groups after reattach"
    );
    assert_eq!(
        bucket_legacy.identity(),
        OrchestrationIdentity::NameCwd {
            name: orchestration_name.to_string(),
            cwd: cwd.clone(),
        },
        "a token-less bucket must fall back to the legacy (name, cwd) identity"
    );

    // ---- Rebuild the tabs, exactly as the hydration loop in `ui.rs` does.
    let mut tab_manager = dot_agent_deck::tab::TabManager::new(ctrl.clone());
    for bucket in &partition.orchestration_buckets {
        // `None` local config → the config is synthesised from the bucket's own
        // role metadata, the remote-reconnect path.
        let orch_config = resolve_orch_config_for_hydration(None, bucket);
        assert_eq!(
            orch_config.roles.len(),
            2,
            "synthesised config must carry both roles; got {orch_config:?}"
        );
        let mut role_pane_ids: Vec<Option<String>> = vec![None; orch_config.roles.len()];
        for slot in &bucket.role_slots {
            role_pane_ids[slot.role_index] = Some(slot.pane_id.clone());
        }
        tab_manager
            .open_orchestration_tab_with_existing_role_panes(
                &orch_config,
                &bucket.cwd,
                role_pane_ids,
                bucket.display_title.as_deref(),
            )
            .expect("rebuilding an orchestration tab from its bucket should succeed");
    }

    // Three orchestration tabs (plus the dashboard), each owning its own two
    // role panes and nothing else.
    let orchestration_tabs: Vec<&dot_agent_deck::tab::Tab> = tab_manager
        .tabs()
        .iter()
        .filter(|t| matches!(t, dot_agent_deck::tab::Tab::Orchestration { .. }))
        .collect();
    assert_eq!(
        orchestration_tabs.len(),
        3,
        "reattach must rebuild three distinct orchestration tabs"
    );
    for pane_id in [
        pane_id_for("a", "orchestrator"),
        pane_id_for("a", "coder"),
        pane_id_for("b", "orchestrator"),
        pane_id_for("b", "coder"),
        pane_id_for("legacy", "orchestrator"),
        pane_id_for("legacy", "coder"),
    ] {
        let owning: Vec<usize> = tab_manager
            .tabs()
            .iter()
            .enumerate()
            .filter(|(_, t)| match t {
                dot_agent_deck::tab::Tab::Orchestration { role_pane_ids, .. } => {
                    role_pane_ids.iter().any(|p| p == pane_id)
                }
                _ => false,
            })
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            owning.len(),
            1,
            "pane {pane_id} must belong to exactly one rebuilt tab; got tabs {owning:?}"
        );
    }

    // ---- PRD #140 review (B): a dead role slot must not alias across the two
    // partitioned tabs. Both tabs' `coder` role dies (a `clear = false` worker
    // that exited cleanly is absent from `list_agents`), so each tab rebuilds
    // with role_index 1 empty. Pre-fix the synthetic id was namespaced by
    // `(cwd, orchestration_name)` alone, so both tabs minted the SAME id and
    // their two distinct dead roles shared ONE placeholder card.
    let mut state = AppState::default();
    let mut dead_ids: Vec<String> = Vec::new();
    for bucket in [bucket_a, bucket_b] {
        let mut role_pane_ids: Vec<Option<String>> =
            vec![Some(format!("{}-orchestrator", bucket.cwd)), None];
        fill_dead_slots_with_placeholders(
            &mut role_pane_ids,
            &bucket.identity(),
            &bucket.cwd,
            &mut state,
        );
        let dead_id = role_pane_ids[1]
            .clone()
            .expect("the dead role slot must be filled with a synthetic id");
        assert!(is_dead_slot_pane_id(&dead_id));
        dead_ids.push(dead_id);
    }
    assert_ne!(
        dead_ids[0], dead_ids[1],
        "two partitioned same-(name, cwd) tabs must mint DISTINCT dead-slot ids"
    );
    let placeholder_cards = state
        .sessions
        .values()
        .filter(|s| s.pane_id.as_deref().is_some_and(is_dead_slot_pane_id))
        .count();
    assert_eq!(
        placeholder_cards, 2,
        "each partitioned tab's dead role needs its OWN placeholder card"
    );
    // The legacy (token-less) identity keeps the pre-review byte format, so an
    // older client's reconnect still reproduces the same id it always did.
    assert_eq!(
        dead_slot_pane_id(&bucket_legacy.identity(), 1),
        dead_slot_pane_id(
            &OrchestrationIdentity::NameCwd {
                name: orchestration_name.to_string(),
                cwd: cwd.clone(),
            },
            1
        )
    );

    drop(tab_manager);
    drop(ctrl);
    for id in &spawned_ids {
        let _ = server.registry.close_agent(id);
    }
}

// ---------------------------------------------------------------------------
// PRD #104 R1 (reviewer): the M4 reproducer in `tests/snapshot_replay_dims.rs`
// pins `parser_init_dims` in isolation, but a regression that swapped the
// helper out for hard-coded `24, 80` at the `hydrate_from_daemon` call-site
// would still pass that test (it only proves the helper itself is correct).
//
// This test exercises the actual call-site: spawn a real agent at 40×120 on
// the in-process daemon, hydrate via `EmbeddedPaneController::hydrate_from_daemon`,
// and assert the pane's vt100 parser is sized to the daemon-reported dims.
// A regression that re-introduces the hard-coded fall-back would fail here
// even if `parser_init_dims` itself stayed correct.
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hydrate_sizes_parser_to_daemon_reported_pty_dims() {
    let server = start_real_server().await;
    let client = DaemonClient::new(server.path.clone());

    // Daemon-side spawn at non-default 40×120. Pre-PRD the client's parser
    // would have been built at 24×80 regardless; the fix wires `record.rows`
    // / `record.cols` through `parser_init_dims` into `vt100::Parser::new`.
    let agent_id = client
        .start_agent(StartAgentOptions {
            command: Some("sh -c 'sleep 30'".into()),
            rows: 40,
            cols: 120,
            ..Default::default()
        })
        .await
        .expect("start_agent should succeed");

    let ctrl = Arc::new(EmbeddedPaneController::new(
        server.path.clone(),
        tokio::runtime::Handle::current(),
    ));

    let hydrated = {
        let ctrl = ctrl.clone();
        tokio::task::spawn_blocking(move || ctrl.hydrate_from_daemon())
            .await
            .unwrap()
    };
    assert_eq!(hydrated.len(), 1, "single agent should hydrate as one pane");
    let pane_id = hydrated[0].pane_id.clone();

    let screen = ctrl
        .get_screen(&pane_id)
        .expect("hydrated pane must expose its vt100 parser");
    let size = {
        let parser = screen.lock().unwrap();
        parser.screen().size()
    };
    assert_eq!(
        size,
        (40, 120),
        "PRD #104 R1: hydrate_from_daemon must size the parser to AgentRecord.rows/cols, \
         not the pre-PRD hard-coded 24×80 placeholder"
    );

    drop(ctrl);
    let _ = server.registry.close_agent(&agent_id);
}

// ---------------------------------------------------------------------------
// PRD #89 M2b.1 — warm-daemon orchestration-tab hydration regression guard.
// ---------------------------------------------------------------------------
// The snapshot-fallback restore branch (M2b.3) rebuilds an orchestration tab
// from the disk snapshot only when the daemon is EMPTY. The warm-daemon case
// is already covered by the PRD #76 M2.12 + #111 hydration path: each role
// agent carries `TabMembership::Orchestration` and `hydrate_from_daemon`
// echoes it back, so the TUI can place every role pane at its `role_index`
// and recover the start-role cursor. This test pins that end-to-end so a
// regression in warm-daemon orchestration hydration fails here rather than
// silently shifting work onto the snapshot path.
//
// Written as a sync `#[test]` driving an explicit multi-thread runtime rather
// than `#[tokio::test]`: the linkage-check (PRD #77 Decision 17) ties each
// `#[spec(...)]` to the next `fn` definition and the function-name prefix, and
// its scanner only recognises a plain `fn` (not `async fn`). `block_on` keeps
// the async daemon/hydrate flow intact while exposing a sync `fn` to the gate.

/// Scenario: Spawn three orchestration role agents (orchestrator + coder +
/// reviewer) on a warm in-process daemon, each tagged with its
/// `TabMembership::Orchestration` role_index / role_name / is_start_role, then
/// build a fresh controller and hydrate. Asserts warm-daemon hydration
/// reproduces every role as a pane, that placing each hydrated pane at its
/// `role_index` yields the orchestrator + role panes in their saved display
/// order, and that the start (orchestrator) role — i.e. the `start_role_index`
/// cursor — is recoverable from `is_start_role`.
#[spec("session/restore/007")]
#[test]
fn restore_007_warm_daemon_hydrates_orchestration_roles_in_order() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build multi-thread runtime");
    rt.block_on(restore_007_warm_daemon_hydrates_orchestration_roles_in_order_inner());
}

async fn restore_007_warm_daemon_hydrates_orchestration_roles_in_order_inner() {
    let server = start_real_server().await;
    let client = DaemonClient::new(server.path.clone());

    let orchestration_name = "tdd-cycle";
    let cwd = server._dir.path().to_string_lossy().into_owned();
    let role_names = ["orchestrator", "coder", "reviewer"];
    let mut spawned_ids: Vec<String> = Vec::new();
    for (role_index, role_name) in role_names.iter().enumerate() {
        let pane_env = format!("pane-{role_name}");
        let id = client
            .start_agent(StartAgentOptions {
                command: Some("sh -c 'sleep 30'".to_string()),
                cwd: Some(cwd.clone()),
                display_name: Some((*role_name).to_string()),
                env: vec![("DOT_AGENT_DECK_PANE_ID".to_string(), pane_env)],
                tab_membership: Some(TabMembership::Orchestration {
                    name: orchestration_name.to_string(),
                    role_index,
                    role_name: (*role_name).to_string(),
                    is_start_role: role_index == 0,
                    orchestration_cwd: Some(cwd.clone()),
                    display_title: None,
                    orchestration_id: None,
                }),
                ..Default::default()
            })
            .await
            .expect("start_agent should succeed");
        spawned_ids.push(id);
    }

    let ctrl = Arc::new(EmbeddedPaneController::new(
        server.path.clone(),
        tokio::runtime::Handle::current(),
    ));
    let hydrated = {
        let ctrl = ctrl.clone();
        tokio::task::spawn_blocking(move || ctrl.hydrate_from_daemon())
            .await
            .unwrap()
    };

    assert_eq!(
        hydrated.len(),
        role_names.len(),
        "every orchestration role should hydrate as a pane; got {hydrated:?}"
    );

    // Place each hydrated pane at its role_index, exactly as the production
    // hydration loop in ui.rs does, then assert the orchestrator + role panes
    // come back in their saved display order with the start role recoverable.
    let mut role_pane_ids: Vec<Option<String>> = vec![None; role_names.len()];
    let mut role_name_by_index: Vec<Option<String>> = vec![None; role_names.len()];
    let mut start_role_index: Option<usize> = None;
    for h in &hydrated {
        let Some(TabMembership::Orchestration {
            name,
            role_index,
            role_name,
            is_start_role,
            ..
        }) = &h.tab_membership
        else {
            panic!("hydrated pane lost its Orchestration tab membership: {h:?}");
        };
        assert_eq!(
            name, orchestration_name,
            "hydrated pane must carry the orchestration name"
        );
        assert!(
            *role_index < role_names.len(),
            "role_index {role_index} out of range"
        );
        role_pane_ids[*role_index] = Some(h.pane_id.clone());
        role_name_by_index[*role_index] = Some(role_name.clone());
        if *is_start_role {
            start_role_index = Some(*role_index);
        }
    }

    // Every role slot is filled — no gaps in the orchestrator + role panes.
    assert!(
        role_pane_ids.iter().all(Option::is_some),
        "every role slot must be filled by hydration; got {role_pane_ids:?}"
    );
    // The role names land back in their saved display order.
    let recovered_order: Vec<String> = role_name_by_index
        .into_iter()
        .map(|n| n.expect("each filled slot has a role name"))
        .collect();
    let expected_order: Vec<String> = role_names.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        recovered_order, expected_order,
        "warm-daemon hydration must reproduce the role panes in saved order"
    );
    // The start_role_index cursor (orchestrator at index 0) is recoverable.
    assert_eq!(
        start_role_index,
        Some(0),
        "the start (orchestrator) role must be recoverable from is_start_role"
    );

    drop(ctrl);
    for id in &spawned_ids {
        let _ = server.registry.close_agent(id);
    }
}

/// Scenario: Spawn a registry agent whose spawn-time `agent_type` is `None`
/// (the "No agent" case) and drive a live `AppState` session on the same
/// `agent_id` + `pane_id` to `Working` with an active tool, `tool_count > 0`,
/// an event-derived ClaudeCode type and a first prompt; calling `ListAgents`
/// over the real attach socket must return the record with `live = Some(...)`
/// carrying that status, the event-derived agent_type (overriding the `None`
/// spawn-time value), the active tool, the tool count and the prompts. The same
/// registry served via the empty dummy-state `serve_attach` path must return
/// the record with `live == None` — today's behavior, no harness regression.
/// Fork issue #513: also reserve a `registration_generation` for the pane
/// before serving and assert the populated-state `ListAgents` reply joins in
/// this daemon's `AppState::daemon_boot_id` and that exact generation, while
/// the dummy-state path still sets a (different, freshly-minted) boot id but
/// leaves `registration_generation` at `None` since nothing was reserved
/// against its empty `AppState`.
#[spec("session/live/002")]
#[test]
fn live_002_list_agents_attaches_live_snapshot() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build multi-thread runtime");
    rt.block_on(live_002_list_agents_attaches_live_snapshot_inner());
}

async fn live_002_list_agents_attaches_live_snapshot_inner() {
    // Issue #668: bare registry, so it arms the wrapped-child lifetime bound
    // itself rather than inheriting it from `start_real_server`.
    child_lifetime_bound::arm();

    let registry = Arc::new(AgentPtyRegistry::new());
    let pane = "pane-live";

    // Registry record: spawn-time agent_type is None — the legacy "No agent"
    // case the PRD fixes. The event-derived type below must override it.
    let agent_id = registry
        .spawn_agent(SpawnOptions {
            command: Some("sleep 30"),
            env: vec![(DOT_AGENT_DECK_PANE_ID.to_string(), pane.to_string())],
            agent_type: None,
            ..SpawnOptions::default()
        })
        .expect("spawn should succeed");

    // Live, event-derived session state — the store the join must read.
    let state: SharedState = Arc::new(tokio::sync::RwLock::new(AppState::default()));
    // Fork issue #513: reserve a registration_generation for this pane so the
    // ListAgents handler's daemon_boot_id/registration_generation join
    // (src/daemon_protocol.rs) has something real to attach.
    let expected_generation;
    let expected_boot_id;
    {
        let mut guard = state.write().await;
        drive_session_to_working(&mut guard, "sess-live", pane, &agent_id);
        expected_generation = guard.reserve_registration_generation(pane);
        expected_boot_id = guard.daemon_boot_id().to_string();
    }

    // Populated-state path: ListAgents must attach the snapshot.
    let (_dir, path, handle) = start_server_with_state(registry.clone(), state.clone()).await;
    let client = DaemonClient::new(path);
    let records = client
        .list_agents()
        .await
        .expect("list_agents should succeed");
    let rec = records
        .iter()
        .find(|r| r.id == agent_id)
        .expect("spawned agent must appear in list_agents");

    assert!(
        rec.agent_type.is_none(),
        "precondition: this record's spawn-time agent_type is None"
    );
    let live = rec
        .live
        .as_ref()
        .expect("reconnect must attach the live SessionSnapshot");
    assert_eq!(live.status, SessionStatus::Working, "live status restored");
    assert_eq!(
        live.agent_type,
        Some(AgentType::ClaudeCode),
        "event-derived agent_type must override the None spawn-time type"
    );
    assert_eq!(
        live.active_tool.as_ref().map(|t| t.name.as_str()),
        Some("Edit"),
        "active tool name preserved across reconnect"
    );
    assert!(
        live.tool_count > 0,
        "tool_count must be > 0, got {}",
        live.tool_count
    );
    assert!(
        live.first_prompts
            .iter()
            .any(|p| p.as_str() == "build the feature"),
        "first prompt context preserved, got {:?}",
        live.first_prompts
    );
    assert_eq!(
        live.last_user_prompt.as_deref(),
        Some("build the feature"),
        "last_user_prompt preserved"
    );

    // Fork issue #513: the same join must also attach this daemon's
    // daemon_boot_id and the pane's reserved registration_generation.
    assert_eq!(
        rec.daemon_boot_id.as_deref(),
        Some(expected_boot_id.as_str()),
        "ListAgents must join this daemon's AppState::daemon_boot_id"
    );
    assert_eq!(
        rec.registration_generation,
        Some(expected_generation),
        "ListAgents must join the pane's registration_generation reserved via \
         reserve_registration_generation"
    );

    // Dummy-state path: serve_attach uses an empty AppState → no snapshot,
    // exactly today's behavior (older daemon / test harness). No regression.
    let (_ddir, dpath, dhandle) = start_dummy_server_on(registry.clone()).await;
    let dclient = DaemonClient::new(dpath);
    let drecords = dclient
        .list_agents()
        .await
        .expect("list_agents should succeed");
    let drec = drecords
        .iter()
        .find(|r| r.id == agent_id)
        .expect("spawned agent must appear in dummy-state list_agents");
    assert!(
        drec.live.is_none(),
        "empty dummy-state serve_attach must yield live == None; got {:?}",
        drec.live
    );
    // Fork issue #513: the join still runs against the dummy path's empty
    // AppState, so it still mints and attaches *a* daemon_boot_id (just not
    // the populated-state one above), but registration_generation stays None
    // since nothing was ever reserved against that empty AppState.
    assert!(
        drec.daemon_boot_id.is_some(),
        "even the dummy-state AppState mints a daemon_boot_id, so ListAgents \
         must still set it; got {:?}",
        drec.daemon_boot_id
    );
    assert_eq!(
        drec.registration_generation, None,
        "dummy-state AppState has no pane_registration_generation entries, so \
         this must stay None; got {:?}",
        drec.registration_generation
    );

    handle.abort();
    dhandle.abort();
    let _ = registry.close_agent(&agent_id);
}

/// Scenario: With two `SessionState`s in `AppState.sessions` that both map to
/// the same agent (same `agent_id` + `pane_id`, e.g. a `/clear` restart that
/// left a stale entry) but different `last_activity` and distinguishing
/// status/prompt, the `ListAgents` join must attach the snapshot from the entry
/// with the most-recent `last_activity` (the live session), not the dead
/// predecessor.
#[spec("session/live/003")]
#[test]
fn live_003_join_picks_newest_last_activity() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build multi-thread runtime");
    rt.block_on(live_003_join_picks_newest_last_activity_inner());
}

async fn live_003_join_picks_newest_last_activity_inner() {
    // Issue #668: bare registry, as in `live_002` above.
    child_lifetime_bound::arm();

    let registry = Arc::new(AgentPtyRegistry::new());
    let pane = "pane-dup";
    let agent_id = registry
        .spawn_agent(SpawnOptions {
            command: Some("sleep 30"),
            env: vec![(DOT_AGENT_DECK_PANE_ID.to_string(), pane.to_string())],
            agent_type: None,
            ..SpawnOptions::default()
        })
        .expect("spawn should succeed");

    let older = Utc::now() - chrono::Duration::seconds(120);
    let newer = Utc::now();
    let dead = make_session(
        "dead-sess",
        pane,
        &agent_id,
        SessionStatus::Idle,
        "STALE PROMPT",
        older,
    );
    let live = make_session(
        "live-sess",
        pane,
        &agent_id,
        SessionStatus::Working,
        "FRESH PROMPT",
        newer,
    );

    let state: SharedState = Arc::new(tokio::sync::RwLock::new(AppState::default()));
    {
        let mut guard = state.write().await;
        // Insert the dead predecessor first; the live session is newer.
        guard.sessions.insert("dead-sess".to_string(), dead);
        guard.sessions.insert("live-sess".to_string(), live);
    }

    let (_dir, path, handle) = start_server_with_state(registry.clone(), state.clone()).await;
    let client = DaemonClient::new(path);
    let records = client
        .list_agents()
        .await
        .expect("list_agents should succeed");
    let rec = records
        .iter()
        .find(|r| r.id == agent_id)
        .expect("spawned agent must appear in list_agents");
    let snap = rec
        .live
        .as_ref()
        .expect("a live snapshot must be attached for the duplicated agent");
    assert_eq!(
        snap.status,
        SessionStatus::Working,
        "newest-wins: must take the live (newer last_activity) session's status, not the dead Idle predecessor"
    );
    assert_eq!(
        snap.last_user_prompt.as_deref(),
        Some("FRESH PROMPT"),
        "newest-wins: must take the newer session's prompt, not the stale predecessor"
    );

    handle.abort();
    let _ = registry.close_agent(&agent_id);
}

/// Scenario: A warm in-process daemon carries two agents — agent A whose
/// spawn-time `agent_type` is `None` (the "No agent" case) driven via
/// `apply_event` to a live `Working` session with an active `Edit` tool,
/// `tool_count > 0`, an event-derived `ClaudeCode` type and a first prompt; and
/// agent B (spawn-time `OpenCode`) with NO live session. Hydrating a fresh
/// controller from that daemon threads the live `SessionSnapshot` through
/// `HydratedPane.live` (agent A `Some`, agent B `None`); seeding each hydrated
/// session the way `ui.rs` does — `AppState::seed_hydrated_session` — makes
/// agent A's card carry the snapshot's `status` / `agent_type` (overriding the
/// `None` spawn-time value) / `active_tool` / `tool_count` / `first_prompts` /
/// `last_user_prompt`, NOT a bare `Idle` / "No agent" placeholder, while agent
/// B's snapshot-absent card falls back to today's bare placeholder (Idle,
/// spawn-time `OpenCode`). Each pane seeds exactly one card — no duplicate.
#[spec("session/live/004")]
#[test]
fn live_004_hydrated_session_seeds_from_live_snapshot_with_fallback() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build multi-thread runtime");
    rt.block_on(live_004_hydrated_session_seeds_from_live_snapshot_with_fallback_inner());
}

async fn live_004_hydrated_session_seeds_from_live_snapshot_with_fallback_inner() {
    // Issue #668: bare registry, as in `live_002` / `live_003` above —
    // `start_server_with_state` takes the registry from its caller, so the
    // caller is where the bound has to be armed.
    child_lifetime_bound::arm();

    let registry = Arc::new(AgentPtyRegistry::new());
    let state: SharedState = Arc::new(tokio::sync::RwLock::new(AppState::default()));
    let (_dir, path, handle) = start_server_with_state(registry.clone(), state.clone()).await;
    let client = DaemonClient::new(path.clone());

    // Agent A: spawn-time agent_type None — the "No agent" case. It WILL get a
    // live event-derived session below, which the snapshot must surface.
    //
    // PRD #365 M2: the daemon mints `pane_id` itself now and ignores what a
    // client proposes, so `DOT_AGENT_DECK_PANE_ID` below no longer becomes
    // the agent's real `pane_id_env` — look the actual minted value up via
    // the registry (in-process here, so no wire round trip needed) and use
    // THAT everywhere a pane_id has to match what `hydrate_from_daemon`
    // will report back (`h.pane_id`), instead of the proposed literal.
    let agent_a = client
        .start_agent(StartAgentOptions {
            command: Some("sh -c 'sleep 30'".into()),
            agent_type: None,
            env: vec![(
                DOT_AGENT_DECK_PANE_ID.to_string(),
                "pane-live-a".to_string(),
            )],
            ..Default::default()
        })
        .await
        .expect("start_agent A should succeed");
    let pane_a = registry
        .agent_records()
        .iter()
        .find(|r| r.id == agent_a)
        .and_then(|r| r.pane_id_env.clone())
        .expect("agent A must have a daemon-minted pane_id_env");

    // Agent B: spawn-time agent_type OpenCode but NO live session → live None.
    // The fallback must seed the bare placeholder from this spawn-time value.
    let agent_b = client
        .start_agent(StartAgentOptions {
            command: Some("sh -c 'sleep 30'".into()),
            agent_type: Some(AgentType::OpenCode),
            env: vec![(
                DOT_AGENT_DECK_PANE_ID.to_string(),
                "pane-bare-b".to_string(),
            )],
            ..Default::default()
        })
        .await
        .expect("start_agent B should succeed");
    let pane_b = registry
        .agent_records()
        .iter()
        .find(|r| r.id == agent_b)
        .and_then(|r| r.pane_id_env.clone())
        .expect("agent B must have a daemon-minted pane_id_env");

    // Drive ONLY agent A's session to Working (same apply_event flow the daemon
    // uses for hook events).
    {
        let mut guard = state.write().await;
        drive_session_to_working(&mut guard, "sess-a", &pane_a, &agent_a);
    }

    // Hydrate a fresh controller from the warm daemon.
    let ctrl = Arc::new(EmbeddedPaneController::new(
        path,
        tokio::runtime::Handle::current(),
    ));
    let hydrated = {
        let ctrl = ctrl.clone();
        tokio::task::spawn_blocking(move || ctrl.hydrate_from_daemon())
            .await
            .unwrap()
    };
    assert_eq!(
        hydrated.len(),
        2,
        "both agents should hydrate; got {hydrated:?}"
    );

    let h_a = hydrated
        .iter()
        .find(|h| h.agent_id == agent_a)
        .expect("agent A must hydrate as a pane");
    let h_b = hydrated
        .iter()
        .find(|h| h.agent_id == agent_b)
        .expect("agent B must hydrate as a pane");

    // M2.1: the live snapshot threads through HydratedPane.live.
    let live_a = h_a
        .live
        .as_ref()
        .expect("agent A's hydrated pane must carry the live SessionSnapshot");
    assert_eq!(
        live_a.status,
        SessionStatus::Working,
        "live status threaded"
    );
    assert_eq!(
        live_a.agent_type,
        Some(AgentType::ClaudeCode),
        "event-derived agent_type must override the None spawn-time value"
    );
    assert_eq!(
        live_a.active_tool.as_ref().map(|t| t.name.as_str()),
        Some("Edit"),
        "active tool threaded through HydratedPane.live"
    );
    assert!(live_a.tool_count > 0, "tool_count threaded");
    assert!(
        live_a
            .first_prompts
            .iter()
            .any(|p| p.as_str() == "build the feature"),
        "first prompt threaded, got {:?}",
        live_a.first_prompts
    );
    assert!(
        h_b.live.is_none(),
        "agent B has no live session → HydratedPane.live must be None; got {:?}",
        h_b.live
    );

    // M2.2: seed each hydrated session exactly the way the ui.rs hydration loop
    // does — a snapshot-aware insert that seeds from `h.live` when present and
    // falls back to today's bare placeholder when absent. PRD #110 agent_id
    // minting is preserved on the seeded card.
    let mut tui_state = AppState::default();
    for h in &hydrated {
        tui_state.register_pane(h.pane_id.clone());
        tui_state.seed_hydrated_session(
            h.pane_id.clone(),
            h.cwd.clone(),
            h.agent_type.clone(),
            Some(h.agent_id.clone()),
            h.live.as_ref(),
        );
    }

    // Snapshot-seeded card (A): the real live state, NOT Idle / "No agent".
    let a_sessions: Vec<&SessionState> = tui_state
        .sessions
        .values()
        .filter(|s| s.pane_id.as_deref() == Some(pane_a.as_str()))
        .collect();
    assert_eq!(
        a_sessions.len(),
        1,
        "exactly one card for the live pane (no duplicate); got {a_sessions:?}"
    );
    let sess_a = a_sessions[0];
    assert_eq!(
        sess_a.status,
        SessionStatus::Working,
        "seeded card must show the live Working status, not Idle"
    );
    assert_eq!(
        sess_a.agent_type,
        AgentType::ClaudeCode,
        "seeded card must show the event-derived agent_type, not None ('No agent')"
    );
    assert_eq!(
        sess_a.active_tool.as_ref().map(|t| t.name.as_str()),
        Some("Edit"),
        "seeded card must keep its active tool across the reconnect"
    );
    assert!(
        sess_a.tool_count > 0,
        "seeded card must keep its tool count"
    );
    assert!(
        sess_a
            .first_prompts
            .iter()
            .any(|p| p.as_str() == "build the feature"),
        "seeded card must keep its first-prompt context, got {:?}",
        sess_a.first_prompts
    );
    assert_eq!(
        sess_a.last_user_prompt.as_deref(),
        Some("build the feature"),
        "seeded card must keep its last_user_prompt"
    );
    assert_eq!(
        sess_a.agent_id.as_deref(),
        Some(agent_a.as_str()),
        "PRD #110 agent_id minting must be preserved on the seeded card"
    );

    // Fallback card (B): no snapshot → today's bare placeholder.
    let b_sessions: Vec<&SessionState> = tui_state
        .sessions
        .values()
        .filter(|s| s.pane_id.as_deref() == Some(pane_b.as_str()))
        .collect();
    assert_eq!(
        b_sessions.len(),
        1,
        "exactly one card for the bare pane (no duplicate); got {b_sessions:?}"
    );
    let sess_b = b_sessions[0];
    assert_eq!(
        sess_b.status,
        SessionStatus::Idle,
        "no snapshot must fall back to today's bare Idle placeholder"
    );
    assert_eq!(
        sess_b.agent_type,
        AgentType::OpenCode,
        "fallback must seed the spawn-time agent_type"
    );
    assert!(
        sess_b.active_tool.is_none(),
        "bare placeholder has no active tool"
    );

    drop(ctrl);
    handle.abort();
    let _ = registry.close_agent(&agent_a);
    let _ = registry.close_agent(&agent_b);
}

/// Scenario: After hydration seeds a card from a live `SessionSnapshot` via
/// `AppState::seed_hydrated_session` (PRD #110 `agent_id` minted on the seeded
/// placeholder), a subsequent post-reconnect `SessionStart` event from the SAME
/// agent — same `pane_id` + `agent_id`, a distinct `session_id` — must remap
/// onto the hydrated card rather than spawning a second one. Asserts exactly one
/// session/pane survives for that agent (no duplicate) and the minted `agent_id`
/// is preserved through the remap.
#[spec("session/live/005")]
#[test]
fn live_005_post_reconnect_session_start_remaps_onto_seeded_card() {
    let pane = "pane-remap";
    let agent_id = "agent-remap-xyz";

    // The live snapshot the daemon would have attached on reconnect.
    let snap = SessionSnapshot {
        status: SessionStatus::Working,
        agent_type: Some(AgentType::ClaudeCode),
        active_tool: Some(ActiveTool {
            name: "Read".into(),
            detail: Some("src/main.rs".into()),
        }),
        tool_count: 2,
        first_prompts: vec!["build the feature".into()],
        last_user_prompt: Some("build the feature".into()),
        live_target: None,
        last_activity_ms: None,
        shell_synthetic_working: false,
        monitored_wait_active: false,
        wait_synthetic_working: false,
        shell_descendant_busy: false,
        wait_deferred_revert: false,
        model: None,
    };

    // Hydration seeds the card from the snapshot; agent_id is minted on it so
    // the same-agent reuse guard in apply_event can remap a later SessionStart.
    let mut state = AppState::default();
    state.register_pane(pane.to_string());
    state.seed_hydrated_session(
        pane.to_string(),
        None,
        None, // spawn-time agent_type None — overridden by the snapshot
        Some(agent_id.to_string()),
        Some(&snap),
    );
    assert_eq!(
        state
            .sessions
            .values()
            .filter(|s| s.pane_id.as_deref() == Some(pane))
            .count(),
        1,
        "precondition: exactly one seeded card before the SessionStart"
    );

    // A post-reconnect SessionStart from the SAME agent (same pane + agent_id,
    // distinct session_id) must collapse onto the seeded card.
    state.apply_event(AgentEvent {
        session_id: "real-sess".into(),
        agent_type: AgentType::ClaudeCode,
        event_type: EventType::SessionStart,
        tool_name: None,
        tool_detail: None,
        cwd: None,
        timestamp: Utc::now(),
        user_prompt: None,
        metadata: HashMap::new(),
        pane_id: Some(pane.to_string()),
        agent_id: Some(agent_id.to_string()),
        agent_version: None,
        schema_version: None,
        live_target: None,
        model: None,
    });

    let sessions: Vec<&SessionState> = state
        .sessions
        .values()
        .filter(|s| s.pane_id.as_deref() == Some(pane))
        .collect();
    assert_eq!(
        sessions.len(),
        1,
        "post-reconnect SessionStart from the same agent must remap onto the \
         hydrated card, not spawn a duplicate; got {sessions:?}"
    );
    assert_eq!(
        sessions[0].agent_id.as_deref(),
        Some(agent_id),
        "PRD #110 agent_id must be preserved through the remap"
    );
}

// ---------------------------------------------------------------------------
// Mock daemon for the wire-boundary hardening test (session/live/007).
// ---------------------------------------------------------------------------

/// Returns true if `s` carries any ASCII control byte (`< 0x20` or DEL
/// `0x7f`) — the same "no raw control bytes survive into the rendered cell"
/// policy `is_valid_cwd` / `is_valid_display_name` enforce elsewhere on the
/// `list_agents` wire boundary.
fn has_control_bytes(s: &str) -> bool {
    s.bytes().any(|b| b < 0x20 || b == 0x7f)
}

/// Mock that mimics a hostile / malformed daemon: replies to ListAgents with
/// ONE `AgentRecord` whose live `SessionSnapshot` carries ANSI escapes, NUL
/// bytes, and other control chars in `last_user_prompt`, in every
/// `first_prompts` entry, and in `active_tool.name` / `.detail`, where
/// `last_user_prompt`, `active_tool.name`, `active_tool.detail`, AND every
/// `first_prompts` entry are ALSO over-long (~100 KiB each), and where
/// `first_prompts` carries 6 entries (double `MAX_FIRST_PROMPTS`). The
/// TUI-side `list_agents` boundary must scrub control bytes from AND
/// length-clamp every one of these strings before they can corrupt a rebuilt
/// card — not just the `first_prompts` entries.
async fn run_hostile_live_list_server(listener: UnixListener) {
    loop {
        let (mut stream, _) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => return,
        };
        tokio::spawn(async move {
            let req = match read_frame(&mut stream).await {
                Ok(Some((KIND_REQ, payload))) => {
                    match serde_json::from_slice::<AttachRequest>(&payload) {
                        Ok(r) => r,
                        Err(_) => return,
                    }
                }
                _ => return,
            };
            if let AttachRequest::ListAgents = req {
                let over_long = "a".repeat(100_000);
                let hostile = AgentRecord {
                    id: "hostile-live-7".into(),
                    pane_id_env: Some("pane-hostile".into()),
                    display_name: None,
                    cwd: None,
                    tab_membership: None,
                    agent_type: Some(AgentType::ClaudeCode),
                    rows: 0,
                    cols: 0,
                    live: Some(SessionSnapshot {
                        status: SessionStatus::Working,
                        agent_type: Some(AgentType::ClaudeCode),
                        active_tool: Some(ActiveTool {
                            // Oversized in BOTH dimensions: control-laden AND
                            // over-long (~100 KiB) so the length clamp must
                            // apply here too, not only control-stripping.
                            name: format!("Ed\x1bit\x00{over_long}"),
                            detail: Some(format!("src/\x1b[2Jmain.rs\x07{over_long}")),
                        }),
                        tool_count: 3,
                        // Oversized in BOTH dimensions: 6 entries (> the
                        // MAX_FIRST_PROMPTS cap of 3), each control-laden and
                        // each over-long.
                        first_prompts: (0..6)
                            .map(|i| format!("p{i} \x1b[31m\x00{over_long}"))
                            .collect(),
                        // Oversized in BOTH dimensions, like the active-tool
                        // strings: control-laden AND over-long (~100 KiB).
                        last_user_prompt: Some(format!(
                            "run \x1b[31mhostile\x07 \x00prompt {over_long}"
                        )),
                        live_target: None,
                        last_activity_ms: None,
                        shell_synthetic_working: false,
                        monitored_wait_active: false,
                        wait_synthetic_working: false,
                        shell_descendant_busy: false,
                        wait_deferred_revert: false,
                        model: None,
                    }),
                    spawned_at_ms: None,
                    daemon_boot_id: None,
                    registration_generation: None,
                    outstanding_delegation: None,
                    silence_watch: None,
                    delegation_commission: None,
                };
                let resp = AttachResponse {
                    ok: true,
                    agent_records: Some(vec![hostile]),
                    ..Default::default()
                };
                let _ = write_resp(&mut stream, &resp).await;
            } else {
                let _ = write_resp(&mut stream, &AttachResponse::ok()).await;
            }
        });
    }
}

/// Scenario: A hostile / malformed daemon advertises via `list_agents` an
/// `AgentRecord.live` whose prompt and active-tool strings carry ANSI escapes,
/// NUL bytes, and other control chars AND are over-long (~100 KiB each), and
/// whose `first_prompts` is oversized (6 entries, each also over-long).
/// Calling `DaemonClient::list_agents` against that daemon must return the
/// record with its live snapshot preserved (the agent is real) but SCRUBBED —
/// no control bytes survive in `last_user_prompt`, any `first_prompts` entry,
/// or `active_tool.name` / `.detail` — and CLAMPED — every one of
/// `last_user_prompt`, `active_tool.name`, `active_tool.detail`, and each
/// `first_prompts` entry is length-bounded to <= 65536 bytes, and
/// `first_prompts` is cut to at most `MAX_FIRST_PROMPTS` (3) entries — so a
/// malformed daemon can't corrupt the rebuilt card (parallels
/// embed/attach/005's `tab_membership` scrub).
// Written as a sync `#[test]` driving an explicit multi-thread runtime rather
// than `#[tokio::test]`: the linkage-check (PRD #77 Decision 17) ties each
// `#[spec(...)]` to the next plain `fn` definition and the function-name
// prefix, and does not recognize a `#[tokio::test] async fn` — so the spec'd
// entry point must be a sync `#[test]` that block_on's the async body.
#[spec("session/live/007")]
#[test]
fn live_007_list_agents_sanitizes_and_clamps_hostile_live_snapshot() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build multi-thread runtime");
    rt.block_on(live_007_list_agents_sanitizes_and_clamps_hostile_live_snapshot_inner());
}

async fn live_007_list_agents_sanitizes_and_clamps_hostile_live_snapshot_inner() {
    let (dir, path, listener) = {
        let _g = HARNESS_BIND_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = test_temp::tempdir().unwrap();
        let path = dir.path().join("attach.sock");
        let listener = UnixListener::bind(&path).expect("bind mock attach socket");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        (dir, path, listener)
    };
    let server_handle = tokio::spawn(async move {
        run_hostile_live_list_server(listener).await;
    });

    let client = DaemonClient::new(path);
    let records = client
        .list_agents()
        .await
        .expect("list_agents must succeed");
    assert_eq!(
        records.len(),
        1,
        "one hostile record advertised; got {records:?}"
    );
    let live = records[0].live.as_ref().expect(
        "the live snapshot must be preserved (the agent is real) — scrubbed/clamped, not dropped",
    );

    // No raw control bytes survive into any rendered string, AND each string
    // is length-bounded — a 100 KiB prompt or tool string must be clamped, not
    // passed through verbatim once the control bytes are stripped.
    let last = live.last_user_prompt.as_deref().unwrap_or_default();
    assert!(
        !has_control_bytes(last),
        "last_user_prompt must be scrubbed of control bytes; got {last:?}"
    );
    assert!(
        last.len() <= 65536,
        "last_user_prompt must be length-clamped, not passed through verbatim; got {} bytes",
        last.len()
    );
    let tool = live
        .active_tool
        .as_ref()
        .expect("active_tool must be preserved");
    assert!(
        !has_control_bytes(&tool.name),
        "active_tool.name must be scrubbed of control bytes; got {:?}",
        tool.name
    );
    assert!(
        tool.name.len() <= 65536,
        "active_tool.name must be length-clamped, not passed through verbatim; got {} bytes",
        tool.name.len()
    );
    let detail = tool.detail.as_deref().unwrap_or_default();
    assert!(
        !has_control_bytes(detail),
        "active_tool.detail must be scrubbed of control bytes; got {:?}",
        tool.detail
    );
    assert!(
        detail.len() <= 65536,
        "active_tool.detail must be length-clamped, not passed through verbatim; got {} bytes",
        detail.len()
    );

    // first_prompts clamped to <= MAX_FIRST_PROMPTS (3) entries, each scrubbed
    // and length-bounded so an over-long prompt can't blow up the card.
    assert!(
        live.first_prompts.len() <= 3,
        "first_prompts must be clamped to <= MAX_FIRST_PROMPTS (3); got {} entries",
        live.first_prompts.len()
    );
    for (i, p) in live.first_prompts.iter().enumerate() {
        assert!(
            !has_control_bytes(p),
            "first_prompts[{i}] must be scrubbed of control bytes; got {p:?}"
        );
        assert!(
            p.len() <= 65536,
            "first_prompts[{i}] must be length-clamped, not passed through verbatim; got {} bytes",
            p.len()
        );
    }

    server_handle.abort();
    drop(dir);
}

/// Scenario: A live `SessionState` whose EVENT-DERIVED `agent_type` is
/// `AgentType::None` (the agent has emitted events but never identified itself)
/// is snapshotted via `SessionState::live_snapshot` and seeded onto a
/// reconnected card whose SPAWN-TIME `agent_type` is `Some(ClaudeCode)`.
/// `live_snapshot` must map `AgentType::None` to `Option::None` so the snapshot
/// does NOT shadow the spawn-time fallback, and `seed_hydrated_session` must
/// therefore surface the REAL `ClaudeCode` type on the card — not "No agent".
#[spec("session/live/008")]
#[test]
fn live_008_event_none_agent_type_falls_back_to_spawn_time() {
    let pane = "pane-none-type";
    let agent_id = "agent-none-type";

    // A live session that has emitted events but never resolved its agent type.
    let session = SessionState {
        session_id: format!("pane-{pane}"),
        agent_type: AgentType::None,
        cwd: None,
        status: SessionStatus::Working,
        active_tool: None,
        started_at: Utc::now(),
        last_activity: Utc::now(),
        recent_events: VecDeque::new(),
        tool_count: 0,
        last_user_prompt: None,
        first_prompts: Vec::new(),
        pane_id: Some(pane.to_string()),
        agent_id: Some(agent_id.to_string()),
        display_name: None,
        pending_permission_tool: None,
        shell_synthetic_working: false,
        monitored_wait_active: false,
        wait_synthetic_working: false,
        shell_descendant_busy: false,
        wait_deferred_revert: false,
        model: None,
        expects_agent_report: false,
    };

    // The fix lands here: an event-derived AgentType::None must snapshot as
    // Option::None so the spawn-time fallback in seed_hydrated_session wins.
    let snap = session.live_snapshot();
    assert_eq!(
        snap.agent_type, None,
        "live_snapshot must map event-derived AgentType::None to Option::None so it does \
         not shadow the spawn-time agent_type; got {:?}",
        snap.agent_type
    );

    // Seed a reconnected card: the spawn-time type is the REAL ClaudeCode.
    let mut state = AppState::default();
    state.register_pane(pane.to_string());
    state.seed_hydrated_session(
        pane.to_string(),
        None,
        Some(AgentType::ClaudeCode), // spawn-time agent_type — the real one
        Some(agent_id.to_string()),
        Some(&snap),
    );

    let sessions: Vec<&SessionState> = state
        .sessions
        .values()
        .filter(|s| s.pane_id.as_deref() == Some(pane))
        .collect();
    assert_eq!(
        sessions.len(),
        1,
        "exactly one seeded card for the pane; got {sessions:?}"
    );
    assert_eq!(
        sessions[0].agent_type,
        AgentType::ClaudeCode,
        "event-derived AgentType::None must fall back to the spawn-time ClaudeCode, not \
         seed the card as 'No agent'"
    );
}

/// Scenario: Rehydrate one history-only Codex card and one view-only Codex card
/// from daemon `SessionSnapshot` JSON after a detach/reconnect. Each rebuilt
/// session must retain its non-live writability so input remains refused rather
/// than silently reverting to the legacy live default.
#[spec("session/live/010")]
#[test]
fn live_010_rehydrate_preserves_history_and_view_only_writability() {
    for (pane_id, writable, expected) in [
        ("pane-history", "history-only", Writable::HistoryOnly),
        ("pane-view", "none", Writable::None),
    ] {
        let snapshot_json = serde_json::json!({
            "status": "idle",
            "agent_type": "codex",
            "active_tool": null,
            "tool_count": 0,
            "first_prompts": [],
            "last_user_prompt": null,
            "live_target": {
                "kind": if writable == "history-only" { "process" } else { "none" },
                "writable": writable,
            }
        });
        let snapshot: SessionSnapshot = serde_json::from_value(snapshot_json)
            .expect("a reconnect snapshot with live_target must deserialize");
        let mut state = AppState::default();
        state.register_pane(pane_id.to_string());
        state.seed_hydrated_session(
            pane_id.to_string(),
            None,
            Some(AgentType::Codex),
            Some(format!("agent-{pane_id}")),
            Some(&snapshot),
        );

        let session = state
            .sessions
            .values()
            .find(|session| session.pane_id.as_deref() == Some(pane_id))
            .expect("rehydration creates one card for the pane");
        assert_eq!(
            session.writable(),
            expected,
            "reconnecting {writable} pane {pane_id} must preserve its input refusal"
        );
    }
}

/// Scenario: Spawn a managed pane through the daemon attach API, drive it to `Thinking` through the real `agent-event --type running` CLI with the daemon-injected pane and agent ids, then hydrate a fresh TUI controller. Assert the rebuilt card restores `Thinking` rather than falling back to the bare `Idle` placeholder.
#[spec("session/live/011")]
#[test]
fn live_011_real_agent_event_cli_status_survives_reconnect() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build real-agent-event reconnect runtime");
    rt.block_on(live_011_real_agent_event_cli_status_survives_reconnect_inner());
}

async fn live_011_real_agent_event_cli_status_survives_reconnect_inner() {
    let daemon = start_agent_event_daemon().await;
    let cwd = test_temp::tempdir().expect("allocate real-agent-event pane cwd");
    let client = DaemonClient::new(daemon.attach_path.clone());
    // PRD fork#365 M2: the daemon mints and owns `pane_id`, so this reads
    // back the actual minted value via `start_agent_with_pane_id` rather
    // than assume `CLI_REHYDRATION_PANE` (its proposed
    // `DOT_AGENT_DECK_PANE_ID`) survives.
    let (agent_id, pane_id) = client
        .start_agent_with_pane_id(StartAgentOptions {
            command: Some("cat".to_string()),
            cwd: Some(cwd.path().to_string_lossy().into_owned()),
            env: vec![(
                DOT_AGENT_DECK_PANE_ID.to_string(),
                CLI_REHYDRATION_PANE.to_string(),
            )],
            agent_type: Some(AgentType::Pi),
            ..StartAgentOptions::default()
        })
        .await
        .expect("spawn the pane through the TUI's StartAgent attach path");
    let pane_id = pane_id.expect("PRD fork#365 daemon always mints and returns a pane_id");
    let pane_id = pane_id.as_str();
    let observed = run_real_agent_event(&daemon, pane_id, &agent_id, cwd.path()).await;
    assert_eq!(observed.event_type, EventType::Thinking);
    assert_eq!(observed.pane_id.as_deref(), Some(pane_id));
    assert_eq!(observed.agent_id.as_deref(), Some(agent_id.as_str()));

    let controller = Arc::new(EmbeddedPaneController::new(
        daemon.attach_path.clone(),
        tokio::runtime::Handle::current(),
    ));
    let hydrated = {
        let controller = controller.clone();
        tokio::task::spawn_blocking(move || controller.hydrate_from_daemon())
            .await
            .expect("fresh TUI hydration task did not panic")
    };
    let pane = hydrated
        .iter()
        .find(|pane| pane.agent_id == agent_id)
        .unwrap_or_else(|| {
            panic!(
                "fresh TUI did not hydrate the CLI-driven agent {agent_id:?}; hydrated={hydrated:?}"
            )
        });

    let mut fresh_tui_state = AppState::default();
    fresh_tui_state.register_pane(pane.pane_id.clone());
    fresh_tui_state.seed_hydrated_session(
        pane.pane_id.clone(),
        pane.cwd.clone(),
        pane.agent_type.clone(),
        Some(pane.agent_id.clone()),
        pane.live.as_ref(),
    );
    let rebuilt = fresh_tui_state
        .sessions
        .values()
        .find(|session| session.pane_id.as_deref() == Some(pane_id))
        .expect("fresh TUI must rebuild one card for the CLI-driven pane");
    assert_eq!(
        rebuilt.status,
        SessionStatus::Thinking,
        "the real CLI event reached the daemon with pane_id={:?} agent_id={:?}, but reconnect rebuilt the card as {:?} from hydrated.live={:?}",
        observed.pane_id,
        observed.agent_id,
        rebuilt.status,
        pane.live
    );

    drop(controller);
}

/// Scenario: A pane is promoted to `Working` by the daemon's synthesized
/// `ShellBusy` (PRD #370 — a foreground shell command with no agent event in
/// between), then the TUI reconnects and rehydrates that card from the daemon's
/// `SessionSnapshot`. When the command finishes and the daemon broadcasts the
/// paired `ShellIdle`, the rehydrated card must return to `Idle` — the
/// promotion's synthetic provenance has to survive the reconnect, or the
/// dashboard shows `Working` forever (fork issue #21). The shell pane has also
/// been through a same-agent `/clear` restart first, so the synthesized events
/// are built while its hook generation and its stable card id disagree. A
/// second pane whose `Working` came from a REAL agent event must NOT be
/// reverted by the same `ShellIdle`.
#[spec("session/live/015")]
#[test]
fn live_015_rehydration_preserves_shell_synthetic_working() {
    /// A real, agent-emitted hook event (carries the spawn's `agent_id`).
    fn hook_event(pane_id: &str, event_type: EventType, tool_name: Option<&str>) -> AgentEvent {
        AgentEvent {
            session_id: format!("sess-{pane_id}"),
            agent_type: AgentType::ClaudeCode,
            event_type,
            tool_name: tool_name.map(str::to_string),
            tool_detail: None,
            cwd: None,
            timestamp: Utc::now(),
            user_prompt: None,
            metadata: HashMap::new(),
            pane_id: Some(pane_id.to_string()),
            agent_id: Some(format!("agent-{pane_id}")),
            agent_version: None,
            schema_version: None,
            live_target: None,
            model: None,
        }
    }

    /// A same-agent `/clear` / thread restart: a fresh hook `SessionStart`
    /// under a NEW session id but the SAME `agent_id`. `apply_event`'s reuse
    /// guard remaps it back onto the stable card while the pane's hook
    /// GENERATION rolls forward, so afterwards the pane's hook session id is no
    /// longer a key into `sessions`.
    fn rollover_event(pane_id: &str) -> AgentEvent {
        AgentEvent {
            session_id: format!("sess-{pane_id}-gen2"),
            ..hook_event(pane_id, EventType::SessionStart, None)
        }
    }

    /// The shell-activity monitor's synthesized event, built the way
    /// `run_shell_activity_monitor` in `src/daemon.rs` builds it: the session
    /// id and the owning agent id are resolved off the DAEMON's state
    /// INDEPENDENTLY — the authoritative hook generation for `session_id`, and
    /// the agent id from the pane's current CARD (`pane_session_id`), because
    /// after a rollover the generation is not a key into `sessions`. The agent
    /// type is left neutral. That production seam is pinned directly by
    /// `shell_activity_monitor_stamps_the_owning_agent_across_a_session_rollover`
    /// in `src/daemon.rs`; this mirrors its output shape.
    fn shell_event(state: &AppState, pane_id: &str, event_type: EventType) -> AgentEvent {
        let session_id = state
            .pane_hook_session_id(pane_id)
            .expect("the pane has a known hook session");
        let agent_id = state
            .pane_session_id(pane_id)
            .and_then(|card_id| state.sessions.get(&card_id))
            .and_then(|card| card.agent_id.clone());
        AgentEvent {
            session_id,
            agent_type: AgentType::None,
            event_type,
            tool_name: None,
            tool_detail: None,
            cwd: None,
            timestamp: Utc::now(),
            user_prompt: None,
            metadata: HashMap::new(),
            pane_id: Some(pane_id.to_string()),
            agent_id,
            agent_version: None,
            schema_version: None,
            live_target: None,
            model: None,
        }
    }

    const SHELL: &str = "pane-shell";
    const REAL: &str = "pane-real";

    // --- daemon side -------------------------------------------------------
    // `pane-shell`: idle agent, then a foreground shell command promotes it to
    // a SYNTHETIC Working. `pane-real`: the agent itself is working (ToolStart),
    // which must never be revertible by a shell signal.
    let mut daemon = AppState::default();
    for pane in [SHELL, REAL] {
        daemon.register_pane(pane.to_string());
        daemon.apply_event(hook_event(pane, EventType::SessionStart, None));
    }
    // `pane-shell` additionally survives a same-agent restart BEFORE the shell
    // command starts, so the synthesized events below are built while the
    // pane's hook generation and its stable card id DISAGREE — the divergence
    // that made a `sessions[hook_generation]` agent lookup miss and re-emit
    // `agent_id: None`.
    daemon.apply_event(rollover_event(SHELL));
    assert_ne!(
        daemon.pane_hook_session_id(SHELL).as_deref(),
        Some(format!("sess-{SHELL}").as_str()),
        "precondition: the same-agent restart rolls the hook generation past \
         the stable card id"
    );
    assert!(
        !daemon
            .sessions
            .contains_key(&daemon.pane_hook_session_id(SHELL).unwrap()),
        "precondition: after the rollover the hook generation is NOT a key \
         into `sessions`, so the agent id must come from the pane's card"
    );

    let busy = shell_event(&daemon, SHELL, EventType::ShellBusy);
    assert_eq!(
        busy.agent_id.as_deref(),
        Some(format!("agent-{SHELL}").as_str()),
        "the synthesized event must still carry the owning agent id after a \
         rollover — an unstamped one cannot reach a hydrated card"
    );
    daemon.apply_event(busy);
    daemon.apply_event(hook_event(REAL, EventType::ToolStart, Some("Bash")));
    for pane in [SHELL, REAL] {
        assert_eq!(
            daemon.sessions[&format!("sess-{pane}")].status,
            SessionStatus::Working,
            "precondition: {pane} is Working on the daemon before the reconnect"
        );
    }

    // --- reconnect: the daemon's snapshot crosses the wire and seeds the TUI --
    let mut tui = AppState::default();
    for pane in [SHELL, REAL] {
        let snapshot = daemon.sessions[&format!("sess-{pane}")].live_snapshot();
        let json = serde_json::to_string(&snapshot).expect("the snapshot serializes");
        let snapshot: SessionSnapshot =
            serde_json::from_str(&json).expect("the snapshot deserializes");
        tui.register_pane(pane.to_string());
        tui.seed_hydrated_session(
            pane.to_string(),
            None,
            Some(AgentType::ClaudeCode),
            Some(format!("agent-{pane}")),
            Some(&snapshot),
        );
    }

    // --- the foreground command finishes: ShellIdle fans out to both -------
    for pane in [SHELL, REAL] {
        let idle = shell_event(&daemon, pane, EventType::ShellIdle);
        daemon.apply_event(idle.clone());
        tui.apply_event(idle);
    }

    // The synthesized event has to LAND on the rehydrated card, not mint a
    // second one beside it. The hydration-minted card is keyed
    // `pane-{pane_id}` while the event still reports under the daemon's hook
    // session id, so only `apply_event`'s same-pane reuse guard — which
    // matches on `agent_id` — can bring the two together.
    for pane in [SHELL, REAL] {
        let cards: Vec<&str> = tui
            .sessions
            .iter()
            .filter(|(_, session)| session.pane_id.as_deref() == Some(pane))
            .map(|(id, _)| id.as_str())
            .collect();
        assert_eq!(
            cards.len(),
            1,
            "a synthesized shell event must remap onto the rehydrated card for \
             {pane}, not spawn a phantom second one; got {cards:?}"
        );
    }

    // The synthetic promotion must be revertible on BOTH sides — the whole
    // point of issue #21 is that the rehydrated TUI disagreed with the daemon.
    assert_eq!(
        daemon.sessions[&format!("sess-{SHELL}")].status,
        SessionStatus::Idle,
        "daemon control: the paired ShellIdle reverts its own synthetic promotion"
    );
    assert_eq!(
        tui.sessions[&format!("pane-{SHELL}")].status,
        SessionStatus::Idle,
        "a card rehydrated mid-ShellBusy must still be revertible by the paired \
         ShellIdle — otherwise it reads Working forever (fork issue #21)"
    );

    // ...and the real, agent-emitted Working must survive it untouched.
    assert_eq!(
        daemon.sessions[&format!("sess-{REAL}")].status,
        SessionStatus::Working,
        "daemon control: a stale ShellIdle must not revert a real agent Working"
    );
    assert_eq!(
        tui.sessions[&format!("pane-{REAL}")].status,
        SessionStatus::Working,
        "rehydration must not make a real, agent-emitted Working clearable by ShellIdle"
    );
}

// ---------------------------------------------------------------------------
// Fork issue #36 — the snapshot/subscribe window.
// ---------------------------------------------------------------------------

/// The pane/agent/session identity the issue #36 harness below reconnects to.
const RACE_PANE: &str = "pane-race-36";
const RACE_AGENT: &str = "agent-race-36";
const RACE_SESSION: &str = "sess-race-36";

/// How long the mock daemon sits on the `SubscribeEvents` connection before it
/// registers a receiver and acknowledges it. Long enough that a client which
/// snapshots WITHOUT waiting for the acknowledgement deterministically wins the
/// race to `ListAgents` — that is the window issue #36 is about, made
/// reproducible instead of left to scheduler luck.
const RACE_SUBSCRIBE_DELAY: Duration = Duration::from_millis(500);

/// Mock daemon for fork issue #36: it is SLOW to service its `SubscribeEvents`
/// connection, and it broadcasts an event the instant it has served the
/// `ListAgents` snapshot.
///
/// The two behaviours together reproduce the daemon's real ordering guarantee
/// and the client-side gap it exposes:
///
/// * Like the real `handle_subscribe_events`, the broadcast receiver is
///   registered immediately BEFORE the OK `RESP` is written — so a client that
///   has read that RESP can no longer miss anything. Unlike the real daemon it
///   takes [`RACE_SUBSCRIBE_DELAY`] to get there, standing in for a daemon that
///   has not yet got round to the connection.
/// * The `ShellIdle` is broadcast right after the snapshot is serialized,
///   modelling the shell-activity monitor firing the paired edge in exactly the
///   window between snapshot capture and subscription.
///
/// A client that snapshots first therefore has no receiver registered when the
/// event goes out, and `broadcast::Sender::send` drops it — the edge is lost
/// with nothing to replay it. A client that waits for the subscription
/// acknowledgement first receives it.
async fn run_delayed_subscribe_server(
    listener: UnixListener,
    event_tx: tokio::sync::broadcast::Sender<BroadcastMsg>,
    record: AgentRecord,
    idle_event: AgentEvent,
) {
    loop {
        let (mut stream, _) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => return,
        };
        let event_tx = event_tx.clone();
        let record = record.clone();
        let idle_event = idle_event.clone();
        tokio::spawn(async move {
            let req = match read_frame(&mut stream).await {
                Ok(Some((KIND_REQ, payload))) => {
                    match serde_json::from_slice::<AttachRequest>(&payload) {
                        Ok(r) => r,
                        Err(_) => return,
                    }
                }
                _ => return,
            };
            match req {
                AttachRequest::ListAgents => {
                    let resp = AttachResponse {
                        ok: true,
                        agent_records: Some(vec![record]),
                        ..Default::default()
                    };
                    let _ = write_resp(&mut stream, &resp).await;
                    // The snapshot is now on the wire and already stale: the
                    // foreground command has finished. `send` errs when there
                    // are no receivers — which is precisely the lost edge.
                    let _ = event_tx.send(BroadcastMsg::Event(idle_event));
                }
                AttachRequest::SubscribeEvents => {
                    tokio::time::sleep(RACE_SUBSCRIBE_DELAY).await;
                    let mut rx = event_tx.subscribe();
                    let payload = serde_json::to_vec(&AttachResponse::ok())
                        .expect("AttachResponse must serialize");
                    if write_frame(&mut stream, KIND_RESP, &payload).await.is_err() {
                        return;
                    }
                    while let Ok(msg) = rx.recv().await {
                        let payload = serde_json::to_vec(&msg).expect("BroadcastMsg serializes");
                        if write_frame(&mut stream, KIND_EVENT, &payload)
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
                AttachRequest::AttachStream { .. } => {
                    let _ = write_resp(&mut stream, &AttachResponse::ok()).await;
                    loop {
                        match read_frame(&mut stream).await {
                            Ok(None) | Err(_) => break,
                            Ok(Some(_)) => continue,
                        }
                    }
                }
                _ => {
                    let _ = write_resp(&mut stream, &AttachResponse::ok()).await;
                }
            }
        });
    }
}

/// Scenario: A daemon-side pane is `Working` only because the shell-activity
/// monitor synthesized a `ShellBusy`, and the foreground command finishes in
/// the window between the reconnecting TUI capturing its `ListAgents` snapshot
/// and its event stream coming up. Drive the real reconnect bootstrap — the
/// production event subscriber plus `hydrate_from_daemon` — against a daemon
/// that is deliberately slow to acknowledge the subscription and broadcasts the
/// paired `ShellIdle` the moment it has served the snapshot. The rebuilt card
/// must end up `Idle`: the edge has to be received (subscribe before snapshot)
/// AND held until the pane exists (buffer across hydration), or the pane reads
/// `Working` forever with nothing left to correct it.
#[spec("session/live/016")]
#[test]
fn live_016_shell_idle_in_the_snapshot_subscribe_window_still_clears_the_card() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("multi-thread runtime");
    rt.block_on(live_016_shell_idle_in_the_snapshot_subscribe_window_still_clears_the_card_inner());
}

async fn live_016_shell_idle_in_the_snapshot_subscribe_window_still_clears_the_card_inner() {
    // The snapshot the daemon serializes: `Working`, and flagged as the
    // shell-activity monitor's SYNTHETIC promotion (fork issue #21's marker, so
    // the paired `ShellIdle` is entitled to revert it).
    let record = AgentRecord {
        id: RACE_AGENT.to_string(),
        pane_id_env: Some(RACE_PANE.to_string()),
        display_name: None,
        cwd: None,
        tab_membership: None,
        agent_type: Some(AgentType::ClaudeCode),
        rows: 24,
        cols: 80,
        live: Some(SessionSnapshot {
            status: SessionStatus::Working,
            agent_type: Some(AgentType::ClaudeCode),
            active_tool: None,
            tool_count: 0,
            first_prompts: Vec::new(),
            last_user_prompt: None,
            live_target: None,
            last_activity_ms: None,
            shell_synthetic_working: true,
            monitored_wait_active: false,
            wait_synthetic_working: false,
            shell_descendant_busy: false,
            wait_deferred_revert: false,
            model: None,
        }),
        spawned_at_ms: None,
        daemon_boot_id: None,
        registration_generation: None,
        outstanding_delegation: None,
        silence_watch: None,
        delegation_commission: None,
    };
    // The paired `ShellIdle`, shaped the way `run_shell_activity_monitor`
    // stamps it: neutral agent type, the owning `agent_id` (so
    // `apply_event`'s same-pane reuse guard can remap it onto the seeded card,
    // whose id is `pane-{pane_id}` rather than the daemon's session id).
    let idle_event = AgentEvent {
        session_id: RACE_SESSION.to_string(),
        agent_type: AgentType::None,
        event_type: EventType::ShellIdle,
        tool_name: None,
        tool_detail: None,
        cwd: None,
        timestamp: Utc::now(),
        user_prompt: None,
        metadata: HashMap::new(),
        pane_id: Some(RACE_PANE.to_string()),
        agent_id: Some(RACE_AGENT.to_string()),
        agent_version: None,
        schema_version: None,
        live_target: None,
        model: None,
    };

    let (dir, path, listener) = {
        let _g = HARNESS_BIND_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = test_temp::tempdir().unwrap();
        let path = dir.path().join("attach.sock");
        let listener = UnixListener::bind(&path).expect("bind mock attach socket");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        (dir, path, listener)
    };
    // Bound to `_` so the initial Receiver drops immediately: the only
    // receivers must be the ones the mock registers per subscribe connection,
    // otherwise a broadcast with no subscriber would be silently kept alive by
    // this one and the lost-edge case could not be reproduced.
    let (event_tx, _) = tokio::sync::broadcast::channel::<BroadcastMsg>(16);
    let server = tokio::spawn(run_delayed_subscribe_server(
        listener,
        event_tx.clone(),
        record,
        idle_event,
    ));

    let state: SharedState = Arc::new(tokio::sync::RwLock::new(AppState::default()));

    // --- the production reconnect bootstrap, in the order `main` runs it ----
    let gate = HydrationGate::armed();
    let subscriber = tokio::spawn(run_event_subscriber(
        path.clone(),
        state.clone(),
        gate.clone(),
    ));

    let ctrl = Arc::new(
        EmbeddedPaneController::new(path.clone(), tokio::runtime::Handle::current())
            .with_hydration_gate(gate.clone()),
    );
    let hydrated = {
        let ctrl = ctrl.clone();
        tokio::task::spawn_blocking(move || ctrl.hydrate_from_daemon())
            .await
            .unwrap()
    };
    assert_eq!(hydrated.len(), 1, "the single daemon agent must hydrate");
    assert_eq!(hydrated[0].pane_id, RACE_PANE);

    // ...and the seeding `run_tui` performs for each hydrated pane, followed by
    // the gate release that lets held events land.
    {
        let mut st = state.write().await;
        for h in &hydrated {
            st.register_pane(h.pane_id.clone());
            st.seed_hydrated_session(
                h.pane_id.clone(),
                h.cwd.clone(),
                h.agent_type.clone(),
                Some(h.agent_id.clone()),
                h.live.as_ref(),
            );
        }
        assert_eq!(
            st.sessions[&format!("pane-{RACE_PANE}")].status,
            SessionStatus::Working,
            "precondition: the card seeds Working from the (already stale) snapshot"
        );
    }
    gate.mark_seeded();

    // The card must come back to Idle off the buffered edge. Polled rather than
    // asserted immediately because the subscriber applies it on its own task.
    let cleared = {
        let state = state.clone();
        wait_for(Duration::from_secs(5), Duration::from_millis(25), || {
            race_pane_is_idle(&state)
        })
        .await
    };

    let status = state.read().await.sessions[&format!("pane-{RACE_PANE}")]
        .status
        .clone();
    assert!(
        cleared,
        "the ShellIdle broadcast between snapshot capture and the event stream \
         coming up must still reach the rebuilt card — the card is stuck at \
         {status:?} (fork issue #36)"
    );
    assert_eq!(
        status,
        SessionStatus::Idle,
        "a card rebuilt from a snapshot that was already stale must be corrected \
         by the edge that made it stale (fork issue #36)"
    );

    subscriber.abort();
    server.abort();
    drop(ctrl);
    drop(dir);
}

/// Synchronous peek at the race pane's status for [`wait_for`]'s `FnMut() ->
/// bool` predicate, which cannot await. `try_read` is enough: the subscriber
/// holds the write lock only for the duration of one `apply_event`, so a
/// contended poll simply retries on the next tick.
fn race_pane_is_idle(state: &SharedState) -> bool {
    match state.try_read() {
        Ok(st) => st
            .sessions
            .get(&format!("pane-{RACE_PANE}"))
            .is_some_and(|s| s.status == SessionStatus::Idle),
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Issues #49 / #28 — the TUI re-subscribes on reconnect but never
// re-hydrates. `session/live/012` (above) closes the BOOTSTRAP snapshot/
// subscribe window; these close the same hazard LATER in the connection's
// life, once the subscription that bootstrap already ordered correctly dies
// mid-session and the reconnect loop re-subscribes without draining a fresh
// `list_agents` snapshot.
// ---------------------------------------------------------------------------

const RECONNECT_PANE: &str = "pane-reconnect-49";
const RECONNECT_AGENT: &str = "agent-reconnect-49";

/// Which shape the mock daemon's forced tear-down takes — the two ways
/// issues #49/#28 report the subscription dying mid-session. The approved
/// design (`docs`/PRD decision recorded in the work-done report) is "always
/// re-hydrate on reconnect", not just on `Lagged`, so both variants must
/// converge the same way.
#[derive(Clone, Copy)]
enum ReconnectTeardown {
    /// `KIND_STREAM_END` carrying the documented `"lagged"` reason —
    /// `handle_subscribe_events`'s `RecvError::Lagged` arm.
    Lagged,
    /// The connection simply drops with no `KIND_STREAM_END` frame at all —
    /// stands in for a daemon restart or a bare transport failure.
    Dropped,
}

/// Mock daemon for issues #49/#28. `ListAgents` always answers from
/// `record`'s CURRENT value, read fresh on every call, so a later call sees
/// whatever the test has since mutated. The FIRST `SubscribeEvents`
/// connection acknowledges normally and then does nothing but wait for
/// `teardown` — it deliberately never calls `rx.recv()`, so nothing broadcast
/// while it is the live subscription can ever reach the client through it —
/// then ends the stream per `reason`. Every LATER `SubscribeEvents`
/// connection (the reconnect) forwards broadcast events normally.
///
/// This makes the outage window exact by construction rather than by timing:
/// only events sent after the reconnect's OWN `subscribe()` call can ever be
/// observed, which is exactly how a real `tokio::sync::broadcast::Sender`
/// behaves (a new subscriber never sees history) — no sleep, no race.
async fn run_reconnect_teardown_server(
    listener: UnixListener,
    event_tx: tokio::sync::broadcast::Sender<BroadcastMsg>,
    record: Arc<Mutex<AgentRecord>>,
    teardown: Arc<tokio::sync::Notify>,
    reason: ReconnectTeardown,
) {
    let subscribe_count = Arc::new(AtomicUsize::new(0));
    loop {
        let (mut stream, _) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => return,
        };
        let event_tx = event_tx.clone();
        let record = record.clone();
        let teardown = teardown.clone();
        let subscribe_count = subscribe_count.clone();
        tokio::spawn(async move {
            let req = match read_frame(&mut stream).await {
                Ok(Some((KIND_REQ, payload))) => {
                    match serde_json::from_slice::<AttachRequest>(&payload) {
                        Ok(r) => r,
                        Err(_) => return,
                    }
                }
                _ => return,
            };
            match req {
                AttachRequest::ListAgents => {
                    let snapshot = record.lock().unwrap_or_else(|p| p.into_inner()).clone();
                    let resp = AttachResponse {
                        ok: true,
                        agent_records: Some(vec![snapshot]),
                        ..Default::default()
                    };
                    let _ = write_resp(&mut stream, &resp).await;
                }
                AttachRequest::SubscribeEvents => {
                    let rx = event_tx.subscribe();
                    if write_resp(&mut stream, &AttachResponse::ok())
                        .await
                        .is_err()
                    {
                        return;
                    }
                    let is_first =
                        subscribe_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
                    if is_first {
                        // Never drains `rx` — see the function doc. Waits
                        // ONLY for the test's explicit signal, so the
                        // tear-down happens exactly when the test wants it.
                        drop(rx);
                        teardown.notified().await;
                        match reason {
                            ReconnectTeardown::Lagged => {
                                let _ = write_frame(&mut stream, KIND_STREAM_END, b"lagged").await;
                            }
                            ReconnectTeardown::Dropped => {
                                // No STREAM_END frame — just drop the stream.
                            }
                        }
                    } else {
                        let mut rx = rx;
                        while let Ok(msg) = rx.recv().await {
                            let payload =
                                serde_json::to_vec(&msg).expect("BroadcastMsg serializes");
                            if write_frame(&mut stream, KIND_EVENT, &payload)
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
                AttachRequest::AttachStream { .. } => {
                    let _ = write_resp(&mut stream, &AttachResponse::ok()).await;
                    loop {
                        match read_frame(&mut stream).await {
                            Ok(None) | Err(_) => break,
                            Ok(Some(_)) => continue,
                        }
                    }
                }
                _ => {
                    let _ = write_resp(&mut stream, &AttachResponse::ok()).await;
                }
            }
        });
    }
}

/// Drives the real production reconnect bootstrap — `reconnect::run_event_subscriber`,
/// `EmbeddedPaneController::hydrate_from_daemon`, and `AppState::seed_hydrated_session`
/// together — against [`run_reconnect_teardown_server`], tears the live subscription
/// down via `reason` once bootstrap has completed and the card reads the bootstrap
/// `Working` snapshot, and asserts the pane converges to `Idle` — the daemon's truth,
/// which only a fresh `list_agents` drained on reconnect can recover. Shared by
/// session/live/017 and session/live/018; only the tear-down shape differs.
async fn assert_reconnect_recovers_the_missed_status(reason: ReconnectTeardown) {
    let record = Arc::new(Mutex::new(AgentRecord {
        id: RECONNECT_AGENT.to_string(),
        pane_id_env: Some(RECONNECT_PANE.to_string()),
        display_name: None,
        cwd: None,
        tab_membership: None,
        agent_type: Some(AgentType::ClaudeCode),
        rows: 24,
        cols: 80,
        live: Some(SessionSnapshot {
            status: SessionStatus::Working,
            agent_type: Some(AgentType::ClaudeCode),
            active_tool: None,
            tool_count: 0,
            first_prompts: Vec::new(),
            last_user_prompt: None,
            live_target: None,
            last_activity_ms: None,
            shell_synthetic_working: false,
            monitored_wait_active: false,
            wait_synthetic_working: false,
            shell_descendant_busy: false,
            wait_deferred_revert: false,
            model: None,
        }),
        spawned_at_ms: None,
        daemon_boot_id: None,
        registration_generation: None,
        outstanding_delegation: None,
        silence_watch: None,
        delegation_commission: None,
    }));

    let (dir, path, listener) = {
        let _g = HARNESS_BIND_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = test_temp::tempdir().unwrap();
        let path = dir.path().join("attach.sock");
        let listener = UnixListener::bind(&path).expect("bind mock attach socket");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        (dir, path, listener)
    };
    let (event_tx, _) = tokio::sync::broadcast::channel::<BroadcastMsg>(16);
    let teardown = Arc::new(tokio::sync::Notify::new());
    let server = tokio::spawn(run_reconnect_teardown_server(
        listener,
        event_tx.clone(),
        record.clone(),
        teardown.clone(),
        reason,
    ));

    let state: SharedState = Arc::new(tokio::sync::RwLock::new(AppState::default()));

    // --- the production reconnect bootstrap, in the order `main` runs it ---
    let gate = HydrationGate::armed();
    let subscriber = tokio::spawn(run_event_subscriber(
        path.clone(),
        state.clone(),
        gate.clone(),
    ));

    let ctrl = Arc::new(
        EmbeddedPaneController::new(path.clone(), tokio::runtime::Handle::current())
            .with_hydration_gate(gate.clone()),
    );
    let hydrated = {
        let ctrl = ctrl.clone();
        tokio::task::spawn_blocking(move || ctrl.hydrate_from_daemon())
            .await
            .unwrap()
    };
    assert_eq!(hydrated.len(), 1, "the single daemon agent must hydrate");
    assert_eq!(hydrated[0].pane_id, RECONNECT_PANE);

    {
        let mut st = state.write().await;
        for h in &hydrated {
            st.register_pane(h.pane_id.clone());
            st.seed_hydrated_session(
                h.pane_id.clone(),
                h.cwd.clone(),
                h.agent_type.clone(),
                Some(h.agent_id.clone()),
                h.live.as_ref(),
            );
        }
        assert_eq!(
            st.sessions[&format!("pane-{RECONNECT_PANE}")].status,
            SessionStatus::Working,
            "precondition: the card seeds Working from the bootstrap snapshot"
        );
    }
    gate.mark_seeded();
    assert!(
        gate.is_subscribed(),
        "precondition: the bootstrap subscription must be up before the outage is staged"
    );

    // The outage: the daemon's own truth moves to Idle while the ONLY current
    // subscription (the mock's first `SubscribeEvents` connection) is
    // structurally incapable of observing it — see
    // `run_reconnect_teardown_server`'s doc comment. This is exactly the kind
    // of broadcast a real daemon emits and a really-disconnected client
    // misses.
    teardown.notify_one();
    {
        let mut rec = record.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(live) = rec.live.as_mut() {
            live.status = SessionStatus::Idle;
        }
    }
    let corrected_event = AgentEvent {
        session_id: format!("sess-{RECONNECT_AGENT}"),
        agent_type: AgentType::ClaudeCode,
        event_type: EventType::Idle,
        tool_name: None,
        tool_detail: None,
        cwd: None,
        timestamp: Utc::now(),
        user_prompt: None,
        metadata: HashMap::new(),
        pane_id: Some(RECONNECT_PANE.to_string()),
        agent_id: Some(RECONNECT_AGENT.to_string()),
        agent_version: None,
        schema_version: None,
        live_target: None,
        model: None,
    };
    let _ = event_tx.send(BroadcastMsg::Event(corrected_event));

    let corrected = {
        let state = state.clone();
        wait_for(
            Duration::from_secs(8),
            Duration::from_millis(25),
            || match state.try_read() {
                Ok(st) => st
                    .sessions
                    .get(&format!("pane-{RECONNECT_PANE}"))
                    .is_some_and(|s| s.status == SessionStatus::Idle),
                Err(_) => false,
            },
        )
        .await
    };

    let status = state.read().await.sessions[&format!("pane-{RECONNECT_PANE}")]
        .status
        .clone();

    subscriber.abort();
    server.abort();
    drop(ctrl);
    drop(dir);

    assert!(
        corrected,
        "reconnect must re-hydrate a fresh `list_agents` snapshot and recover the \
         status change broadcast during the outage — the card is stuck at {status:?} \
         instead of Idle (issues #49 / #28)"
    );
}

/// Scenario: Bootstrap a reconnecting TUI against a mock daemon (same
/// production path as `session/live/016`), then force the live
/// `SubscribeEvents` connection to close with the daemon's documented
/// `KIND_STREAM_END "lagged"` tear-down while the daemon's own status moves
/// from `Working` to `Idle`. The TUI's reconnect loop re-subscribes; the card
/// must land on `Idle` because reconnect drains a fresh `list_agents`
/// snapshot, not stay stuck on the `Working` it saw before the outage.
#[spec("session/live/017")]
#[test]
fn live_017_lagged_teardown_mid_session_is_recovered_by_reconnect_rehydration() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("multi-thread runtime");
    rt.block_on(assert_reconnect_recovers_the_missed_status(
        ReconnectTeardown::Lagged,
    ));
}

/// Scenario: Same bootstrap as `session/live/017`, but the subscription dies
/// WITHOUT a `lagged` reason — the connection just drops, standing in for a
/// daemon restart or a bare transport failure. The approved design is
/// "always re-hydrate on reconnect", not just on `lagged`, so the card must
/// still recover to `Idle` off a fresh snapshot after this shape of outage.
#[spec("session/live/018")]
#[test]
fn live_018_transport_drop_mid_session_is_also_recovered_by_reconnect_rehydration() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("multi-thread runtime");
    rt.block_on(assert_reconnect_recovers_the_missed_status(
        ReconnectTeardown::Dropped,
    ));
}

/// Scenario: PRD fork#378 reviewer/auditor round 2 (HIGH 1 / F8): a session
/// with a known model is snapshotted via `live_snapshot()`, crosses the wire
/// as JSON, and is restored via `seed_hydrated_session()` on a fresh
/// `AppState` standing in for a reconnecting TUI. The rehydrated card's
/// `model` must still be `Some(..)` AND the card must still render it —
/// today `SessionSnapshot` carries no `model` field at all, so a reconnect
/// silently drops it, and because Claude Code posts `model` only on
/// `SessionStart`, the badge stays degraded for the rest of the session.
/// `live_target` and `shell_synthetic_working` are in this same struct for
/// exactly this reason (fork issue #21, PRD #20 blocker-4); this follows
/// their round-trip precedent (`session/live/010`, `session/live/011`).
#[spec("session/live/019")]
#[test]
fn live_019_rehydration_preserves_model() {
    let mut daemon = AppState::default();
    daemon.register_pane("pane-model".to_string());
    daemon.apply_event(AgentEvent {
        session_id: "sess-model".to_string(),
        agent_type: AgentType::ClaudeCode,
        event_type: EventType::SessionStart,
        tool_name: None,
        tool_detail: None,
        cwd: None,
        timestamp: Utc::now(),
        user_prompt: None,
        metadata: HashMap::new(),
        pane_id: Some("pane-model".to_string()),
        agent_id: Some("agent-model".to_string()),
        agent_version: None,
        schema_version: None,
        live_target: None,
        model: Some("Opus".to_string()),
    });
    assert_eq!(
        daemon.sessions["sess-model"].model.as_deref(),
        Some("Opus"),
        "precondition: the daemon-side session carries the model"
    );

    // --- reconnect: the daemon's snapshot crosses the wire and seeds the TUI --
    let snapshot = daemon.sessions["sess-model"].live_snapshot();
    let json = serde_json::to_string(&snapshot).expect("the snapshot serializes");
    let snapshot: SessionSnapshot = serde_json::from_str(&json).expect("the snapshot deserializes");

    let mut tui = AppState::default();
    tui.register_pane("pane-model".to_string());
    tui.seed_hydrated_session(
        "pane-model".to_string(),
        None,
        Some(AgentType::ClaudeCode),
        Some("agent-model".to_string()),
        Some(&snapshot),
    );

    let rehydrated = tui
        .sessions
        .values()
        .find(|session| session.pane_id.as_deref() == Some("pane-model"))
        .expect("rehydration creates one card for the pane");
    assert_eq!(
        rehydrated.model.as_deref(),
        Some("Opus"),
        "a reconnected card must retain the model it had before the \
         detach — SessionSnapshot must carry a model field"
    );

    // The model must also still RENDER, not just survive in state.
    let width: u16 = 80;
    let density = dot_agent_deck::ui::CardDensityKind::Normal;
    let height = density.rendered_height();
    let buffer = dot_agent_deck::ui::render_card_for_mode_to_buffer(
        rehydrated,
        None,
        Some(1),
        density,
        0,
        false,
        dot_agent_deck::ui::UiMode::Normal,
        width,
        height,
        true,
    );
    let text: String = (0..buffer.area().height)
        .map(|y| {
            (0..buffer.area().width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("ClaudeCode (Opus)"),
        "the rehydrated card must still render its model:\n{text}"
    );
}
