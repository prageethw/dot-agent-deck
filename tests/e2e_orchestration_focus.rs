#![cfg(feature = "e2e")]

//! L2 end-to-end coverage for PRD #393 M5: the real-binary proof of the
//! lock-governed focus contract, the CLAUDE.md rule 4 headline test for this
//! PRD. `orchestration/lock/*` (`e2e_orchestration_lock.rs`) and
//! `tabs/orchestration/010`/`012`/`020`/`025`/`026` (`src/tab.rs`) each pin
//! one mechanism in isolation — the keystroke gate, the auto-focus chain, the
//! lock-gated call site's `TabManager`-level contract. Nothing before this
//! test drives all of it together, end to end, in a real pane, the way a
//! human actually experiences it: locked by default, a worker visibly pulling
//! focus when it needs the human and visibly releasing it once resolved,
//! `Ctrl+e` handing control back, and a manual focus choice actually sticking
//! once it does.
//!
//! `orchestration_focus_001` — the PRD #373 M2 inactivity snap-back's PTY
//! proof — was deleted outright at M4 along with the production behavior it
//! covered (see `fa651ac`); this file and the `orchestration/focus` catalog
//! section did not survive that deletion. `focus_002` re-establishes both,
//! covering the DIFFERENT (successor) behavior M4/M4b/M5 actually shipped.
//!
//! Uses the `orch-focus-lifecycle` fixture (`orchestrator` start role plus
//! `alpha` and `beta`, all plain `printf`+`sleep` stubs — no LLM tokens
//! spent), the same 3-role fixture `tabs/orchestration/008`
//! (`e2e_orchestration_pane_column.rs`) opens, because the "manual focus
//! sticks" half needs a role OTHER than the one going `WaitingForInput` to
//! prove the human's choice survives — a 2-role fixture where the focused
//! role and the waiting role are the same pane can't tell a genuine stick
//! apart from `auto_focus_waiting_pane`'s own no-flicker same-pane no-op.
//! `write_beta_agent` (that other file's runtime patch making `beta`
//! self-post real hooks) is deliberately NOT used here — this test drives
//! `WaitingForInput` synthetically over the hook socket instead, exactly as
//! `orchestration/lock/010` does, so `beta`'s checked-in placeholder script
//! (a plain sentinel-then-sleep, no different from `alpha`'s) is fine as is.
//!
//! `open_orchestration` and `expanded_header` mirror
//! `e2e_orchestration_pane_column.rs`'s `open_focus_lifecycle_orchestration`
//! and the deleted `orchestration_focus_001`'s helpers of the same shape;
//! `role_agent_record` and `inject_role_status` mirror
//! `e2e_orchestration_lock.rs`'s `worker_agent_record`/`inject_worker_status`
//! (generalized from a single hardcoded "worker" role name to any role in the
//! fixture) rather than reinventing the `pane_id_env` + `agent_id`
//! reuse-guard fix issue #395 cost that file a real bug for.
//!
//! Decision 6: gated behind the `e2e` feature so `cargo test-fast` never
//! compiles it.

mod common;

use std::time::Duration;

use common::TuiDeck;
use dot_agent_deck::event::{AgentEvent, AgentType, EventType};
use spec::spec;

/// Drive the new-pane dialog to open the (single) orchestration in the
/// `orch-focus-lifecycle` fixture. Mirrors
/// `e2e_orchestration_pane_column.rs::open_focus_lifecycle_orchestration`'s
/// shape (renamed locally since this file has no other `open_orchestration`
/// to collide with, unlike `e2e_orchestration_lock.rs`'s own same-named
/// helper for the 2-role `orch-deck` fixture) —
/// with no `[[modes]]` defined the Mode chip row is `[No mode] [Orch:
/// focus-lifecycle] [schedule]`, so ONE Right selects the orchestration;
/// selecting an orchestration hides the Command field, so a second Enter
/// submits the form. Lands with the orchestrator (start) role focused in
/// `PaneInput` mode, the tab's default LOCKED state untouched.
fn open_orchestration(deck: &TuiDeck) {
    deck.send_bytes(b"\x0e"); // Ctrl+n -> directory picker
    deck.send_bytes(b" "); // Space -> confirm current dir -> new-pane form
    deck.wait_for_string("No mode"); // form up, Mode field focused at "No mode"
    deck.send_bytes(b"\x1b[C"); // Right -> [Orch: focus-lifecycle]
    deck.send_bytes(b"\r"); // Mode -> Name
    deck.send_bytes(b"\r"); // submit (Command hidden for an orchestration)
}

/// The rendered grid's expanded-pane top border fuses the pane's title
/// directly into the box-drawing corner as `┌<role>` in `PaneInput` mode
/// (`TerminalWidget`, `src/terminal_widget.rs`; the precedent needle is
/// `tests/common/mod.rs::find_pane_box_left_edge`'s `plain_needle`, reused
/// verbatim by the deleted `orchestration_focus_001`). Only the currently
/// focused role ever renders this way — every other role collapses to a
/// small numbered card with no live PTY body (`orchestration/layout/004`) —
/// so this string's presence on the settled grid names which role currently
/// holds focus with live content on screen: the observable this whole test
/// is built on, per CLAUDE.md rule 4's "assert on the rendered grid" bar.
fn expanded_header(role: &str) -> String {
    format!("┌{role}")
}

/// The `orch-focus-lifecycle` fixture's full daemon registry record for
/// `role`. Mirrors `e2e_orchestration_lock.rs::worker_agent_record`,
/// generalized from a single hardcoded "worker" role name to any role name in
/// this 3-role fixture, since `focus_002` needs to target `alpha` and `beta`
/// independently.
fn role_agent_record(
    socket: &std::path::Path,
    role: &str,
) -> dot_agent_deck::agent_pty::AgentRecord {
    common::agent_records_on(socket)
        .into_iter()
        .find(|r| {
            matches!(
                &r.tab_membership,
                Some(dot_agent_deck::agent_pty::TabMembership::Orchestration { role_name, .. })
                    if role_name == role
            )
        })
        .unwrap_or_else(|| {
            panic!(
                "orch-focus-lifecycle fixture's {role} role pane must be registered with the daemon"
            )
        })
}

/// Inject a synthetic `AgentEvent` for `role`'s real `(pane_id_env,
/// agent_id)` pair over the deck's hook socket — the SAME bare-`AgentEvent`,
/// no-`DaemonMessage`-envelope wire the real `dot-agent-deck agent-event
/// --type running|waiting|finished` CLI already rides for Pi's status
/// reporting. Mirrors `e2e_orchestration_lock.rs::inject_worker_status`
/// (generalized to any role) rather than writing a second injector — that
/// file's version already carries the issue #395 fix (both `pane_id` AND
/// `agent_id` set to the role's REAL values, not just `pane_id`, so
/// `AppState::apply_event`'s same-pane reuse guard updates the pane's
/// existing session in place instead of forking a second, disconnected one
/// on the same `pane_id` whose status then races the real one for which
/// wins `build_pane_status`'s join).
///
/// Blocks not on the daemon's broadcast (fires whether or not `apply_event`
/// actually accepted the event) but on `ListAgents`' `AgentRecord.live` join
/// reporting the expected `SessionStatus` back for `role`'s pane — proof the
/// daemon's OWN state, not just its wire, reflects the change before the
/// caller starts asserting on focus movement driven by it.
#[cfg(unix)]
fn inject_role_status(
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
            panic!("inject_role_status: no expected SessionStatus mapping wired up for {other:?}")
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
         for pane {pane_id} (agent_id {agent_id}) within 10s — the hook socket write \
         was accepted, but AppState::apply_event may have rejected it or applied it \
         to the wrong session.",
    );
}

/// Scenario: PRD #393 M5, the CLAUDE.md rule 4 headline test for this PRD —
/// the full lock-governed focus contract in one real Orchestration tab, as a
/// user experiences it, asserted purely on the rendered grid. (1) A freshly
/// opened tab (LOCKED, the default) shows the orchestrator role's expanded
/// pane box. (2) Injecting `WaitingForInput` for the non-orchestrator `alpha`
/// role visibly steers focus onto ITS expanded box. (3) Injecting `Thinking`
/// for `alpha` (status clears) visibly returns focus to the orchestrator's
/// expanded box — the all-clear edge. (4) `Ctrl+d` then `Ctrl+e` unlocks,
/// surfacing the `Pane entry: unlocked` status message. (5) With the tab now
/// unlocked, manually focusing the OTHER non-orchestrator role (`beta`, not
/// `alpha` — so the fresh waiting episode below is a genuine steer-attempt
/// rather than a same-pane no-op `auto_focus_waiting_pane` would no-op on
/// regardless) and then injecting a fresh `WaitingForInput`/`Thinking` pair
/// for `alpha` moves focus nowhere: `beta`'s expanded box, and a sentinel
/// typed into it, survive both the waiting pane appearing and its all-clear
/// — because while unlocked `src/ui.rs`'s per-frame call site
/// (`if ui.command_entry_locked { observe_waiting_panes(...); ... }`) never
/// even calls `observe_waiting_panes`, so there is no auto-focus branch left
/// to fight the human's manual choice.
#[spec("orchestration/focus/002")]
#[test]
fn orchestration_focus_002_lock_governed_focus_contract_on_real_binary() {
    const BETA_STICK_SENTINEL: &str = "FOCUS002_BETA_STICK_6d4e";

    let deck = TuiDeck::builder()
        .with_pty_size(160, 45)
        .launch_with_fixture("orch-focus-lifecycle");
    deck.wait_for_string("No active sessions");

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent"); // new-pane form closed -> tab up, orchestrator focused

    let socket = deck.attach_socket_path().to_path_buf();
    let alpha_record = role_agent_record(&socket, "alpha");
    let alpha_id = alpha_record.id.clone();
    let alpha_pane_id = alpha_record
        .pane_id_env
        .clone()
        .expect("alpha role pane must have a DOT_AGENT_DECK_PANE_ID recorded");
    let alpha_session_id = format!("{alpha_id}-focus002-session");

    // --- Step 1: LOCKED by default, focus sits on the orchestrator pane. ---
    let orch_expanded = expanded_header("orchestrator");
    let alpha_expanded = expanded_header("alpha");
    let beta_expanded = expanded_header("beta");
    assert!(
        deck.wait_for_grid_predicate_within(Duration::from_secs(5), |grid| {
            grid.contains(&orch_expanded)
        }),
        "a freshly opened orchestration tab never showed the orchestrator \
         role's expanded pane box (needle {orch_expanded:?}) — expected the \
         default LOCKED state to leave focus on the orchestrator.\n\
         === rendered grid ===\n{}",
        deck.snapshot_grid()
    );

    // --- Step 2: alpha goes WaitingForInput and visibly pulls focus. ---
    inject_role_status(
        &deck,
        &socket,
        &alpha_pane_id,
        &alpha_id,
        &alpha_session_id,
        EventType::WaitingForInput,
    );
    assert!(
        deck.wait_for_grid_predicate_within(Duration::from_secs(10), |grid| {
            grid.contains(&alpha_expanded)
        }),
        "injecting WaitingForInput for the non-orchestrator alpha role never \
         steered focus onto its expanded pane box (needle {alpha_expanded:?}) \
         — expected the locked tab's auto-focus chain to pull focus onto a \
         waiting role.\n\
         === rendered grid ===\n{}",
        deck.snapshot_grid()
    );

    // --- Step 3: alpha resolves; focus returns to the orchestrator on the
    // all-clear edge. ---
    inject_role_status(
        &deck,
        &socket,
        &alpha_pane_id,
        &alpha_id,
        &alpha_session_id,
        EventType::Thinking,
    );
    assert!(
        deck.wait_for_grid_predicate_within(Duration::from_secs(10), |grid| {
            grid.contains(&orch_expanded)
        }),
        "alpha's status clearing from WaitingForInput never returned focus \
         to the orchestrator's expanded pane box (needle {orch_expanded:?}) \
         — expected the locked tab's all-clear edge to fire.\n\
         === rendered grid ===\n{}",
        deck.snapshot_grid()
    );

    // --- Step 4: Ctrl+d then Ctrl+e unlocks. ---
    deck.send_bytes(b"\x04"); // Ctrl+d -> Normal (command) mode
    deck.send_bytes(b"\x05"); // Ctrl+e -> Action::ToggleOrchestrationLock
    assert!(
        deck.wait_for_grid_predicate_within(Duration::from_secs(5), |grid| {
            grid.contains("Pane entry: unlocked")
        }),
        "Ctrl+d then Ctrl+e never surfaced the 'Pane entry: unlocked' \
         status message — expected the command-mode chord to flip \
         ui.command_entry_locked to unlocked.\n\
         === rendered grid ===\n{}",
        deck.snapshot_grid()
    );

    // --- Step 5: manual focus on beta (a NON-orchestrator role, deliberately
    // NOT alpha — see the doc comment) sticks across both a fresh waiting
    // episode and its all-clear, because unlocked runs no auto-focus branch
    // at all. Still in Normal mode from the Ctrl+e above: digit '3' ->
    // Jump3 -> Action::FocusCard(2) -> role_pane_ids[2] == beta.
    // `focus_deck` re-enters PaneInput mode on success, so no extra Ctrl+d
    // is needed before the sentinel below.
    deck.send_bytes(b"3");
    assert!(
        deck.wait_for_grid_predicate_within(Duration::from_secs(5), |grid| {
            grid.contains(&beta_expanded)
        }),
        "manually jumping to the beta role (digit '3') never showed its \
         expanded pane box (needle {beta_expanded:?}) — cannot proceed with \
         the manual-focus-sticks assertion below without confirming the \
         initial focus target first.\n\
         === rendered grid ===\n{}",
        deck.snapshot_grid()
    );

    // A fresh waiting episode on alpha — a DIFFERENT role than the one
    // manually focused. Under the LOCKED contract (steps 1-3 above) this
    // would steer focus onto alpha; unlocked, it must not.
    inject_role_status(
        &deck,
        &socket,
        &alpha_pane_id,
        &alpha_id,
        &alpha_session_id,
        EventType::WaitingForInput,
    );
    // No in-process signal for "the deck observed the event and chose not to
    // move focus" — poll for a bounded stretch and confirm beta's expanded
    // box is what's there throughout, exactly as `wait_for_grid_predicate_within`
    // is used as a bounded no-op wait in the deleted `orchestration_focus_001`.
    let alpha_stole_focus = deck.wait_for_grid_predicate_within(Duration::from_secs(3), |grid| {
        grid.contains(&alpha_expanded)
    });
    assert!(
        !alpha_stole_focus,
        "while UNLOCKED, injecting WaitingForInput for alpha steered focus \
         onto its expanded pane box (needle {alpha_expanded:?}) anyway — \
         expected the unlocked tab to run no auto-focus branch at all, \
         leaving the human's manual focus on beta untouched.\n\
         === rendered grid ===\n{}",
        deck.snapshot_grid()
    );
    assert!(
        deck.snapshot_grid().contains(&beta_expanded),
        "beta's expanded pane box was gone after a waiting episode arrived \
         on alpha while unlocked — manual focus must survive untouched.\n\
         === rendered grid ===\n{}",
        deck.snapshot_grid()
    );

    // alpha resolves; under the LOCKED contract this would fire the
    // all-clear move back to the orchestrator. Unlocked, it must not either
    // — beta stays focused.
    inject_role_status(
        &deck,
        &socket,
        &alpha_pane_id,
        &alpha_id,
        &alpha_session_id,
        EventType::Thinking,
    );
    let snapped_to_orchestrator = deck
        .wait_for_grid_predicate_within(Duration::from_secs(3), |grid| {
            grid.contains(&orch_expanded)
        });
    assert!(
        !snapped_to_orchestrator,
        "while UNLOCKED, alpha's status clearing fired an all-clear move \
         back to the orchestrator's expanded pane box (needle \
         {orch_expanded:?}) anyway — expected no auto-focus branch to run \
         at all while unlocked.\n\
         === rendered grid ===\n{}",
        deck.snapshot_grid()
    );

    // The final proof, not just that beta's box is still drawn but that it
    // genuinely still holds live PTY focus: a keystroke typed now must
    // appear inside beta's own expanded box.
    deck.send_keys(format!("{BETA_STICK_SENTINEL}\r").as_bytes());
    assert!(
        deck.wait_for_grid_predicate_within(Duration::from_secs(5), |grid| {
            grid.contains(&beta_expanded) && grid.contains(BETA_STICK_SENTINEL)
        }),
        "after surviving both a waiting episode and its all-clear while \
         unlocked, a keystroke typed at the end never showed up inside \
         beta's own expanded pane box — manual focus must have stuck all \
         the way through.\n\
         === rendered grid ===\n{}",
        deck.snapshot_grid()
    );
}
