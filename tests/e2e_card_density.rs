#![cfg(all(feature = "e2e", unix))]

//! L2 (real-binary PTY) coverage for the dashboard's card density ladder.
//!
//! Issue #1568 added a 3-row Minimal density that the deck uses only when the
//! 6-row Compact card cannot fit every card. The L1 tests in
//! `tests/render_dashboard.rs` pin the layout decision through the
//! `render_card_grid_to_buffer` seam; this file proves the running binary makes
//! the same decision in a real terminal, with cards that arrive over the hook
//! socket. Decision 6: gated behind the `e2e` feature so `cargo test-fast`
//! never compiles it.

mod common;

use common::{TuiDeck, write_hook_line};
use spec::spec;

/// Eight cards: more than a 30-row terminal holds at Compact (8 x 5 = 40 rows)
/// and fewer than it holds at Minimal (8 x 3 = 24 rows), whatever the hints bar
/// wraps to at this width.
const CARDS: usize = 8;
const COLS: u16 = 79;
const ROWS: u16 = 30;

/// Inject a synthetic Claude Code `SessionStart` hook so a dashboard card
/// exists. Mirrors `e2e_mouse_inline.rs`.
fn send_session_start(deck: &TuiDeck, session_id: &str, pane_id: &str, cwd: &str) {
    let event = serde_json::json!({
        "session_id": session_id,
        "agent_type": "claude_code",
        "event_type": "session_start",
        "timestamp": "2026-06-07T12:00:00Z",
        "pane_id": pane_id,
        "cwd": cwd,
    });
    write_hook_line(deck.hook_socket_path(), &event.to_string())
        .expect("write SessionStart hook to per-test socket");
}

/// The `(top_row, bottom_row)` of every card, top to bottom, read off the
/// corners in column 0 — at 79 columns the deck draws one card column, so each
/// card starts and ends at the left edge of the screen.
fn card_spans(grid: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut top = None;
    for (y, line) in grid.lines().enumerate() {
        match line.chars().next() {
            Some('┌' | '┏') => top = Some(y),
            Some('└' | '┗') => {
                if let Some(t) = top.take() {
                    spans.push((t, y));
                }
            }
            _ => {}
        }
    }
    spans
}

/// Every card's height, top to bottom — see [`card_spans`].
fn card_heights(grid: &str) -> Vec<usize> {
    card_spans(grid)
        .iter()
        .map(|(top, bottom)| bottom - top + 1)
        .collect()
}

/// Scenario: Start the deck in a 79x30 terminal and send eight agents' session
/// starts over the hook socket — more than the screen holds at the 6-row
/// Compact card, but with more rows than eight 3-row Minimal cards need. Every
/// one of the eight cards appears on screen, the spare rows are dealt to the
/// first cards so the column runs straight to the stats bar with no blank band
/// (issue #858), a 3-row card has its directory folded onto its role row (issue
/// #847) and a taller card keeps `Dir:` on its own row, every card has its
/// `Last:` counter, and the deck title shows no hidden-card marker, so nothing
/// needs scrolling.
#[spec("dashboard/density/009")]
#[test]
fn density_009_real_binary_draws_every_card_at_minimal() {
    let deck = TuiDeck::builder()
        .with_pty_size(COLS, ROWS)
        .launch_with_fixture("minimal");
    deck.wait_for_string("No active agents");

    let ids: Vec<String> = (1..=CARDS).map(|i| format!("minimal-{i:02}")).collect();
    for (i, id) in ids.iter().enumerate() {
        send_session_start(&deck, id, &format!("pane-min-{i:02}"), "/tmp/density-dir");
    }

    // One capture under the parser lock, so the frame the assertions below read
    // is the frame that was judged ready — a redraw cannot land in between.
    let grid = deck.capture_screen_when("every card drawn", |screen| {
        let grid = screen.contents();
        (card_heights(&grid).len() == CARDS && ids.iter().all(|id| grid.contains(id.as_str())))
            .then_some(grid)
    });
    for id in &ids {
        assert!(
            grid.contains(id.as_str()),
            "card {id} must be on screen without scrolling:\n{grid}"
        );
    }

    // Issue #858: whatever the hints bar wraps to, the rows on offer are 3n or
    // more and under 4n here, so the cards are 3 or 4 rows. The rows beyond 3 per
    // card are dealt to the FIRST cards (the fork's even division), so heights
    // never grow down the column and differ by at most one; no card is under 3
    // rows; the cards sit back to back; and the last card's bottom border is the
    // row directly above the stats bar, with no blank band between them.
    let spans = card_spans(&grid);
    let heights = card_heights(&grid);
    let lines: Vec<&str> = grid.lines().collect();
    assert!(
        heights.iter().all(|&h| h >= 3),
        "no card may be shorter than 3 rows, got heights {heights:?}:\n{grid}"
    );
    assert!(
        heights.windows(2).all(|pair| pair[0] >= pair[1])
            && heights.iter().max().unwrap_or(&0) - heights.iter().min().unwrap_or(&0) <= 1,
        "the spare rows go to the first cards, so heights are non-increasing and differ \
         by at most one, got {heights:?}:\n{grid}"
    );
    assert!(
        spans.windows(2).all(|pair| pair[0].1 + 1 == pair[1].0),
        "the cards must sit back to back, got spans {spans:?}:\n{grid}"
    );
    let last_bottom = spans.last().map(|&(_, bottom)| bottom).unwrap_or_default();
    let below_last_card = lines.get(last_bottom + 1).copied().unwrap_or_default();
    assert!(
        below_last_card
            .trim_start()
            .starts_with(&format!("{CARDS} active")),
        "the stats bar must sit directly under the last card with no blank band, got \
         {below_last_card:?} on the row after the last card (heights {heights:?}):\n{grid}"
    );

    // A 3-row card is the fork's folded Minimal form (issue #847): its ONE content
    // row carries the role name and `Dir: <basename>`, `Dir:` with a single space
    // where upstream's separate row reads `Dir:  <basename>`. A card dealt 4 rows
    // keeps its role row to itself and puts `Dir:` on the row beneath.
    for &(top, bottom) in &spans {
        // Drop the border glyph, which is `│` or `┃` depending on selection.
        let body = |row: usize| -> String { lines[row].chars().skip(1).collect() };
        let id = ids
            .iter()
            .find(|id| body(top + 1).starts_with(id.as_str()))
            .unwrap_or_else(|| panic!("row {} names no card:\n{grid}", top + 1));
        if bottom - top + 1 == 3 {
            assert!(
                body(top + 1).starts_with(&format!("{id}  Dir: density-dir")),
                "card {id} is 3 rows, so it must fold its directory onto its role row:\n{grid}"
            );
        } else {
            assert!(
                !body(top + 1).contains("Dir:")
                    && body(top + 2).contains("Dir:")
                    && body(top + 2).contains("density-dir"),
                "card {id} is {} rows, so it must keep `Dir:` on its own row:\n{grid}",
                bottom - top + 1
            );
        }
    }
    assert_eq!(
        grid.matches("Last:").count(),
        CARDS,
        "every Minimal card keeps its bottom-border counters:\n{grid}"
    );
    assert!(
        !grid.contains('↓') && !grid.contains('↑'),
        "no card is hidden, so the deck title carries no scroll marker:\n{grid}"
    );
}
