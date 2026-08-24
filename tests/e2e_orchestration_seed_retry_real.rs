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
//! (issue #194's exact observed symptom).
//!
//! Fork#257 M2: for `seed/015` (Claude) only, `run_real_seed_retry` hosts
//! the real agent behind `CR_SUPPRESSING_WRAPPER_PY` — a test-only PTY
//! relay that deliberately drops the ORIGINAL write's confirming CR before
//! it ever reaches the agent, so the text lands in the composer but never
//! submits. That turns "no duplication observed" into a POSITIVE proof:
//! since the original CR is proven (via an independent marker file — see
//! `run_real_seed_retry`) to have never reached the agent, any submission
//! observed can only be the confirmation-retry's own later bare CR, and the
//! retry becomes deterministic rather than racing the confirming hook event
//! (nothing can confirm the original write, so the grace period always
//! elapses and the retry always attempts). This did NOT require the backoff
//! floor to become overridable — the discriminating power comes entirely
//! from the test-harness-side relay, not from a production timing knob.
//! Confirmed on two consecutive real Claude runs (~22s each).
//!
//! Fork#257 review round: the marker used to be created inside the same
//! branch that removes the byte, so a relay mutation that stopped removing
//! it while leaving the marker-creation call intact would still pass —
//! proving only "this code path ran", not "a byte actually went missing"
//! (reviewer P1). The relay (`CR_SUPPRESSING_WRAPPER_PY`) now tracks two
//! cumulative byte counters — total bytes read from the daemon and total
//! bytes actually forwarded to the agent, on the daemon->agent direction
//! only, via a write-all loop that reports genuine completion — and writes
//! those counts, not a flag, into the marker at drop time. The assertion
//! below reads those counts and requires the difference to be exactly 1,
//! so a mutation that retains the byte is caught by the counts even if it
//! leaves every other line of the drop branch untouched. Mutation-checked
//! locally (rule 5 exception (a) — this test self-skips in CI for lack of
//! credentials, so CI cannot witness the check): reverting the byte-removal
//! slice while leaving `dropped_cr`/the marker write intact reproducibly
//! fails this assertion with a reported difference of 0. The marker
//! directory is now a fresh, private, per-run `common::harness_tempdir()`
//! rather than a fixed global path keyed only by the sentinel (P2a — two
//! concurrent runs could previously observe or delete each other's
//! marker), and the relay validates `CR_SUPPRESS_MARKER` and creates the
//! marker with `O_CREAT|O_EXCL` before ever trusting it (P2c/audit —
//! closes the symlink-race window a predictable path left open). The
//! relay's teardown now signals the agent's whole process group and waits
//! (bounded) for it to be reaped instead of signalling only the direct
//! child and `_exit`ing immediately (P2b/audit — no orphaned descendant of
//! the real agent can survive relay teardown as a live, still-running
//! process). Two bounds were added by hand while verifying this fix
//! locally, not assumed up front: the signal handler stops handling
//! SIGTERM/SIGHUP the instant one fires, before doing anything else,
//! because a second signal landing while `reap_process_tree` was still in
//! its own wait otherwise re-entered the handler and restarted cleanup on
//! top of the still-running outer call; and the final wait for the KILLed
//! child is itself bounded rather than an unconditional blocking
//! `waitpid`, because a real `claude` process was observed sitting in
//! macOS's own "trying to exit" kernel teardown for well over a minute
//! after SIGKILL — an unbounded wait would have hung the relay itself
//! indefinitely, trading the orphan this fix exists to prevent for a hung
//! test. Once both signals are sent and the bound elapses, the relay gives
//! up waiting on that specific child and exits anyway — the kill cannot be
//! un-sent, so init/launchd reparents and reaps it once the kernel
//! actually finishes. Both forwarding directions also use a
//! write-all loop with EINTR/EAGAIN handling instead of a single
//! best-effort `os.write` (P1b/audit — POSIX permits short writes to a
//! PTY, so the old code could silently drop bytes other than the intended
//! CR while still reporting success), and the real agent's argv[0] is
//! resolved to an absolute, executable path before `pty.fork()` so a
//! reordered or poisoned `PATH` cannot substitute a different binary at
//! the point the relay holds real agent credentials (audit). See
//! `tests/fixtures/cr_suppressing_wrapper.py`'s own doc comment for the
//! full mechanism.
//!
//! `seed/016` (Codex) is UNCHANGED and still only proves the older, weaker
//! claim — no duplication was observed in WHICHEVER branch actually ran,
//! not that the standalone-CR dedup path was positively exercised. The
//! relay technique was attempted against Codex and abandoned this round:
//! routing `orchestrator_command` through the relay requires bypassing
//! `wrap_launch_command`'s rewrite into `dot-agent-deck wrap --agent codex
//! -- codex …` (Codex is the one [`IntegrationStrategy::Wrapper`] agent —
//! `src/agent_registry.rs` — so unlike Claude's `NativeHooks` strategy, its
//! production path always runs through that wrapper). With the wrapper
//! bypassed, the relay verifiably forwarded the seed text and both CRs
//! byte-for-byte (confirmed via ad hoc instrumentation: exactly one text
//! chunk then two separate `\r`s over a 90s+ window, the second correctly
//! passed through) — yet Codex never visibly reacted to ANY of it: no text
//! appeared in its composer, no hook event fired, and the sentinel never
//! appeared. Codex's `Wrapper` strategy therefore provides something this
//! round did not identify beyond PTY hosting and `CODEX_HOME` pinning
//! (`run_wrap_pty`, `src/wrap.rs`) — proving the suppressed-CR scenario for
//! Codex would mean nesting the relay INSIDE `dot-agent-deck wrap`'s own
//! spawn (so the wrapper's setup stays intact) rather than bypassing it,
//! which was assessed as materially more real-agent iteration risk than
//! this round's budget covered and was not attempted. Recorded as a known
//! gap in PRD fork#257, not a silent downgrade: `seed/016`'s own doc
//! comment states exactly this limitation.
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

use common::{TuiDeck, commit_fixture};
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

/// Test-only PTY relay standing in for the real agent binary, so the
/// ORIGINAL write's confirming CR can be deliberately dropped before it ever
/// reaches the real agent — see the file's own doc comment for the
/// mechanism and why it makes the confirmation-retry's own CR the sole
/// possible cause of any observed submission. Sourced from a tracked file
/// (not inlined) so it stays readable/diffable on its own; embedded at
/// compile time and written into the fixture's workdir at runtime, mirroring
/// how `orchestration_toml` below is written dynamically rather than shipped
/// as a static fixture file.
const CR_SUPPRESSING_WRAPPER_PY: &str = include_str!("fixtures/cr_suppressing_wrapper.py");

/// Must match `.with_pty_size(120, 40)` on both cases below — passed
/// explicitly to the relay rather than queried at runtime, since it hosts
/// the real agent's inner pty before that agent can report anything itself.
const RELAY_PTY_ROWS: &str = "40";
const RELAY_PTY_COLS: &str = "120";

/// Env var the relay (`CR_SUPPRESSING_WRAPPER_PY`) reads: the path of an
/// empty marker file it creates the instant it drops the original CR.
/// Independent, externally-checkable evidence that suppression genuinely
/// engaged during THIS run, so a regression that silently stops dropping
/// the byte fails loudly (no marker) instead of quietly collapsing back to
/// today's weaker whichever-branch-ran proof while still reporting green.
const CR_SUPPRESS_MARKER_ENV: &str = "CR_SUPPRESS_MARKER";

/// The marker path must be decided BEFORE the deck launches (it rides in as
/// an env var on `TuiDeckBuilder`, which only forwards vars set before
/// `launch_with_fixture`), so it cannot live under `deck.workdir()` — that
/// path is only known afterward. Fork#257 review round (P2a/P2c): a fixed
/// global path keyed only by the per-case sentinel let two concurrent runs
/// observe or delete each other's marker, and its predictability opened a
/// same-user symlink-race window. `common::harness_tempdir()` gives each
/// call a fresh, private (0700) directory nothing else could have written
/// into before this call created it, so no stale-file removal is needed —
/// there is nothing to remove from a directory that did not exist a moment
/// ago — and the relay itself creates the marker file with `O_CREAT|O_EXCL`
/// (see `cr_suppressing_wrapper.py`), so a pre-existing object at the path
/// would make the relay fail loudly rather than accept it as evidence. The
/// returned `TempDir` guard must stay alive for the whole test — dropping
/// it early removes the directory the relay is about to write into.
fn cr_suppress_marker_dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = common::harness_tempdir().expect("create marker dir");
    let marker = dir.path().join("cr-suppress-marker");
    (dir, marker)
}

struct RealRetryCase<'a> {
    agent_name: &'a str,
    agent_type: AgentType,
    orchestrator_command: String,
    input_ready_needle: &'a str,
    sentinel: &'a str,
    /// `Some(marker_path)` routes this case's real agent through
    /// `CR_SUPPRESSING_WRAPPER_PY` for the suppressed-first-CR discriminating
    /// proof; `None` spawns `orchestrator_command` directly, unchanged from
    /// before that mechanism existed — see `orchestration_seed_016_…`'s own
    /// doc comment for why Codex stays on this path this round.
    cr_suppress_marker: Option<std::path::PathBuf>,
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
/// orchestration hides the Command field, so a second Enter submits. Commits
/// the just-written `.dot-agent-deck.toml` first (restored 2026-08-24, fork
/// issue #373): current `main`'s new-pane spawn runs isolated-clone
/// provisioning that needs a ref to branch from, which the harness's own
/// bare `git init` does not provide on its own — see
/// `common::commit_fixture`'s doc comment. Called after the config has
/// already been written into `deck.workdir()` by `run_real_seed_retry`.
fn open_orchestration(deck: &TuiDeck) {
    commit_fixture(deck.workdir());
    deck.send_keys(b"\x0e"); // Ctrl+n -> directory picker
    deck.send_keys(b" "); // Space -> confirm current dir -> new-pane form
    deck.wait_for_string("No mode"); // form up, Mode field focused at "No mode"
    deck.send_keys(b"\x1b[C"); // Right -> [Orch: seed-retry]
    deck.send_keys(b"\r"); // Mode -> Name
    deck.send_keys(b"\r"); // submit (Command hidden for an orchestration)
}

fn run_real_seed_retry(deck: TuiDeck, case: RealRetryCase<'_>) {
    deck.wait_for_string("No active sessions");

    // Host the real agent behind the CR-suppressing relay instead of
    // spawning it directly — see `CR_SUPPRESSING_WRAPPER_PY`'s doc comment
    // for the mechanism. Written into the fixture's own workdir (like
    // `.dot-agent-deck.toml` below) rather than shipped as a static fixture
    // file, since both are per-run generated config for a throwaway
    // orchestration. Only when this case opts in (`cr_suppress_marker`
    // present) — `orchestration_seed_016_…`'s own doc comment records why
    // Codex does not.
    let effective_command = match &case.cr_suppress_marker {
        Some(_) => {
            let relay_path = deck.workdir().join("cr_suppressing_wrapper.py");
            std::fs::write(&relay_path, CR_SUPPRESSING_WRAPPER_PY)
                .expect("write CR-suppressing relay");
            format!(
                "python3 {} {RELAY_PTY_ROWS} {RELAY_PTY_COLS} {}",
                relay_path.display(),
                case.orchestrator_command
            )
        }
        None => case.orchestrator_command.clone(),
    };

    std::fs::write(
        deck.workdir().join(".dot-agent-deck.toml"),
        orchestration_toml(&effective_command, case.sentinel),
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
    // and acted on the sentinel directive naming it. For a
    // `cr_suppress_marker`-carrying case, by the time this returns, the
    // ORIGINAL write's confirming CR has already been dropped by the relay
    // (it can only ever reach the agent before the sentinel does — the
    // relay drops it off the very first `\r` byte it ever sees, and that
    // byte lands well before the agent could possibly act on the prompt it
    // terminates), so any submission observed below is attributable ONLY
    // to the confirmation-retry's own bare CR. For a case with no marker
    // (Codex — see `orchestration_seed_016_…`'s doc comment), this is
    // still the older race against the real confirming hook event the
    // module doc describes — no arbitrary sleep needed either way.
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

    // Independent, externally-checkable evidence that the suppression this
    // scenario depends on genuinely engaged during THIS run, rather than
    // the assertions below merely re-observing today's older, weaker
    // "whichever branch ran" proof by coincidence. A regression that
    // silently stopped the relay from dropping the CR (or a bug that
    // routed the orchestrator command around the relay entirely) would
    // leave this marker absent while the sentinel above could still have
    // been satisfied by a genuine but UNsuppressed original submit — this
    // is what turns that into a loud failure instead of a silent loss of
    // coverage. Skipped entirely for a case with no marker (`None`), which
    // never claims the suppressed-CR proof in the first place.
    if let Some(marker) = &case.cr_suppress_marker {
        let marker_contents = std::fs::read_to_string(marker).unwrap_or_else(|err| {
            panic!(
                "the CR-suppressing relay never reported dropping the original \
                 write's CR (could not read marker file at {marker:?}: {err}) — \
                 the suppression this scenario depends on did not engage, so the \
                 assertions below would only be re-proving the old \
                 whichever-branch-ran claim, not the suppressed-CR one"
            )
        });
        // Fork#257 review P1: the marker used to be an empty flag file
        // created inside the same branch that removes the byte, so it only
        // proved "this code path ran". It now carries two cumulative byte
        // counts — total bytes read from the daemon and total bytes
        // actually forwarded to the agent (via a write-all loop that
        // reports genuine completion) — tracked independently of the
        // `dropped_cr` branch. A relay mutation that stops removing the
        // byte but leaves the marker-creation call intact still creates a
        // marker, but these counts show a difference of 0, not 1, which is
        // what this assertion — not the marker's mere existence — catches.
        let mut counts = marker_contents.split_whitespace();
        let parse_next = |counts: &mut std::str::SplitWhitespace<'_>| -> u64 {
            counts
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| {
                    panic!("malformed CR-suppress marker contents: {marker_contents:?}")
                })
        };
        let bytes_from_daemon = parse_next(&mut counts);
        let bytes_to_agent = parse_next(&mut counts);
        assert_eq!(
            bytes_from_daemon.saturating_sub(bytes_to_agent),
            1,
            "the relay's marker reports {bytes_from_daemon} bytes read from the \
             daemon and {bytes_to_agent} bytes actually forwarded to the agent \
             at the moment it dropped the CR — these are independent byte \
             counters derived from what was actually read and actually \
             written, not the `dropped_cr` flag or the marker's mere \
             existence, so a regression that stops removing the byte (even \
             one that still sets the flag and still creates a marker) shows \
             a difference of 0 here, not 1, which is what this assertion \
             catches"
        );
        let _ = std::fs::remove_file(marker);
    }

    // The contract under test: for a `cr_suppress_marker`-carrying case,
    // because the relay proved above that the ORIGINAL write's CR never
    // reached the agent, the pointer's text could only have been submitted
    // by the confirmation-retry's own later, bare CR landing on a composer
    // the first write had already populated — so observing the pointer
    // submitted through exactly ONE native prompt-submission event, with
    // the pointer text appearing exactly ONCE inside it, is a positive
    // proof that the retry fired and that its CR is what caused the
    // submission. For a case with no marker, this only re-proves the
    // older, weaker claim: no duplication was observed in whichever branch
    // actually ran (fork#197 M4, issue #194 either way — never a second
    // independent submit, and never the retry fusing a duplicate copy into
    // the composer the first write already held).
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
         genuine submit; for a `cr_suppress_marker`-carrying case, the \
         marker check above already proved the ORIGINAL write's CR never \
         reached the agent, so an extra submission here cannot be the \
         original either — it would be a second, unexplained delivery)",
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

/// Scenario: Open a real orchestration whose orchestrator (start) role is a genuine interactive Haiku Claude Code process hosted behind a test-only CR-suppressing relay (`CR_SUPPRESSING_WRAPPER_PY`) that drops the ORIGINAL spawn-time write's confirming CR before it reaches the agent, so the seed text lands in the composer but never submits; confirm via a per-run private marker file — whose content is two independent cumulative byte counts (bytes read from the daemon, bytes actually forwarded to the agent) rather than a flag — that the drop genuinely happened and that exactly one byte went missing, then let the daemon's confirmation-retry (grace period shrunk to 250ms via `DOT_AGENT_DECK_TEST_CONFIRMATION_GRACE_PERIOD_MS`, now firing deterministically since nothing can ever confirm the suppressed original) send its own bare CR into that already-populated composer, and assert the pointer reached the agent through exactly one native prompt-submission event containing the pointer text exactly once — a positive proof that the retry's CR, not the original write's, is what caused the submission (fork #194, fork#197 M4's decided submit-only mechanism) — before confirming the agent genuinely read the file the pointer names via its fixed sentinel token.
#[spec("orchestration/seed/015")]
#[test]
fn orchestration_seed_015_real_claude_confirmation_retry_never_duplicates_the_prompt() {
    // Decision 26 runtime-skip: a missing CLI or credentials is an
    // environmental condition, not a broken test.
    skip_unless!(common::check_claude_available());

    let (_cr_suppress_marker_dir, cr_suppress_marker) = cr_suppress_marker_dir();
    let deck = TuiDeck::builder()
        .with_pty_size(120, 40)
        .with_imported_claude_credentials()
        .with_claude_trust_workdir()
        .with_env(
            "DOT_AGENT_DECK_TEST_CONFIRMATION_GRACE_PERIOD_MS",
            CONFIRMATION_GRACE_PERIOD_OVERRIDE_MS,
        )
        .with_env(
            CR_SUPPRESS_MARKER_ENV,
            cr_suppress_marker.to_str().expect("marker path is UTF-8"),
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
            cr_suppress_marker: Some(cr_suppress_marker),
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
            cr_suppress_marker: None,
        },
    );
}
