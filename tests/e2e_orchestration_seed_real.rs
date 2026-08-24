#![cfg(feature = "e2e")]

//! L2 REAL-agent proof for the orchestrator spawn-time seed delivery
//! (upstream issue #424, CLAUDE.md rule 4). Every `orchestration/seed/*`
//! test up to `010` drives `deliver_orchestrator_prompt` in-process against
//! an injected clock and a `SendResultPaneController` double standing in for
//! the PTY — proof the client-side retry/confirmation *logic* is correct,
//! never proof a real Claude Code process ever received the seed pointer,
//! read the file it names, and acted on it. #424's own bug (a fixed 500ms
//! readiness buffer outrun by 5 MCP servers and a 10.9 KB `SessionStart`
//! hook) could only ever be caught by a test that drives a real startup —
//! `grep -rln "skip_unless" tests/e2e_*.rs | xargs grep -ln "Acknowledge
//! your role\|orchestrator-context.md"` returned nothing before this file.
//!
//! Uses the `orch-seed-remit-real` fixture: an orchestration whose
//! orchestrator (start) role is a REAL interactive `claude --model
//! claude-haiku-4-5-20251001 --allowedTools Bash` pane, with a
//! `prompt_template` that becomes the first section of the generated
//! `.dot-agent-deck/orchestrator-context.md` — a directive to output a
//! fixed, uniquely-named sentinel token verbatim as its first action. The
//! `worker` role is a `cat` stand-in; only the orchestrator's remit
//! delivery is under test.
//!
//! The assertion chain proves the full contract, not just that bytes
//! reached a PTY (exactly the insufficiency #424 itself exists to fix):
//!   1. A native `UserPromptSubmit`-mapped `Thinking` hook event, carrying
//!      the literal pointer text naming `orchestrator-context.md`, proves
//!      the spawn-time seed was genuinely SUBMITTED inside the real Claude
//!      Code session (never a synthetic/fabricated event).
//!   2. The sentinel token appearing in the pane's cumulative output proves
//!      the agent genuinely READ the file the pointer named and ACTED on
//!      its content, rather than merely having text typed at it.
//!
//! Cost note (Decision 23): one short interactive Haiku turn. Local-only
//! (Decision 8 / rule 5 exception (a)): gated on the `e2e` feature so CI's
//! `cargo test-fast` never compiles it; this file's real-agent tier has no
//! CI credentials, so a local run is the only way to exercise it at all —
//! it self-skips in CI. Flaky-tolerant (real LLM + real network) per rule
//! 4 — run once, never looped.

mod common;

use std::time::Duration;

use common::{TuiDeck, commit_fixture};
use dot_agent_deck::event::{AgentType, EventType};
use spec::spec;

/// The fixed, uniquely-named token the fixture's `prompt_template` directs
/// the orchestrator to output verbatim once it has read
/// `.dot-agent-deck/orchestrator-context.md`. Fixed rather than randomly
/// generated per run (matching this harness's other real-agent sentinels,
/// e.g. `e2e_shell_activity_real_agent.rs::SENTINEL`) — unique enough that
/// its appearance in the pane cannot be coincidental, without needing
/// runtime randomness this test has no use for.
const SEED_SENTINEL: &str = "ORCH-SEED-011-REMIT-OK-6d29f4";

/// The exact spawn-time pointer text `deliver_orchestrator_prompt`
/// (`src/ui.rs`) submits into the orchestrator's pane — kept in lockstep
/// with `ORCHESTRATOR_CONTEXT_POINTER` there. Matched as a `user_prompt`
/// substring on the native hook event below, so this test breaks loudly
/// (not silently) if the production pointer text ever changes.
const DELIVERED_POINTER: &str = "Read .dot-agent-deck/orchestrator-context.md";

/// Drive the new-pane dialog to open the (single) orchestration in the
/// `orch-seed-remit-real` fixture. Mirrors
/// `tests/e2e_orchestration_lock.rs::open_orchestration` — with no
/// `[[modes]]` defined the Mode chip row is `[No mode] [Orch: seed-011-remit]
/// [schedule]`, so ONE Right selects the orchestration; selecting an
/// orchestration hides the Command field, so a second Enter submits the
/// form. Lands with the orchestrator (start) role focused in `PaneInput`
/// mode. Commits the fixture's `.dot-agent-deck.toml` first (restored
/// 2026-08-24, fork issue #373): current `main`'s new-pane spawn runs
/// isolated-clone provisioning that needs a ref to branch from, which the
/// harness's own bare `git init` does not provide on its own — see
/// `common::commit_fixture`'s doc comment and `e2e_orchestration_remit.rs`'s
/// shared `common::open_orchestration` for the pattern this now follows.
fn open_orchestration(deck: &TuiDeck) {
    // Isolated-clone provisioning needs a ref to branch from — an unborn
    // HEAD (the harness's own bare `git init`) does not provide one.
    commit_fixture(deck.workdir());
    deck.send_keys(b"\x0e"); // Ctrl+n -> directory picker
    deck.send_keys(b" "); // Space -> confirm current dir -> new-pane form
    deck.wait_for_string("No mode"); // form up, Mode field focused at "No mode"
    deck.send_keys(b"\x1b[C"); // Right -> [Orch: seed-011-remit]
    deck.send_keys(b"\r"); // Mode -> Name
    deck.send_keys(b"\r"); // submit (Command hidden for an orchestration)
}

/// The fixture's real Claude orchestrator role pane's daemon registry
/// record. Mirrors `tests/e2e_orchestration_lock.rs::worker_agent_record`,
/// specialized to the start role.
fn orchestrator_agent_record(socket: &std::path::Path) -> dot_agent_deck::agent_pty::AgentRecord {
    common::agent_records_on(socket)
        .into_iter()
        .find(|r| {
            matches!(
                &r.tab_membership,
                Some(dot_agent_deck::agent_pty::TabMembership::Orchestration { role_name, .. })
                    if role_name == "orchestrator"
            )
        })
        .unwrap_or_else(|| {
            panic!("orch-seed-remit-real fixture's orchestrator role pane must be registered with the daemon")
        })
}

/// Scenario: Open a real orchestration whose orchestrator (start) role is a genuine interactive Haiku Claude Code process, let the daemon deliver the spawn-time seed pointer through the production `deliver_orchestrator_prompt` path with no test intervention, and assert two things in order: a native `UserPromptSubmit`-mapped `Thinking` hook event carrying the literal pointer text proves the seed was genuinely submitted inside the real session, and the fixture's fixed sentinel token appearing in the pane's output proves the agent actually read `.dot-agent-deck/orchestrator-context.md` and acted on it — the full contract, not just bytes on a PTY.
#[spec("orchestration/seed/011")]
#[test]
fn orchestration_seed_011_real_claude_orchestrator_reads_and_acts_on_remit() {
    // Decision 26 runtime-skip: a missing CLI or credentials is an
    // environmental condition, not a broken test.
    skip_unless!(common::check_claude_available());

    let deck = TuiDeck::builder()
        .with_pty_size(120, 40)
        .with_imported_claude_credentials()
        // The orchestrator's cwd is the deck's own workdir (the copied
        // `orch-seed-remit-real` fixture root); pre-trust it so the real
        // claude's first-run onboarding/trust gates clear with no keystroke
        // and the spawn-time seed pointer below isn't swallowed answering
        // them.
        .with_claude_trust_workdir()
        .launch_with_fixture("orch-seed-remit-real");
    deck.wait_for_string("No active sessions");

    let socket = deck.attach_socket_path().to_path_buf();
    let events = deck.subscribe_events();

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent"); // new-pane form closed -> tab up, orchestrator focused

    // Precondition: the real Claude Code process genuinely booted to its
    // interactive prompt. A miss here is a boot/auth/trust failure, never a
    // seed-delivery failure — worth distinguishing in the panic message.
    assert!(
        deck.wait_for_grid_string_within("? for shortcuts", Duration::from_secs(120)),
        "the real interactive Claude orchestrator never became visibly \
         input-ready within 120s — a boot/auth/trust failure, not a seed- \
         delivery failure.\nFinal grid:\n{}",
        deck.snapshot_grid()
    );

    let agent_id = orchestrator_agent_record(&socket).id;

    // Step 1 of the contract: native proof the spawn-time pointer was
    // genuinely SUBMITTED inside the real session — Claude Code's own
    // UserPromptSubmit hook, mapped to EventType::Thinking
    // (`src/hook.rs:111` / `src/state.rs`), carrying the literal pointer
    // text as `user_prompt`. Never a fabricated/synthetic event.
    let submitted = events.wait_for(
        |event| {
            event.event_type == EventType::Thinking
                && event.agent_type == AgentType::ClaudeCode
                && event.agent_id.as_deref() == Some(agent_id.as_str())
                && event
                    .user_prompt
                    .as_deref()
                    .is_some_and(|prompt| prompt.contains(DELIVERED_POINTER))
        },
        Duration::from_secs(60),
    );
    eprintln!(
        "orchestration_seed_011: native UserPromptSubmit observed at {:?} carrying the pointer: {:?}",
        submitted.timestamp, submitted.user_prompt
    );

    // Step 2 of the contract: the agent genuinely READ the file the pointer
    // named and ACTED on its content — the exact thing #424's bug proved a
    // "bytes reached the PTY" check cannot establish. Scanned over the
    // pane's cumulative output, not a single grid frame, so the token isn't
    // missed if it has already scrolled by the time this runs.
    assert!(
        deck.wait_for_stream_string_within(SEED_SENTINEL, Duration::from_secs(90)),
        "the real Claude orchestrator never echoed the sentinel token {SEED_SENTINEL:?} within \
         90s of the spawn-time seed pointer being submitted — the pointer reached the session \
         (see the native event above) but the agent never demonstrably read \
         `.dot-agent-deck/orchestrator-context.md` and acted on it.\nFinal grid:\n{}",
        deck.snapshot_grid()
    );
}
