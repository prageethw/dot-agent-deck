#![cfg(feature = "e2e")]

//! L2 (real-binary PTY) coverage for orchestration selection highlights.
//!
//! Selection cue note: PRD #13 Option A cues the highlighted card with a `▸`
//! title prefix (and a cyan-bold border), not an absolute background fill, and
//! `▸` is used ONLY for that selection marker (src/ui.rs `render_session_card`).
//! So "no card carries the highlight" is asserted as the absence of `▸` from the
//! rendered grid — the same observable the L1 selection tests use.
//!
//! Decision 6: gated behind the `e2e` feature so `cargo test-fast` never
//! compiles it.

mod common;

use common::{TuiDeck, commit_fixture};
use spec::spec;

/// `role` appears somewhere on the settled grid as its own token — bounded
/// on both sides by anything that is NOT an identifier character (a card
/// border glyph, whitespace, a newline, or start/end of the grid) rather
/// than specifically a space. PRD fork#405 M1 moved the role name off the
/// card's title/border row onto its own unconditional body row directly
/// beneath it, and `render_session_card` (`src/ui.rs`) pushes that row's
/// text with NO leading or trailing pad — so a `" {role} "`-style
/// space-bounded needle can never match post-M1 (the character immediately
/// to the left of the role name is the card's own border glyph, not a
/// space). Word-boundary matching finds the role regardless of which
/// border glyph or how much padding surrounds it, while still ruling out
/// `"worker"` matching inside this deck's own `"worker-deck"` chrome title
/// (the char right after "worker" there is `-`, a word char, so the match
/// is rejected) — the exact collision the old `" worker "` needle existed
/// to avoid.
fn grid_has_role(grid: &str, role: &str) -> bool {
    common::contains_word_token(grid, role)
}

/// Drive the new-pane dialog to open the (single) orchestration in the fixture.
/// The Mode chip row begins `[No mode] [Orch: …]`, so ONE Right selects
/// the orchestration; selecting an
/// orchestration HIDES the Command field, so a second Enter submits the form.
fn open_orchestration(deck: &TuiDeck) {
    deck.send_keys(b"\x0e"); // Ctrl+n → directory picker
    deck.send_keys(b" "); // Space → confirm current dir → new-pane form
    deck.wait_for_string("No mode"); // form up, Mode field focused at "No mode"
    deck.send_keys(b"\x1b[C"); // Right → [Orch: demo-orch]
    deck.send_keys(b"\r"); // Mode → Name
    deck.send_keys(b"\r"); // submit (Command hidden for an orchestration)
}

/// Scenario: PR #151 manual-test regression (Issue 1) against the real binary —
/// Enter must PAINT the selection highlight on the Orchestration deck after a
/// tab round-trip. Open an orchestration (two `cat` role cards), detach to
/// Normal mode, arm a role with `j` (a `▸` marker appears), then round-trip
/// Orchestration → Dashboard → Orchestration (the highlight clears, SC1). On
/// return the orchestration re-focuses its remembered role pane, so pressing
/// Enter re-focuses the SAME pane — not a focus transition — and under the
/// pre-fix behavior the reconcile never re-arms the highlight, so `▸` never
/// reappears (this `wait_for_string` times out). The unified fix makes
/// Action::Focus set `selected_index` directly, so Enter paints the highlight on
/// the orchestration deck just like the Dashboard. This is the higher-risk
/// VISUAL behavior the L1 mocks repeatedly missed (they never run the real
/// reconcile + focus-restore on an orchestration tab), hence the L2 coverage.
#[spec("dashboard/selection/019")]
#[test]
fn selection_019_enter_paints_highlight_on_orchestration_real_binary() {
    let deck = TuiDeck::builder()
        .with_pty_size(120, 40)
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("No active agents");

    // The Orchestration tab opened below needs isolated-clone provisioning,
    // which needs a ref to branch from — an unborn HEAD (the harness's own
    // bare `git init`) does not provide one.
    commit_fixture(deck.workdir());

    // Open the orchestration tab (Dashboard + Orchestration = the ≥2 tabs the
    // round-trip needs). Its two `cat` role panes render as deck cards.
    // Fork-only (`349e895`): the dashboard title bar itself now reads
    // `worker-deck`, so a bare "worker" needle would match the chrome from
    // frame one — `grid_has_role` bounds the match to the "worker" role
    // card's own identity text (word-boundary, not `-deck`) so this
    // genuinely waits for the 2nd role card to render.
    open_orchestration(&deck);
    deck.wait_until_grid("2nd role card (worker) rendered", |g| {
        grid_has_role(g, "worker")
    }); // the 2nd role card → orchestration deck is up

    // Detach to Normal mode on the Orchestration tab so `j` / Enter drive the
    // deck (Enter would otherwise type into the focused role pane).
    deck.send_bytes(b"\x04"); // Ctrl+D → Normal mode
    // Arm a role: `j` activates the deck selection, painting the `▸` marker.
    deck.send_bytes(b"j");
    deck.wait_for_string("\u{25b8}"); // ▸ — a role card is now highlighted

    // Round-trip: Orchestration → Dashboard → Orchestration. The highlight
    // clears on the tab switch (SC1 / orchestration_003).
    deck.send_bytes(b"\x1b[D"); // Left → previous tab → Dashboard
    // A bare "worker" needle can never succeed post-`349e895`: the
    // Dashboard's own title bar reads `worker-deck — N/M session(s)`, so a
    // bare "worker" substring is permanently present in the deck's own
    // chrome regardless of whether role cards are showing. `grid_has_role`
    // bounds the match to the role card's own identity text — the title
    // bar's `worker-deck` has a hyphen (a word char) right after "worker",
    // so it can never satisfy the word-boundary check.
    deck.wait_until_grid("worker role card gone", |g| !grid_has_role(g, "worker")); // left the orchestration (role cards gone; role panes are excluded from the Dashboard)
    deck.wait_for_absence("\u{25b8}"); // highlight cleared
    deck.send_bytes(b"\x1b[C"); // Right → next tab → Orchestration
    deck.wait_until_grid("worker role card rendered", |g| grid_has_role(g, "worker")); // back on the Orchestration deck

    // THE REGRESSION: Enter on the Orchestration deck must RESTORE the previous
    // role AND paint its highlight. Pre-fix the role pane is already focused, so
    // Enter is not a focus transition and the highlight never reappears — this
    // wait times out. Post-fix Action::Focus sets `selected_index` directly.
    deck.send_bytes(b"\r"); // Enter → Action::Focus (restore + paint)
    deck.wait_for_string("\u{25b8}"); // ▸ must reappear on the restored role
}
