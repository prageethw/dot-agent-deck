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
/// synthetic `SessionStart` hooks (one Claude Code carrying `model:
/// "Opus"`, one Codex carrying `model: "gpt-5.1-codex-mini"`) so two cards
/// render, and confirm neither shows its agent-type label or model at rest
/// (default hidden). Press a bare `m` and confirm both labels AND their
/// models appear as `ClaudeCode (Opus)` / `Codex (5.1-codex-mini)` once the
/// status bar reports `Agent badge: shown` — the Codex fixture's `gpt-`
/// vendor prefix is stripped by `normalize_model_label`, while `Opus`
/// matches no vendor prefix and passes through unchanged, which is why the
/// two fixtures render differently; press `m` again and confirm both
/// disappear once it reports `Agent badge: hidden`. Presses only `m`, never
/// `\x0d` — under a legacy PTY `Ctrl+M` decodes as Enter and `FocusPane`
/// wins first (by design), so this is the only door that works everywhere.
#[spec("dashboard/agent-badge/003")]
#[test]
fn agent_badge_003_m_toggles_badges_on_every_card_real_binary() {
    let deck = TuiDeck::launch_with_fixture("minimal");
    deck.wait_for_string("No active sessions");

    // Session ids stay at or under the dashboard's 11-char truncation limit
    // (`id_display` in `render_session_card`) so the full id is what
    // `wait_for_string` looks for — a longer id would truncate before this
    // test's badge assertions and time out for an unrelated reason.
    let claude_event = serde_json::json!({
        "session_id": "claude-bdg",
        "agent_type": "claude_code",
        "event_type": "session_start",
        "timestamp": "2026-08-15T12:00:00Z",
        "pane_id": "pane-badge-claude",
        // PRD fork#378: the agent's active model, posted top-level exactly
        // as a real hook payload carries it (see
        // tests/codex_hook_ingestion.rs's schema-accurate `model` key).
        "model": "Opus",
    });
    write_hook_line(deck.hook_socket_path(), &claude_event.to_string())
        .expect("write claude_code SessionStart hook to per-test socket");
    deck.wait_for_string("claude-bdg");

    let codex_event = serde_json::json!({
        "session_id": "codex-bdg",
        "agent_type": "codex",
        "event_type": "session_start",
        "timestamp": "2026-08-15T12:00:01Z",
        "pane_id": "pane-badge-codex",
        "model": "gpt-5.1-codex-mini",
    });
    write_hook_line(deck.hook_socket_path(), &codex_event.to_string())
        .expect("write codex SessionStart hook to per-test socket");
    deck.wait_for_string("codex-bdg");

    // Default hidden: this is the only coverage of default-hidden through the
    // real `render_frame` — no L1 seam reaches it. Assert absence of the
    // *normalized* Codex string ("5.1-codex-mini") rather than the raw
    // fixture value ("gpt-5.1-codex-mini"): normalization only strips the
    // `gpt-` prefix, so the normalized form is a substring of the raw one,
    // and its absence is the stronger check — it is what would actually
    // render if the badge leaked at rest.
    deck.wait_for_absence("ClaudeCode");
    deck.wait_for_absence("Codex");
    deck.wait_for_absence("Opus");
    deck.wait_for_absence("5.1-codex-mini");

    deck.send_keys(b"m");
    deck.wait_for_string("Agent badge: shown");
    deck.wait_for_string("ClaudeCode (Opus)");
    deck.wait_for_string("Codex (5.1-codex-mini)");

    deck.send_keys(b"m");
    deck.wait_for_string("Agent badge: hidden");
    deck.wait_for_absence("ClaudeCode");
    deck.wait_for_absence("Codex");
    deck.wait_for_absence("Opus");
    deck.wait_for_absence("5.1-codex-mini");
}
