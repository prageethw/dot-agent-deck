#![cfg(feature = "e2e")]
#![cfg(unix)]

//! L2 REAL-agent proof for fork #197 M4's decided mechanism (fork #194):
//! a confirmation-retry sends a BARE submit, never the prompt text a
//! second time, so the agent genuinely receives the spawn-time seed
//! pointer exactly ONCE. `orchestration/seed/001`-`010`/`012`-`014` drive
//! `deliver_orchestrator_prompt` in-process against an injected clock and
//! a `SendResultPaneController` double — proof the client-side retry/
//! confirmation *logic* is correct, never proof a real agent process ever
//! received a genuinely-single submission of the pointer text. `seed/011`
//! is that proof for the happy path (no retry); this file is that proof
//! for the path #194 was actually filed against — a confirmation-retry
//! that must not duplicate what the agent sees.
//!
//! The user decision recorded in the PRD is that this MUST cover OpenCode
//! as well as Claude: the write path is agent-agnostic by construction
//! (`b"\r"` written with no branch on `AgentType`), but CR-as-submit is
//! documented only for claude and codex (`src/agent_pty.rs:3566`,
//! `:3668`), and nothing documents how OpenCode treats a STANDALONE CR
//! arriving seconds after a separate write — a different case from the CR
//! fused to its own payload, which is what works today.
//!
//! No attempt is made to artificially force the retry path (no hook for
//! shrinking `CONFIRMATION_GRACE_PERIOD` exists, and adding one is a
//! production-code change outside this task's remit). The assertion is
//! written at the level of the OBSERVABLE CONTRACT instead, so it holds
//! whether or not a retry actually fires this run: the spawn-time pointer
//! must reach the agent through exactly ONE native prompt-submission
//! event, and that event's `user_prompt` must contain the pointer text
//! exactly ONCE — never concatenated with itself, issue #194's exact
//! observed symptom (a confirmation-retry re-writing the full prompt text
//! into a composer that already held it, so one CR submits BOTH copies as
//! one message). A real boot that is slower than `CONFIRMATION_GRACE_
//! PERIOD` (2s) — exactly what #194's own incident report describes —
//! naturally exercises the retry path with no engineering required.
//!
//! Cost note (Decision 23): one short interactive turn per agent. Both
//! cases are local-only (Decision 8 / rule 5 exception (a)): gated on the
//! `e2e` feature so CI's `cargo test-fast` never compiles this file; the
//! real-agent tier has no CI credentials, so a local run is the only way
//! to exercise it — it self-skips in CI. Flaky-tolerant (real LLM + real
//! network) per rule 4 — run once, never looped. No `[reel]` marker: this
//! is a regression proof, not a showcase.

mod common;

use std::time::Duration;

use common::TuiDeck;
use dot_agent_deck::event::{AgentType, EventType};
use spec::spec;

const CLAUDE_MODEL: &str = "claude-haiku-4-5-20251001";
const OPENCODE_MODEL: &str = "openrouter/openai/gpt-4o-mini";

/// Fixed, uniquely-named per agent so a stale pane from one case can never
/// satisfy the other. Matches this harness's other real-agent sentinels
/// (e.g. `orchestration/seed/011`'s `SEED_SENTINEL`) — fixed rather than
/// randomly generated, since uniqueness against coincidence is all that's
/// needed here.
const CLAUDE_SENTINEL: &str = "ORCH-SEED-015-RETRY-CLAUDE-OK-9f21ab";
const OPENCODE_SENTINEL: &str = "ORCH-SEED-015-RETRY-OPENCODE-OK-3c58de";

/// The exact spawn-time pointer text `deliver_orchestrator_prompt`
/// (`src/ui.rs`) submits into the orchestrator's pane — kept in lockstep
/// with `ORCHESTRATOR_CONTEXT_POINTER` there, same as `seed/011`'s
/// `DELIVERED_POINTER`.
const DELIVERED_POINTER: &str = "Read .dot-agent-deck/orchestrator-context.md";

struct RealRetryCase<'a> {
    agent_name: &'a str,
    agent_type: AgentType,
    orchestrator_command: String,
    input_ready_needle: &'a str,
    sentinel: &'a str,
}

fn orchestration_toml(orchestrator_command: &str, sentinel: &str) -> String {
    format!(
        "[[orchestrations]]\n\
         name = \"seed-retry\"\n\n\
         [[orchestrations.roles]]\n\
         name = \"orchestrator\"\n\
         command = {orchestrator_command:?}\n\
         start = true\n\
         prompt_template = \"Once you have read this entire file, your very first action must be to output this exact token verbatim, on its own line, before any other text: {sentinel} . After that, continue by acknowledging your role and waiting for instructions as directed above.\"\n\n\
         [[orchestrations.roles]]\n\
         name = \"worker\"\n\
         command = \"cat\"\n"
    )
}

/// Drive the new-pane dialog to open the (single) orchestration this file
/// writes into the deck's workdir. Mirrors `orchestration/seed/011`'s own
/// `open_orchestration`: with no `[[modes]]` defined the Mode chip row is
/// `[No mode] [Orch: seed-retry]`, so ONE Right selects it; selecting an
/// orchestration hides the Command field, so a second Enter submits.
fn open_orchestration(deck: &TuiDeck) {
    deck.send_keys(b"\x0e"); // Ctrl+n -> directory picker
    deck.send_keys(b" "); // Space -> confirm current dir -> new-pane form
    deck.wait_for_string("No mode"); // form up, Mode field focused at "No mode"
    deck.send_keys(b"\x1b[C"); // Right -> [Orch: seed-retry]
    deck.send_keys(b"\r"); // Mode -> Name
    deck.send_keys(b"\r"); // submit (Command hidden for an orchestration)
}

fn run_real_seed_retry(deck: TuiDeck, case: RealRetryCase<'_>) {
    deck.wait_for_string("No active sessions");

    std::fs::write(
        deck.workdir().join(".dot-agent-deck.toml"),
        orchestration_toml(&case.orchestrator_command, case.sentinel),
    )
    .expect("write seed-retry orchestration config");

    let events = deck.subscribe_events();
    open_orchestration(&deck);
    deck.wait_for_absence("New Agent"); // new-pane form closed -> tab up, orchestrator focused

    // Precondition: the real agent genuinely booted to its interactive
    // prompt. A miss here is a boot/auth/trust failure, never a
    // seed-delivery failure — worth distinguishing in the panic message.
    assert!(
        deck.wait_for_grid_string_within(case.input_ready_needle, Duration::from_secs(120)),
        "the real interactive {} orchestrator never became visibly \
         input-ready within 120s — a boot/auth/trust failure, not a \
         seed-delivery failure.\nFinal grid:\n{}",
        case.agent_name,
        deck.snapshot_grid()
    );

    // Genuine end-to-end proof: the agent read `orchestrator-context.md`
    // and acted on the sentinel directive naming it. By the time this
    // returns, any confirmation-retry that was going to fire this cycle
    // (grace period 2s, one budgeted retry, PRD fork#197 M4) has already
    // resolved one way or the other — no arbitrary sleep needed.
    assert!(
        deck.wait_for_stream_string_within(case.sentinel, Duration::from_secs(90)),
        "the real {} orchestrator never echoed the sentinel token {:?} within \
         90s of the spawn-time seed pointer being submitted — either the pointer \
         never reached the session, or the agent never demonstrably read \
         `.dot-agent-deck/orchestrator-context.md` and acted on it.\nFinal grid:\n{}",
        case.agent_name,
        case.sentinel,
        deck.snapshot_grid()
    );

    // The contract under test: the pointer must have reached the agent
    // through exactly ONE native prompt-submission event, with the
    // pointer text appearing exactly ONCE inside it — never a second
    // independent submit from a confirmation-retry, and never the retry
    // fusing a duplicate copy into the composer the first write already
    // populated (issue #194's exact observed symptom).
    let submissions: Vec<_> = events
        .snapshot()
        .into_iter()
        .filter(|event| {
            event.event_type == EventType::Thinking
                && event.agent_type == case.agent_type
                && event
                    .user_prompt
                    .as_deref()
                    .is_some_and(|prompt| prompt.contains(DELIVERED_POINTER))
        })
        .collect();

    assert!(
        !submissions.is_empty(),
        "the real {} orchestrator never emitted a native prompt-submission event \
         carrying the spawn-time seed pointer, even though the sentinel above \
         proves the agent read and acted on the file it names — a hook-plumbing \
         gap, not a duplication failure",
        case.agent_name
    );
    assert_eq!(
        submissions.len(),
        1,
        "the spawn-time seed pointer must be submitted through exactly ONE \
         native prompt-submission event — observed {} separate submission \
         events carrying it: {:?} (issue #194 — a confirmation-retry that \
         independently re-dispatches the pointer, rather than a single \
         genuine submit)",
        submissions.len(),
        submissions
    );
    let pointer_occurrences = submissions[0]
        .user_prompt
        .as_deref()
        .unwrap_or_default()
        .matches(DELIVERED_POINTER)
        .count();
    assert_eq!(
        pointer_occurrences, 1,
        "the single submitted prompt must contain the seed pointer text exactly \
         ONCE, not concatenated with itself — issue #194's exact observed symptom: \
         a confirmation-retry re-writing the FULL prompt text into a composer \
         that already held it, so one CR submits BOTH copies as one message. \
         observed {pointer_occurrences} occurrences in {:?}",
        submissions[0].user_prompt
    );
}

/// Scenario: Open a real orchestration whose orchestrator (start) role is a genuine interactive Haiku Claude Code process, let the daemon deliver the spawn-time seed pointer through the production `deliver_orchestrator_prompt` path with no test intervention (including any confirmation-retry that the real boot timing genuinely triggers), and assert the pointer reached the agent through exactly one native prompt-submission event containing the pointer text exactly once — never duplicated by a confirmation-retry (fork #194, fork#197 M4's decided submit-only mechanism) — before confirming the agent genuinely read the file the pointer names via its fixed sentinel token.
#[spec("orchestration/seed/015")]
#[test]
fn orchestration_seed_015_real_claude_confirmation_retry_never_duplicates_the_prompt() {
    // Decision 26 runtime-skip: a missing CLI or credentials is an
    // environmental condition, not a broken test.
    skip_unless!(common::check_claude_available());

    let deck = TuiDeck::builder()
        .with_pty_size(120, 40)
        .with_imported_claude_credentials()
        .with_claude_trust_workdir()
        .launch_with_fixture("minimal");

    run_real_seed_retry(
        deck,
        RealRetryCase {
            agent_name: "Claude Code",
            agent_type: AgentType::ClaudeCode,
            orchestrator_command: format!("claude --model {CLAUDE_MODEL} --allowedTools Bash"),
            input_ready_needle: "? for shortcuts",
            sentinel: CLAUDE_SENTINEL,
        },
    );
}

/// Scenario: Open a real orchestration whose orchestrator (start) role is a genuine interactive OpenCode process on a cheap mini model, let the daemon deliver the spawn-time seed pointer through the production `deliver_orchestrator_prompt` path with no test intervention, and assert the pointer reached the agent through exactly one native prompt-submission event containing the pointer text exactly once — the user-decided coverage gap this PRD's probe surfaced: CR-as-submit is documented only for claude/codex, and nothing documents how OpenCode treats a standalone CR arriving after a separate write — before confirming the agent genuinely read the file the pointer names via its fixed sentinel token.
#[spec("orchestration/seed/016")]
#[test]
fn orchestration_seed_016_real_opencode_confirmation_retry_never_duplicates_the_prompt() {
    skip_unless!(common::check_opencode_available());

    let deck = TuiDeck::builder()
        .with_pty_size(120, 40)
        .with_imported_opencode_credentials()
        .launch_with_fixture("minimal");

    run_real_seed_retry(
        deck,
        RealRetryCase {
            agent_name: "OpenCode",
            agent_type: AgentType::OpenCode,
            orchestrator_command: format!("opencode --model {OPENCODE_MODEL} --auto"),
            input_ready_needle: "Ask anything...",
            sentinel: OPENCODE_SENTINEL,
        },
    );
}
