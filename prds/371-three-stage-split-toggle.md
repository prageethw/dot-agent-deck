# PRD #371: Three-stage `Ctrl+l` pane-split toggle (Default / Narrow / Hidden)

**Status**: Implementation complete — PR pending
**Priority**: Medium
**Created**: 2026-08-04
**GitHub Issue**: [#371](https://github.com/vfarcic/dot-agent-deck/issues/371) (closed upstream as not-planned; this fork continues the work independently)
**Related**: Split from #361, alongside #372/#373/#374 (siblings). `src/ui.rs` (`DASHBOARD_LEFT_PERCENT`/`DASHBOARD_PANES_PERCENT`, `ORCHESTRATION_LEFT_PERCENT`/`ORCHESTRATION_PANES_PERCENT`, `split_cards_area`, `compute_frame_layout`, `split_stage_percents`), `src/tab.rs` (`Tab::Dashboard`, `Tab::Orchestration`, `SplitStage`, `next_split_stage`), `src/keybindings.rs` (`Action`/`ActionSpec`)

## Problem Statement

Both the Dashboard tab and Orchestration tabs render a horizontal split between a left sidebar (deck cards / role list) and a right pane column (agent terminals), and both are fixed-ratio constants today:

- Dashboard: `DASHBOARD_LEFT_PERCENT = 33` / `DASHBOARD_PANES_PERCENT = 67` (`src/ui.rs:1948-1949`)
- Orchestration: `ORCHESTRATION_LEFT_PERCENT = 34` / `ORCHESTRATION_PANES_PERCENT = 66` (`src/ui.rs:1950-1951`)

Both tabs already route through the same `split_cards_area(main_area, pane_ids, left_percent, panes_percent)` helper (`src/ui.rs:11362-11376`), called from `compute_frame_layout`'s `ActiveTabView::Dashboard` and `ActiveTabView::Orchestration` arms (`src/ui.rs:11289-11330`) — so the two tabs are structurally identical on this seam, differing only in which two constants they pass in.

On a laptop-sized terminal, roughly a third of the width goes to a sidebar that is often just a short list of cards or roles, leaving the working pane column narrower than it could be. There is no way to reclaim that width, or to go briefly full-width on the pane column (e.g. to read a wide log or diff) without permanently losing the sidebar, short of editing the source constant and rebuilding.

## Solution Overview

Add a keybinding action (default `Ctrl+l`, remappable through the existing `keybindings.rs` `Action`/`ActionSpec` system — `Ctrl+l` is unbound today, verified against the `ACTIONS` table in `src/keybindings.rs`) that cycles a tab's sidebar/pane-column split through **three** stages, looping back to the first:

1. **Default** — today's fixed ratio for that tab type (33/67 Dashboard, 34/66 Orchestration).
2. **Narrow** — 25/75 (sidebar shrinks to roughly a quarter width).
3. **Hidden** — sidebar collapsed to 0 width; the pane column takes the full tab area.

Pressing the chord again returns to Default, and the cycle repeats. State is **per-tab**: cycling one tab's stage never affects another open tab's stage — including tabs of the other type (toggling a Dashboard tab's stage doesn't move an open Orchestration tab, and vice versa).

**Both tab types are in scope from the start**, sharing one `Action` and one stage-cycle resolver function — investigation found the two tabs' layouts are already structurally identical on this seam, so this is a clean symmetric extension, not two separate features bolted together.

No persistence across restarts for v1 (every tab resets to Default on relaunch) — see Open Questions for why this isn't proposed as trivial.

## Scope

### In Scope

- One new `Action` (`CycleSplitStage`), section `Global`, default chord `Ctrl+l`, registered in `src/keybindings.rs`'s `ACTIONS` table.
- A `SplitStage` enum (`Default`, `Narrow`, `Hidden`) and a pure "next stage" resolver function (`Default → Narrow → Hidden → Default → …`), unit-tested independently of rendering.
- Per-tab state: a `split_stage: SplitStage` field added to both `Tab::Dashboard` and `Tab::Orchestration` in `src/tab.rs` (alongside their existing per-tab fields, e.g. `selected_session_id`, `role_pane_ids`), defaulting to `SplitStage::Default`. Threaded through to the corresponding `ActiveTabView::Dashboard`/`ActiveTabView::Orchestration` variants in `src/ui.rs` for the layout pass to read.
- The two `split_cards_area` call sites in `compute_frame_layout` (`src/ui.rs:11289-11330`) resolve the active stage's percentages instead of the fixed constants directly.
- L1 snapshot coverage pinning all three stages' geometry, for both an Orchestration tab and a Dashboard tab.
- L2 (PTY/vt100) coverage driving the chord through the full 3-stage cycle on both tab types, and asserting cross-tab and cross-tab-type isolation.
- `docs/keyboard-shortcuts.md` updated with the new binding and its 3-stage, both-tab-types scope; changelog fragment.

### Out of Scope

- More than three stages, or a user-configurable ratio for the Narrow stage — this is a fixed 3-stage cycle, matching the scope of the original request.
- Persisting the toggled stage across restarts (see Open Questions).
- Mode tabs — their 50/50 agent/side-pane split (`src/ui.rs:11262-11275`) is a different layout shape (no sidebar/pane-column split, no `split_cards_area` call) and is not part of this toggle.
- Changing the Default ratios themselves (33/67, 34/66) — those stay exactly as they are today; this only adds two additional stages reachable by cycling.

## Technical Approach

### Dashboard/Orchestration parity — investigated, confirmed symmetric

`DASHBOARD_LEFT_PERCENT`/`DASHBOARD_PANES_PERCENT` and `ORCHESTRATION_LEFT_PERCENT`/`ORCHESTRATION_PANES_PERCENT` are defined together (`src/ui.rs:1948-1951`) specifically so a shared helper (`split_cards_area`) and shared per-tab-type dims helpers (`dashboard_pane_dims`/`orchestration_role_pane_dims`, both delegating to `right_column_pane_dims`) can't drift apart. `compute_frame_layout`'s `ActiveTabView::Dashboard` and `ActiveTabView::Orchestration` arms are near-identical — both filter `all_pane_ids`, both call `split_cards_area` with their own two constants, both call `cards_pane_rects` on the result. This means extending the toggle to Dashboard tabs is a clean symmetric extension of the same resolver and the same call-site change, not a second feature.

### Stage resolver

```rust
enum SplitStage { Default, Narrow, Hidden }

fn next_split_stage(current: SplitStage) -> SplitStage {
    match current {
        SplitStage::Default => SplitStage::Narrow,
        SplitStage::Narrow => SplitStage::Hidden,
        SplitStage::Hidden => SplitStage::Default,
    }
}

/// `default_left`/`default_panes` are the tab type's own Default-stage
/// constants (33/67 for Dashboard, 34/66 for Orchestration) so one resolver
/// serves both tab types without hardcoding either ratio.
fn split_stage_percents(stage: SplitStage, default_left: u16, default_panes: u16) -> (u16, u16) {
    match stage {
        SplitStage::Default => (default_left, default_panes),
        SplitStage::Narrow => (25, 75),
        SplitStage::Hidden => (0, 100),
    }
}
```

Both `ActiveTabView::Dashboard` and `ActiveTabView::Orchestration` arms in `compute_frame_layout` call `split_stage_percents(view_stage, DASHBOARD_LEFT_PERCENT, DASHBOARD_PANES_PERCENT)` / `(…, ORCHESTRATION_LEFT_PERCENT, ORCHESTRATION_PANES_PERCENT)` respectively, then pass the result into the existing `split_cards_area` call unchanged.

### Hidden stage rendering

`split_cards_area` already handles a `Constraint::Percentage(0)` sidebar chunk correctly — `Layout::horizontal` produces a zero-width `Rect` for the sidebar and gives the remainder to the panes chunk, and a zero-width `Rect` renders nothing. This means Hidden needs no new branch or widget-suppression logic — it reuses the exact same code path as Default and Narrow with different numbers. Confirmed in practice by the M1/M4 implementation and its L1 snapshot.

### Where the per-tab state lives

`src/tab.rs`'s `Tab::Dashboard` and `Tab::Orchestration` variants already carry per-tab UI state alongside their structural fields (e.g. `selected_session_id`, `focused_role_pane_id`) — `split_stage: SplitStage` follows that existing pattern rather than introducing a new tab-keyed side-table. The `Tab` enum has no `Serialize`/`Deserialize` derive today (session snapshots are built from `Tab` state into separate `Saved*` structs at snapshot time, not derived automatically), which is why persistence is not free — see Open Questions.

### Experimental-flag question (CLAUDE.md rule 9)

**Resolved: No — visible by default.** A keybinding-driven layout affordance with no persisted state, and existing L1/L2 test coverage is sufficient verification. Matches precedent (PRD #336/#333/#341).

## Success Criteria

- In an Orchestration tab, pressing the chord cycles the sidebar/pane-column split Default → Narrow (25/75) → Hidden (sidebar gone, pane column full-width) → Default, looping indefinitely.
- The same cycle works identically on a Dashboard tab, using that tab's own Default ratio (33/67) as stage 1.
- The cycle is scoped per tab: toggling one tab's stage does not change another open tab's stage, whether that other tab is the same type or the other type.
- No effect on Mode tabs.
- The chord is remappable through the same config mechanism as every other keybinding.

## Milestones

- [x] **M1 — `SplitStage` enum and resolver.** Pure-data type and `next_split_stage`/`split_stage_percents` functions, unit-tested (including the Hidden-stage zero-width rendering assumption).
- [x] **M2 — Per-tab state added.** `split_stage` field on both `Tab::Dashboard` and `Tab::Orchestration`, defaulting to `SplitStage::Default`; threaded through to `ActiveTabView`.
- [x] **M3 — `CycleSplitStage` action wired.** New `Action` registered with default chord `Ctrl+l`; pressing it in either tab type advances that tab's stage.
- [x] **M4 — Layout call sites resolve the active stage.** Both `compute_frame_layout` arms use `split_stage_percents` instead of the fixed constants directly.
- [x] **M5 — L1 snapshot coverage.** `insta` render tests (`layout_001_ctrl_l_cycles_dashboard_split_stages`, `layout_002_ctrl_l_cycles_orchestration_split_stages`, `src/ui.rs`) pin all three stages' geometry for an Orchestration tab and a Dashboard tab.
- [x] **M6 — L2 coverage.** `tests/e2e_orchestration_pane_column.rs::orchestration_006_ctrl_l_cycles_pane_column_split_stages` and `tests/e2e_dashboard_pane_column.rs::dashboard_001_ctrl_l_cycles_dashboard_split_stage_isolated_from_orchestration` drive the chord through the full 3-stage cycle on both tab types and assert cross-tab / cross-tab-type isolation.
- [x] **M7 — Docs and changelog.** `docs/keyboard-shortcuts.md` updated with the new binding and its scope; changelog fragment added.

## Risks

- **Snapshot churn.** Existing full-frame Dashboard/Orchestration snapshots are unaffected in content (Default stage is numerically identical to the pre-existing behavior), but any snapshot that happens to pin the *type* of `ActiveTabView` variant fields needed updating once `split_stage` was added — already accounted for in the landed diff.
- **Chord conflicts.** `Ctrl+l` is free today (verified against the `ACTIONS` default table on `upstream/main`); re-check before landing in case another default binding was added upstream in the meantime.

## Open Questions

1. **Persistence across restarts.** Not proposed as in-scope: `Tab` has no `Serialize`/`Deserialize` derive, and session snapshots are built into separate `Saved*` structs rather than mirroring `Tab` automatically, so wiring `split_stage` through snapshot/restore is real (if probably small) additional work, not a free field default. Left for a follow-up if users want it.
2. **Experimental flag gating** — resolved: no, visible by default (see Technical Approach above).
3. **Action naming** — `CycleSplitStage` (as landed) vs. `ToggleSplitStage` (reads oddly for a 3-state cycle vs. a 2-state toggle) vs. `CyclePaneSplit`. Settled as `CycleSplitStage` in the landed implementation; noted here only for history.
