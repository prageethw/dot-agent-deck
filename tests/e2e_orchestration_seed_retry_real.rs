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
//! The user decision recorded in the PRD is that this MUST cover Codex as
//! well as Claude: the write path is agent-agnostic by construction (`b"\r"`
//! written with no branch on `AgentType`), and CR-as-submit is documented for
//! claude and codex (`src/agent_pty.rs:3566`, `:3668`) — this test verifies
//! that documented behavior actually holds for the confirmation-retry's
//! STANDALONE CR, arriving seconds after a separate write, a different case
//! from the CR fused to its own payload that `:3566`/`:3668` document
//! directly. OpenCode and Pi are explicitly OUT of scope for this PRD's M4
//! real-agent verification — the user rescoped away from gating on
//! installing the OpenCode CLI and provisioning an OpenRouter key, recording
//! the standalone-CR question on those two harnesses as an accepted,
//! documented unknown rather than a verified one (see the PRD's *Decisions
//! taken* section, recorded during the OpenCode-to-Codex rescoping).
//!
//! Fork#197 M4 Part 2 shrinks `CONFIRMATION_GRACE_PERIOD` (2s production
//! default) to `CONFIRMATION_GRACE_PERIOD_OVERRIDE_MS` below via a
//! test-only, `cfg(any(test, debug_assertions))`-gated hook
//! (`confirmation_grace_period()`, `src/ui.rs`) set through
//! `TuiDeckBuilder::with_env` — but this does NOT make the confirmation-
//! retry fire deterministically, and this file does not claim it does
//! (corrected reviewer F2, PRD fork#197: it previously did). The retry is
//! gated on TWO conditions ANDed together — the grace period above, and
//! the separate fixed 500ms confirmation-retry backoff floor
//! (`send_retry_delay(1)`, `src/ui.rs`), which this override cannot touch
//! — so the earliest a retry can fire is `max(grace_period, 500ms)` ==
//! 500ms for any override at or below that, making the override a no-op
//! on timing by itself. Whether a retry actually fires is a race against
//! the confirming event (`UserPromptSubmit`/`session.prompt`), which
//! fires at SUBMISSION time — before any inference — typically a local
//! hook round trip of tens of milliseconds, so in practice confirmation
//! usually lands well inside that 500ms window and the retry usually does
//! NOT fire. Measured directly: the retry fired at +503ms in one run and
//! not at all in another. The assertions below hold identically either
//! way — the spawn-time pointer must reach the agent through exactly ONE
//! native prompt-submission event, and that event's `user_prompt` must
//! contain the pointer text exactly ONCE, never concatenated with itself
//! (issue #194's exact observed symptom) — so this file proves no
//! duplication was observed in WHICHEVER branch actually ran on a given
//! execution, not that the confirmation-retry's standalone-CR dedup path
//! was positively exercised. Making that positive proof deterministic
//! would require the backoff floor itself to become overridable, which is
//! production code and out of scope for a test-only change.
//!
//! Cost note (Decision 23): one short interactive turn per agent. Both
//! cases are local-only (Decision 8 / rule 5 exception (a)): gated on the
//! `e2e` feature so CI's `cargo test-fast` never compiles this file; the
//! real-agent tier has no CI credentials, so a local run is the only way
//! to exercise it — it self-skips in CI. Flaky-tolerant (real LLM + real
//! network) per rule 4 — run once, never looped. No `[reel]` marker: this
//! is a regression proof, not a showcase.
//!
//! PRD fork#197 M4 Part 2: both cases set
//! `DOT_AGENT_DECK_TEST_CONFIRMATION_GRACE_PERIOD_MS` (see
//! `CONFIRMATION_GRACE_PERIOD_OVERRIDE_MS` below) on the spawned binary via
//! `TuiDeckBuilder::with_env`. See the paragraph above for why this does
//! NOT make the confirmation-retry this file exists to prove non-
//! duplicating fire deterministically — the fixed 500ms
//! confirmation-retry backoff floor is unaffected by it either way.

mod common;

use std::path::Path;
use std::time::Duration;

use common::TuiDeck;
use dot_agent_deck::event::{AgentType, EventType};
use spec::spec;

const CLAUDE_MODEL: &str = "claude-haiku-4-5-20251001";

/// PATH for the spawned deck (→ daemon → agents) with the freshly-built
/// `dot-agent-deck` binary's dir prepended to the host PATH, matching
/// `codex/live/001` and `orchestration/delegate/009`'s established shape for
/// a real interactive Codex booted through this harness: the wrapper seam
/// (`dot-agent-deck wrap --agent codex -- codex …`) resolves it, while the
/// rest of the host PATH is preserved so the real `codex` binary still
/// resolves.
fn path_with_binary_dir() -> String {
    let bin = env!("CARGO_BIN_EXE_dot-agent-deck");
    let bin_dir = Path::new(bin)
        .parent()
        .expect("test binary has a parent dir")
        .to_str()
        .expect("binary directory is UTF-8");
    format!("{bin_dir}:{}", std::env::var("PATH").unwrap_or_default())
}

/// Fixed, uniquely-named per agent so a stale pane from one case can never
/// satisfy the other. Matches this harness's other real-agent sentinels
/// (e.g. `orchestration/seed/011`'s `SEED_SENTINEL`) — fixed rather than
/// randomly generated, since uniqueness against coincidence is all that's
/// needed here.
const CLAUDE_SENTINEL: &str = "ORCH-SEED-015-RETRY-CLAUDE-OK-9f21ab";
const CODEX_SENTINEL: &str = "ORCH-SEED-016-RETRY-CODEX-OK-4e91cb";

/// The exact spawn-time pointer text `deliver_orchestrator_prompt`
/// (`src/ui.rs`) submits into the orchestrator's pane — kept in lockstep
/// with `ORCHESTRATOR_CONTEXT_POINTER` there, same as `seed/011`'s
/// `DELIVERED_POINTER`.
const DELIVERED_POINTER: &str = "Read .dot-agent-deck/orchestrator-context.md";

/// PRD fork#197 M4 Part 2: override for `confirmation_grace_period()`
/// (`src/ui.rs`), forwarded to the spawned binary via
/// `DOT_AGENT_DECK_TEST_CONFIRMATION_GRACE_PERIOD_MS`. Chosen deliberately,
/// not copied from a suggestion:
///
/// - Above `SUBMIT_DELAY` (150ms, `src/pane_input.rs`, tuned against
///   claude): the ORIGINAL write's own text-then-`\r` sequence already
///   completes — including that internal delay — before
///   `AwaitingConfirmation::since` is even recorded, so this value's
///   window never overlaps the original write finishing.
/// - Below the fixed 500ms confirmation-retry backoff floor
///   (`send_retry_delay(1)`, `src/ui.rs`) that `schedule_send_retry` sets
///   after every landed write: the retry is ALSO gated on that backoff
///   regardless of this override, so anything at or below it makes 500ms
///   the binding constraint either way — never this constant. This means
///   the override CANNOT change when a retry fires, only when it becomes
///   eligible to be attempted; see the module doc above for what that
///   implies (corrected reviewer F2, PRD fork#197 — this bullet
///   previously argued the opposite, that a genuine LLM call's ~1s+
///   latency makes the retry fire deterministically before the
///   confirming event can land. That argument was a category error: the
///   confirming event is `UserPromptSubmit`/`session.prompt`, which fires
///   at SUBMISSION time, before any inference — a local hook round trip
///   of tens of milliseconds, not a full LLM turn — so it routinely beats
///   the 500ms floor and the retry routinely does not fire at all).
const CONFIRMATION_GRACE_PERIOD_OVERRIDE_MS: &str = "250";

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
    // (grace period 250ms via DOT_AGENT_DECK_TEST_CONFIRMATION_GRACE_PERIOD_MS,
    // gated together with the fixed 500ms send_retry_delay(1) backoff
    // floor — see the module doc for why that makes firing a race rather
    // than deterministic — one budgeted retry, PRD fork#197 M4) has
    // already resolved one way or the other — no arbitrary sleep needed.
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

/// Scenario: Open a real orchestration whose orchestrator (start) role is a genuine interactive Haiku Claude Code process, with the confirmation-retry grace period shrunk to 250ms via `DOT_AGENT_DECK_TEST_CONFIRMATION_GRACE_PERIOD_MS` (a no-op below the fixed 500ms retry backoff floor, so whether a retry actually fires is a race against the real confirming hook event, not a deterministic outcome — see the module doc), let the daemon deliver the spawn-time seed pointer through the production `deliver_orchestrator_prompt` path, and assert the pointer reached the agent through exactly one native prompt-submission event containing the pointer text exactly once — never duplicated by a confirmation-retry (fork #194, fork#197 M4's decided submit-only mechanism), true whichever branch actually ran — before confirming the agent genuinely read the file the pointer names via its fixed sentinel token.
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
        .with_env(
            "DOT_AGENT_DECK_TEST_CONFIRMATION_GRACE_PERIOD_MS",
            CONFIRMATION_GRACE_PERIOD_OVERRIDE_MS,
        )
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

/// Scenario: Open a real orchestration whose orchestrator (start) role is a genuine interactive Codex process on a cheap model, with the confirmation-retry grace period shrunk to 250ms via `DOT_AGENT_DECK_TEST_CONFIRMATION_GRACE_PERIOD_MS` (a no-op below the fixed 500ms retry backoff floor, so whether a retry — and therefore a standalone CR arriving seconds after a separate write — actually fires this run is a race against the real confirming hook event, not a deterministic outcome; see the module doc), let the daemon deliver the spawn-time seed pointer through the production `deliver_orchestrator_prompt` path, and assert the pointer reached the agent through exactly one native prompt-submission event containing the pointer text exactly once — true whichever branch actually ran, so this does not positively prove Codex's documented CR-as-submit behavior (`src/agent_pty.rs:3566`, `:3668`) held for a standalone retry CR specifically, only that no duplication was observed — before confirming the agent genuinely read the file the pointer names via its fixed sentinel token.
#[spec("orchestration/seed/016")]
#[test]
fn orchestration_seed_016_real_codex_confirmation_retry_never_duplicates_the_prompt() {
    skip_unless!(common::check_codex_available());

    let orchestrator_command = format!(
        "codex --model {} --sandbox workspace-write --ask-for-approval never -c 'sandbox_workspace_write.network_access=true' -c 'model_reasoning_effort=\"low\"'",
        common::codex_test_model(),
    );

    let deck = TuiDeck::builder()
        .with_pty_size(120, 40)
        .with_env("PATH", path_with_binary_dir())
        .with_imported_codex_credentials()
        .with_env(
            "DOT_AGENT_DECK_TEST_CONFIRMATION_GRACE_PERIOD_MS",
            CONFIRMATION_GRACE_PERIOD_OVERRIDE_MS,
        )
        .launch_with_fixture("minimal");

    // `input_ready_needle` is the SAME needle `codex/live/001`
    // (`tests/e2e_codex_wrapper.rs`) and `orchestration/delegate/009`
    // (`tests/e2e_codex_delegate.rs`) already rely on for a genuine
    // interactive Codex TUI, so it is not itself in question. PRD
    // fork#197, resume item 5: a prior run of this test panicked at this
    // wait with a COMPLETELY BLANK pane for the full 120s, even though the
    // daemon's own event log showed Codex had booted. The captured
    // recording (`.dot-agent-deck/recordings/orchestration_seed_016_…`)
    // showed why: Codex was alive and rendering, but stuck at its own
    // first-run "Do you trust the contents of this directory?" prompt
    // instead of its normal interactive UI — `common::import_codex_credentials`
    // seeded trust for the raw (symlinked) tempdir path only, while Codex's
    // own `getcwd()` reports the macOS-resolved `/private/var/folders/…`
    // form, so the exact-string `trust_level` lookup never matched. Fixed
    // at the source (`tests/common/mod.rs`) by trusting both the raw and
    // canonicalized path, mirroring `with_claude_trust_workdir`'s existing
    // fix for the identical class of bug — not by picking a different
    // needle here, since no needle can appear in a pane that never leaves
    // the trust prompt.
    run_real_seed_retry(
        deck,
        RealRetryCase {
            agent_name: "Codex",
            agent_type: AgentType::Codex,
            orchestrator_command,
            input_ready_needle: common::codex_test_model(),
            sentinel: CODEX_SENTINEL,
        },
    );
}
