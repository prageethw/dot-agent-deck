//! L1 coverage for the same-cwd orchestration warning in the new-pane form.
//!
//! The test drives the production form renderer through a narrow public
//! `TestBackend` seam. The seam accepts the form cwd plus the `(cwd, name)`
//! pairs learned from live daemon records so both the collision and
//! fresh-cwd cases exercise the same warning decision used by the
//! interactive flow.

use dot_agent_deck::ui::render_new_pane_orchestration_guard_to_buffer;
use spec::spec;

fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    let area = buffer.area();
    let mut out = String::new();
    for y in 0..area.height {
        for x in 0..area.width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

/// Scenario: Render the new-pane form with an orchestration selected, first
/// for a cwd absent from the daemon's live orchestration records and then for
/// a `/work/already-live`-picked directory whose one live orchestration's
/// ACTUAL reported cwd is the sibling workspace path
/// `/work/already-live-already-live-orchestrator-1` (issue #605; the
/// previous fixture injected a cwd literally identical to the picked
/// directory, a shape production can no longer produce, so it silently
/// passed against dead code). Only the collision warns that the working
/// tree is shared and points the user at a worktree, without blocking the
/// form.
///
/// PRD fork#760 fix round correction (reviewer N10 / auditor N11): the
/// doubled-basename shape this fixture injects is
/// [`crate::live_orchestration_occupies`]'s LEGACY `scan_by_name`
/// reconstruction shape (`<dir-basename>-<sanitize(live-Name)>`) — it is
/// what production emits ONLY when a live peer's Name happens to sanitize
/// to `<dir-basename>-orchestrator-N` (a coincidence, not the common case),
/// never what a BLANK-slug open (the default, most common path) produces.
/// A blank-slug open's real cwd is the single, non-doubled
/// `<dir-basename>-orchestrator-N` shape (`/work/already-live-orchestrator-1`
/// here, not doubled) — recognized by the newer, structural `scan_by_shape`
/// half of the same function, pinned separately by
/// `orchestration/identity/041` in `src/ui.rs`, not by this test. This test
/// exercises `scan_by_name` alone; it is a real, still-reachable case (a
/// typed Name coincidentally matching the auto-generated shape), not the
/// shape production "always" produces.
#[spec("orchestration/guard/001")]
#[test]
fn guard_001_warns_for_same_cwd_live_orchestration_only() {
    let fresh = buffer_text(&render_new_pane_orchestration_guard_to_buffer(
        "/work/fresh",
        &[("/work/other-other-orchestrator-1", "other-orchestrator-1")],
        100,
        28,
    ));
    // Fork #122: the new-pane form always shows a "Worktree:" slug field once
    // an orchestration is selected, independent of this warning — so a bare
    // "worktree" substring no longer distinguishes the two. "/worktree-prd" is
    // the warning copy's own text and doesn't appear in the field label.
    assert!(
        !fresh.contains(".dot-agent-deck") && !fresh.contains("/worktree-prd"),
        "a fresh cwd must not show the shared-resource warning, got:\n{fresh}"
    );

    let collision = buffer_text(&render_new_pane_orchestration_guard_to_buffer(
        "/work/already-live",
        &[(
            "/work/already-live-already-live-orchestrator-1",
            "already-live-orchestrator-1",
        )],
        100,
        28,
    ));
    assert!(
        collision.contains("working tree"),
        "same-cwd orchestration warning must name the shared working tree, got:\n{collision}"
    );
    assert!(
        collision.to_lowercase().contains("worktree"),
        "same-cwd orchestration warning must point the user at a worktree, got:\n{collision}"
    );
    assert!(
        collision.contains("[Submit]"),
        "the warning is non-blocking: the form must retain its Submit action, got:\n{collision}"
    );
}

// `orchestration/guard/002` moved into `src/ui.rs`'s own `#[cfg(test)] mod
// tests` (fork#192 review round 2, F3/F8): the corrected contract needs the
// click-hit-test rects `render_new_pane_form` returns to pin that a colliding
// `[Submit]` is present-but-INERT (excluded from the rects, not merely
// removed from the row) and that `[Cancel]`'s rect doesn't move — neither is
// reachable through this file's buffer-only public seam.
