//! PRD #155 — centralized color palette (single source of truth).
//!
//! Before this module the TUI's semantic colors were scattered as inline
//! `Color::X` literals across the deck-card and embedded-pane render paths,
//! and the two surfaces drifted apart (a working agent could look different as
//! a deck card vs. as an embedded pane). This palette names the semantic
//! **roles** once and both render paths resolve their colors through it, so a
//! given state renders identically everywhere (PRD #155 Option A).
//!
//! ## Border policy (Option A — identical in both render paths)
//!
//! The card/pane border encodes **STATUS** in both the dashboard deck and the
//! embedded panes. Selection and focus are conveyed by dedicated **accent**
//! roles that never reuse a status color, so status / selection / focus are
//! always visually distinct. The unified border-resolution precedence is:
//!
//! 1. **selected** → [`SELECTED`] (Magenta) + BOLD + the `▸ ` title marker.
//! 2. else **focused AND live** → [`FOCUSED`] (Cyan).
//! 3. else → the agent's **status** role ([`status_color`]).
//!
//! The per-card status **badge** always shows status, so the accent override
//! in (1)/(2) never loses status information.
//!
//! ### Why (2) requires "live" (issue #88 follow-up)
//!
//! For an **embedded pane**, "focused" and "keystrokes reach it" are different
//! facts: in command mode the focused pane is still the one `Ctrl+D` / `Enter`
//! return you to, but it accepts no keys. The Cyan accent originally rendered
//! in both cases, which made the loudest border signal on screen claim "type
//! here" while the keyboard was driving the deck — the mode was invisible on a
//! full-screen mode tab, where nothing else in the frame changes.
//!
//! So for panes, (2) applies only in `UiMode::PaneInput`
//! (`TerminalWidget::with_input_active`). In command mode the focused pane
//! falls through to (3) and reports its agent's status like any other pane,
//! while **border thickness** (`BorderType::Thick`) carries focus instead.
//! Colour answers "are my keystrokes landing here?", thickness answers "which
//! pane is focused?" — one channel each, no longer competing. A **deck card**
//! has no input mode, so its precedence is unchanged.
//!
//! Thickness was chosen over a fourth accent colour because the 16-colour-safe
//! palette is full (green/blue/yellow/red are statuses, cyan is focus, magenta
//! is selection) and the remaining candidates are grays — the exact
//! light-background hazard PRD #13 exists to prevent. `BorderType` never feeds
//! `Block::inner`, so it costs no layout: the pane's inner area, its PTY size,
//! and the PRD #84 invariant-3 contract are all unaffected.
//!
//! All roles are **named ANSI** colors only — no absolute `Color::Rgb`, which
//! the theme guards (`theme/contrast/001`) forbid so terminal themes can remap
//! them.

use ratatui::style::Color;

use crate::state::SessionStatus;

// ---------------------------------------------------------------------------
// Status roles
// ---------------------------------------------------------------------------

/// Working — the agent is actively running a tool / producing output.
pub const STATUS_WORKING: Color = Color::Green;
/// Thinking — the agent is reasoning before acting.
pub const STATUS_THINKING: Color = Color::Blue;
/// Waiting — the agent needs user input to proceed.
pub const STATUS_WAITING: Color = Color::Yellow;
/// Error — the agent hit a failure.
pub const STATUS_ERROR: Color = Color::Red;
/// Idle — no current activity (dimmed).
pub const STATUS_IDLE: Color = Color::DarkGray;

// ---------------------------------------------------------------------------
// Accent roles (must be distinct from every status color and from each other)
// ---------------------------------------------------------------------------

/// The focused embedded pane. Cyan was previously used for both focus and
/// selection; Option A keeps focus on Cyan and moves selection to [`SELECTED`]
/// so the two are provably distinct.
pub const FOCUSED: Color = Color::Cyan;
/// The selected deck card (rendered BOLD with a `▸ ` title marker). Magenta is
/// free — status uses green/blue/yellow/red and focus uses cyan — so selection
/// never collides with a status color or with focus (PRD #155 criterion #3).
pub const SELECTED: Color = Color::Magenta;

/// Resolve a session status to its centralized border/badge role color. This
/// is the single source of truth shared by the deck-card render path
/// (`src/ui.rs`) and the embedded-pane render path (`src/terminal_widget.rs`),
/// so a given state shows the same border color in both contexts.
pub fn status_color(status: &SessionStatus) -> Color {
    match status {
        SessionStatus::Working => STATUS_WORKING,
        SessionStatus::Thinking => STATUS_THINKING,
        // Compacting is a thinking-adjacent transient state; it shares the
        // thinking role rather than introducing a sixth status color.
        SessionStatus::Compacting => STATUS_THINKING,
        SessionStatus::WaitingForInput => STATUS_WAITING,
        SessionStatus::Error => STATUS_ERROR,
        SessionStatus::Idle => STATUS_IDLE,
        // PRD #162 forward-compat: an unknown wire status renders with the
        // neutral idle color so it never masquerades as an active state.
        SessionStatus::Unknown => STATUS_IDLE,
    }
}

/// This status's rank in the PRD #333 fixed priority order — lower ranks
/// win. Mirrors the aliasing [`status_color`] already applies (Compacting
/// shares Thinking's rank, Unknown shares Idle's) so a status that resolves
/// to the same color also resolves to the same priority.
fn priority_rank(status: &SessionStatus) -> u8 {
    match status {
        SessionStatus::Error => 0,
        SessionStatus::WaitingForInput => 1,
        SessionStatus::Working => 2,
        SessionStatus::Thinking | SessionStatus::Compacting => 3,
        SessionStatus::Idle | SessionStatus::Unknown => 4,
    }
}

/// PRD #333 M1 — resolve the single highest-priority `SessionStatus` among an
/// orchestration tab's pane statuses, per the fixed order Error > NeedsInput >
/// Working > Thinking > Idle (ties within a rank keep whichever status was
/// encountered first). An empty slice (a tab with no panes) falls back to
/// `Idle`, the same neutral "nothing going on" state an all-Idle tab
/// resolves to.
pub fn highest_priority_status(statuses: &[SessionStatus]) -> SessionStatus {
    statuses
        .iter()
        .min_by_key(|status| priority_rank(status))
        .cloned()
        .unwrap_or(SessionStatus::Idle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scenario: feed `highest_priority_status` slices covering every rank in
    /// the PRD #333 table (including the Compacting→Thinking and
    /// Unknown→Idle aliases) and confirm it always returns the single
    /// highest-priority status present, plus the defined no-panes fallback.
    #[test]
    fn highest_priority_status_orders_by_priority() {
        use SessionStatus::*;

        assert_eq!(highest_priority_status(&[Idle, Error, Idle]), Error);
        assert_eq!(
            highest_priority_status(&[Working, WaitingForInput, Idle]),
            WaitingForInput
        );
        assert_eq!(highest_priority_status(&[Idle, Idle, Idle]), Idle);
        assert_eq!(highest_priority_status(&[Thinking, Working]), Working);
        assert_eq!(highest_priority_status(&[Compacting, Idle]), Compacting);
        assert_eq!(highest_priority_status(&[Unknown, Idle]), Unknown);
        assert_eq!(highest_priority_status(&[Error, WaitingForInput]), Error);
        assert_eq!(highest_priority_status(&[]), Idle);
    }
}
