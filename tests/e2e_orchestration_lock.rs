#![cfg(feature = "e2e")]

//! L2 end-to-end coverage for PRD #361 Item 3: the command-entry lock on
//! Orchestration tabs. By default, a keystroke typed while a non-
//! orchestrator role pane is focused must not reach that pane's PTY; the
//! orchestrator pane's own input is never gated; `Ctrl+e` toggles the lock;
//! and the small set of always-available `Ctrl+`-chords (resolved before the
//! PTY-forward fallback the lock gates) keep working regardless of lock
//! state or which pane is focused.
//!
//! `orchestration_lock_004`/`005` use the `orch-deck` fixture (two stub
//! `cat` roles, no LLM tokens spent) shared with the PRD #336/#361 Item 4
//! pane-column tests. `orchestration_lock_006` (CLAUDE.md rule 4 /
//! Greptile PR #374 follow-up) instead uses the `orch-lock-live` fixture,
//! whose worker role is a REAL interactive Claude Haiku agent, so the same
//! gate is proven against a genuine agent's input rather than just a `cat`
//! stub's echo.
//!
//! Decision 6: gated behind the `e2e` feature so `cargo test-fast` never
//! compiles it.

mod common;

use std::time::Duration;

use common::TuiDeck;
use dot_agent_deck::event::{AgentEvent, AgentType, EventType};
use spec::spec;

/// Drive the new-pane dialog to open the (single) orchestration in the
/// `orch-deck` fixture. Mirrors `e2e_orchestration_pane_column.rs::open_orchestration`
/// — with no `[[modes]]` defined the Mode chip row is `[No mode] [Orch: …]
/// [schedule]`, so ONE Right selects the orchestration; selecting an
/// orchestration hides the Command field, so a second Enter submits the
/// form. Lands with the orchestrator (start) role focused in `PaneInput`
/// mode.
fn open_orchestration(deck: &TuiDeck) {
    deck.send_keys(b"\x0e"); // Ctrl+n -> directory picker
    deck.send_keys(b" "); // Space -> confirm current dir -> new-pane form
    deck.wait_for_string("No mode"); // form up, Mode field focused at "No mode"
    deck.send_keys(b"\x1b[C"); // Right -> [Orch: demo-orch]
    deck.send_keys(b"\r"); // Mode -> Name
    deck.send_keys(b"\r"); // submit (Command hidden for an orchestration)
}

/// Switch focus from the orchestrator role to the `orch-deck` fixture's
/// second role ("worker", `role_pane_ids` index 1): Ctrl+D back to Normal
/// mode, then digit `2` (`Jump2` -> `Action::FocusCard(1)`) — the same
/// mechanism `focus/orchestration/001` pins for "1-9 on an orchestration tab
/// jumps to role pane N and focuses it". `focus_deck` re-enters `PaneInput`
/// mode on success, so no separate Enter is needed.
fn focus_worker_role(deck: &TuiDeck) {
    deck.send_bytes(b"\x04"); // Ctrl+D -> Normal mode
    deck.send_keys(b"2"); // Jump2 -> focus role index 1 ("worker")
}

/// Scenario: Open a real orchestration tab (default LOCKED) and confirm the
/// focused orchestrator pane's own input is never gated, while a keystroke
/// aimed at the non-orchestrator "worker" role does not reach its PTY. Enter
/// command mode (`Ctrl+d`) and send `Ctrl+e` to unlock — post-M1 the chord
/// only resolves in command mode — then `Ctrl+d` back into `PaneInput` and
/// confirm a keystroke into the still-focused worker pane now forwards and
/// echoes.
#[spec("orchestration/lock/004")]
#[test]
fn orchestration_lock_004_forwarding_gated_by_lock_state() {
    const ORCH_SENTINEL: &str = "LOCK004_ORCH_9f21";
    const WORKER_LOCKED_SENTINEL: &str = "LOCK004_WORKER_LOCKED_7ac4";
    const WORKER_UNLOCKED_SENTINEL: &str = "LOCK004_WORKER_UNLOCKED_c83e";

    let deck = TuiDeck::builder()
        .with_pty_size(120, 40)
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("No active sessions");

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent"); // new-pane form closed -> tab up, orchestrator focused

    // The orchestrator pane is NEVER gated: even though the tab starts
    // LOCKED by default, typing into the currently-focused orchestrator
    // role must still reach its PTY.
    deck.send_keys(format!("{ORCH_SENTINEL}\r").as_bytes());
    deck.wait_for_string(ORCH_SENTINEL);

    // Focus the non-orchestrator "worker" role. The tab is still locked —
    // nothing has toggled it yet.
    focus_worker_role(&deck);

    // Locked: a keystroke aimed at the worker pane must NOT reach its PTY.
    deck.send_keys(format!("{WORKER_LOCKED_SENTINEL}\r").as_bytes());
    let leaked = deck.wait_for_grid_string_within(WORKER_LOCKED_SENTINEL, Duration::from_secs(2));
    assert!(
        !leaked,
        "a keystroke typed into the non-orchestrator worker pane reached \
         its PTY while the orchestration tab's command-entry lock was \
         engaged (the default state) — expected it to be dropped before \
         Action::ForwardToPane.\nGrid:\n{}",
        deck.snapshot_grid()
    );

    // Ctrl+e only resolves in command mode (M1): Ctrl+d into command mode,
    // Ctrl+e to unlock, then Ctrl+d back into PaneInput so the sentinel
    // below actually reaches the worker pane's PTY.
    deck.send_bytes(b"\x04"); // Ctrl+d -> command mode
    deck.send_bytes(b"\x05"); // Ctrl+e == 0x05 -> unlock
    deck.send_bytes(b"\x04"); // Ctrl+d -> back to PaneInput

    // Unlocked: typing into the still-focused worker pane must now forward
    // normally.
    deck.send_keys(format!("{WORKER_UNLOCKED_SENTINEL}\r").as_bytes());
    deck.wait_for_string(WORKER_UNLOCKED_SENTINEL);
}

/// Scenario: Open a real orchestration tab, focus the non-orchestrator
/// "worker" role while the tab is LOCKED, then press `Ctrl+t`
/// (`toggle_layout`) and confirm it still fires and surfaces its
/// `Layout: …` status message — global chords resolve before the PTY-forward
/// fallback the lock gates. Regression guard against an overly-broad gate
/// implementation; its RED status today comes only from the crate-wide
/// compile failure shared with `orchestration/lock/001`-`004`.
#[spec("orchestration/lock/005")]
#[test]
fn orchestration_lock_005_global_chord_unaffected_by_lock_state() {
    let deck = TuiDeck::builder()
        .with_pty_size(120, 40)
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("No active sessions");

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent");

    // Focus the non-orchestrator worker role — still LOCKED, the tab's
    // default state.
    focus_worker_role(&deck);

    deck.send_bytes(b"\x14"); // Ctrl+t (toggle_layout)
    deck.wait_for_string("Layout:");
}

/// Uniquely-named sentinel files (rule 4) the worker directive asks a REAL
/// agent to create — distinct per lock state so a leaked/buffered locked
/// directive can never be confused with the unlocked one that is expected to
/// land.
const LIVE_LOCKED_SENTINEL: &str = "lock006_locked_9d3f.txt";
const LIVE_UNLOCKED_SENTINEL: &str = "lock006_unlocked_5a71.txt";

/// A directive typed straight into a pane's PTY (never `WriteAndSubmit`,
/// which is a daemon RPC that bypasses the lock's keystroke-forwarding gate
/// entirely) asking a real agent to create `sentinel`. Cheap and
/// deterministic: the file's presence/absence on disk is proof the agent did
/// or did not genuinely receive and act on the instruction, independent of
/// terminal echo/redraw variance.
fn create_sentinel_directive(sentinel: &str) -> String {
    format!(
        "Use the Bash tool to create an empty file named {sentinel} in the \
         current directory, then stop and say nothing else.\r"
    )
}

/// The `orch-deck`/`orch-lock-live` fixtures' worker role pane's full daemon
/// registry record. `AgentRecord.id` (the registry's own monotonic counter)
/// and `AgentRecord.pane_id_env` (the `DOT_AGENT_DECK_PANE_ID` the pane was
/// spawned with) are two DISTINCT fields — `spawn_worker_stub` in
/// `tests/orchestration_delegate.rs` sets them to different values
/// explicitly to prove the point. Anything that means "the pane" as
/// `managed_pane_ids` / `role_pane_ids` / `pane.focused_pane_id()` /
/// `build_pane_status`'s join understand it — i.e. anything routed as an
/// `AgentEvent.pane_id` — must read `pane_id_env`, never `id`.
fn worker_agent_record(socket: &std::path::Path) -> dot_agent_deck::agent_pty::AgentRecord {
    common::agent_records_on(socket)
        .into_iter()
        .find(|r| {
            matches!(
                &r.tab_membership,
                Some(dot_agent_deck::agent_pty::TabMembership::Orchestration { role_name, .. })
                    if role_name == "worker"
            )
        })
        .expect("orch-lock-live fixture's worker role pane must be registered with the daemon")
}

/// The `orch-lock-live` fixture's real Claude worker role pane's daemon
/// registry id, used to inspect its cumulative PTY history independent of
/// the outer terminal's redraws.
fn worker_agent_id(socket: &std::path::Path) -> String {
    worker_agent_record(socket).id
}

/// Scenario: Open a real orchestration tab (`cat` orchestrator, a REAL
/// interactive Claude Haiku worker) locked by default, focus the worker, and
/// type a create-sentinel-file directive — confirm the file is never created
/// since the keystrokes never reach the agent. Enter command mode (`Ctrl+d`)
/// and send `Ctrl+e` to unlock, `Ctrl+d` back into `PaneInput`, then type a
/// second directive with a different sentinel — confirm the real agent now
/// receives it and creates that file, proving the lock gates a genuine
/// agent's input, not just a `cat` stub's echo.
#[spec("orchestration/lock/006")]
#[test]
fn orchestration_lock_006_real_agent_gated_by_lock_state() {
    // Decision 26 runtime-skip: a missing CLI or credentials is an
    // environmental condition, not a broken test.
    skip_unless!(common::check_claude_available());

    let deck = TuiDeck::builder()
        .with_pty_size(120, 40)
        .with_imported_claude_credentials()
        // The worker's cwd is the deck's own workdir (the copied
        // `orch-lock-live` fixture root); pre-trust it so the real claude's
        // first-run onboarding/trust gates clear with no keystroke and the
        // directives below aren't swallowed answering them.
        .with_claude_trust_workdir()
        .launch_with_fixture("orch-lock-live");
    deck.wait_for_string("No active sessions");

    let socket = deck.attach_socket_path().to_path_buf();
    let cwd = deck.workdir().to_path_buf();

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent"); // new-pane form closed -> tab up, orchestrator focused

    let worker_id = worker_agent_id(&socket);
    if !common::wait_until_panes_settled(
        &socket,
        std::slice::from_ref(&worker_id),
        Duration::from_millis(1000),
        Duration::from_secs(3),
        Duration::from_secs(60),
    ) {
        eprintln!("warning: the real worker pane did not settle within 60s; proceeding anyway");
    }

    // Focus the non-orchestrator "worker" role. Still LOCKED — nothing has
    // toggled it yet.
    focus_worker_role(&deck);

    // Locked: a directive typed toward the real agent's PTY must not reach
    // it, so it must never act on it.
    deck.send_keys(create_sentinel_directive(LIVE_LOCKED_SENTINEL).as_bytes());
    let created = common::wait_for_path(&cwd.join(LIVE_LOCKED_SENTINEL), Duration::from_secs(20));
    assert!(
        !created,
        "a directive typed into the real Claude worker pane while the \
         orchestration tab's command-entry lock was engaged (the default) \
         reached the agent, which created {LIVE_LOCKED_SENTINEL} — expected \
         the keystrokes to be dropped before Action::ForwardToPane.\n\
         === worker pane (normalized, tail) ===\n{}",
        tail(&common::pane_search_key_on(&socket, &worker_id)),
    );

    // Ctrl+e only resolves in command mode (M1): Ctrl+d into command mode,
    // Ctrl+e to unlock, then Ctrl+d back into PaneInput so the directive
    // below actually reaches the worker pane's PTY.
    deck.send_bytes(b"\x04"); // Ctrl+d -> command mode
    deck.send_bytes(b"\x05"); // Ctrl+e == 0x05 -> unlock
    deck.send_bytes(b"\x04"); // Ctrl+d -> back to PaneInput

    // Unlocked: the same kind of directive into the still-focused worker
    // pane now forwards, and the real agent genuinely acts on it.
    deck.send_keys(create_sentinel_directive(LIVE_UNLOCKED_SENTINEL).as_bytes());
    assert!(
        common::wait_for_path(&cwd.join(LIVE_UNLOCKED_SENTINEL), Duration::from_secs(120)),
        "after Ctrl+e unlocked the tab, a directive typed into the still- \
         focused real Claude worker pane never resulted in \
         {LIVE_UNLOCKED_SENTINEL} being created — expected the keystrokes to \
         forward and the agent to act on them.\n\
         === worker pane (normalized, tail) ===\n{}",
        tail(&common::pane_search_key_on(&socket, &worker_id)),
    );

    // The locked directive must have been genuinely DROPPED, not merely
    // delayed/buffered and flushed once the tab unlocked.
    assert!(
        !cwd.join(LIVE_LOCKED_SENTINEL).exists(),
        "the locked-state directive's sentinel {LIVE_LOCKED_SENTINEL} appeared \
         after unlocking — the lock must drop gated keystrokes outright, not \
         queue them for delivery once unlocked"
    );
}

/// Last ~2 000 characters of a normalized pane key — enough context for a
/// failure message without dumping a megabyte of scrollback.
fn tail(text: &str) -> &str {
    &text[text.len().saturating_sub(2000)..]
}

/// Scenario: PRD #393 M1 — the real-pane proof that `Ctrl+e` is claimed only
/// in command mode. On a real Orchestration tab (`orch-lock-bash-role`
/// fixture: a real interactive bash/readline orchestrator role, plus a
/// `cat`-stub worker role), with the orchestrator pane focused in
/// `PaneInput` mode: type a partial line, home the cursor with `Ctrl+a`
/// (readline's `beginning-of-line`, a chord the deck never binds, so it is
/// safe to use as a control that proves the harness can observe cursor
/// movement at all), then send `Ctrl+e` (`0x05`) and confirm the cursor
/// returns to end-of-line — proving readline's `end-of-line` genuinely ran
/// rather than the byte being claimed as `Action::ToggleOrchestrationLock`.
/// Then press `Ctrl+d` to reach command mode and `Ctrl+e` again, focus the
/// non-orchestrator worker role, and confirm a keystroke now reaches its PTY
/// (the same unlocked-forwarding technique `orchestration/lock/004`-`006`
/// use) — proving the chord still toggles the lock from command mode. RED
/// today: the global keybinding resolver claims `Ctrl+e` as
/// `Action::ToggleOrchestrationLock` on every Orchestration tab regardless
/// of mode (PRD #374), so the first `Ctrl+e` in `PaneInput` mode never
/// reaches the orchestrator's PTY and the cursor never returns to
/// end-of-line.
#[spec("orchestration/lock/008")]
#[test]
fn orchestration_lock_008_ctrl_e_scoped_to_command_mode_on_real_panes() {
    const PARTIAL_LINE: &str = "LOCK008_PARTIAL_f3d1";
    const WORKER_UNLOCKED_SENTINEL: &str = "LOCK008_WORKER_UNLOCKED_7be2";

    let deck = TuiDeck::builder()
        .with_pty_size(120, 40)
        .launch_with_fixture("orch-lock-bash-role");
    deck.wait_for_string("No active sessions");

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent"); // new-pane form closed -> tab up, orchestrator focused
    deck.wait_for_string("[Command Mode Ctrl+D]"); // live PTY, PaneInput mode
    deck.wait_for_string("CTRLL>");

    // --- Part 1: the bug, as the user sees it in the orchestrator's own
    // pane (never gated by the lock, so this isolates the assertion from
    // lock state entirely). ---

    // Baseline captured BEFORE typing starts, so the first cursor read below
    // has something to diverge from (see
    // `TuiDeck::wait_for_terminal_cursor_change_then_settle`'s doc comment
    // for why a divergence check, not just a stability check, is required).
    let pre_type = deck.terminal_cursor_snapshot();

    deck.send_keys(PARTIAL_LINE.as_bytes()); // no trailing \r -- never submitted
    assert!(
        deck.wait_for_grid_string_within(PARTIAL_LINE, Duration::from_secs(3)),
        "the partial line never appeared on the rendered grid\nGrid:\n{}",
        deck.snapshot_grid()
    );
    // Not a plain `terminal_cursor_snapshot()`: the substring landing on the
    // grid only proves those bytes were applied, not that the deck has
    // finished echoing (more may be in flight) or repainted its own
    // block-cursor overlay at the final position (PRD #393 flake).
    let end_of_line =
        deck.wait_for_terminal_cursor_change_then_settle(pre_type, Duration::from_secs(3));

    // Ctrl+a (0x01) == readline's beginning-of-line. The deck binds no
    // action to this chord, so it is a safe control: if the cursor does not
    // move here, the harness's cursor-observation technique itself is
    // broken, independent of anything Ctrl+e does.
    deck.send_bytes(b"\x01");
    let start_of_line =
        deck.wait_for_terminal_cursor_change_then_settle(end_of_line, Duration::from_secs(3));
    assert!(
        (start_of_line.row, start_of_line.col) != (end_of_line.row, end_of_line.col),
        "control check failed: Ctrl+a (readline beginning-of-line) never \
         moved the terminal cursor away from {end_of_line:?} — the harness \
         cannot observe cursor movement in this pane at all, so a Ctrl+e \
         failure below would not be trustworthy either.\nGrid:\n{}",
        deck.snapshot_grid()
    );
    assert_eq!(
        start_of_line.row, end_of_line.row,
        "Ctrl+a must move the cursor within the same row, got \
         start_of_line={start_of_line:?} vs end_of_line={end_of_line:?}"
    );
    assert!(
        start_of_line.col < end_of_line.col,
        "Ctrl+a must move the cursor LEFT toward the beginning of the \
         line, got start_of_line={start_of_line:?} vs end_of_line={end_of_line:?}"
    );

    // Ctrl+e (0x05) == readline's end-of-line. This is the assertion that
    // must be RED today: the global resolver claims 0x05 as
    // Action::ToggleOrchestrationLock on every Orchestration tab regardless
    // of mode (PRD #374), so the byte never reaches the PTY and the cursor
    // never moves back.
    deck.send_bytes(b"\x05");
    let after_ctrl_e =
        deck.wait_for_terminal_cursor_change_then_settle(start_of_line, Duration::from_secs(3));
    assert!(
        (after_ctrl_e.row, after_ctrl_e.col) == (end_of_line.row, end_of_line.col),
        "Ctrl+e did not reach the focused orchestrator role pane's PTY — \
         readline's end-of-line never ran, so the cursor never returned to \
         {end_of_line:?} (settled at {after_ctrl_e:?}). The global \
         keybinding resolver claimed 0x05 as \
         Action::ToggleOrchestrationLock even though a role pane was \
         focused in PaneInput mode — orchestration tabs claim Ctrl+e \
         mode-independently (PRD #393 Problem 1 / PRD #374).\nGrid:\n{}",
        deck.snapshot_grid()
    );

    // --- Part 2: the chord must still work, from command mode. ---

    deck.send_bytes(b"\x04"); // Ctrl+d -> Normal (command) mode
    deck.send_bytes(b"\x05"); // Ctrl+e -> Action::ToggleOrchestrationLock

    // Focus the non-orchestrator "worker" role and confirm a keystroke now
    // reaches its PTY — the same unlocked-forwarding technique
    // `orchestration/lock/004`-`006` use to observe the lock's toggle state.
    focus_worker_role(&deck);
    deck.send_keys(format!("{WORKER_UNLOCKED_SENTINEL}\r").as_bytes());
    assert!(
        deck.wait_for_grid_string_within(WORKER_UNLOCKED_SENTINEL, Duration::from_secs(3)),
        "after Ctrl+d then Ctrl+e from command mode, a keystroke typed into \
         the non-orchestrator worker pane never reached its PTY — expected \
         the command-mode Ctrl+e to have toggled the command-entry lock \
         from its default LOCKED state to unlocked.\nGrid:\n{}",
        deck.snapshot_grid()
    );
}

/// Inject a synthetic `AgentEvent` for the worker's real `(pane_id_env,
/// agent_id)` pair over the deck's hook socket — the SAME bare-`AgentEvent`,
/// no-`DaemonMessage`-envelope wire the real `dot-agent-deck agent-event
/// --type running|waiting|finished` CLI already rides for Pi's status
/// reporting (`src/main.rs`'s `AgentEvent` command,
/// `src/daemon.rs::run_hook_loop`'s `serde_json::from_str::<AgentEvent>`
/// fallback). Stands in for a real extension's status report against a
/// `cat`-stub role pane, which sends no hooks of its own.
///
/// PRD #393 issue #395 follow-up: both `pane_id` AND `agent_id` must be the
/// worker's REAL values (`worker_agent_record`'s `pane_id_env` / `id`).
/// `pane_id` alone is not enough — `AppState::apply_event`'s same-pane reuse
/// guard (`src/state.rs`) only updates the pane's existing placeholder
/// session in place when `session.agent_id == event.agent_id`; an event
/// carrying `agent_id: None` fails that guard (and the immediately-following
/// retire block explicitly skips a `None`-agent_id event too, PRD #110's
/// backward-compat carve-out), so it falls through and creates a SECOND,
/// disconnected session on the same `pane_id` instead of updating the real
/// one. `build_pane_status` (`src/ui.rs`) then joins `state.sessions` into a
/// `pane_id`-keyed `HashMap` with no dedupe, so which of the two sessions'
/// status "wins" for that pane is UNSPECIFIED — the gate's carve-out check
/// can silently see the stale placeholder instead of the fresh status.
///
/// Blocks not on the daemon's broadcast (which fires whether or not
/// `apply_event` actually accepted/applied the event onto the right
/// session — a wrong pane id or a rejected event sails through it exactly
/// the same as an accepted one) but on `ListAgents`' `AgentRecord.live`
/// join reporting the expected `SessionStatus` back for the worker's pane —
/// proof the daemon's OWN state, not just its wire, reflects the change.
#[cfg(unix)]
fn inject_worker_status(
    deck: &TuiDeck,
    socket: &std::path::Path,
    pane_id: &str,
    agent_id: &str,
    session_id: &str,
    event_type: EventType,
) {
    let expected_status = match event_type {
        EventType::WaitingForInput => dot_agent_deck::state::SessionStatus::WaitingForInput,
        EventType::Thinking => dot_agent_deck::state::SessionStatus::Thinking,
        other => {
            panic!("inject_worker_status: no expected SessionStatus mapping wired up for {other:?}")
        }
    };
    let event = AgentEvent {
        session_id: session_id.to_string(),
        agent_type: AgentType::Pi,
        event_type: event_type.clone(),
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
    };
    let line = serde_json::to_string(&event).expect("serialize synthetic AgentEvent");
    common::write_hook_line(deck.hook_socket_path(), &line)
        .expect("inject synthetic AgentEvent over hook socket");

    let applied = common::wait_until(Duration::from_secs(10), || {
        common::agent_records_on(socket).into_iter().any(|r| {
            r.pane_id_env.as_deref() == Some(pane_id)
                && r.live.as_ref().map(|s| &s.status) == Some(&expected_status)
        })
    });
    assert!(
        applied,
        "the daemon's own ListAgents/live-status join never reported {event_type:?} \
         for the worker pane {pane_id} (agent_id {agent_id}) within 10s — the hook \
         socket write was accepted (see above), but AppState::apply_event may have \
         rejected it or applied it to the wrong session; the pane's redraw below \
         cannot be trusted to reflect it either.",
    );
}

/// Scenario: PRD #393 M3 (decision 4), the real-PTY proof — deliberately
/// NOT a real-agent test. The PRD's test plan calls this "the strongest
/// candidate for a real-agent (Haiku) test", but a real worker self-skips
/// in CI for missing credentials (see `orchestration/lock/006`, which
/// "passes" in ~0.1s having executed nothing there) — that would give ZERO
/// automated coverage of M3's headline behavior on this fork's CI, which is
/// worse than a stand-in that actually runs. Open a real `orch-deck`
/// orchestration (LOCKED, the default), focus the non-orchestrator "worker"
/// role, and confirm a keystroke is dropped as usual (the ordinary
/// `orchestration/lock/004` baseline). Inject a synthetic `AgentEvent`
/// reporting the worker's pane `WaitingForInput` over the hook socket — the
/// same wire the real `agent-event` CLI rides — and confirm the SAME kind
/// of keystroke now reaches the worker's PTY and echoes on the grid.
/// Inject `Thinking` (status clears, as if the agent resumed after being
/// answered), re-focus the worker explicitly (isolating this from PRD
/// #373/#393 M4's SEPARATE all-clear auto-focus, already covered by
/// `tabs/orchestration/025`), and confirm a further keystroke is dropped
/// again — the gate re-engages the instant the carve-out's condition stops
/// holding.
#[spec("orchestration/lock/010")]
#[test]
fn orchestration_lock_010_waiting_carve_out_on_real_panes() {
    const WORKER_LOCKED_SENTINEL: &str = "LOCK010_LOCKED_4b7e";
    const WORKER_WAITING_SENTINEL: &str = "LOCK010_WAITING_9c2f";
    const WORKER_RELOCKED_SENTINEL: &str = "LOCK010_RELOCKED_e814";

    let deck = TuiDeck::builder()
        .with_pty_size(120, 40)
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("No active sessions");

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent"); // new-pane form closed -> tab up, orchestrator focused

    let socket = deck.attach_socket_path().to_path_buf();
    let worker_record = worker_agent_record(&socket);
    let worker_id = worker_record.id.clone();
    let worker_pane_id = worker_record
        .pane_id_env
        .clone()
        .expect("worker role pane must have a DOT_AGENT_DECK_PANE_ID recorded");
    let session_id = format!("{worker_id}-lock010-session");

    // Focus the non-orchestrator "worker" role. The tab is still locked —
    // nothing has toggled it yet.
    focus_worker_role(&deck);

    // Baseline: locked, no status ever reported for the worker's pane —
    // dropped, the ordinary orchestration/lock/004 behavior.
    deck.send_keys(format!("{WORKER_LOCKED_SENTINEL}\r").as_bytes());
    let leaked = deck.wait_for_grid_string_within(WORKER_LOCKED_SENTINEL, Duration::from_secs(2));
    assert!(
        !leaked,
        "a keystroke into the locked worker pane reached its PTY before any \
         WaitingForInput status was ever reported for it — expected the \
         ordinary orchestration/lock/004 baseline (dropped).\nGrid:\n{}",
        deck.snapshot_grid()
    );

    // The worker reports WaitingForInput.
    inject_worker_status(
        &deck,
        &socket,
        &worker_pane_id,
        &worker_id,
        &session_id,
        EventType::WaitingForInput,
    );

    // Locked but WaitingForInput: the carve-out opens, and the keystroke
    // must reach the PTY and echo. `send_keys_until_grid_string_within`
    // retries the SEND (Decision 21's helper, not a bare sleep) because the
    // status just arrived over an async daemon round-trip with no
    // in-process signal this test can await instead of the grid itself —
    // issue #395's exact race, guarded against rather than reproduced.
    assert!(
        deck.send_keys_until_grid_string_within(
            format!("{WORKER_WAITING_SENTINEL}\r").as_bytes(),
            WORKER_WAITING_SENTINEL,
            Duration::from_secs(10),
        ),
        "a keystroke into the worker pane never reached its PTY after it \
         reported WaitingForInput while the orchestration tab's \
         command-entry lock was engaged — expected PRD #393 decision 4's \
         carve-out to pass it through.\nGrid:\n{}",
        deck.snapshot_grid()
    );

    // The status clears (the agent resumes, as if it just got answered) —
    // the gate must re-engage instantly.
    inject_worker_status(
        &deck,
        &socket,
        &worker_pane_id,
        &worker_id,
        &session_id,
        EventType::Thinking,
    );

    // Re-focus explicitly: this isolates the LOCK's own re-engagement from
    // PRD #373/#393 M4's SEPARATE all-clear auto-focus, which may have
    // already steered focus back to the orchestrator once nothing was left
    // waiting (already covered by `tabs/orchestration/025`) — this test
    // must not ride that as an accidental proxy for the lock re-engaging.
    focus_worker_role(&deck);
    deck.send_keys(format!("{WORKER_RELOCKED_SENTINEL}\r").as_bytes());
    let leaked = deck.wait_for_grid_string_within(WORKER_RELOCKED_SENTINEL, Duration::from_secs(2));
    assert!(
        !leaked,
        "a keystroke into the worker pane reached its PTY after its status \
         cleared from WaitingForInput back to Thinking — expected the \
         command-entry lock to re-engage the instant the carve-out's \
         condition stopped holding.\nGrid:\n{}",
        deck.snapshot_grid()
    );
}
