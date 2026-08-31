#![cfg(all(feature = "e2e", feature = "e2e-live"))]

//! PTY-attached Codex wrapper coverage for PRD #20 M7. The synthetic case pins
//! deterministic plumbing; the real case runs a cheap Codex model against a
//! uniquely named fixture sentinel. Both assert the user-visible dashboard.

mod common;

use std::time::Duration;

use common::TuiDeck;
use dot_agent_deck::event::{
    AGENT_EVENT_SCHEMA_VERSION, AgentType, EventType, LiveTarget, SendResult, TargetKind, Writable,
};
use spec::spec;

const SENTINEL_NAME: &str = "codex_sentinel_a7c91f.txt";
const INTERACTIVE_PROOF_NAME: &str = "codex-interactive-proof.txt";

fn path_with_binary_dir() -> String {
    let bin = env!("CARGO_BIN_EXE_dot-agent-deck");
    let bin_dir = std::path::Path::new(bin)
        .parent()
        .expect("test binary has a parent dir")
        .to_str()
        .expect("binary directory is UTF-8");
    format!("{bin_dir}:{}", std::env::var("PATH").unwrap_or_default())
}

/// Scenario: Restore a pane running `dot-agent-deck wrap --agent codex` around
/// a deterministic shell stand-in that emits realistic Codex JSONL turn-start
/// and turn-completed records. Subscribe to the real daemon event stream and
/// detach to the dashboard; events must carry the Codex identity and schema
/// version while the visible card moves Thinking → Idle and reads `Codex`.
/// Send identity-bound input and require it to reach the wrapped child exactly once.
#[spec("codex/wrap/001")]
#[test]
fn codex_wrap_001_synthetic_jsonl_reaches_dashboard() {
    let command = "dot-agent-deck wrap --agent codex -- /bin/sh codex-standin.sh";
    let deck = TuiDeck::builder()
        .with_pty_size(180, 45)
        .with_env("PATH", path_with_binary_dir())
        .with_continue_session("", command)
        .launch_with_fixture("codex-synthetic");

    deck.wait_for_string("[Command Mode Ctrl+D]");
    let events = deck.subscribe_events();
    deck.send_bytes(b"\x04");
    deck.wait_for_string("Dir:");

    let working = events.wait_for(
        |event| event.agent_type == AgentType::Codex && event.event_type == EventType::Thinking,
        Duration::from_secs(15),
    );
    assert_eq!(working.schema_version, Some(AGENT_EVENT_SCHEMA_VERSION));
    assert_eq!(working.agent_type, AgentType::Codex);
    assert_eq!(
        working.live_target,
        Some(LiveTarget {
            kind: TargetKind::Pty,
            writable: Writable::Live,
        }),
        "a wrapper running inside a daemon-managed pane is backed by that live PTY, not a standalone history-only process"
    );
    let pane_id = working
        .pane_id
        .as_deref()
        .expect("managed wrapper event carries its pane id");
    // Issue #494: `compute_write_and_submit_outcome`'s paned branch fails
    // closed on a missing `expected_agent_id` (and, when the dashboard's
    // embedded live preview is attached to this pane, on a session mismatch
    // too). A legitimate dashboard write carries both, mirroring exactly how
    // `daemon_client::write_and_submit_with_identity` rides them alongside
    // the base `write-and-submit` shape as additive JSON keys — this proves a
    // real identity-bearing write still succeeds under the new gate, rather
    // than proving the old permissive bare-2-field shape used to work.
    let response = common::attach_json_request_on(
        deck.attach_socket_path(),
        &serde_json::json!({
            "op": "write-and-submit",
            "pane_id": pane_id,
            "text": "MANAGED-WRAPPER-WRITE",
            "expected_agent_id": working.agent_id.as_deref().expect(
                "managed wrapper event carries its agent id"
            ),
            "expected_session_id": working.session_id,
        }),
    )
    .expect("write through managed wrapper pane");
    assert_eq!(
        response.send_result,
        Some(SendResult::Applied),
        "dashboard writes to a managed wrapped Codex pane must be applied to its live PTY"
    );
    assert!(
        common::wait_for_file_substr_count(
            &deck.workdir().join("managed-wrapper-input.log"),
            "MANAGED-WRAPPER-WRITE",
            1,
            Duration::from_secs(15),
        ),
        "the managed wrapper declared Live but the submitted write never reached its child"
    );
    assert!(
        deck.wait_for_stream_string_within("Thinking", Duration::from_secs(10)),
        "the wrapped Codex card never visibly entered Thinking:\n{}",
        deck.snapshot_grid()
    );
    // No agent-type badge to key off anymore; the sole restored pane's
    // retained identity text is its daemon-minted pane id, truncated to the
    // dashboard's 11-char `id_display` prefix (`mint_pane_id`'s
    // "pane-<16-hex-nonce>-<seq>" shape is always longer than that, so the
    // truncation is exercised on every run, not just when it happens to be).
    let id_display = pane_id.get(..11).unwrap_or(pane_id);
    assert!(
        deck.wait_for_grid_string_within(id_display, Duration::from_secs(10)),
        "the live dashboard card did not show its pane identity ({id_display}):\n{}",
        deck.snapshot_grid()
    );

    let idle = events.wait_for(
        |event| event.agent_type == AgentType::Codex && event.event_type == EventType::Idle,
        Duration::from_secs(15),
    );
    assert_eq!(idle.schema_version, Some(AGENT_EVENT_SCHEMA_VERSION));
    assert!(
        deck.wait_for_grid_string_within("Idle", Duration::from_secs(10)),
        "the wrapped Codex card never visibly completed its turn:\n{}",
        deck.snapshot_grid()
    );
}

/// Scenario: With Codex authentication copied into an isolated HOME, submit a
/// bare interactive `codex` command through the normal Ctrl+N new-pane flow on
/// the cheap model, type a prompt into its live pane, and wait for it to create a
/// proof file naming the fixture sentinel. Detach to the dashboard and observe
/// the automatically wrapped Codex card transition visibly from Thinking to Idle.
#[spec("codex/live/001")]
#[test]
fn codex_live_001_real_interactive_new_pane_runs_and_reports_status() {
    skip_unless!(common::check_codex_available());

    let prompt = format!(
        "Use the shell to list the current directory and confirm {SENTINEL_NAME} exists. Then write exactly {SENTINEL_NAME} followed by a newline to {INTERACTIVE_PROOF_NAME}. Do not modify any other file."
    );
    let command = format!(
        "codex --model {} --sandbox workspace-write --ask-for-approval never -c 'model_reasoning_effort=\"low\"'",
        common::codex_test_model(),
    );
    let config_dir = common::harness_tempdir().expect("Codex new-pane config");
    let config_path = config_dir.path().join("config.toml");
    std::fs::write(&config_path, format!("default_command = {command:?}\n"))
        .expect("write bare Codex default command");
    let deck = TuiDeck::builder()
        .with_pty_size(180, 45)
        .with_env("PATH", path_with_binary_dir())
        .with_env("DOT_AGENT_DECK_CONFIG", config_path.to_string_lossy())
        .with_imported_codex_credentials()
        .launch_with_fixture("codex-live");

    deck.wait_for_string("No active sessions");
    let events = deck.subscribe_events();
    deck.send_keys(b"\x0e");
    deck.wait_for_string("Select Directory");
    deck.send_keys(b" ");
    deck.wait_for_string("Tab: switch");
    deck.send_keys(b"\r");
    deck.send_keys(b"\r");
    deck.send_keys(b"\r");
    deck.wait_for_string("[Command Mode Ctrl+D]");
    assert!(
        deck.wait_for_grid_string_within(common::codex_test_model(), Duration::from_secs(30)),
        "the bare interactive Codex UI never became ready in the new pane:\n{}",
        deck.snapshot_grid()
    );
    deck.send_keys(prompt.as_bytes());
    deck.wait_for_string(SENTINEL_NAME);
    deck.send_keys(b"\r");

    let thinking = events.wait_for(
        |event| event.agent_type == AgentType::Codex && event.event_type == EventType::Thinking,
        Duration::from_secs(120),
    );
    assert_eq!(thinking.agent_type, AgentType::Codex);
    // Polled on CONTENT, not existence: a shell redirect creates the proof file
    // before it writes into it, so waiting only for the path to appear can read
    // an empty string (PRD #225). Same exact-match semantics as before —
    // trimmed contents must equal the sentinel name.
    //
    // Bounded at 120s, not the old 180s: 180s IS nextest's kill window for the
    // whole test, so a wait that long can never actually elapse — the harness
    // kills the test first and the diagnostics below are never printed. 120s
    // leaves headroom for this assertion to fail *with* its message (measured
    // in isolation: the whole test is ~16s).
    let proof_path = deck.workdir().join(INTERACTIVE_PROOF_NAME);
    if let Err(observed) =
        common::wait_for_file_trimmed_eq(&proof_path, SENTINEL_NAME, Duration::from_secs(120))
    {
        panic!(
            "interactive Codex never completed the requested shell work; observed: {observed}; \
             final grid:\n{}",
            deck.snapshot_grid()
        );
    }
    deck.send_keys(b"/exit");
    deck.wait_for_string("/exit");
    deck.send_keys(b"\r");
    let idle = events.wait_for(
        |event| event.agent_type == AgentType::Codex && event.event_type == EventType::Idle,
        Duration::from_secs(120),
    );
    assert_eq!(idle.agent_type, AgentType::Codex);

    deck.send_bytes(b"\x04");
    deck.wait_for_string("Dir:");
    // No agent-type badge to key off anymore; with no form-supplied pane name,
    // `resolve_display_name` falls back to the literal spawned command, so the
    // card's identity text is the `codex …` invocation itself.
    assert!(
        deck.wait_for_grid_string_within("codex", Duration::from_secs(30)),
        "the automatically wrapped interactive session never rendered a card for its command:\n{}",
        deck.snapshot_grid()
    );
    assert!(
        deck.wait_for_grid_string_within("Idle", Duration::from_secs(30)),
        "the live interactive Codex card never visibly completed its turn:\n{}",
        deck.snapshot_grid()
    );
}

/// Scenario: With Codex authentication copied into an isolated HOME, submit a
/// bare interactive `codex` command through the normal Ctrl+N new-pane flow on
/// the cheap model, send a minimal reply-only prompt (no shell tool use, no
/// sentinel-file proof), and subscribe to the real daemon event stream. The
/// wait requires a captured `AgentEvent` with BOTH `model.is_some()` AND
/// `live_target.is_some()` — the `live_target` clause matters because it is
/// what disambiguates a wrap-emitted event from Codex's native hook path
/// (which always reports `live_target: None`); without it the wait could be
/// satisfied by the hook path instead, making the test pass without
/// exercising wrap at all. Only an event meeting both clauses can then be
/// asserted to carry the real model it is running — proving Codex's model
/// reporting reaches the daemon end to end through a genuine spawn via wrap,
/// not from byte-frozen capture data pinned to one Codex TUI rendering.
/// Deliberately does not gate on a `Thinking`/`Idle` transition: on a trusted
/// session, those depend on Codex's own native hooks firing inside the
/// interactive TUI, a separate, already-documented gap
/// (`common::codex_test_model`'s doc comment) unrelated to this issue — the
/// model carrier this test asserts on is the status-neutral event #652/#657
/// built specifically so model reporting does not depend on that hook path at
/// all.
#[spec("codex/live/002")]
#[test]
fn codex_live_002_real_interactive_session_reports_active_model() {
    skip_unless!(common::check_codex_available());

    const PROMPT_MARKER: &str = "codex_live_002_model_probe_5d13af";
    let prompt = format!(
        "This is prompt marker {PROMPT_MARKER}. Reply with exactly the single word acknowledged \
         and do nothing else. Do not use any tools."
    );
    let command = format!(
        "codex --model {} --sandbox workspace-write --ask-for-approval never -c 'model_reasoning_effort=\"low\"'",
        common::codex_test_model(),
    );
    let config_dir = common::harness_tempdir().expect("Codex new-pane config");
    let config_path = config_dir.path().join("config.toml");
    std::fs::write(&config_path, format!("default_command = {command:?}\n"))
        .expect("write bare Codex default command");
    let deck = TuiDeck::builder()
        .with_pty_size(180, 45)
        .with_env("PATH", path_with_binary_dir())
        .with_env("DOT_AGENT_DECK_CONFIG", config_path.to_string_lossy())
        .with_imported_codex_credentials()
        .launch_with_fixture("codex-live");

    deck.wait_for_string("No active sessions");
    let events = deck.subscribe_events();
    deck.send_keys(b"\x0e");
    deck.wait_for_string("Select Directory");
    deck.send_keys(b" ");
    deck.wait_for_string("Tab: switch");
    deck.send_keys(b"\r");
    deck.send_keys(b"\r");
    deck.send_keys(b"\r");
    deck.wait_for_string("[Command Mode Ctrl+D]");
    assert!(
        deck.wait_for_grid_string_within(common::codex_test_model(), Duration::from_secs(30)),
        "the bare interactive Codex UI never became ready in the new pane:\n{}",
        deck.snapshot_grid()
    );
    deck.send_keys(prompt.as_bytes());
    deck.wait_for_string(PROMPT_MARKER);
    deck.send_keys(b"\r");

    // The core assertion (issue #658): a captured `AgentEvent` from a live,
    // real Codex process must carry the model it is actually running, read
    // off that process's own status bar through the real wrap integration —
    // not asserted against frozen capture bytes the way `codex/wrap/015`-
    // `018` pin the extraction logic in isolation.
    let model_event = events.wait_for(
        |event| {
            event.agent_type == AgentType::Codex
                && event.model.is_some()
                && event.live_target.is_some()
        },
        Duration::from_secs(60),
    );
    assert_eq!(
        model_event.model.as_deref(),
        Some(common::codex_test_model()),
        "a live Codex session's AgentEvent stream must report the real active model observed \
         off its own status bar; full event: {model_event:?}"
    );

    deck.send_bytes(b"\x04");
    deck.wait_for_string("Dir:");
}

/// Scenario: Run a deterministic terminal probe beneath `dot-agent-deck wrap`
/// in a daemon-managed pane, resize the outer PTY, send a line, and press Ctrl+C.
/// The child must see all three descriptors as TTYs, receive SIGWINCH plus input,
/// and observe SIGINT without losing transparent terminal behavior.
#[spec("codex/wrap/002")]
#[test]
fn codex_wrap_002_preserves_tty_resize_input_and_interrupt() {
    let command = "dot-agent-deck wrap --agent codex -- ./tty-probe.sh";
    let mut deck = TuiDeck::builder()
        .with_env("PATH", path_with_binary_dir())
        .with_continue_session("tty-probe", command)
        .launch_with_fixture("codex-tty-probe");
    deck.wait_for_string("[Command Mode Ctrl+D]");
    let record = deck.workdir().join("tty-probe.log");
    let started =
        common::wait_for_file_substr_count(&record, "isatty(2)=", 1, Duration::from_secs(10));

    deck.resize(150, 50);
    let resized = common::wait_for_file_substr_count(&record, "WINCH", 1, Duration::from_secs(5));
    deck.send_keys(b"transparent-input\r");
    let input = common::wait_for_file_substr_count(
        &record,
        "INPUT=transparent-input",
        1,
        Duration::from_secs(5),
    );
    deck.send_keys(b"\x03");
    let interrupted = common::wait_for_file_substr_count(&record, "INT", 1, Duration::from_secs(5));
    let observed = std::fs::read_to_string(&record).unwrap_or_default();

    assert!(
        started,
        "the wrapped TTY probe never started; record={observed:?}"
    );
    assert!(
        observed.contains("isatty(0)=true")
            && observed.contains("isatty(1)=true")
            && observed.contains("isatty(2)=true"),
        "the wrapper must preserve TTY identity on stdin/stdout/stderr; observed:\n{observed}"
    );
    assert!(
        resized,
        "the wrapped child did not receive SIGWINCH after resize; observed:\n{observed}"
    );
    assert!(
        input,
        "ordinary input did not transparently reach the wrapped child; observed:\n{observed}"
    );
    assert!(
        interrupted,
        "Ctrl+C did not reach the wrapped child as SIGINT; observed:\n{observed}"
    );
}
