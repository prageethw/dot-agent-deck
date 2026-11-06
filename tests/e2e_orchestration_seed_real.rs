#![cfg(feature = "e2e")]

//! L2 REAL-agent proof for the orchestrator spawn-time seed delivery
//! (upstream issue #424, CLAUDE.md rule 4). Synthetic tests can show that a seed
//! was queued and acknowledged, never that a real Claude Code process read the
//! file the seed names and acted on it. #424's own bug (a fixed 500ms readiness
//! buffer outrun by 5 MCP servers and a 10.9 KB `SessionStart` hook) could only
//! ever be caught by a test that drives a real startup.
//!
//! Upstream-sync net merge (2026-11-05): the seed is no longer typed into the
//! pane. PRD #1666 posts it as the first message of the orchestrator pane's
//! inbox and the agent takes it with `inbox fetch`, so step 1 below observes the
//! inbox (a message ACKNOWLEDGED by this very agent) instead of a native
//! `UserPromptSubmit` event carrying the old literal pointer text, which no
//! longer exists anywhere in `src/`. Step 2 is unchanged.
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
//!   1. A message in the orchestrator pane's inbox, bound to this agent and
//!      ACKNOWLEDGED, proves the spawn-time seed was genuinely consumed by
//!      the real Claude Code session (never a synthetic/fabricated event).
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
#[path = "support/inbox_view.rs"]
mod inbox_view;

use std::time::Duration;

use common::{TuiDeck, commit_fixture};
use dot_agent_deck::inbox::StateName;
use spec::spec;

/// The fixed, uniquely-named token the fixture's `prompt_template` directs
/// the orchestrator to output verbatim once it has read
/// `.dot-agent-deck/orchestrator-context.md`. Fixed rather than randomly
/// generated per run (matching this harness's other real-agent sentinels,
/// e.g. `e2e_shell_activity_real_agent.rs::SENTINEL`) — unique enough that
/// its appearance in the pane cannot be coincidental, without needing
/// runtime randomness this test has no use for.
const SEED_SENTINEL: &str = "ORCH-SEED-011-REMIT-OK-6d29f4";

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

/// Scenario: Open a real orchestration whose orchestrator (start) role is a genuine interactive Haiku Claude Code process and let the daemon deliver the spawn-time seed through the inbox with no test intervention. Assert in order that the orchestrator pane's inbox holds a message acknowledged by that agent, and that the agent then echoes a uniquely-named sentinel token its `prompt_template` directed it to output, proving it read `.dot-agent-deck/orchestrator-context.md` and acted on it.
#[spec("orchestration/seed/011")]
#[test]
fn orchestration_seed_011_real_claude_orchestrator_reads_and_acts_on_remit() {
    // Decision 26 runtime-skip: a missing CLI or credentials is an
    // environmental condition, not a broken test.
    skip_unless!(common::check_claude_available());

    let deck = TuiDeck::builder()
        .with_pty_size(120, 40)
        .with_imported_claude_credentials()
        // PRD fork#544 M2b made isolated-clone provisioning the sole spawn
        // path for every orchestration, so the orchestrator's real cwd is
        // NOT the deck's own workdir (the copied `orch-seed-remit-real`
        // fixture root) — it's an isolated-clone sibling directory
        // `with_claude_trust_workdir` also predicts and pre-trusts (fork
        // issue #373). Pre-trust both so the real claude's first-run
        // onboarding/trust gates clear with no keystroke and the spawn-time
        // seed pointer below isn't swallowed answering them.
        .with_claude_trust_workdir()
        .launch_with_fixture("orch-seed-remit-real");
    deck.wait_for_string("No active agents");

    let socket = deck.attach_socket_path().to_path_buf();

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

    let record = orchestrator_agent_record(&socket);
    let agent_id = record.id.clone();
    let pane = record
        .pane_id_env
        .clone()
        .expect("the orchestrator role pane's agent record must carry its pane id");

    // Step 1 of the contract: the spawn-time seed is a message in the
    // orchestrator pane's inbox (PRD #1666), and the real session consumed it:
    // a message bound to THIS agent reaches `Acknowledged`. The state is read
    // back from the daemon, not inferred from the pane.
    let acknowledged = common::wait_until(Duration::from_secs(60), || {
        inbox_view::messages(&socket, &pane).iter().any(|message| {
            message.state == StateName::Acknowledged
                && message.bind_agent.as_deref() == Some(agent_id.as_str())
        })
    });
    assert!(
        acknowledged,
        "the real Claude orchestrator never acknowledged a seed message in its inbox within 60s \
         of becoming input-ready; inbox = {:?}\nFinal grid:\n{}",
        inbox_view::messages(&socket, &pane),
        deck.snapshot_grid()
    );

    // Step 2 of the contract: the agent genuinely READ the file the pointer
    // named and ACTED on its content — the exact thing #424's bug proved a
    // "bytes reached the PTY" check cannot establish. Scanned over the
    // pane's cumulative output, not a single grid frame, so the token isn't
    // missed if it has already scrolled by the time this runs.
    assert!(
        deck.wait_for_stream_string_within(SEED_SENTINEL, Duration::from_secs(90)),
        "the real Claude orchestrator never echoed the sentinel token {SEED_SENTINEL:?} within \
         90s of acknowledging its seed message — the seed reached the session (see the inbox \
         assertion above) but the agent never demonstrably read \
         `.dot-agent-deck/orchestrator-context.md` and acted on it.\nFinal grid:\n{}",
        deck.snapshot_grid()
    );
}
