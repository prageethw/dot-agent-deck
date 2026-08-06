#![cfg(all(feature = "e2e", unix))]

//! L2 PTY-attached real-agent rot canary for PRD #386's descendant-scan
//! shell-activity signal (M6a, catalog `status/shell-activity/005`).
//!
//! Every earlier test in this PRD (`status/shell-activity/001`-`004`) proves
//! the mechanism against a synthetic or stand-in process — a spawned
//! `sleep`, a hand-`setsid()`'d `python3` child. None of them prove Claude
//! Code itself still `setsid`-detaches its real Bash-tool child, which is
//! the one fact the whole signal rests on (PRD #386 Risks: "Claude Code
//! ceasing to `setsid` its Bash-tool child — total false negative, and
//! silent"). This test spawns a genuine interactive Haiku Claude agent
//! through the normal new-pane flow (the same path a user drives), lets it
//! make one real Bash tool call that runs long enough to observe, and
//! asserts the daemon's `run_shell_activity_monitor` actually synthesizes a
//! `ShellBusy` broadcast event for that pane while the call is in flight.
//!
//! Hand-seeds nothing: no synthetic `SessionStart`, no fabricated `pane_id`
//! — the pane id comes from the real spawn path (`AgentType::from_command`
//! infers `ClaudeCode` from the typed command, and the daemon assigns
//! `pane_id_env` itself), exactly as #370's own test failed to do. The
//! assertion is on the broadcast `AgentEvent` observed over a
//! `SubscribeEvents` connection, never on the rendered `Working` badge —
//! the badge is already `Working` from `ToolStart` regardless of whether
//! this signal fires at all, so a badge-only assertion would pass with the
//! mechanism completely dead, which is precisely how #370 shipped green.
//!
//! `sleep` cannot be the long-command instrument: Claude Code blocks long
//! `sleep` at the tool layer and emits no `ToolStart` at all (PRD #386,
//! measured). `ping -c 20 127.0.0.1 > /dev/null` is real, non-blocked,
//! foreground work that runs ~19-20s — long enough to observe the daemon's
//! 500ms poll catch the rising edge well before the command finishes.
//!
//! Cost note (Decision 23): one Haiku-4.5 interactive turn, one Bash tool
//! call. Local-only (Decision 8) and real-agent (rule 5 exception (a)):
//! gated on the `e2e` feature so CI's `cargo test-fast` never compiles it,
//! and this file's real-agent tier has no CI credentials, so a local run is
//! the only way to exercise it at all.

mod common;

use std::time::Duration;

use common::TuiDeck;
use dot_agent_deck::event::EventType;
use spec::spec;

const HAIKU_MODEL: &str = "claude-haiku-4-5-20251001";
const PANE_NAME_SUFFIX: &str = "shell-activity-005-haiku";
/// Unique so the prompt unambiguously names one real file rather than
/// leaving the model to invent a command — the sentinel + directive-prompt
/// pairing CLAUDE.md rule 4 asks for so the assertion survives LLM
/// phrasing/tool variance.
const SENTINEL: &str = "shell-activity-005-sentinel-e4b7f1.txt";
const SENTINEL_CONTENT: &str = "SHELL_ACTIVITY_005_OK";

/// Scenario: launch a real interactive Haiku Claude agent through the normal Ctrl+N new-pane flow (per-folder trust pre-seeded, `--allowedTools Bash`), then type a directive prompt naming a uniquely-named sentinel fixture file that instructs the agent to run `ping -c 20 127.0.0.1 > /dev/null` as its one Bash tool call. While that ~20s foreground command is in flight, assert — over a `SubscribeEvents` connection, never against the rendered badge — that the daemon's shell-activity monitor synthesizes a `ShellBusy` broadcast event for the pane.
#[spec("status/shell-activity/005")]
#[test]
fn shell_activity_005_real_claude_bash_child_trips_the_descendant_scan() {
    skip_unless!(common::check_claude_available());

    let deck = TuiDeck::builder()
        .with_imported_claude_credentials()
        .launch_with_fixture("minimal");
    deck.wait_for_string("No active sessions");

    std::fs::write(deck.workdir().join(SENTINEL), SENTINEL_CONTENT)
        .expect("write shell-activity-005 sentinel fixture");

    let cwd = deck.workdir().to_path_buf();
    let mut trust_paths = vec![cwd.to_string_lossy().into_owned()];
    if let Ok(canonical) = cwd.canonicalize() {
        let canonical = canonical.to_string_lossy().into_owned();
        if !trust_paths.contains(&canonical) {
            trust_paths.push(canonical);
        }
    }
    common::seed_claude_trust_in_home(deck.home_dir(), &trust_paths)
        .expect("seed Claude onboarding and per-folder trust");

    let events = deck.subscribe_events();

    // The normal, user-driven new-pane flow — no synthetic hook, no
    // fabricated pane id. `AgentType::from_command` infers `ClaudeCode`
    // from the typed command text, which is what makes the daemon's
    // per-pane argv-shape selection (`shell_tool_shape_key`) pick
    // `CLAUDE_BASH_TOOL_SHAPE` for this pane later.
    deck.send_keys(b"\x0e");
    deck.wait_for_string("Select Directory");
    deck.send_keys(b" ");
    deck.wait_for_string("New Agent");
    deck.send_keys(b"\t");
    deck.send_keys(PANE_NAME_SUFFIX.as_bytes());
    deck.send_keys(b"\t");
    deck.send_keys(format!("claude --model {HAIKU_MODEL} --allowedTools Bash").as_bytes());
    let (submit_col, submit_row) = deck
        .find_in_grid("[Submit]")
        .expect("new-pane form should render [Submit]");
    deck.click(submit_col, submit_row);

    assert!(
        deck.wait_for_grid_string_within("? for shortcuts", Duration::from_secs(120)),
        "the genuine interactive Claude prompt never became ready:\n{}",
        deck.snapshot_grid()
    );

    let record = common::agent_records_on(deck.attach_socket_path())
        .into_iter()
        .find(|record| {
            record
                .display_name
                .as_deref()
                .is_some_and(|name| name.ends_with(PANE_NAME_SUFFIX))
        })
        .unwrap_or_else(|| {
            panic!("the real Claude pane ending in {PANE_NAME_SUFFIX:?} must be registered")
        });
    let agent_id = record.id;
    let pane_id = record
        .pane_id_env
        .expect("a real spawned pane must carry a pane_id_env — the daemon assigns it at spawn");

    let prompt = format!(
        "There is a file named {SENTINEL} in the current directory. Use the Bash tool exactly \
         once to run this single command: ping -c 20 127.0.0.1 > /dev/null; cat {SENTINEL}. Do \
         not run any other command. After the tool call finishes, reply with only the exact \
         file contents and nothing else."
    );
    deck.send_keys(prompt.as_bytes());
    deck.send_keys(b"\r");

    // Precondition: the real Bash tool call actually started. This is the
    // native ToolStart hook event, not a fabricated one.
    let tool_start = events.wait_for(
        |event| {
            event.agent_id.as_deref() == Some(agent_id.as_str())
                && event.event_type == EventType::ToolStart
                && event.tool_name.as_deref() == Some("Bash")
        },
        Duration::from_secs(120),
    );
    eprintln!(
        "shell-activity-005: ToolStart observed at {:?} for pane {pane_id:?}",
        tool_start.timestamp
    );

    // The load-bearing assertion: the daemon's descendant-scan poll must
    // synthesize a ShellBusy event for THIS pane while the ~20s ping is
    // still in flight. `ping` began running the moment ToolStart fired (the
    // whole Bash-tool shell is what Claude Code setsid-detaches, not just a
    // backgrounded sub-command), so a 15s window comfortably sits inside the
    // command's ~19-20s lifetime — if the monitor's 500ms poll can see the
    // detached descendant at all, it has several polls' worth of margin to
    // report it within this window. A miss here means either Claude Code
    // stopped setsid-detaching its Bash-tool child (the total-false-negative
    // risk this canary exists to catch) or the descendant scan itself is not
    // reaching this pane — never an artifact of the window being too tight.
    let shell_busy = events.wait_for(
        |event| {
            event.event_type == EventType::ShellBusy
                && event.pane_id.as_deref() == Some(pane_id.as_str())
        },
        Duration::from_secs(15),
    );
    eprintln!(
        "shell-activity-005: ShellBusy observed at {:?}, {}ms after ToolStart — the descendant \
         scan found Claude Code's real Bash-tool child in a POSIX session of its own",
        shell_busy.timestamp,
        (shell_busy.timestamp - tool_start.timestamp).num_milliseconds()
    );

    // Soft, reel-style confirmation that the observed ping really was the
    // one the prompt asked for — logged, not gating, since matching the
    // model's final free-text reply is inherently more phrasing-sensitive
    // than the two typed hook/synthetic events above.
    let saw_sentinel_reply =
        deck.wait_for_grid_string_within(SENTINEL_CONTENT, Duration::from_secs(30));
    eprintln!(
        "shell-activity-005: sentinel content echoed back in the pane = {saw_sentinel_reply}"
    );
}
