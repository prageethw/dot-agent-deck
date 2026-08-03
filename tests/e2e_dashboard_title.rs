#![cfg(feature = "e2e")]

//! Fork-only L2 coverage for the `worker-deck` title-bar rename.
//!
//! This fork never pushes upstream (`prageethw/dot-agent-deck` only) but
//! still carries test coverage like normal so it survives future `git pull`s
//! from `vfarcic/dot-agent-deck` with lower merge-conflict risk.
//!
//! The title bar is painted by the private `render_frame` (`src/ui.rs`), which
//! has no public `..._to_buffer` L1 test seam — unlike `render_session_card` /
//! `render_dashboard_cards_to_buffer`, nothing exposes the title span to an
//! in-process `TestBackend` test. The real-binary PTY harness is the only way
//! to observe it without adding new production surface, so this is authored
//! as L2 rather than L1.
//!
//! Decision 6: gated behind the `e2e` feature so `cargo test-fast` never
//! compiles it.

mod common;

use common::TuiDeck;
use spec::spec;

/// Scenario: Launch the deck against the empty `minimal` fixture and wait for
/// the dashboard's empty state. The rendered grid must contain the fork's
/// `worker-deck` app name and must NOT contain the upstream literal
/// `dot-agent-deck` string — the title bar's leading styled span, not the
/// trailing `N session(s)` text (which is unaffected and unasserted here).
#[spec("dashboard/title/001")]
#[test]
fn title_001_bar_shows_worker_deck_not_upstream_name() {
    let deck = TuiDeck::launch_with_fixture("minimal");
    deck.wait_for_string("No active sessions");

    let grid = deck.snapshot_grid();
    assert!(
        grid.contains("worker-deck"),
        "the fork-only title bar must render `worker-deck`:\n{grid}"
    );
    assert!(
        !grid.contains("dot-agent-deck"),
        "the fork-only title bar must NOT render the upstream `dot-agent-deck` \
         app name:\n{grid}"
    );
}
