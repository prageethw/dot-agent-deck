#![cfg(feature = "e2e")]

//! L2 end-to-end hook-delivery tests. Each function spawns the real
//! `dot-agent-deck` binary inside an isolated PTY, writes a hook
//! payload to the per-test hook socket, and asserts on the rendered
//! grid through a `vt100` parser. PRD #77 Decision 2 + Decision 6.
//!
//! Decision 6: this file is gated behind the `e2e` feature so CI
//! (which runs only `cargo test-fast`) never compiles it.

mod common;

use common::{TuiDeck, write_hook_line};
use spec::spec;

/// Scenario: Launch the deck against the `minimal` fixture, wait
/// for the empty dashboard to render, then write a synthetic
/// Claude Code `SessionStart` hook payload (with `pane_id =
/// pane-m2-001`, `session_id = m2demo`, `agent_type = claude_code`)
/// directly to the per-test hook socket. The deck's daemon auto-
/// registers the unknown pane on its first `SessionStart` event,
/// so a card showing `m2demo` should appear on the dashboard within
/// the test budget. No real LLM tokens are spent — the harness
/// injects the event in-process.
#[spec("hooks/delivery/001")]
#[test]
fn delivery_001_session_start_creates_card() {
    // PRD #77 catalog: hooks/delivery/001 — A Claude Code SessionStart
    // hook arriving at the daemon's hook socket creates a session entry
    // on the dashboard. The harness redirects `DOT_AGENT_DECK_SOCKET`
    // to a per-test path so the deck-spawned daemon binds there;
    // `write_hook_line` then injects the JSON payload that the daemon
    // already accepts on the hook socket (see `run_hook_loop` in
    // `src/daemon.rs`).
    let deck = TuiDeck::launch_with_fixture("minimal");

    // Wait for the deck to finish painting its initial dashboard so the
    // attach-side `subscribe_events` connection is live before we inject
    // — otherwise a fast write can land before the TUI subscribes. The
    // empty-state line is sufficient evidence the dashboard rendered;
    // wait_until_quiescent would race the TUI's periodic redraw tick.
    deck.wait_for_string("No active sessions");

    // The hook event uses a session_id short enough to render in full
    // (the dashboard truncates to 11 chars), and a fresh pane_id that
    // the deck has not seen — `apply_event`'s SessionStart auto-register
    // branch will adopt it and a card will appear.
    let event = serde_json::json!({
        "session_id": "m2demo",
        "agent_type": "claude_code",
        "event_type": "session_start",
        "timestamp": "2026-05-26T12:00:00Z",
        "pane_id": "pane-m2-001",
    });

    write_hook_line(deck.hook_socket_path(), &event.to_string())
        .expect("write SessionStart hook to per-test socket");

    // Asserting via `wait_for_string` against the rendered grid — the
    // catalog explicitly says "loose substring match on the session_id
    // or display_name".
    deck.wait_for_string("m2demo");
}

/// Scenario: Issue #393 — connect directly to the daemon's hook socket and
/// write a partial line with no terminating newline, then hold the
/// connection open without ever completing it. Asserts the daemon
/// closes/rejects the connection within a bounded window instead of
/// blocking `next_line()` on it forever, which is today's behavior: the
/// hook-socket reader in `run_hook_loop` (`src/daemon.rs`) has no cap and
/// no total-operation deadline, unlike the reply-path read in `src/hook.rs`
/// this fix is meant to mirror.
#[spec("hooks/delivery/008")]
#[test]
fn delivery_008_unterminated_hook_socket_line_is_bounded() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};

    let deck = TuiDeck::launch_with_fixture("minimal");
    deck.wait_for_string("No active sessions");

    let mut stream = UnixStream::connect(deck.hook_socket_path()).expect("connect to hook socket");
    stream
        .set_read_timeout(Some(Duration::from_millis(250)))
        .expect("set read timeout");

    // No trailing newline: `BufReader::lines().next_line()` never completes
    // this line, so a peer that keeps the connection open without ever
    // sending one — exactly what an attacker or a wedged agent would do —
    // should not be able to hold the daemon's read loop (and its
    // accumulating buffer) open forever.
    stream
        .write_all(b"{\"session_id\":\"issue-393-unterminated\"")
        .expect("write partial hook payload");
    stream.flush().expect("flush partial hook payload");

    // Comfortably above the 5s total-operation deadline the reply-path read
    // in `src/hook.rs` already uses for the same idiom — a correct fix is
    // expected to close well inside this budget.
    let budget = Duration::from_secs(10);
    let deadline = Instant::now() + budget;
    let mut buf = [0u8; 64];
    let mut closed = false;
    while Instant::now() < deadline {
        match stream.read(&mut buf) {
            Ok(0) => {
                closed = true;
                break;
            }
            Ok(_) => continue,
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(_) => {
                closed = true;
                break;
            }
        }
    }

    assert!(
        closed,
        "expected the daemon to close/reject a hook-socket connection that sent a line with no \
         terminating newline within {budget:?}, but it never did — today's `next_line()` read \
         in `run_hook_loop` (src/daemon.rs) has no cap and no total-operation deadline (issue \
         #393), so a peer that never completes its line can hold the connection open \
         indefinitely"
    );
}
