#![cfg(feature = "e2e")]

//! L2 end-to-end coverage for the command-entry lock on Orchestration tabs.
//!
//! The lock is unconditional (fork #346 graduated it out from behind the
//! `experimental` flag): every deck locks a non-orchestrator role pane's
//! input by default, with no flag and no project config required anywhere.
//! `orchestration/lock/015`/`016` pin that with NO `.dot-agent-deck.toml`
//! discoverable anywhere (via `DOT_AGENT_DECK_FEATURES_CONFIG` pointed at a
//! path that provably does not exist, forcing the exact "missing file"
//! resolution branch a real deck launched from a config-less home directory
//! hits), the lock still holds and `Ctrl+e` still resolves — the permanent
//! anti-regression property that a config-less deck is never silently
//! unlocked.
//!
//! A keystroke typed while a non-orchestrator role pane is focused
//! must not reach that pane's PTY; the orchestrator pane's own input is never
//! gated; `Ctrl+d` then `Ctrl+e` toggles the lock; a pane reporting
//! `WaitingForInput` is not gated at all; and the always-available
//! `Ctrl+`-chords (resolved before the PTY-forward fallback the lock gates)
//! keep working regardless of lock state or which pane is focused.
//!
//! `orchestration_lock_008`/`009`/`010`/`011` use the `orch-deck` fixture (two
//! stub `cat` roles, no LLM tokens spent); `009` observes `Ctrl+e` reaching a
//! PTY through the tty's own `^E` caret echo, which asks nothing of the program
//! occupying the pane. `orchestration_lock_012` uses `orch-lock-live`, whose
//! worker role is a REAL interactive Claude Haiku agent, so the same gate is
//! proven against a genuine agent's input rather than a `cat` stub's echo — it
//! self-skips where no credentials are configured.
//!
//! Gated behind the `e2e` feature so `cargo test-fast` never compiles it.

mod common;

use std::time::Duration;

use common::TuiDeck;
use dot_agent_deck::event::{AgentEvent, AgentType, EventType};
use spec::spec;

/// Drive the new-pane dialog to open the (single) orchestration in the
/// `orch-deck` / `orch-lock-*` fixtures. Mirrors
/// `e2e_orchestration_pane_column.rs::open_orchestration` — with no
/// `[[modes]]` defined the Mode chip row is `[No mode] [Orch: …] [schedule]`,
/// so ONE Right selects the orchestration; selecting an orchestration hides
/// the Command field, so a second Enter submits the form. Lands with the
/// orchestrator (start) role focused in `PaneInput` mode.
fn open_orchestration(deck: &TuiDeck) {
    deck.send_keys(b"\x0e"); // Ctrl+n -> directory picker
    deck.send_keys(b" "); // Space -> confirm current dir -> new-pane form
    deck.wait_for_string("No mode"); // form up, Mode field focused at "No mode"
    deck.send_keys(b"\x1b[C"); // Right -> [Orch: …]
    deck.send_keys(b"\r"); // Mode -> Name
    deck.send_keys(b"\r"); // submit (Command hidden for an orchestration)
}

/// Switch focus from the orchestrator role to the fixture's second role
/// ("worker", `role_pane_ids` index 1): Ctrl+D back to Normal mode, then digit
/// `2` (`Jump2` -> `Action::FocusCard(1)`) — the same mechanism
/// `focus/orchestration/001` pins for "1-9 on an orchestration tab jumps to
/// role pane N and focuses it". `focus_deck` re-enters `PaneInput` mode on
/// success, so no separate Enter is needed.
fn focus_worker_role(deck: &TuiDeck) {
    deck.send_bytes(b"\x04"); // Ctrl+D -> Normal mode
    deck.send_keys(b"2"); // Jump2 -> focus role index 1 ("worker")
}

/// Scenario: Open a real orchestration tab (default LOCKED) and confirm the
/// focused orchestrator pane's own input is never gated, while a keystroke
/// aimed at the non-orchestrator "worker" role does not reach its PTY and
/// surfaces the corrected `Ctrl+d`, `Ctrl+e`, `Ctrl+d` unlock hint (issue
/// #302 defect 2 — the old wording stopped one keypress short). Enter
/// command mode (`Ctrl+d`) and send `Ctrl+e` to unlock — the chord only
/// resolves in command mode — then `Ctrl+d` back into `PaneInput` and confirm
/// a keystroke into the still-focused worker pane now forwards and echoes.
#[spec("orchestration/lock/008")]
#[test]
fn lock_008_forwarding_gated_by_lock_state() {
    const ORCH_SENTINEL: &str = "LOCK008_ORCH_9f21";
    const WORKER_LOCKED_SENTINEL: &str = "LOCK008_WORKER_LOCKED_7ac4";
    const WORKER_UNLOCKED_SENTINEL: &str = "LOCK008_WORKER_UNLOCKED_c83e";

    let deck = TuiDeck::builder()
        .with_env("DOT_AGENT_DECK_EXPERIMENTAL", "1")
        .with_pty_size(120, 40)
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("No active sessions");

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent"); // form closed -> tab up, orchestrator focused

    // The orchestrator pane is NEVER gated: even though the deck starts LOCKED
    // by default, typing into the currently-focused orchestrator role must
    // still reach its PTY.
    deck.send_keys(format!("{ORCH_SENTINEL}\r").as_bytes());
    deck.wait_for_string(ORCH_SENTINEL);

    // Focus the non-orchestrator "worker" role. Still locked — nothing has
    // toggled it yet.
    focus_worker_role(&deck);

    // Locked: a keystroke aimed at the worker pane must NOT reach its PTY.
    deck.send_keys(format!("{WORKER_LOCKED_SENTINEL}\r").as_bytes());
    let leaked = deck.wait_for_grid_string_within(WORKER_LOCKED_SENTINEL, Duration::from_secs(2));
    assert!(
        !leaked,
        "a keystroke typed into the non-orchestrator worker pane reached its \
         PTY while the command-entry lock was engaged (the default state) — \
         expected it to be dropped before Action::ForwardToPane.\nGrid:\n{}",
        deck.snapshot_grid()
    );

    // Issue #302 defect 2: the dropped keystroke's status message must name
    // the FULL three-chord round trip. The old wording ("Ctrl+d then Ctrl+e
    // to unlock") was literally true but left the reader in command mode —
    // anyone following it exactly unlocks and then finds their typing going
    // to the deck rather than the pane. This is the wording the upstream
    // maintainer suggested in #445.
    assert!(
        deck.snapshot_grid()
            .contains("Pane locked — Ctrl+d, Ctrl+e, Ctrl+d to type here"),
        "the dropped-keystroke status message did not carry the corrected \
         unlock hint — expected `Pane locked — Ctrl+d, Ctrl+e, Ctrl+d to type \
         here`, naming the full round trip back into the pane, not the old \
         `Ctrl+d then Ctrl+e to unlock` which stops one keypress short.\n\
         Grid:\n{}",
        deck.snapshot_grid()
    );

    // Ctrl+e only resolves in command mode: Ctrl+d into command mode, Ctrl+e
    // to unlock, then Ctrl+d back into PaneInput so the sentinel below
    // actually reaches the worker pane's PTY.
    deck.send_bytes(b"\x04"); // Ctrl+d -> command mode
    deck.send_bytes(b"\x05"); // Ctrl+e == 0x05 -> unlock
    deck.send_bytes(b"\x04"); // Ctrl+d -> back to PaneInput

    // Unlocked: typing into the still-focused worker pane must now forward.
    deck.send_keys(format!("{WORKER_UNLOCKED_SENTINEL}\r").as_bytes());
    deck.wait_for_string(WORKER_UNLOCKED_SENTINEL);
}

/// Scenario: The real-pane proof that `Ctrl+e` is claimed only in command
/// mode. On a real Orchestration tab (`orch-deck` fixture, two `cat` stub
/// roles) with the orchestrator pane focused and the deck typing into it: type
/// a partial line, send `Ctrl+e` (`0x05`), and confirm a literal `^E` lands in
/// that pane — the tty's own caret echo, which proves the byte reached the PTY
/// rather than being claimed as `Action::ToggleOrchestrationLock`. Then press
/// `Ctrl+d` to reach command mode and send `0x05` again: no second `^E` may
/// appear (the chord is claimed there), the deck must report `Pane entry:
/// unlocked`, and jumping to the non-orchestrator worker role must then let a
/// keystroke reach its PTY — proving the same chord still toggles the lock
/// from the one mode it IS claimed in.
#[spec("orchestration/lock/009")]
#[test]
fn lock_009_ctrl_e_scoped_to_command_mode_on_real_panes() {
    const PARTIAL_LINE: &str = "LOCK009_PARTIAL_f3d1";
    const WORKER_UNLOCKED_SENTINEL: &str = "LOCK009_WORKER_UNLOCKED_7be2";

    let deck = TuiDeck::builder()
        .with_env("DOT_AGENT_DECK_EXPERIMENTAL", "1")
        .with_pty_size(120, 40)
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("No active sessions");

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent"); // form closed -> tab up, orchestrator focused
    deck.wait_for_string("[Command Mode Ctrl+D]"); // live PTY, PaneInput mode

    // --- Part 1: in PaneInput the chord must reach the PTY, observed in the
    // orchestrator's own pane (never gated by the lock, so this isolates the
    // assertion from lock state entirely). ---

    // The oracle is the tty line discipline's caret echo (`ECHOCTL`), NOT
    // readline: a control byte delivered to a pane echoes as two literal
    // characters, `^E`. That is deliberately a property of the terminal rather
    // than of whatever program occupies the pane — an earlier revision of this
    // test drove a real `bash --noprofile --norc -i` role and asserted
    // readline's `beginning-of-line`/`end-of-line` cursor moves, which fails
    // outright wherever bash is built without readline (this repo's own devbox
    // bash reports no `emacs` option at all, so `Ctrl+a` echoed `^A` and moved
    // the cursor two columns the WRONG way). What this test needs to prove is
    // only that `0x05` was forwarded rather than swallowed; the caret echo
    // shows exactly that, everywhere, and matches what `orchestration/lock/008`
    // already relies on for ordinary characters.
    deck.send_keys(PARTIAL_LINE.as_bytes()); // no trailing \r -- never submitted
    assert!(
        deck.wait_for_grid_string_within(PARTIAL_LINE, Duration::from_secs(3)),
        "the partial line never appeared on the rendered grid\nGrid:\n{}",
        deck.snapshot_grid()
    );

    // Anchored to the partial line so this cannot match a stray `^E` painted
    // anywhere else on the grid.
    let echoed = format!("{PARTIAL_LINE}^E");
    deck.send_bytes(b"\x05");
    assert!(
        deck.wait_for_grid_string_within(&echoed, Duration::from_secs(3)),
        "Ctrl+e did not reach the focused orchestrator role pane's PTY — the \
         tty never echoed `^E` after {PARTIAL_LINE}. The global keybinding \
         resolver claimed 0x05 as Action::ToggleOrchestrationLock even though a \
         role pane was focused in PaneInput mode.\nGrid:\n{}",
        deck.snapshot_grid()
    );

    // --- Part 2: from command mode the same chord must be claimed by the deck
    // instead, and must actually toggle the lock. ---

    deck.send_bytes(b"\x04"); // Ctrl+d -> Normal (command) mode
    deck.send_bytes(b"\x05"); // Ctrl+e -> Action::ToggleOrchestrationLock

    // The deck reports the toggle. Waiting on this also sequences the rest of
    // the test behind the mode switch actually having been applied.
    assert!(
        deck.wait_for_grid_string_within("Pane entry: unlocked", Duration::from_secs(3)),
        "Ctrl+e from command mode did not toggle the command-entry lock — the \
         deck never reported `Pane entry: unlocked`.\nGrid:\n{}",
        deck.snapshot_grid()
    );
    // The mirror of Part 1: claimed here means NOT forwarded, so no second
    // caret may have joined the first.
    assert!(
        !deck.snapshot_grid().contains(&format!("{echoed}^E")),
        "Ctrl+e in command mode was ALSO forwarded to the orchestrator pane's \
         PTY — a second `^E` echoed after {echoed}. The chord must be claimed \
         by the deck in command mode, not delivered to the pane.\nGrid:\n{}",
        deck.snapshot_grid()
    );

    // Jump straight to the worker from command mode. Deliberately NOT
    // `focus_worker_role`, which opens with its own `Ctrl+d`: that helper
    // assumes it is called from PaneInput, and `Ctrl+d` is a TOGGLE, so using
    // it here would drop back INTO the pane and type the `2` at the
    // orchestrator instead of jumping. The sentinel would then land in the
    // orchestrator's own never-gated pane and this test would pass without the
    // lock having been consulted at all.
    deck.send_keys(b"2"); // Jump2 -> focus role index 1 ("worker")
    deck.send_keys(format!("{WORKER_UNLOCKED_SENTINEL}\r").as_bytes());
    assert!(
        deck.wait_for_grid_string_within(WORKER_UNLOCKED_SENTINEL, Duration::from_secs(3)),
        "after Ctrl+d then Ctrl+e from command mode, a keystroke typed into the \
         non-orchestrator worker pane never reached its PTY — expected the \
         command-mode Ctrl+e to have toggled the command-entry lock from its \
         default LOCKED state to unlocked.\nGrid:\n{}",
        deck.snapshot_grid()
    );
}

/// Scenario: Open a real orchestration tab, focus the non-orchestrator
/// "worker" role while the deck is LOCKED, then press `Ctrl+t`
/// (`toggle_layout`) and confirm it still fires and surfaces its `Layout: …`
/// status message — global chords resolve before the PTY-forward fallback the
/// lock gates. Regression guard against an overly-broad gate implementation.
#[spec("orchestration/lock/010")]
#[test]
fn lock_010_global_chord_unaffected_by_lock_state() {
    let deck = TuiDeck::builder()
        .with_env("DOT_AGENT_DECK_EXPERIMENTAL", "1")
        .with_pty_size(120, 40)
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("No active sessions");

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent");

    // Focus the non-orchestrator worker role — still LOCKED, the default.
    focus_worker_role(&deck);

    deck.send_bytes(b"\x14"); // Ctrl+t (toggle_layout)
    deck.wait_for_string("Layout:");
}

/// The `orch-deck` / `orch-lock-live` fixtures' worker role pane's full daemon
/// registry record. `AgentRecord.id` (the registry's own monotonic counter)
/// and `AgentRecord.pane_id_env` (the `DOT_AGENT_DECK_PANE_ID` the pane was
/// spawned with) are two DISTINCT fields. Anything that means "the pane" as
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
        .expect("the fixture's worker role pane must be registered with the daemon")
}

/// Inject a synthetic `AgentEvent` for the worker's real `(pane_id_env,
/// agent_id)` pair over the deck's hook socket — the SAME bare-`AgentEvent`,
/// no-`DaemonMessage`-envelope wire the real `dot-agent-deck agent-event
/// --type running|waiting|finished` CLI already rides for status reporting
/// (`src/main.rs`'s `AgentEvent` command, `src/daemon.rs::run_hook_loop`'s
/// `serde_json::from_str::<AgentEvent>` fallback). Stands in for a real
/// extension's status report against a `cat`-stub role pane, which sends no
/// hooks of its own.
///
/// Both `pane_id` AND `agent_id` must be the worker's REAL values
/// (`worker_agent_record`'s `pane_id_env` / `id`). `pane_id` alone is not
/// enough — `AppState::apply_event`'s same-pane reuse guard only updates the
/// pane's existing placeholder session in place when
/// `session.agent_id == event.agent_id`; an event carrying `agent_id: None`
/// fails that guard (and the immediately-following retire block explicitly
/// skips a `None`-agent_id event too), so it falls through and creates a
/// SECOND, disconnected session on the same `pane_id` instead of updating the
/// real one. Which of the two then answers for that pane is unspecified.
///
/// Blocks not on the daemon's broadcast (which fires whether or not
/// `apply_event` actually accepted the event — a wrong pane id or a rejected
/// event sails through it identically) but on `ListAgents`' `AgentRecord.live`
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
         socket write was accepted, but AppState::apply_event may have rejected it \
         or applied it to the wrong session; the pane's redraw below cannot be \
         trusted to reflect it either.",
    );
}

/// Scenario: The `WaitingForInput` carve-out's real-PTY proof — deliberately
/// NOT a real-agent test. A real worker self-skips wherever credentials are
/// absent (see `orchestration/lock/012`, which "passes" in ~0.1s having
/// executed nothing there); that would give ZERO automated coverage of this
/// carve-out in CI, which is worse than a stand-in that actually runs. The
/// status arrives over the genuine production wire either way; what a
/// stand-in gives up is only proof that some particular agent emits that
/// status, which is that agent's contract and not this feature's.
///
/// Open a real `orch-deck` orchestration (LOCKED, the default), focus the
/// non-orchestrator "worker" role, and confirm a keystroke is dropped as usual
/// (the `orchestration/lock/008` baseline). Inject a synthetic `AgentEvent`
/// reporting the worker's pane `WaitingForInput` over the hook socket — the
/// same wire the real `agent-event` CLI rides — and confirm the SAME kind of
/// keystroke now reaches the worker's PTY and echoes on the grid. Inject
/// `Thinking` (status clears, as if the agent resumed after being answered),
/// re-focus the worker explicitly (isolating this from the SEPARATE all-clear
/// auto-focus, covered by `orchestration/focus/*`), and confirm a further
/// keystroke is dropped again — the gate re-engages the instant the carve-out's
/// condition stops holding.
#[spec("orchestration/lock/011")]
#[test]
fn lock_011_waiting_carve_out_on_real_panes() {
    const WORKER_LOCKED_SENTINEL: &str = "LOCK011_LOCKED_4b7e";
    const WORKER_WAITING_SENTINEL: &str = "LOCK011_WAITING_9c2f";
    const WORKER_RELOCKED_SENTINEL: &str = "LOCK011_RELOCKED_e814";

    let deck = TuiDeck::builder()
        .with_env("DOT_AGENT_DECK_EXPERIMENTAL", "1")
        .with_pty_size(120, 40)
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("No active sessions");

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent"); // form closed -> tab up, orchestrator focused

    let socket = deck.attach_socket_path().to_path_buf();
    let worker_record = worker_agent_record(&socket);
    let worker_id = worker_record.id.clone();
    let worker_pane_id = worker_record
        .pane_id_env
        .clone()
        .expect("worker role pane must have a DOT_AGENT_DECK_PANE_ID recorded");
    let session_id = format!("{worker_id}-lock011-session");

    // Focus the non-orchestrator "worker" role. Still locked.
    focus_worker_role(&deck);

    // Baseline: locked, no status ever reported for the worker's pane —
    // dropped, the ordinary orchestration/lock/008 behaviour.
    deck.send_keys(format!("{WORKER_LOCKED_SENTINEL}\r").as_bytes());
    let leaked = deck.wait_for_grid_string_within(WORKER_LOCKED_SENTINEL, Duration::from_secs(2));
    assert!(
        !leaked,
        "a keystroke into the locked worker pane reached its PTY before any \
         WaitingForInput status was ever reported for it — expected the ordinary \
         orchestration/lock/008 baseline (dropped).\nGrid:\n{}",
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

    // Locked but WaitingForInput: the carve-out opens, and the keystroke must
    // reach the PTY and echo. `send_keys_until_grid_string_within` retries the
    // SEND because the status just arrived over an async daemon round-trip
    // with no in-process signal this test can await instead of the grid
    // itself.
    assert!(
        deck.send_keys_until_grid_string_within(
            format!("{WORKER_WAITING_SENTINEL}\r").as_bytes(),
            WORKER_WAITING_SENTINEL,
            Duration::from_secs(10),
        ),
        "a keystroke into the worker pane never reached its PTY after it \
         reported WaitingForInput while the command-entry lock was engaged — \
         expected the carve-out to pass it through.\nGrid:\n{}",
        deck.snapshot_grid()
    );

    // The status clears (the agent resumes, as if it just got answered) — the
    // gate must re-engage instantly.
    inject_worker_status(
        &deck,
        &socket,
        &worker_pane_id,
        &worker_id,
        &session_id,
        EventType::Thinking,
    );

    // Re-focus explicitly: this isolates the LOCK's own re-engagement from the
    // SEPARATE all-clear auto-focus, which may have already steered focus back
    // to the orchestrator once nothing was left waiting — this test must not
    // ride that as an accidental proxy for the lock re-engaging.
    focus_worker_role(&deck);
    deck.send_keys(format!("{WORKER_RELOCKED_SENTINEL}\r").as_bytes());
    let leaked = deck.wait_for_grid_string_within(WORKER_RELOCKED_SENTINEL, Duration::from_secs(2));
    assert!(
        !leaked,
        "a keystroke into the worker pane reached its PTY after its status \
         cleared from WaitingForInput back to Thinking — expected the \
         command-entry lock to re-engage the instant the carve-out's condition \
         stopped holding.\nGrid:\n{}",
        deck.snapshot_grid()
    );
}

/// Uniquely-named sentinel files the worker directive asks a REAL agent to
/// create — distinct per lock state so a leaked/buffered locked directive can
/// never be confused with the unlocked one that is expected to land.
const LIVE_LOCKED_SENTINEL: &str = "lock012_locked_9d3f.txt";
const LIVE_UNLOCKED_SENTINEL: &str = "lock012_unlocked_5a71.txt";

/// A directive typed straight into a pane's PTY (never `WriteAndSubmit`, which
/// is a daemon RPC that bypasses the lock's keystroke-forwarding gate
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

/// Last ~2 000 characters of a normalized pane key — enough context for a
/// failure message without dumping a megabyte of scrollback.
fn tail(text: &str) -> &str {
    &text[text.len().saturating_sub(2000)..]
}

/// Scenario: Open a real orchestration tab (`cat` orchestrator, a REAL
/// interactive Claude Haiku worker) locked by default, focus the worker, and
/// type a create-sentinel-file directive — confirm the file is never created
/// since the keystrokes never reach the agent. Enter command mode (`Ctrl+d`)
/// and send `Ctrl+e` to unlock, `Ctrl+d` back into `PaneInput`, then type a
/// second directive with a different sentinel — confirm the real agent now
/// receives it and creates that file, proving the lock gates a genuine agent's
/// input, not just a `cat` stub's echo. Self-skips where the CLI or
/// credentials are absent.
#[spec("orchestration/lock/012")]
#[test]
fn lock_012_real_agent_gated_by_lock_state() {
    // A missing CLI or credentials is an environmental condition, not a broken
    // test.
    skip_unless!(common::check_claude_available());

    let deck = TuiDeck::builder()
        .with_env("DOT_AGENT_DECK_EXPERIMENTAL", "1")
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
    deck.wait_for_absence("New Agent"); // form closed -> tab up, orchestrator focused

    let worker_id = worker_agent_record(&socket).id;
    if !common::wait_until_panes_settled(
        &socket,
        std::slice::from_ref(&worker_id),
        Duration::from_millis(1000),
        Duration::from_secs(3),
        Duration::from_secs(60),
    ) {
        eprintln!("warning: the real worker pane did not settle within 60s; proceeding anyway");
    }

    // Focus the non-orchestrator "worker" role. Still LOCKED.
    focus_worker_role(&deck);

    // Locked: a directive typed toward the real agent's PTY must not reach it,
    // so it must never act on it.
    deck.send_keys(create_sentinel_directive(LIVE_LOCKED_SENTINEL).as_bytes());
    let created = common::wait_for_path(&cwd.join(LIVE_LOCKED_SENTINEL), Duration::from_secs(20));
    assert!(
        !created,
        "a directive typed into the real Claude worker pane while the \
         command-entry lock was engaged (the default) reached the agent, which \
         created {LIVE_LOCKED_SENTINEL} — expected the keystrokes to be dropped \
         before Action::ForwardToPane.\n\
         === worker pane (normalized, tail) ===\n{}",
        tail(&common::pane_search_key_on(&socket, &worker_id)),
    );

    // Ctrl+e only resolves in command mode: Ctrl+d into command mode, Ctrl+e
    // to unlock, then Ctrl+d back into PaneInput so the directive below
    // actually reaches the worker pane's PTY.
    deck.send_bytes(b"\x04"); // Ctrl+d -> command mode
    deck.send_bytes(b"\x05"); // Ctrl+e == 0x05 -> unlock
    deck.send_bytes(b"\x04"); // Ctrl+d -> back to PaneInput

    // Unlocked: the same kind of directive into the still-focused worker pane
    // now forwards, and the real agent genuinely acts on it.
    deck.send_keys(create_sentinel_directive(LIVE_UNLOCKED_SENTINEL).as_bytes());
    assert!(
        common::wait_for_path(&cwd.join(LIVE_UNLOCKED_SENTINEL), Duration::from_secs(120)),
        "after Ctrl+e unlocked the deck, a directive typed into the \
         still-focused real Claude worker pane never resulted in \
         {LIVE_UNLOCKED_SENTINEL} being created — expected the keystrokes to \
         forward and the agent to act on them.\n\
         === worker pane (normalized, tail) ===\n{}",
        tail(&common::pane_search_key_on(&socket, &worker_id)),
    );

    // The locked directive must have been genuinely DROPPED, not merely
    // delayed/buffered and flushed once the deck unlocked.
    assert!(
        !cwd.join(LIVE_LOCKED_SENTINEL).exists(),
        "the locked-state directive's sentinel {LIVE_LOCKED_SENTINEL} appeared \
         after unlocking — the lock must drop gated keystrokes outright, not \
         queue them for delivery once unlocked"
    );
}

/// A path that provably does not exist on disk: `root` is a real, empty
/// harness-managed tempdir, but its `project` subdirectory is never created,
/// so `root/project/.dot-agent-deck.toml` cannot exist. Pointing
/// `DOT_AGENT_DECK_FEATURES_CONFIG` at it forces `features_config_path()`'s
/// override branch (checked FIRST, before any ancestor walk — `src/config.rs`)
/// to resolve to a missing file, driving `load_features_file` down its exact
/// `ErrorKind::NotFound -> Features::default()` branch — the SAME branch a
/// real ancestor walk hits when no `.dot-agent-deck.toml` exists anywhere
/// above the process cwd, which is fork #346's reported scenario (a deck
/// launched from the maintainer's home directory, with no project config
/// anywhere in its ancestry). `src/config.rs`'s own doc comment on
/// `DOT_AGENT_DECK_FEATURES_CONFIG` calls this override "so tests never touch
/// the real cwd" — this reuses that sanctioned mechanism rather than
/// `features::set_for_test`, which would only prove the gate's own if/else and
/// say nothing about the no-config resolution fork #346 is actually about.
/// The returned `TempDir` must be kept alive for the caller's whole test, or
/// its `Drop` removes `root` (harmless here, since `project` was never
/// created under it either way, but keeping it alive matches the harness's
/// own convention for tempdir-backed fixtures).
fn ghost_features_config_path() -> (tempfile::TempDir, String) {
    let root = common::harness_tempdir().expect("create ghost tempdir");
    let ghost = root
        .path()
        .join("project")
        .join(".dot-agent-deck.toml")
        .to_str()
        .expect("ghost path is UTF-8")
        .to_string();
    (root, ghost)
}

/// Scenario: With no project config discoverable anywhere (the
/// `ghost_features_config_path` mechanism above), a real Orchestration tab
/// still LOCKS: a keystroke typed at the focused non-orchestrator worker pane
/// must not reach its PTY and the lock's own status message must appear,
/// while the orchestrator pane's own input still reaches its PTY untouched.
/// The lock is unconditional (fork #346), so it holds even with no config
/// present anywhere, matching `orchestration/lock/008`'s behaviour with no
/// flag involved at all.
#[spec("orchestration/lock/015")]
#[test]
fn lock_015_gate_holds_with_no_project_config_present() {
    const ORCH_SENTINEL: &str = "LOCK015_ORCH_2e9a";
    const WORKER_SENTINEL: &str = "LOCK015_WORKER_c714";

    let (_ghost_root, ghost_config) = ghost_features_config_path();

    // Deliberately NO `DOT_AGENT_DECK_EXPERIMENTAL`: fork #346 is about the
    // real no-flag-set, no-config-anywhere default, not the flag's own on/off
    // behaviour (already covered by `orchestration/lock/008`/`014`).
    let deck = TuiDeck::builder()
        .with_env("DOT_AGENT_DECK_FEATURES_CONFIG", ghost_config.as_str())
        .with_pty_size(120, 40)
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("No active sessions");

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent"); // form closed -> tab up, orchestrator focused

    // The orchestrator pane's own input must still reach its PTY even with no
    // project config present anywhere.
    deck.send_keys(format!("{ORCH_SENTINEL}\r").as_bytes());
    deck.wait_for_string(ORCH_SENTINEL);

    focus_worker_role(&deck);

    // Locked: a keystroke aimed at the worker pane must NOT reach its PTY.
    deck.send_keys(format!("{WORKER_SENTINEL}\r").as_bytes());
    let leaked = deck.wait_for_grid_string_within(WORKER_SENTINEL, Duration::from_secs(2));
    assert!(
        !leaked,
        "a keystroke typed into the non-orchestrator worker pane reached its \
         PTY with NO project config present anywhere — expected the \
         command-entry lock to hold regardless of the experimental flag (fork \
         #346: the flag gate must be removed so this surface works with no \
         project config present, the maintainer's actual environment).\n\
         Grid:\n{}",
        deck.snapshot_grid()
    );

    // And the lock's own status message must have appeared for the dropped
    // keystroke.
    assert!(
        deck.snapshot_grid().contains("Pane locked"),
        "the command-entry lock dropped a keystroke but never reported its \
         status message with no project config present anywhere.\nGrid:\n{}",
        deck.snapshot_grid()
    );
}

/// Scenario: With no project config discoverable anywhere (same mechanism as
/// `orchestration/lock/015`), send `Ctrl+e` from command mode on a real
/// Orchestration tab and confirm the deck still claims it as
/// `Action::ToggleOrchestrationLock` — reported via `Pane entry: unlocked` —
/// rather than leaving it unclaimed for the global keybinding resolver to
/// fall through to the PTY. `Ctrl+e`'s binding resolution is unconditional
/// (fork #346), so it still resolves with no config present — the mirror of
/// `orchestration/lock/009`'s proof, with no flag involved at all. Also reads
/// the persistent LOCKED/UNLOCKED chip immediately right of the mode chip
/// before and after the toggle, proving `lock_context_for_tab`'s render path
/// is likewise unconditional and not still routed through a discoverable
/// config or the (now-removed) flag.
#[spec("orchestration/lock/016")]
#[test]
fn lock_016_ctrl_e_resolves_with_no_project_config_present() {
    let (_ghost_root, ghost_config) = ghost_features_config_path();

    let deck = TuiDeck::builder()
        .with_env("DOT_AGENT_DECK_FEATURES_CONFIG", ghost_config.as_str())
        .with_pty_size(120, 40)
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("No active sessions");

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent");

    let locked_tail = wait_for_chip_tail(&deck, Duration::from_secs(2));
    assert!(
        locked_tail
            .as_deref()
            .is_some_and(|tail| tail.starts_with(" LOCKED ")),
        "the persistent lock chip did not read ` LOCKED ` with no project \
         config present anywhere. Observed tail: {locked_tail:?}\nGrid:\n{}",
        deck.snapshot_grid()
    );

    deck.send_bytes(b"\x04"); // Ctrl+d -> command mode
    deck.send_bytes(b"\x05"); // Ctrl+e -> Action::ToggleOrchestrationLock

    assert!(
        deck.wait_for_grid_string_within("Pane entry: unlocked", Duration::from_secs(3)),
        "Ctrl+e from command mode was not claimed as \
         Action::ToggleOrchestrationLock with no project config present \
         anywhere — the deck never reported `Pane entry: unlocked` (fork \
         #346).\nGrid:\n{}",
        deck.snapshot_grid()
    );

    // Back into PaneInput: the persistent chip is drawn only in that mode
    // (`lock_context_for_tab` is scoped to `UiMode::PaneInput`, per src/ui.rs)
    // — `wait_for_chip_tail`'s " TYPING " anchor cannot appear while still in
    // command mode.
    deck.send_bytes(b"\x04");

    let unlocked_tail = wait_for_chip_tail(&deck, Duration::from_secs(2));
    assert!(
        unlocked_tail
            .as_deref()
            .is_some_and(|tail| tail.starts_with(" UNLOCKED ")),
        "the persistent lock chip did not read ` UNLOCKED ` after Ctrl+e with \
         no project config present anywhere. Observed tail: \
         {unlocked_tail:?}\nGrid:\n{}",
        deck.snapshot_grid()
    );
}

/// Scenario: Open a real orchestration tab (default LOCKED), focus the
/// non-orchestrator "worker" role, and send a bracketed paste at it — confirm
/// it is dropped exactly like an ordinary keystroke (issue #302 defect 1:
/// `Event::Paste` previously called `embedded.write_raw_bytes` directly,
/// bypassing `gate_pane_input_key` entirely). Build real scrollback in the
/// worker pane, re-lock, scroll back via the mouse wheel (the one scroll door
/// that survives a `PaneInput` re-entry, since keyboard PageUp/PageDown is
/// command-mode only and re-entering `PaneInput` itself snaps scrollback to
/// live output), and confirm a further dropped paste does not yank the view
/// back to live output. Finally, in one burst, drop a paste and immediately
/// unlock-and-forward a bare Enter — with a visible marker sent only AFTER
/// the Enter, since anything forwarded before it (even the marker itself)
/// would debounce it unconditionally and mask the very thing being tested —
/// proving the drop left no `SUBMIT_DEBOUNCE` (150ms) state behind for it to
/// trip. Not by comparing its arrival time against a fixed bound (render and
/// harness-polling latency for the whole burst can itself run past 150ms on
/// a loaded CI runner, making any fixed bound either flaky or vacuous), but
/// by comparing it against a plain control keystroke's round trip measured
/// moments later on the same runner, canceling the runner's shared baseline
/// instead of hard-coding one.
#[spec("orchestration/lock/017")]
#[test]
fn lock_017_paste_gated_by_lock_state() {
    const LOCKED_PASTE_SENTINEL: &str = "LOCK017_LOCKED_PASTE_2f6a";
    const UNLOCKED_PASTE_SENTINEL: &str = "LOCK017_UNLOCKED_PASTE_9d47";
    const SCROLL_TOP_MARKER: &str = "LOCK017_SCROLL_TOP_MARKER_a91c";
    const SCROLL_BOTTOM_MARKER: &str = "LOCK017_SCROLL_BOTTOM_c73e";
    const REGATED_PASTE_SENTINEL: &str = "LOCK017_REGATED_PASTE_5b18";
    const TIMING_DROP_SENTINEL: &str = "LOCK017_TIMING_DROP_7ee2";
    const TIMING_SENTINEL: &str = "LOCK017_TIMING_e04d";
    const CONTROL_SENTINEL: &str = "LOCK017_CONTROL_b3af";
    // Never sent to the grid; polling for it up to a timeout is this test's
    // Decision-21-compliant substitute for a bare `std::thread::sleep`,
    // which `cargo xtask linkage-check` forbids in e2e test bodies.
    const NEVER_APPEARS_SENTINEL: &str = "LOCK017_NEVER_APPEARS_1a0e9c3f";

    let deck = TuiDeck::builder()
        .with_env("DOT_AGENT_DECK_EXPERIMENTAL", "1")
        .with_pty_size(120, 40)
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("No active sessions");

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent"); // form closed -> tab up, orchestrator focused

    // --- Part 1: locked, a bracketed paste at the worker must be dropped
    // exactly like an ordinary keystroke (the orchestration/lock/008
    // baseline), proving Event::Paste no longer bypasses gate_pane_input_key.
    focus_worker_role(&deck);

    let locked_paste = format!("\x1b[200~{LOCKED_PASTE_SENTINEL}\x1b[201~");
    deck.send_keys(locked_paste.as_bytes());
    let leaked = deck.wait_for_grid_string_within(LOCKED_PASTE_SENTINEL, Duration::from_secs(2));
    assert!(
        !leaked,
        "a bracketed paste aimed at the non-orchestrator worker pane reached \
         its PTY while the command-entry lock was engaged — Event::Paste \
         calls embedded.write_raw_bytes directly, never through \
         gate_pane_input_key, so the lock's guarantee was keystroke-only.\n\
         Grid:\n{}",
        deck.snapshot_grid()
    );

    // --- Part 2: build real scrollback in the worker pane, re-lock, scroll
    // back, and prove a further dropped paste does not reset it to live
    // output (reset_scrollback must not fire on a drop).

    // Unlock so the worker can actually receive the bulk content below.
    deck.send_bytes(b"\x04"); // Ctrl+d -> command mode
    deck.send_bytes(b"\x05"); // Ctrl+e -> unlock
    deck.send_bytes(b"\x04"); // Ctrl+d -> back to PaneInput

    let unlocked_paste = format!("\x1b[200~{UNLOCKED_PASTE_SENTINEL}\x1b[201~");
    deck.send_keys(unlocked_paste.as_bytes());
    deck.wait_for_string(UNLOCKED_PASTE_SENTINEL);

    // Enough numbered filler lines to push the top marker well off-screen at
    // scrollback == 0, whatever the worker pane's actual height turns out to
    // be at 120x40.
    //
    // Sent as ONE bracketed paste, not raw `send_keys`: unwrapped, crossterm
    // parses each of the 122 `\r`s here as its own KeyEvent, and every
    // Enter-bearing `Action::ForwardToPane` pays `submit_debounce_duration`'s
    // up-to-150ms synchronous sleep (PRD #76 M2.20, `src/ui.rs`) when it
    // lands under `SUBMIT_DEBOUNCE` of the previous forward — which a burst
    // this dense always does. ~122 * 150ms is >18s of the deck's single
    // event/render thread blocked in `std::thread::sleep`, on top of
    // everything else this test still has to do, well past what a 30s
    // `wait_for_string` budget survives on a loaded CI runner. A bracketed
    // paste is a single `Event::Paste` delivered in one `write_raw_bytes`
    // call with no per-line debounce, exactly like `unlocked_paste` above —
    // and this is genuinely scrollback content the worker pane must render,
    // not a paste-drop assertion, so wrapping it changes nothing this part
    // of the test is trying to prove.
    let mut bulk = format!("{SCROLL_TOP_MARKER}\r");
    for i in 0..120 {
        bulk.push_str(&format!("filler-{i:03}\r"));
    }
    bulk.push_str(&format!("{SCROLL_BOTTOM_MARKER}\r"));
    let bracketed_bulk = format!("\x1b[200~{bulk}\x1b[201~");
    deck.send_keys(bracketed_bulk.as_bytes());
    deck.wait_for_string(SCROLL_BOTTOM_MARKER);

    // Re-lock. The final Ctrl+d back into PaneInput is itself a fresh mode
    // entry, which snaps scrollback to live output (mode/scroll/003) — the
    // right starting point for what follows, since the wheel scroll below
    // must be the ONLY thing that moves the offset off zero.
    deck.send_bytes(b"\x04");
    deck.send_bytes(b"\x05");
    deck.send_bytes(b"\x04");

    // Scroll the worker pane's OWN view back into its scrollback via the
    // mouse wheel: scroll_focused_agent_pane falls through to
    // scroll_focused_pane_scrollback whenever the child has no mouse mode
    // enabled (true for the `cat` stub here) regardless of UiMode, so this is
    // the one scroll door that does not itself re-enter PaneInput and reset
    // what this is about to prove.
    //
    // Polled in batches rather than one blind 60-notch burst then a single
    // check: 60 back-to-back synthetic SGR mouse sequences is, like the bulk
    // send above, a volume nothing else in this suite fires at once, and a
    // fixed count plus one fire-and-forget wait cannot tell "not enough
    // notches yet" apart from "an event got lost" from "still catching up".
    // Each iteration sends another batch — cheap and safe well under the
    // 10,000-line scrollback cap (`PANE_SCROLLBACK_LINES`) — so intermittent
    // event loss self-heals instead of wedging the assertion, matching the
    // retry-not-just-wait shape `send_keys_until_grid_string_within` already
    // uses elsewhere in this harness.
    let (col, row) = deck.wait_for_in_grid(SCROLL_BOTTOM_MARKER);
    let scrolled_to_top = common::wait_until(Duration::from_secs(10), || {
        deck.scroll_n(col, row, false, 20); // false == wheel-up, 20 notches
        deck.wait_for_grid_string_within(SCROLL_TOP_MARKER, Duration::from_millis(200))
    });
    assert!(
        scrolled_to_top,
        "scrolling the focused worker pane's own view back via the wheel \
         never surfaced the marker typed as the very first of 120+ lines \
         within 10s of polled scrolling — the scrollback precondition for \
         the next assertion was never actually established.\nGrid:\n{}",
        deck.snapshot_grid()
    );

    // Locked again, scrolled back: a dropped paste must not touch this
    // pane's scrollback at all.
    let regated_paste = format!("\x1b[200~{REGATED_PASTE_SENTINEL}\x1b[201~");
    deck.send_keys(regated_paste.as_bytes());
    let leaked_again =
        deck.wait_for_grid_string_within(REGATED_PASTE_SENTINEL, Duration::from_secs(2));
    assert!(
        !leaked_again,
        "a second bracketed paste reached the relocked worker pane's PTY.\n\
         Grid:\n{}",
        deck.snapshot_grid()
    );
    assert!(
        deck.snapshot_grid().contains(SCROLL_TOP_MARKER),
        "the dropped paste yanked the worker pane's view back to live output \
         — Event::Paste must not call embedded.reset_scrollback for a paste \
         gate_pane_input_key denies, exactly as a dropped ordinary keystroke \
         already does not.\nGrid:\n{}",
        deck.snapshot_grid()
    );

    // --- Part 3: the drop must also leave last_pane_keystroke_at untouched,
    // so a genuinely forwarded Enter right after it is not needlessly
    // debounced (SUBMIT_DEBOUNCE == 150ms).
    //
    // The visible marker text goes AFTER the tested `\r`, not before it.
    // Every forwarded byte — including an ordinary printable character,
    // per the unconditional restamp at src/ui.rs:10953 — updates
    // `last_pane_keystroke_at`, and the existing unit test
    // `enter_following_recent_keystroke_sleeps_at_least_debounce_minus_elapsed`
    // (src/ui.rs) pins that ANY recently-forwarded keystroke debounces the
    // Enter that follows it, regardless of what that keystroke was. Typing
    // the marker BEFORE the `\r` would make the `\r` see its own
    // just-typed marker text as "the recent keystroke," debouncing it
    // UNCONDITIONALLY — passing or failing on a signal that has nothing to
    // do with the dropped paste. Sending `\r` first, with nothing
    // forwarded in between it and the (denied) drop but the mode-toggle
    // keys — which are not `Action::ForwardToPane` and so never touch the
    // timestamp — means its debounce check reflects ONLY what the drop
    // did or did not do. The marker that follows it is free to restamp
    // harmlessly, since by then the tested check has already run; it
    // exists purely so this test can observe that the whole burst,
    // `\r` included, has been processed and rendered.
    //
    // Concatenated into ONE write so the deck's own event processing —
    // not this test's IPC round trips — sets the (near-zero, if buggy)
    // gap between the drop and the `\r`: splitting this into
    // separately-confirmed sends would let real wall-clock time pass
    // between the (potential) stamp and the forward, which would let even
    // a genuine regression sail under SUBMIT_DEBOUNCE's 150ms window
    // undetected — the debounce check only ever compares against the
    // MOST RECENT stamp, so a stale one is invisible to it by design.
    let mut burst = format!("\x1b[200~{TIMING_DROP_SENTINEL}\x1b[201~"); // dropped, still locked
    burst.push('\x04'); // Ctrl+d -> command mode
    burst.push('\x05'); // Ctrl+e -> unlock
    burst.push('\x04'); // Ctrl+d -> back to PaneInput, now unlocked
    burst.push('\r'); // the tested keystroke: nothing forwarded before it but the drop
    burst.push_str(TIMING_SENTINEL); // marker AFTER `\r`, so it cannot contaminate its debounce check

    let timed_start = std::time::Instant::now();
    deck.send_keys(burst.as_bytes());
    let observed = deck.wait_for_grid_string_within(TIMING_SENTINEL, Duration::from_secs(2));
    let timed_elapsed = timed_start.elapsed();
    assert!(
        observed,
        "the timing sentinel never reached the grid at all.\nGrid:\n{}",
        deck.snapshot_grid()
    );

    // CONTROL: the same shape — a bare Enter, then a marker — sent only
    // once SUBMIT_DEBOUNCE's 150ms window since the last genuinely
    // forwarded byte (the timing `\r` above, which always stamps
    // regardless of this test's outcome — src/ui.rs:10953) has
    // unquestionably elapsed. This round trip's own latency is therefore
    // guaranteed debounce-free and stands purely for "what does an
    // unremarkable round trip cost right now, on this runner" — the same
    // render + harness-polling pipeline the timed round trip above went
    // through, measured moments later under the same load. A fixed
    // absolute bound cannot separate that shared, load-driven baseline
    // from a genuine 150ms debounce sleep: CI observed 433/484/678ms for
    // this test's PRIOR form (marker BEFORE `\r`, production code
    // confirmed not to stamp on a drop), strictly rising across three
    // retries with no code change in between — and that form additionally
    // had the marker-before-`\r` defect above, so part of that number was
    // a guaranteed, unconditional debounce rather than pure baseline
    // noise. Comparing against this control cancels the baseline either
    // way, without needing to disentangle the two.
    let _ = deck.wait_for_grid_string_within(NEVER_APPEARS_SENTINEL, Duration::from_millis(200));
    let control_start = std::time::Instant::now();
    deck.send_keys(format!("\r{CONTROL_SENTINEL}").as_bytes());
    let control_observed =
        deck.wait_for_grid_string_within(CONTROL_SENTINEL, Duration::from_secs(2));
    let control_elapsed = control_start.elapsed();
    assert!(
        control_observed,
        "the control sentinel never reached the grid at all.\nGrid:\n{}",
        deck.snapshot_grid()
    );

    // A genuine SUBMIT_DEBOUNCE sleep is a synchronous ~150ms added directly
    // to the timed round trip and nowhere else, so a 100ms margin above the
    // control leaves ample room for ordinary measurement-to-measurement
    // jitter between two round trips a few hundred milliseconds apart,
    // while staying well short of the 150ms signal a regression would add.
    assert!(
        timed_elapsed < control_elapsed + Duration::from_millis(100),
        "the Enter-bearing keystroke sent immediately after a DROPPED paste \
         took {timed_elapsed:?} to reach the grid, {:?} more than a plain \
         control round trip measured moments later on this same runner \
         ({control_elapsed:?}) — SUBMIT_DEBOUNCE is 150ms, so a gap this \
         large above the runner's own current baseline implicates \
         last_pane_keystroke_at having been stamped by the dropped paste. \
         Event::Paste must not stamp it when gate_pane_input_key denies the \
         write; only a genuinely forwarded keystroke may.\nGrid:\n{}",
        timed_elapsed.saturating_sub(control_elapsed),
        deck.snapshot_grid()
    );
}

/// Poll the rendered grid for the persistent mode chip (` TYPING `, the
/// `PaneInput` label) and return whatever text immediately follows it on that
/// row — `None` if the chip itself never appears within `timeout`. Used by
/// `lock_018` to read the lock chip's text without racing
/// the frame that draws the bottom bar.
fn wait_for_chip_tail(deck: &TuiDeck, timeout: Duration) -> Option<String> {
    common::wait_until(timeout, || {
        deck.snapshot_grid()
            .lines()
            .any(|line| line.contains(" TYPING "))
    });
    deck.snapshot_grid().lines().find_map(|line| {
        line.split_once(" TYPING ")
            .map(|(_, tail)| tail.to_string())
    })
}

/// Scenario: Open a real orchestration tab (default LOCKED), and in
/// `PaneInput` mode (the orchestrator role focused) locate the persistent
/// mode chip on the bottom bar — confirm the cells immediately to its right
/// read ` LOCKED `. Unlock (`Ctrl+d`, `Ctrl+e`, `Ctrl+d`) and confirm the same
/// position now reads ` UNLOCKED ` — both states must render, not just the
/// locked default (issue #302 defect 3: previously NEITHER rendered at all,
/// so a working lock and an inert one were indistinguishable on screen,
/// which is how #303 went unnoticed). Then open the `?` help overlay and confirm
/// `Ctrl+e` is documented there.
///
/// Written as L2 (not the L1 widget snapshot the task named) because no
/// existing `_to_buffer` seam threads Orchestration-tab-and-lock context into
/// `render_bottom_bar` — every current seam (`render_button_bar_for_mode_to_buffer`
/// and siblings) builds a bare `UiState` with no tab/lock parameter, and
/// `UiState`/`render_bottom_bar` are private to `src/ui.rs`. Adding that
/// parameter is itself the production change (a new seam plus the render
/// logic it exercises), which is out of a tester's reach. This drives the
/// real running binary instead, so the LOCKED/UNLOCKED text content is
/// genuinely pinned; it can NOT verify the task's styling requirement
/// (reversed+bold vs dim) — `snapshot_grid()` is text-only. An L1 snapshot
/// pinning the exact styling remains worth adding once the seam exists.
#[spec("orchestration/lock/018")]
#[test]
fn lock_018_persistent_chip_and_help_entry() {
    let deck = TuiDeck::builder()
        .with_env("DOT_AGENT_DECK_EXPERIMENTAL", "1")
        .with_pty_size(120, 40)
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("No active sessions");

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent"); // form closed -> tab up, orchestrator focused
    deck.wait_for_string("[Command Mode Ctrl+D]"); // live PTY, PaneInput mode, frame settled

    // Locate the persistent mode chip (" TYPING " in PaneInput) and read what
    // immediately follows it — that is where the lock chip must sit. Polled
    // (not a single snapshot) since this is the first read after the form
    // closed and the frame carrying the bottom bar may not have painted yet.
    let locked_tail = wait_for_chip_tail(&deck, Duration::from_secs(2));
    assert!(
        locked_tail
            .as_deref()
            .is_some_and(|tail| tail.starts_with(" LOCKED ")),
        "immediately right of the ` TYPING ` mode chip, the bottom bar must \
         read ` LOCKED ` while the command-entry lock is engaged (the \
         default) — without this chip a working lock and an inert one are \
         indistinguishable on screen. Observed tail: {locked_tail:?}\n\
         Grid:\n{}",
        deck.snapshot_grid()
    );

    // Unlock: Ctrl+d into command mode, Ctrl+e to toggle, Ctrl+d back into
    // PaneInput.
    deck.send_bytes(b"\x04");
    deck.send_bytes(b"\x05");
    deck.send_bytes(b"\x04");
    deck.wait_for_string("Pane entry: unlocked");

    let unlocked_tail = wait_for_chip_tail(&deck, Duration::from_secs(2));
    assert!(
        unlocked_tail
            .as_deref()
            .is_some_and(|tail| tail.starts_with(" UNLOCKED ")),
        "immediately right of the ` TYPING ` mode chip, the bottom bar must \
         read ` UNLOCKED ` once the lock is toggled off — an indicator that \
         only ever renders the locked state reproduces the exact ambiguity \
         it exists to remove.\nGrid:\n{}",
        deck.snapshot_grid()
    );

    // `?` (Help) resolves only from `UiMode::Normal` (`dispatch_normal_mode_key`,
    // `src/ui.rs`) — `global_action()`'s always-resolves-from-any-mode table
    // (Dashboard/ToggleLayout/ToggleOrchestrationSplit/ToggleOrchestrationLock/
    // NewPane/ClosePane/Ctrl+PageUp/PageDown) has no entry for it, so from
    // `PaneInput` a bare `?` falls through to `handle_pane_input_key` and is
    // forwarded to the focused pane like any other character instead of
    // opening the overlay. `Ctrl+d` first, matching every other command-mode
    // chord this test already sends.
    deck.send_bytes(b"\x04");
    deck.send_bytes(b"?");
    deck.wait_for_string("Press ? or Esc to close");
    assert!(
        deck.snapshot_grid().contains("Ctrl+e"),
        "the help overlay must document Ctrl+e (ToggleOrchestrationLock) — it \
         does not appear anywhere in the overlay.\nGrid:\n{}",
        deck.snapshot_grid()
    );
}

/// Scenario: At 80 columns — the single most common terminal width — open a
/// real orchestration tab (default LOCKED), focus the non-orchestrator
/// "worker" role, and send a dropped keystroke to surface the corrected
/// unlock hint. Confirm the FULL hint text (`Pane locked — Ctrl+d, Ctrl+e,
/// Ctrl+d to type here`) is visible as a contiguous string in the grid, not
/// truncated by the right-aligned `[Command Mode Ctrl+D]` button drawn into
/// the same row.
///
/// Written as L2, not the L1 widget snapshot the task originally named:
/// every existing `_to_buffer` seam that exercises `render_bottom_bar`'s
/// `PaneInput` arm (`render_button_bar_for_mode_to_buffer` and siblings)
/// builds a bare `UiState` with `status_message` left at its default `None`,
/// and `UiState::new`/its `status_message` field are private to `src/ui.rs`
/// — no seam threads an arbitrary status message into the rendered bar.
/// Adding one is itself a production-code change (a new seam plus whatever
/// it exposes), which is out of a tester's reach — the same reasoning
/// `orchestration/lock/018` already recorded in this file for the LOCKED/
/// UNLOCKED chip. This drives the real running binary instead, through the
/// same dropped-keystroke path `orchestration/lock/008` already pins at
/// 120x40 (where the collision does not occur).
#[spec("orchestration/lock/019")]
#[test]
fn lock_019_hint_visible_at_80_columns() {
    const WORKER_SENTINEL: &str = "LOCK019_WORKER_1c8e";

    let deck = TuiDeck::builder()
        .with_pty_size(80, 40)
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("No active sessions");

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent"); // form closed -> tab up, orchestrator focused

    focus_worker_role(&deck);

    // Locked: a keystroke aimed at the worker pane must NOT reach its PTY,
    // and the drop must surface the unlock hint (orchestration/lock/008).
    deck.send_keys(format!("{WORKER_SENTINEL}\r").as_bytes());
    let leaked = deck.wait_for_grid_string_within(WORKER_SENTINEL, Duration::from_secs(2));
    assert!(
        !leaked,
        "a keystroke typed into the non-orchestrator worker pane reached its \
         PTY while the command-entry lock was engaged (the default state) at \
         80 columns.\nGrid:\n{}",
        deck.snapshot_grid()
    );

    assert!(
        deck.wait_for_grid_string_within(
            "Pane locked — Ctrl+d, Ctrl+e, Ctrl+d to type here",
            Duration::from_secs(2),
        ),
        "at 80 columns the dropped-keystroke status message was not visible \
         as a contiguous string — issue #302 defect 2's corrected hint (`Pane \
         locked — Ctrl+d, Ctrl+e, Ctrl+d to type here`, 49 chars) is drawn \
         into the same row as the right-aligned `[Command Mode Ctrl+D]` \
         button (21 chars); below 87 columns the button (drawn second) wins \
         the overlapping cells and the tail of the message — `type here` — is \
         overwritten. `orchestration/lock/008` pins the same message at \
         120x40, where the two do not overlap.\nGrid:\n{}",
        deck.snapshot_grid()
    );
}
