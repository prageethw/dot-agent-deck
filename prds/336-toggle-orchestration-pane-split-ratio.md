# PRD #336: Toggle orchestration pane-column split ratio

**Status**: Complete
**Priority**: Medium
**Created**: 2026-08-03

## Problem Statement

In `PaneLayout` orchestration tabs, the sidebar (role list, left) and the pane column (agent terminals, right) split at a fixed ratio: `ORCHESTRATION_LEFT_PERCENT = 34` / `ORCHESTRATION_PANES_PERCENT = 66` (`src/ui.rs:1951-1952`). On a laptop screen this leaves the working pane noticeably narrower than it could be, and there is no quick way to reclaim that width — only a config edit and restart.

This is a companion to [#311](https://github.com/vfarcic/dot-agent-deck/issues/311) (removed the collapsed non-focused frames) and [#313](https://github.com/vfarcic/dot-agent-deck/issues/313) (a toggleable full-width zoom). Neither addresses the everyday case: keep the sidebar visible, just narrower.

## Solution Overview

Add a keybinding action, default `Ctrl+l` (remappable via the existing `keybindings.rs` `Action`/`ActionSpec` system), that toggles the orchestration tab's split between the default 34/66 and a narrower-sidebar 25/75 (1/4 sidebar, 3/4 panes) — and back again on a second press. Scoped to orchestration tabs only; no effect elsewhere.

## Scope

### In Scope

- A new `Action` (e.g. `ToggleOrchestrationSplit`) registered in `src/keybindings.rs`'s `ACTIONS` table, default chord `Ctrl+l`, section `Global` or a new orchestration-scoped section if warranted.
- Per-tab (not global) state tracking which ratio an orchestration tab is currently using, defaulting to 34/66, so toggling one tab does not affect others.
- The layout call site(s) that currently read the `ORCHESTRATION_LEFT_PERCENT` / `ORCHESTRATION_PANES_PERCENT` constants directly (`src/ui.rs:2051`, `:11217-11218`) so they resolve the active ratio for that tab instead of the fixed constants.
- L1 snapshot coverage (per CLAUDE.md rule 4) pinning both ratio states' geometry for an orchestration tab.
- `docs/keyboard-shortcuts.md` updated with the new binding; changelog fragment added.

### Out of Scope

- [#312](https://github.com/vfarcic/dot-agent-deck/issues/312) (retiring the global stacked/tiled toggle) and [#313](https://github.com/vfarcic/dot-agent-deck/issues/313) (zoom) — unrelated toggles on the same layout seam.
- Dashboard or mode tabs — this ratio is specific to the orchestration tab's sidebar/pane-column split.
- More than two ratio states (no cycling through arbitrary splits) — this is a two-state toggle, matching the user's request.
- Persisting the toggled state across restarts — resets to the 34/66 default on next launch unless a later PRD asks for persistence.

## Technical Approach

`ORCHESTRATION_LEFT_PERCENT` / `ORCHESTRATION_PANES_PERCENT` are currently fixed `const`s consumed at three call sites in `src/ui.rs`. Introduce a per-tab (or per-`TuiDeck`) piece of state — a bool or small enum, e.g. `orchestration_split_narrow: bool` — that the toggle action flips, and a small resolver function (e.g. `fn orchestration_split_percents(narrow: bool) -> (u16, u16)`) returning `(34, 66)` or `(25, 75)` that replaces direct references to the two constants at their call sites. The constants themselves can remain as the default-state values.

The toggle action needs a place to live: check whether existing per-tab state (alongside `pane_layout` or similar) is the natural home, or whether it needs its own field threaded through the same paths as other tab-scoped UI state.

### Cross-version safety

None. This is TUI-side rendering state, no daemon protocol, no hooks, no orchestration routing — CLAUDE.md rule 12's contract question does not arise. Patch-level bump.

## Success Criteria

- In an orchestration tab, pressing the toggle chord once changes the sidebar/pane-column split from 34/66 to 25/75; the sidebar visibly narrows and the pane column visibly widens.
- Pressing it again returns to the 34/66 default.
- The toggle is scoped per orchestration tab — toggling one tab's ratio does not change another open orchestration tab's ratio.
- No regression to non-orchestration tabs (dashboard, mode tabs) — the toggle has no effect there.
- The chord is remappable through the same config mechanism as every other keybinding.

## Milestones

- [x] **M1 — Per-tab split-ratio state added.** A field tracking narrow/default state exists per orchestration tab, defaulting to the 34/66 ratio.
- [x] **M2 — Toggle action wired.** New `Action` registered with default chord `Ctrl+l`; pressing it in an orchestration tab flips the tab's ratio state.
- [x] **M3 — Layout call sites resolve the active ratio.** The three `ORCHESTRATION_LEFT_PERCENT`/`ORCHESTRATION_PANES_PERCENT` call sites use the per-tab state instead of the fixed constants.
- [x] **M4 — L1 snapshot coverage.** `insta` render tests pin both ratio states' geometry for an orchestration tab, per CLAUDE.md rule 4.
- [x] **M5 — L2 coverage.** A vt100 test drives a real orchestration tab, toggles the chord, and asserts the visible column-width change and the round-trip back to default.
- [x] **M6 — Docs and changelog.** `docs/keyboard-shortcuts.md` updated with the new binding; changelog fragment added.

## Risks

- **Per-tab state placement.** If orchestration tab state is not already structured to hold extra per-tab UI fields cleanly, this could sprawl. Keep the new field adjacent to existing per-tab layout state rather than introducing a new state-tracking mechanism.
- **Chord conflicts.** `Ctrl+l` is free today (verified against the `ACTIONS` default table) but any future default-binding addition should re-check before landing.

## Open Questions

1. Does per-tab UI state already have a natural home (e.g. alongside `pane_layout`), or does this need new plumbing through tab construction/restore?
2. Should the toggle state survive tab restore (session save/resume), or is resetting to default on every restore acceptable for v1?

## Work Log

### 2026-08-03 — Created

Split out of the "1/3 vs 1/4 sidebar width" ask as a quick, scoped toggle. Distinct from #312 (retiring the global layout toggle) and #313 (full zoom) — this is a narrower, additive keybinding on the same layout seam.

### 2026-08-03 — M1-M6 complete

Per-tab split state, the `toggle_orchestration_split` action (default `Ctrl+l`), and the three `ui.rs` call sites all landed with L1 (`orchestration/layout/002`) and L2 (`tabs/orchestration/006`) coverage green. Docs (`docs/keyboard-shortcuts.md`) and the changelog fragment (`changelog.d/336.feature.md`) close out M6 — implementation complete.
