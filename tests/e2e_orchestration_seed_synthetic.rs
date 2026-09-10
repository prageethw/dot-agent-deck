#![cfg(feature = "e2e")]
#![cfg(unix)]

//! Issue #737: `deliver_orchestrator_prompt`'s spawn-time readiness gate
//! (`src/ui.rs`) writes the orchestrator's one-shot seed prompt as soon as
//! `agent_ready` is true — and `agent_ready` is satisfied by ANY event that
//! sets the pane's observed `agent_type`:
//!
//! ```ignore
//! let agent_ready = snapshot.sessions.values().any(|s| {
//!     s.pane_id.as_deref() == Some(start_pane_id.as_str()) && s.agent_type != AgentType::None
//! });
//! ```
//!
//! For a Codex-identity pane spawned through `dot-agent-deck wrap`, that
//! includes the wrapper's own fork-time `SessionStart`
//! (`Emitter::emit_fork_session_start`, `src/wrap.rs`), sent the instant
//! `cmd.spawn()` returns and explicitly documented there as "a
//! CARD-SURFACING signal, not a readiness signal" — the child may still be a
//! launcher for seconds. `state.rs::apply_event` applies that event's
//! `agent_type` unconditionally (it only excludes wrapper-origin starts from
//! *moving* an already-established pane generation, not from setting
//! `agent_type` in the first place), so `agent_ready` can go true within
//! milliseconds of spawn. `deliver_orchestrator_prompt` then waits only
//! `SPAWN_TIME_READINESS_BUFFER` (500ms, `src/ui.rs`) before writing — far
//! short of a real agent's startup gap, which the DAEMON-owned delegate path
//! already prices separately and much higher for exactly this wrapper fact
//! (`WRAPPER_INTERFACE_READINESS_BUFFER`, 5000ms, `src/state.rs`, issue
//! #243). That fix was never ported to this TUI-owned spawn-time path.
//!
//! Because fork #194 / issue #424 write the seed payload exactly ONCE
//! (`attempt_writes_payload`, `MAX_PAYLOAD_SUBMISSIONS`), a write lost to
//! this race is lost for the life of the pane — every later attempt only
//! probes for confirmation with a bare submit, never retypes the text. The
//! result is exactly issue #737's report: a healthy, idle Codex process that
//! has never received a single byte on stdin.
//!
//! `codex_delayed_standin.py` (embedded below, written into the fixture's
//! own workdir at runtime — the same pattern
//! `e2e_orchestration_seed_retry_real.rs` uses for `cr_suppressing_wrapper.py`)
//! stands in for a Codex TUI whose own input-ready transition lands
//! measurably later than the wrapper's fork-time report: it does not touch
//! stdin at all for `STANDIN_READY_DELAY_MS`, then explicitly discards
//! whatever queued on it before reading — mirroring a real TUI's raw-mode
//! transition, which is what makes a too-early write genuinely LOST rather
//! than merely delayed (see the fixture's own doc comment, and
//! `WRAPPER_INTERFACE_READINESS_BUFFER`'s: "written before [the repaint],
//! the payload is gone — not parked, gone"). Only a genuinely-read,
//! non-empty line makes it behave like an agent that received a prompt.
//!
//! `seed/019`'s delay (3s) only has to outlast `SPAWN_TIME_READINESS_BUFFER`
//! (500ms) after the wrapper's fork-time fact — confirmed above to be the
//! fact that actually satisfies `agent_ready` on this path, unlike the
//! bare/undetected launch issue #737's own report used, where no wrapper
//! ever runs and `agent_ready` can structurally never go true at all. The
//! first confirmation-retry floor is pushed out to its maximum test override
//! (`DOT_AGENT_DECK_TEST_UNCONFIRMED_RETRY_BASE_MS=15000`, issue #531) so a
//! later bare-CR probe — which this test is not about — cannot land inside
//! the assertion window and be mistaken for the lost original write.
//! `seed/020` is the control: an (almost) immediate delay proves the SAME
//! harness delivers cleanly when the timing gap this issue is about is not
//! present, isolating the failure to the timing race rather than some other
//! harness defect.

mod common;

use std::time::Duration;

use common::TuiDeck;
use dot_agent_deck::event::{AgentType, EventType};
use spec::spec;

const CODEX_DELAYED_STANDIN_PY: &str = include_str!("fixtures/codex_delayed_standin.py");

/// The exact spawn-time pointer text `deliver_orchestrator_prompt`
/// (`src/ui.rs`) submits into the orchestrator's pane — kept in lockstep
/// with `ORCHESTRATOR_CONTEXT_POINTER` there, same constant
/// `e2e_orchestration_seed_retry_real.rs`'s `DELIVERED_POINTER` pins.
const DELIVERED_POINTER: &str = "Read .dot-agent-deck/orchestrator-context.md";

const STANDIN_READY_MARKER: &str = "STANDIN-READY";

/// Env var the stand-in (`codex_delayed_standin.py`) reads for where to log
/// the (first) line it genuinely reads from stdin. Must be an ABSOLUTE path:
/// the orchestrator role's command runs inside an isolated-clone-provisioned
/// worktree (`open_orchestration`'s doc comment), not the harness's own
/// `deck.workdir()`, so a cwd-relative log file would land somewhere this
/// test can never find it. Mirrors `e2e_orchestration_seed_retry_real.rs`'s
/// `CR_SUPPRESS_MARKER_ENV` / `cr_suppress_marker_dir` for the identical
/// reason.
const STANDIN_LOG_PATH_ENV: &str = "STANDIN_LOG_PATH";

/// PATH for the spawned deck (→ daemon → wrap → stand-in) with the freshly
/// built `dot-agent-deck` binary's dir prepended to the host PATH — the
/// wrapper seam (`dot-agent-deck wrap --agent codex -- python3 …`) resolves
/// it, and the rest of the host PATH is preserved so `python3` still
/// resolves. Mirrors `codex/wrap/001`'s own `path_with_binary_dir`.
fn path_with_binary_dir() -> String {
    let bin = env!("CARGO_BIN_EXE_dot-agent-deck");
    let bin_dir = std::path::Path::new(bin)
        .parent()
        .expect("test binary has a parent dir")
        .to_str()
        .expect("binary directory is UTF-8");
    format!("{bin_dir}:{}", std::env::var("PATH").unwrap_or_default())
}

/// One orchestrator role (the Codex stand-in, `start = true`) plus one inert
/// `worker` role — only the orchestrator's spawn-time delivery is under
/// test. Mirrors `e2e_orchestration_seed_retry_real.rs`'s `orchestration_toml`
/// shape (a `[[orchestrations]]` config written dynamically into the deck's
/// workdir over the `minimal` fixture's own). `command` is Debug-formatted
/// (`{command:?}`) exactly like that file's `orchestration_toml` does for its
/// own dynamically-built command string, so the ABSOLUTE stand-in path it
/// embeds round-trips through TOML's basic-string escaping intact.
fn orchestration_toml(command: &str) -> String {
    format!(
        "[[orchestrations]]\n\
         name = \"delayed-seed\"\n\n\
         [[orchestrations.roles]]\n\
         name = \"orchestrator\"\n\
         command = {command:?}\n\
         start = true\n\
         prompt_template = \"Acknowledge your role and wait for instructions.\"\n\n\
         [[orchestrations.roles]]\n\
         name = \"worker\"\n\
         command = \"cat\"\n"
    )
}

/// Write the stand-in script into the harness's own `deck.workdir()` and
/// return the shell command that runs it through the real wrapper, addressed
/// by ABSOLUTE path — same reasoning as `STANDIN_LOG_PATH_ENV` above: the
/// orchestrator role's own cwd is an isolated clone, not `deck.workdir()`,
/// so a bare relative filename would not resolve there.
fn write_standin_and_build_command(deck: &TuiDeck) -> String {
    let standin_path = deck.workdir().join("codex_delayed_standin.py");
    std::fs::write(&standin_path, CODEX_DELAYED_STANDIN_PY).expect("write delayed Codex stand-in");
    format!(
        "dot-agent-deck wrap --agent codex -- python3 {}",
        standin_path.display()
    )
}

/// Drive the new-pane dialog to open the (single) orchestration this file
/// writes into the deck's workdir. Mirrors
/// `e2e_orchestration_seed_retry_real.rs`'s own `open_orchestration`: with no
/// `[[modes]]` defined the Mode chip row is `[No mode] [Orch: delayed-seed]`,
/// so ONE Right selects it; selecting an orchestration hides the Command
/// field, so a second Enter submits. Commits the just-written
/// `.dot-agent-deck.toml` first — current `main`'s new-pane spawn runs
/// isolated-clone provisioning that needs a ref to branch from.
fn open_orchestration(deck: &TuiDeck) {
    common::commit_fixture(deck.workdir());
    deck.send_keys(b"\x0e"); // Ctrl+n -> directory picker
    deck.send_keys(b" "); // Space -> confirm current dir -> new-pane form
    deck.wait_for_string("No mode"); // form up, Mode field focused at "No mode"
    deck.send_keys(b"\x1b[C"); // Right -> [Orch: delayed-seed]
    deck.send_keys(b"\r"); // Mode -> Name
    deck.send_keys(b"\r"); // submit (Command hidden for an orchestration)
}

/// Scenario: Open an orchestration whose orchestrator (start) role is a real
/// `dot-agent-deck wrap --agent codex` process around a deterministic stand-in
/// deliberately deaf to stdin for 3 seconds — well past
/// `SPAWN_TIME_READINESS_BUFFER` (500ms) — then, mirroring a real Codex TUI's
/// raw-mode transition, explicitly discards whatever accumulated on stdin
/// before ever reading. Confirm the stand-in genuinely reached that flush
/// point, then assert the deck's one-shot spawn-time seed write never reaches
/// it (no stdin log is ever written) and no GENUINE turn is ever confirmed
/// (no `Idle` AgentEvent, which can only follow the stand-in's own
/// `turn.completed` JSONL, itself only printed after a real non-empty stdin
/// read) — issue #737's reported symptom: a healthy, idle Codex process that
/// never received a single byte of input, reproduced from the specific
/// too-early-write mechanism this file's module doc identifies. See the
/// investigation note inline below for why this test does NOT assert on the
/// mere absence of a "Thinking" status — that status is independently, and
/// legitimately, sometimes triggered by this synthetic harness itself.
#[spec("orchestration/seed/019")]
#[test]
fn orchestration_seed_019_wrap_fork_time_readiness_loses_a_slow_codex_seed_prompt() {
    // Independent, per-test private directory for the stand-in's stdin log —
    // see `STANDIN_LOG_PATH_ENV`'s doc comment for why this cannot be a path
    // relative to `deck.workdir()`. Must stay alive for the whole test (like
    // `e2e_orchestration_seed_retry_real.rs`'s own marker dir) — dropping it
    // early removes the directory the stand-in is about to write into.
    let (_log_dir, log_path) = {
        let dir = common::harness_tempdir().expect("create stand-in log dir");
        let path = dir.path().join("standin-input.log");
        (dir, path)
    };

    let deck = TuiDeck::builder()
        .with_pty_size(120, 40)
        .with_env("PATH", path_with_binary_dir())
        .with_env("STANDIN_READY_DELAY_MS", "3000")
        .with_env(
            STANDIN_LOG_PATH_ENV,
            log_path.to_str().expect("log path is UTF-8"),
        )
        // Issue #531 seam: push the first confirmation-retry probe (which
        // would otherwise land ~10s after the lost write, still inside a
        // slow CI run's assertion window) out to its maximum override, so a
        // later bare-CR probe cannot land inside this test's window and be
        // mistaken for the original payload surviving.
        .with_env("DOT_AGENT_DECK_TEST_UNCONFIRMED_RETRY_BASE_MS", "15000")
        .launch_with_fixture("minimal");

    deck.wait_for_string("No active sessions");
    let events = deck.subscribe_events();

    let command = write_standin_and_build_command(&deck);
    std::fs::write(
        deck.workdir().join(".dot-agent-deck.toml"),
        orchestration_toml(&command),
    )
    .expect("write delayed-seed orchestration config");

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent");

    // Precondition: the stand-in genuinely reached its own "about to become
    // ready" flush point. A miss here is a spawn/harness failure, never a
    // delivery-timing failure — worth distinguishing in the panic message.
    assert!(
        deck.wait_for_grid_string_within(STANDIN_READY_MARKER, Duration::from_secs(20)),
        "the delayed Codex stand-in never reached its own ready point within \
         20s of a 3s configured delay — a harness/spawn failure, not the \
         delivery-timing bug this test exists to catch:\n{}",
        deck.snapshot_grid()
    );

    // The core assertion (issue #737): the deck's one-shot seed write landed
    // while the stand-in was still asleep and was genuinely discarded —
    // exactly as a real Codex's own raw-mode transition would discard it —
    // so the stand-in's stdin log is never created at all.
    assert!(
        !log_path.exists(),
        "issue #737 did NOT reproduce: the delayed Codex stand-in's stdin \
         log exists at {log_path:?}, meaning the deck's seed write reached \
         it despite the stand-in staying deaf to stdin for 3s — either the \
         one-shot write landed later than expected or something else in the \
         harness changed; contents: {:?}; final grid:\n{}",
        std::fs::read_to_string(&log_path),
        deck.snapshot_grid()
    );

    // INVESTIGATION NOTE (issue #737 harness false-positive, found when this
    // test first went to CI): an earlier version of this test also asserted
    // that "Thinking" never appears at all — neither on the rendered grid nor
    // as an AgentEvent — for the delayed role. That assertion was ITSELF a
    // false positive and had to be dropped, for a reason independent of
    // issue #737: `codex_delayed_standin.py` must write its own
    // `STANDIN-READY` marker to stdout (see above) so this test can observe,
    // from outside the process, that the stand-in genuinely reached its
    // ready point — there is no other way to confirm that. But
    // `dot-agent-deck wrap`'s `classify_and_emit` (`src/wrap.rs`) tees EVERY
    // line of a Codex-identity child's stdout through a text classifier, and
    // for this synthetic setup `suppress_text_status` is `false`:
    // `codex_spawn_prep` installs and trusts the deck's native Codex hooks
    // (it fires for any Codex-identity pane, `program_is_codex(program) ||
    // pane_id.is_some()`), but this stand-in is a plain Python script that
    // never actually invokes those hooks the way real `codex-cli` would, so
    // no native `UserPromptSubmit`/`Stop` event ever arrives to make the
    // classifier stand down. With suppression off, `classify_line_with`'s
    // generic non-JSON fallback ("any other non-blank output is substantive
    // activity") fires on the READY_MARKER line itself and reaches the
    // daemon as a genuine `Thinking` AgentEvent — despite that line, by
    // construction, predating the stand-in's own stdin read. This is real,
    // independent, and already documented as an accepted tradeoff (the
    // `CODEX` ruleset's own "Accepted risk" doc comment, `src/wrap.rs`) — it
    // is not a reproduction of issue #737, and it is not fixable in the
    // fixture without losing the grid-visible readiness check above (ANY
    // non-blank line the stand-in must emit before genuinely blocking would
    // trip the same fallback). So this test tolerates a stray `Thinking` and
    // instead asserts on a signal that fallback classification cannot
    // produce: `Idle` here is only ever emitted from the stand-in's own
    // JSONL (`classify_codex_json`'s `"turn.completed"` -> `Idle` mapping),
    // which the stand-in only prints after it has genuinely read a
    // non-empty stdin line — exactly the fact `!log_path.exists()` above
    // already establishes never happened. A confirmed `Idle` for this pane
    // would mean the seed write DID land; its absence, together with the
    // never-created stdin log, is the real proof issue #737's loss
    // reproduced.
    assert!(
        events
            .try_wait_for(
                |event| event.agent_type == AgentType::Codex && event.event_type == EventType::Idle,
                Duration::from_secs(4),
            )
            .is_none(),
        "issue #737 did NOT reproduce: the delayed Codex role's stand-in \
         reported a genuinely completed turn (an Idle AgentEvent) even \
         though its stdin log was never created — Idle can only follow the \
         stand-in reading a real, non-empty stdin line: {:?}\nfinal grid:\n{}",
        events.snapshot(),
        deck.snapshot_grid()
    );
}

/// Scenario: The SAME setup as `orchestration/seed/019`, but the stand-in's
/// readiness delay is (near) zero, so it is already blocked reading stdin
/// well before the deck's spawn-time write can land. Assert the write DOES
/// reach it (the stdin log is created and contains the exact seed pointer
/// text) and the role DOES visibly start and finish a turn (Thinking then
/// Idle, both on screen and on the daemon's own event stream) — the control
/// that isolates `seed/019`'s failure to the timing gap specifically, rather
/// than to some other defect in this synthetic harness.
#[spec("orchestration/seed/020")]
#[test]
fn orchestration_seed_020_wrap_immediate_readiness_delivers_the_seed_prompt() {
    let (_log_dir, log_path) = {
        let dir = common::harness_tempdir().expect("create stand-in log dir");
        let path = dir.path().join("standin-input.log");
        (dir, path)
    };

    let deck = TuiDeck::builder()
        .with_pty_size(120, 40)
        .with_env("PATH", path_with_binary_dir())
        .with_env("STANDIN_READY_DELAY_MS", "0")
        .with_env(
            STANDIN_LOG_PATH_ENV,
            log_path.to_str().expect("log path is UTF-8"),
        )
        .launch_with_fixture("minimal");

    deck.wait_for_string("No active sessions");
    let events = deck.subscribe_events();

    let command = write_standin_and_build_command(&deck);
    std::fs::write(
        deck.workdir().join(".dot-agent-deck.toml"),
        orchestration_toml(&command),
    )
    .expect("write delayed-seed orchestration config");

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent");

    assert!(
        deck.wait_for_grid_string_within(STANDIN_READY_MARKER, Duration::from_secs(15)),
        "the immediate-readiness Codex stand-in never reached its own ready \
         point — a harness/spawn failure unrelated to delivery timing:\n{}",
        deck.snapshot_grid()
    );

    assert!(
        common::wait_for_file_substr_count(
            &log_path,
            DELIVERED_POINTER,
            1,
            Duration::from_secs(15)
        ),
        "the immediate-readiness Codex stand-in never received the spawn-time \
         seed pointer at all — this control is supposed to prove the harness \
         delivers cleanly with no timing gap present; observed log contents: \
         {:?}; final grid:\n{}",
        std::fs::read_to_string(&log_path),
        deck.snapshot_grid()
    );

    assert!(
        deck.wait_for_grid_string_within("Thinking", Duration::from_secs(10)),
        "the immediate-readiness Codex role never visibly entered Thinking \
         even though its stdin log shows the seed pointer arrived:\n{}",
        deck.snapshot_grid()
    );
    events.wait_for(
        |event| event.agent_type == AgentType::Codex && event.event_type == EventType::Thinking,
        Duration::from_secs(10),
    );

    assert!(
        deck.wait_for_grid_string_within("Idle", Duration::from_secs(10)),
        "the immediate-readiness Codex role never visibly completed its \
         turn:\n{}",
        deck.snapshot_grid()
    );
    events.wait_for(
        |event| event.agent_type == AgentType::Codex && event.event_type == EventType::Idle,
        Duration::from_secs(10),
    );
}
