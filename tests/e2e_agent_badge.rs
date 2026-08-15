#![cfg(feature = "e2e")]

//! L2 end-to-end test for fork #339's agent-type badge toggle. Spawns the
//! real `dot-agent-deck` binary inside an isolated PTY and drives the
//! deck-global `Ctrl+m` / bare-`m` toggle through the genuine `render_frame`
//! path — the one seam no L1 test reaches. Synthetic (no real agent, no LLM
//! spend), so this file is not `[reel]`-marked.

mod common;

use common::{TuiDeck, write_hook_line};
use spec::spec;

/// Scenario: Launch the deck against the `minimal` fixture, register two
/// synthetic `SessionStart` hooks (one Claude Code, one Codex) so two cards
/// render, and confirm neither shows its agent-type label at rest (default
/// hidden). Press a bare `m` and confirm both labels appear once the status
/// bar reports `Agent badge: shown`; press `m` again and confirm both
/// disappear once it reports `Agent badge: hidden`. Presses only `m`, never
/// `\x0d` — under a legacy PTY `Ctrl+M` decodes as Enter and `FocusPane`
/// wins first (by design), so this is the only door that works everywhere.
#[spec("dashboard/agent-badge/003")]
#[test]
fn agent_badge_003_m_toggles_badges_on_every_card_real_binary() {
    let deck = TuiDeck::launch_with_fixture("minimal");
    deck.wait_for_string("No active sessions");

    let claude_event = serde_json::json!({
        "session_id": "badge-claude",
        "agent_type": "claude_code",
        "event_type": "session_start",
        "timestamp": "2026-08-15T12:00:00Z",
        "pane_id": "pane-badge-claude",
    });
    write_hook_line(deck.hook_socket_path(), &claude_event.to_string())
        .expect("write claude_code SessionStart hook to per-test socket");
    deck.wait_for_string("badge-claude");

    let codex_event = serde_json::json!({
        "session_id": "badge-codex",
        "agent_type": "codex",
        "event_type": "session_start",
        "timestamp": "2026-08-15T12:00:01Z",
        "pane_id": "pane-badge-codex",
    });
    write_hook_line(deck.hook_socket_path(), &codex_event.to_string())
        .expect("write codex SessionStart hook to per-test socket");
    deck.wait_for_string("badge-codex");

    // Default hidden: this is the only coverage of default-hidden through the
    // real `render_frame` — no L1 seam reaches it.
    deck.wait_for_absence("ClaudeCode");
    deck.wait_for_absence("Codex");

    deck.send_keys(b"m");
    deck.wait_for_string("Agent badge: shown");
    deck.wait_for_string("ClaudeCode");
    deck.wait_for_string("Codex");

    deck.send_keys(b"m");
    deck.wait_for_string("Agent badge: hidden");
    deck.wait_for_absence("ClaudeCode");
    deck.wait_for_absence("Codex");
}
