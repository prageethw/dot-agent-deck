// PRD #93 round-5 orchestration lives behind the daemon's attach protocol,
// which this test drives through a real `TabManager` + `EmbeddedPaneController`
// against a real Unix-socket daemon, plus shell-script agent stand-ins on
// `$PATH`. All Unix-only, matching every other real-daemon/real-subprocess
// harness in this suite (`tests/delegate_prompt_injection.rs`,
// `tests/spawn_time_role_prompt_submit_after_session_start.rs`).
#![cfg(unix)]
//! Fork issue #92 P1 (PR #93 pre-merge review): a Pi start-role orchestrator
//! declared BEFORE a worker role in `.dot-agent-deck.toml` can legitimately
//! delegate before the worker's daemon-side registration exists.
//! `TabManager::open_orchestration_tab` (`src/tab.rs`) spawns one role at a
//! time, synchronously, in DECLARATION order. For a Pi start role the
//! orchestrator prompt rides as `seed` on that role's own `StartAgent`, and
//! the daemon makes the seed pullable immediately after spawning the Pi
//! child (`src/daemon_protocol.rs`), before the TUI has even issued the
//! next role's `StartAgent`. A Pi start role listed before a worker can
//! therefore pull its seed, act on it, and `delegate --to worker` while
//! that worker's pane is still being created — `delegate_targets` finds
//! nothing, the zero-resolution rejection added for fork #92 fires, and the
//! agent's first task is lost with no retry anywhere.
//!
//! This test pins the RED failure mode by making the race deterministic
//! instead of trusting real OS scheduling to reproduce it. It declares a Pi
//! start role FIRST and a worker role SECOND — the pre-fix-hostile order —
//! and installs a `StartAgentRegistrationHook` (the fork #92 test seam
//! added to `src/daemon.rs`/`src/daemon_protocol.rs`) that parks the
//! worker's `StartAgent` handling after its child process is spawned but
//! before its `AppState` role/orchestrator maps are published. A real `pi`
//! shell-script shim on `$PATH` immediately calls `get-seed` then
//! `delegate --to coder` the moment it boots; a `cat` stand-in for the
//! worker echoes whatever the daemon injects into its PTY, so a delivered
//! pointer is directly observable in its scrollback.
//!
//! Pre-fix: nothing holds Pi back — it starts and races its delegate call
//! while the gate (armed for the WORKER's registration, which Pi's own
//! spawn never touches) is irrelevant to it, and the seed is consumed well
//! before release. Post-fix, Pi is the LAST role spawned, so its
//! `StartAgent` cannot even be issued by `TabManager`'s synchronous loop
//! until the worker's spawn call returns — which this test holds open
//! indefinitely via the gate — so the seed cannot be consumed while the
//! gate is held, and exactly one delegate pointer arrives once it releases.
//!
//! Assertions are outcome-based (seed-consumption timing, final pointer
//! count), not a pin on which role spawns first — see `src/tab.rs`'s
//! `delegate_030_pi_last_spawn_order_keeps_pane_ids_declaration_indexed`
//! for the separate, spawn-order-agnostic pane-id-indexing guard.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::UnixStream;
use tokio::sync::RwLock;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use dot_agent_deck::agent_pty::AgentPtyRegistry;
use dot_agent_deck::daemon::{Daemon, StartAgentRegistrationHook, run_daemon_with};
use dot_agent_deck::embedded_pane::EmbeddedPaneController;
use dot_agent_deck::project_config::{OrchestrationConfig, OrchestrationRoleConfig};
use dot_agent_deck::state::{AppState, SharedState};
use dot_agent_deck::tab::TabManager;
use spec::spec;

mod common;

const START_ROLE: &str = "orchestrator";
const WORKER_ROLE: &str = "coder";
const SEED_TEXT: &str = "orchestration-startup-race-seed";
const POINTER: &[u8] = b"Read .dot-agent-deck/worker-task-coder.md for your task.";

/// How long the gate holds the worker's registration open, giving the pi
/// shim's real child process ample wall-clock time to race ahead if
/// nothing is actually holding it back. Well under `CREATE_PANE_START_TIMEOUT`
/// (5s, `src/embedded_pane.rs`) — the client-side timeout on the `StartAgent`
/// RPC the gate parks — so the hold itself can never make
/// `open_orchestration_tab` fail with a spurious timeout.
const GATE_HOLD: Duration = Duration::from_millis(1200);

static HARNESS_BIND_LOCK: Mutex<()> = Mutex::new(());

/// A daemon serving the real M1.2 attach protocol (not just the hook
/// socket `common::spawn_inprocess_daemon` exposes), so a real
/// `EmbeddedPaneController` can drive `TabManager::open_orchestration_tab`
/// against it exactly as the production TUI does.
struct DaemonHandle {
    _dir: tempfile::TempDir,
    attach_path: PathBuf,
    pty_registry: Arc<AgentPtyRegistry>,
    handle: JoinHandle<()>,
}

impl Drop for DaemonHandle {
    fn drop(&mut self) {
        self.handle.abort();
        self.pty_registry.shutdown_all();
    }
}

async fn spawn_daemon_with_hook(hook: StartAgentRegistrationHook) -> DaemonHandle {
    common::init_test_env();
    let (dir, hook_path, attach_path) = {
        let _g = HARNESS_BIND_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = common::race_safe_tempdir();
        let hook_path = dir.path().join("hook.sock");
        let attach_path = dir.path().join("attach.sock");
        (dir, hook_path, attach_path)
    };

    let state: SharedState = Arc::new(RwLock::new(AppState::default()));
    let daemon = Daemon::with_attach(state, attach_path.clone())
        .with_idle_shutdown(None)
        .with_lock_dir_override(common::lock_dir_path())
        .with_start_agent_registration_hook(hook);
    let pty_registry = daemon.pty_registry.clone();

    let hook_for_daemon = hook_path.clone();
    let handle = tokio::spawn(async move {
        let _ = run_daemon_with(&hook_for_daemon, daemon).await;
    });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut attach_ready = false;
    while tokio::time::Instant::now() < deadline {
        if attach_path.exists() && UnixStream::connect(&attach_path).await.is_ok() {
            attach_ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        attach_ready,
        "attach socket was not accepting connections within 5s"
    );

    DaemonHandle {
        _dir: dir,
        attach_path,
        pty_registry,
        handle,
    }
}

fn write_executable(path: &Path, contents: &str) {
    std::fs::write(path, contents).expect("write synthetic agent executable");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod synthetic agent executable");
}

/// A real `pi` binary on `$PATH` (its literal name is what makes
/// `AgentType::from_command` classify the start role as Pi, arming the
/// native-seed path in `open_orchestration_tab`). On boot it immediately:
/// (1) calls `get-seed` and writes whatever it got — even an empty string —
/// to `seed_marker` BEFORE doing anything else, so the marker's mere
/// EXISTENCE is proof `get-seed` returned control to the shim, independent
/// of the delegate call that follows; (2) issues exactly one
/// `delegate --to coder`; (3) execs into `cat` so the pane stays alive and
/// echoes anything the daemon later injects (native-pull confirmation, the
/// PTY-fallback safety net, or a stray second delivery all become visible
/// in its own scrollback if they ever fire).
fn write_pi_shim(dir: &Path, seed_marker: &Path) -> PathBuf {
    let path = dir.join("pi");
    let script = format!(
        "#!/bin/sh\nseed=\"$(dot-agent-deck get-seed)\"\nprintf '%s' \"$seed\" > '{marker}'\ndot-agent-deck delegate --to {role} --task \"$seed\"\nexec cat\n",
        marker = seed_marker.display(),
        role = WORKER_ROLE,
    );
    write_executable(&path, &script);
    path
}

/// Prepend `shim_dir` (for the `pi` script) and the built `dot-agent-deck`
/// binary's own directory (for the `get-seed`/`delegate` subcommands the
/// shim invokes) to the real `$PATH`. The daemon spawns children by
/// inheriting ITS OWN process environment (see `agent_pty::spawn`'s
/// `CommandBuilder` doc comment) — since the daemon here runs in-process in
/// this same test binary, mutating this process's `PATH` is what the
/// daemon's spawned children actually see.
fn path_with_shim_and_built_deck(shim_dir: &Path) -> String {
    let deck_dir = Path::new(env!("CARGO_BIN_EXE_dot-agent-deck"))
        .parent()
        .expect("built deck binary has a parent directory");
    format!(
        "{}:{}:{}",
        shim_dir.display(),
        deck_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

fn count_needle(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() || haystack.len() < needle.len() {
        return 0;
    }
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

async fn wait_for_snapshot_needle(
    registry: &AgentPtyRegistry,
    agent_id: &str,
    needle: &[u8],
    timeout: Duration,
) -> Vec<u8> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(snap) = registry.snapshot(agent_id)
            && snap.windows(needle.len()).any(|w| w == needle)
        {
            return snap;
        }
        if tokio::time::Instant::now() >= deadline {
            return registry.snapshot(agent_id).unwrap_or_default();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_agent_id_for_pane(
    registry: &AgentPtyRegistry,
    pane_id: &str,
    timeout: Duration,
) -> String {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let records = registry.agent_records();
        if let Some(rec) = records
            .iter()
            .find(|r| r.pane_id_env.as_deref() == Some(pane_id))
        {
            return rec.id.clone();
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("daemon registry never surfaced an agent for pane_id {pane_id}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Scenario: Open an orchestration whose config declares a Pi start role
/// FIRST and a `coder` worker SECOND (the ordering PR #93 says the daemon
/// must not let bite) through a real `TabManager` + `EmbeddedPaneController`
/// against a real attach-protocol daemon. A registration gate (the fork #92
/// test seam) holds the worker's `StartAgent` handling open after its `cat`
/// stand-in is spawned but before its role/orchestrator maps publish. While
/// held, assert the pi shim's `get-seed` call has NOT produced its marker
/// file — proof the seed was not consumed. After release, assert the
/// marker now holds the exact seed text and the worker's PTY scrollback
/// contains the delegate pointer EXACTLY once (a duplicate would reproduce
/// upstream #330's harm; the RED failure mode this pins is the opposite —
/// today the marker appears almost immediately regardless of the gate, and
/// the worker's scrollback ends up with ZERO pointers because the delegate
/// raced ahead of the worker's own registration).
#[spec("orchestration/delegate/036")]
#[test]
fn delegate_036_pi_start_role_delegate_survives_worker_registration_race() {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("build startup-race runtime")
        .block_on(delegate_036_pi_start_role_delegate_survives_worker_registration_race_inner());
}

async fn delegate_036_pi_start_role_delegate_survives_worker_registration_race_inner() {
    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd.path().to_string_lossy().into_owned();
    let shim_dir = common::race_safe_tempdir();
    let seed_marker = shim_dir.path().join("pi-seed.marker");
    write_pi_shim(shim_dir.path(), &seed_marker);
    // SAFETY: this integration-test BINARY contains exactly one `#[test]`
    // function (nextest already gives it a dedicated process — see the
    // ENV_LOCK precedent in `tests/delegate_prompt_injection.rs`, not
    // needed here for that reason), so no sibling test can observe or race
    // this mutation; the process is discarded when the test ends.
    unsafe {
        std::env::set_var("PATH", path_with_shim_and_built_deck(shim_dir.path()));
    }

    // The gate: parks the WORKER's (role_name == "coder") `StartAgent`
    // handling after `registry.spawn_agent` succeeds but before its
    // `AppState` maps publish — see `StartAgentRegistrationHook`'s doc
    // comment in `src/daemon.rs`. Filtered by role name, not call order, so
    // it fires only for the worker's own spawn regardless of which role
    // the (pre-fix) declaration-order loop happens to spawn first.
    let (gate_entered_tx, gate_entered_rx) = oneshot::channel::<()>();
    let (gate_release_tx, gate_release_rx) = oneshot::channel::<()>();
    let gate_entered_tx = Arc::new(Mutex::new(Some(gate_entered_tx)));
    let gate_release_rx = Arc::new(Mutex::new(Some(gate_release_rx)));
    let hook: StartAgentRegistrationHook = Arc::new(move |role_name: &str| {
        let release_rx = if role_name == WORKER_ROLE {
            gate_release_rx.lock().unwrap().take()
        } else {
            None
        };
        let entered_tx = if release_rx.is_some() {
            gate_entered_tx.lock().unwrap().take()
        } else {
            None
        };
        Box::pin(async move {
            if let Some(tx) = entered_tx {
                let _ = tx.send(());
            }
            if let Some(rx) = release_rx {
                let _ = rx.await;
            }
        })
    });

    let daemon = spawn_daemon_with_hook(hook).await;

    let controller = Arc::new(EmbeddedPaneController::new(
        daemon.attach_path.clone(),
        tokio::runtime::Handle::current(),
    ));

    let config = OrchestrationConfig {
        name: "startup-race".to_string(),
        roles: vec![
            // Declared FIRST — the pre-fix-hostile order the design calls
            // out explicitly. `command = "pi"` is what makes
            // `AgentType::from_command` classify this as the Pi start role
            // and arm the native-seed path in `open_orchestration_tab`.
            OrchestrationRoleConfig {
                name: START_ROLE.to_string(),
                command: "pi".to_string(),
                start: true,
                description: None,
                prompt_template: None,
                clear: false,
            },
            // Declared SECOND. `clear = false` and no on-disk
            // `.dot-agent-deck.toml` (this test never writes one — matching
            // `orchestration/delegate/022`-`025`) means `dispatch_one_owned`
            // finds no role config and falls straight to the no-respawn
            // path: no readiness wait, the prompt writes to this existing
            // `cat` pane as soon as `delegate_targets` resolves it.
            //
            // Raw-mode, not bare `cat`: a bare `cat` pane keeps the PTY
            // slave's default canonical/echo termios, so a single
            // daemon-initiated write is BOTH echoed by the kernel tty layer
            // AND copied through by `cat` itself, landing twice in the
            // master-side scrollback this test asserts against — a harness
            // artifact, not a second delivery. Every other `cat` stand-in in
            // this suite that reads its own scrollback for an exact count
            // wraps it the same way (`tests/idle_worker_detector.rs`,
            // `tests/delegate_prompt_injection.rs`,
            // `src/agent_pty.rs`'s `write_to_pane_notice_bytes_precede_…`
            // test) — `stty -echo -icanon` before `exec cat -u` collapses it
            // back to exactly one occurrence per write.
            OrchestrationRoleConfig {
                name: WORKER_ROLE.to_string(),
                command: "stty -echo -icanon -icrnl -opost min 1 time 0 && exec cat -u".to_string(),
                start: false,
                description: None,
                prompt_template: None,
                clear: false,
            },
        ],
    };

    let open_task: JoinHandle<Result<(usize, Vec<String>), dot_agent_deck::tab::TabError>> =
        tokio::task::spawn_blocking(move || {
            let mut manager = TabManager::new(controller);
            manager.open_orchestration_tab(
                &config,
                &cwd_str,
                Some(SEED_TEXT.to_string()),
                None,
                (24, 80),
            )
        });

    // Confirm the worker's `StartAgent` handling has actually reached the
    // gate (its `cat` child is spawned, its maps are not yet published)
    // before trusting the "seed not consumed" check below — otherwise an
    // absent marker could just mean we checked too early, not that
    // anything held Pi back.
    tokio::time::timeout(Duration::from_secs(5), gate_entered_rx)
        .await
        .expect("worker registration gate was never reached within 5s")
        .expect("gate-entered sender dropped without signaling");

    // While the gate is held: poll for up to GATE_HOLD for the pi shim's
    // seed marker. Its presence here — regardless of what it contains —
    // proves `get-seed` returned control to the shim (and thus that Pi's
    // own `StartAgent` was issued and answered) while the worker's
    // registration is still gated open. That can only happen if nothing in
    // the spawn path is holding Pi back until the worker is registered —
    // exactly the pre-fix defect. It cannot fail for an unrelated reason:
    // the marker write happens unconditionally as the shim's very first
    // action, before it ever touches `delegate`, so a shim that failed to
    // launch or failed to resolve `dot-agent-deck` produces an ABSENT
    // marker (indistinguishable from a correctly-gated Pi) rather than a
    // false positive here — the post-release phase below is what rules
    // that ambiguity out, by requiring the marker to appear AND hold the
    // real seed text once the gate opens.
    let hold_deadline = tokio::time::Instant::now() + GATE_HOLD;
    let mut marker_seen_during_hold = false;
    while tokio::time::Instant::now() < hold_deadline {
        if seed_marker.exists() {
            marker_seen_during_hold = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        !marker_seen_during_hold,
        "the pi shim's seed marker at {} appeared while the worker's registration gate was \
         still held — the daemon let Pi consume its native seed (and therefore run its \
         `delegate --to coder`) before the worker pane's AppState maps were published. This is \
         fork #92 P1: a Pi start role declared before its worker can delegate during the \
         window between the worker's spawn and its registration, and `delegate_targets` will \
         resolve nothing for that call",
        seed_marker.display()
    );

    // Release: the worker's `AppState` maps publish, its `StartAgent` reply
    // reaches `TabManager`'s blocking loop, and (post-fix) Pi — spawned
    // last — can now be issued.
    gate_release_tx
        .send(())
        .expect("gate release receiver still alive");

    let (_tab_index, role_pane_ids) = tokio::time::timeout(Duration::from_secs(10), open_task)
        .await
        .expect("open_orchestration_tab did not return within 10s of releasing the gate")
        .expect("open_orchestration_tab task panicked")
        .expect("open_orchestration_tab returned an error");
    assert_eq!(
        role_pane_ids.len(),
        2,
        "one pane per declared role; got {role_pane_ids:?}"
    );
    let worker_pane_id = &role_pane_ids[1];

    // Outcome 1: native seed consumption. The marker must now exist AND
    // hold the exact seed text — proof `get-seed` genuinely returned the
    // orchestrator prompt natively (not the PTY-injection fallback, which
    // would leave the marker file untouched since the shim never sees
    // fallback-injected bytes as a `get-seed` reply).
    let marker_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut marker_contents = String::new();
    loop {
        if let Ok(contents) = std::fs::read_to_string(&seed_marker) {
            marker_contents = contents;
            break;
        }
        if tokio::time::Instant::now() >= marker_deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        marker_contents, SEED_TEXT,
        "after releasing the gate, the pi shim's seed marker must hold the exact seed text \
         `get-seed` returned — an empty or missing marker means Pi either never started or \
         never received its native seed even after the worker was safely registered"
    );

    // Outcome 2: exactly one delegate pointer reaches the worker's PTY.
    // Zero is the pre-fix loss this test pins (the delegate raced ahead of
    // the worker's registration and `delegate_targets` resolved nothing);
    // two or more would reproduce upstream #330's duplicate-arming harm a
    // retry-on-failure "fix" could reintroduce. Waited with real settle
    // time after the first sighting so a stray SECOND delivery (e.g. a
    // buggy fallback re-arm) has a chance to land before the count is read.
    let worker_agent_id =
        wait_for_agent_id_for_pane(&daemon.pty_registry, worker_pane_id, Duration::from_secs(5))
            .await;
    let first_sighting = wait_for_snapshot_needle(
        &daemon.pty_registry,
        &worker_agent_id,
        POINTER,
        Duration::from_secs(5),
    )
    .await;
    assert!(
        first_sighting.windows(POINTER.len()).any(|w| w == POINTER),
        "the worker's `cat` PTY never showed the delegate pointer within 5s of the gate \
         releasing — the delegate that fork #92 P1 causes to race ahead and be dropped never \
         arrived, even after the worker was safely registered; snapshot = {:?}",
        String::from_utf8_lossy(&first_sighting)
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    let settled = daemon
        .pty_registry
        .snapshot(&worker_agent_id)
        .unwrap_or_default();
    let pointer_count = count_needle(&settled, POINTER);
    assert_eq!(
        pointer_count,
        1,
        "the worker's PTY must show the delegate pointer EXACTLY once — 0 is fork #92 P1's \
         startup-race loss (the delegate raced ahead of the worker's own registration and \
         resolved to nothing), 2+ would be upstream #330's duplicate-arming harm reappearing \
         under a naive retry/defer fix; snapshot = {:?}",
        String::from_utf8_lossy(&settled)
    );
}
