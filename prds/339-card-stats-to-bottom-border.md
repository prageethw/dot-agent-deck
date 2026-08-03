# PRD #339: Move card Last/Tools stats into the bottom border

**Status**: Not started
**Priority**: Medium
**Created**: 2026-08-02

## Problem Statement

Every session card carries two counters — `Last: 2m` (elapsed since last activity) and `Tools: 14`. Where they render depends on how wide the card is (`src/ui.rs:14546`):

- **Wide** (inner width ≥ 60): right-aligned on the `Dir:` line, which reserves ~26 columns and shrinks the directory to `w − 26`.
- **Narrow** (inner width < 60): their own dedicated content row, one row taller.

That single placement decision is the *entire* reason the `wide: bool` axis exists. Its consequences are out of proportion to the eight characters of information involved:

- **Card height has six values instead of three.** `card_height(density, wide)` returns 5/8/10 wide and 6/9/11 narrow (`src/ui.rs:98-108`), and `choose_density` (`:125`) has to take `wide` as an input just to compare against them.
- **Narrowing a card restructures it.** Crossing the 60-column boundary makes cards *taller*, which can push `choose_density` down a tier — Spacious → Normal → Compact — so the card shows **fewer** prompts and tool lines precisely when the user was trying to reclaim space. This is the concrete failure mode that blocks [#336](https://github.com/vfarcic/dot-agent-deck/issues/336) (toggle the orchestration sidebar 34% → 25%): on terminals roughly 183–247 columns wide, the toggle flips cards from wide to narrow, and "give me more room" makes the cards show less.
- **`Dir` is starved for no good reason.** On a 66-column inner card the directory gets 40 columns instead of 60, because 26 are held for two short counters. Long paths are truncated in the wide branch and — a latent bug — *clipped without an ellipsis* in the narrow branch, which never calls `truncate_with_ellipsis` (`src/ui.rs:14583`).
- **`wide` is computed twice, independently.** Once in the layout pass from `col_width` (`src/ui.rs:11562`) and once inside `render_session_card` from the block's inner width (`:14546`). They agree today only because `col_width − 2 == inner.width` by construction — the same class of drift `ORCHESTRATION_*_PERCENT` was extracted into constants to prevent.

The top border already proves the fix: the status badge (` ● Thinking `) renders as a right-aligned border title (`src/ui.rs:14532`) and costs zero content rows. Border cells are paid for by `Borders::ALL` whether or not anything is written into them.

## Solution Overview

Render `Last` / `Tools` as a bottom-border title and delete the `wide` axis.

After this change:
- Card height is a function of density alone — three values, not six.
- The `Dir:` line spans the full inner width in every card.
- Narrowing a card truncates monotonically; nothing restructures, no density tier is lost.
- `wide` is computed nowhere, so the two independent computations can't drift.

This removes a user-facing concept rather than adding one, and it is a prerequisite for #336 shipping without a known legibility regression.

## Scope

### In Scope

- `render_session_card` (`src/ui.rs:14425`): move the `Last`/`Tools` spans out of both the wide `padded_line` branch and the narrow standalone-row branch, into a `Block` bottom title; delete the local `let wide = w >= 60` and both branches it guards.
- `CardDensity::card_height` (`src/ui.rs:98`): drop the `wide` parameter and the `stats_line` term.
- `choose_density` (`src/ui.rs:125`): drop the `wide` parameter.
- The layout caller (`src/ui.rs:11560-11566`): delete the `col_width` / `wide` computation feeding the two calls above.
- `CardDensityKind::rendered_height` (`src/ui.rs:14954`): drop the `wide` parameter from this `#[doc(hidden)]` test seam and update its callers in `tests/`.
- `Dir` line: single un-branched form, truncated with `truncate_with_ellipsis` to the full inner width (fixes the narrow branch's ellipsis-less clip).
- A narrow-width fallback for the bottom title (see Technical Approach) so the counters degrade rather than collide with the border.
- L1 snapshot coverage for the new border content at a wide and a narrow card width, plus updates to the existing `render_dashboard__pane_00*` snapshots.
- `card_height_001_content_derived_values` (`src/ui.rs:21380`), which pins all six heights, reduced to three.

### Out of Scope

- **Which fields appear at which density.** The field-priority ladder — dropping `Dir` in orchestration tabs where every role shares one cwd, reordering so `Prmt` survives longest, deciding whether tool lines earn their rows at Compact — is a separate, larger content-model change and gets its own PRD. This PRD moves two counters and deletes one boolean; it does not renegotiate what a card shows.
- **The agent-type badge.** PRD #20 M5 / finding #9 deliberately restored the badge alongside the display name; shortening or removing it is not touched here.
- **Per-card variable height.** `choose_density` picks one density for all cards and the grid (`row_chunks`, `src/ui.rs:11674`) plus `clamp_scroll_offset` both assume uniform row height. Uniform height is preserved.
- **The #336 ratio toggle itself.** This PRD only removes the obstacle.

## Technical Approach

`Block` already carries two titles (a left-aligned identity title and a right-aligned status title, `src/ui.rs:14526-14536`). ratatui 0.30 supports bottom-positioned titles, so the counters become a third title on the same block — no new widget, no layout change, no content rows.

Placement: bottom-right mirrors the top-right status badge and leaves the bottom-left corner free. Bottom-left is also defensible; pick one and pin it in a snapshot.

### Narrow-width fallback

`Last: 2m  Tools: 14` is ~20 characters. The bottom border has `area.width − 2` usable cells, and the narrowest realistic card is a 25%-of-80-columns sidebar → 20 total → 18 usable. The full form does not always fit, and an over-long border title is worse than a content row because it collides with the corner glyphs.

Define an explicit degradation ladder rather than letting it clip, e.g.:

1. `Last: 2m  Tools: 14` (full)
2. `2m · 14 tools`
3. `2m · 14`
4. omitted entirely

Pick the widest form that fits `area.width − 2` minus a small padding allowance. Encode this as one function with unit tests over a width sweep, so the choice is testable independently of rendering.

### Calculations that change

The change is small in line count but touches every place card geometry is derived. All of these must move together or reserved height drifts from rendered content — the exact failure the `card_height` doc comment (`src/ui.rs:91-97`) warns about:

| Site | Change |
|---|---|
| `CardDensity::card_height` (`:98`) | drop `wide` param, drop `stats_line` term → 5/8/10 |
| `CardDensity` doc comment (`:84-97`) | rewrite the height table (currently documents six values) |
| `choose_density` (`:125-143`) | drop `wide` param; `density.card_height()` call at `:137` |
| Layout caller (`:11560-11566`) | delete `col_width` and `wide`; update both call sites |
| `visible_rows` (`:11648`) | derives from `card_height` — recheck, no signature change expected |
| Grid constraints (`:11674`) | derives from `card_height` — recheck |
| `render_session_card` (`:14546, :14559, :14605`) | delete `wide` and both branches |
| `CardDensityKind::rendered_height` (`:14954`) | drop `wide` param (public test seam) |
| `card_height_001_content_derived_values` (`:21380`) | six assertions → three |
| `tests/snapshots/render_dashboard__pane_00*.snap` | regenerate |

`clamp_scroll_offset` (`:158`) takes rows, not heights, and needs no change — but its callers feed it values derived from `card_height`, so verify the scroll behaviour at each density after the height table shrinks.

### Cross-version safety

None. Pure TUI rendering — no daemon, no protocol, no hooks, no orchestration routing. CLAUDE.md rule 12's contract question does not arise. Patch-level bump.

### Experimental flag

No (CLAUDE.md rule 9 asks the question; the answer here is no). This is a refinement of an existing surface rather than a new one, it is immediately self-evident on screen, and L1 snapshots pin both the border content and the new height table. Shipping it behind a flag would mean maintaining two card-height tables.

## Success Criteria

- `Last` and `Tools` are visible on every card at every density, in the bottom border, costing zero content rows.
- Card height depends only on density: Compact 5, Normal 8, Spacious 10 — no width input anywhere in the height calculation.
- The `Dir:` line uses the full inner width and ellipsizes (never bare-clips) when the path is too long.
- Resizing a card across the old 60-column boundary changes nothing structurally: no height change, no density tier change, no field appearing or disappearing.
- At the narrowest supported card width the counters degrade to a shorter form or are omitted — they never overrun the border.
- No `wide` boolean remains in the card layout or render path.

## Milestones

- [ ] **M1 — Stats move to the bottom border.** `render_session_card` renders `Last`/`Tools` as a bottom-positioned block title; both old branches deleted.
- [ ] **M2 — Height table collapses.** `card_height` and `choose_density` lose the `wide` parameter; the layout caller stops computing it; `CardDensityKind::rendered_height` and its test callers follow.
- [ ] **M3 — Narrow-width fallback.** The degradation ladder is implemented as one testable function with unit tests across a width sweep.
- [ ] **M4 — `Dir` uses full width.** Single un-branched `Dir:` line, ellipsized via `truncate_with_ellipsis`.
- [ ] **M5 — L1 coverage.** New snapshots pin the bottom-border content at a wide and a narrow card width; `card_height_001_content_derived_values` reduced to three assertions; existing `render_dashboard__pane_00*` snapshots regenerated. Each new `#[spec]` test carries a `/// Scenario:` comment (rule 7) and a `tests/CATALOG.md` entry.
- [ ] **M6 — Changelog.** Fragment added via `dot-ai-changelog-fragment`. No user-facing docs currently describe the card stats row; confirm before skipping a docs update.

## Risks

- **Reserved-vs-rendered height drift.** `card_height` exists so reserved height matches the lines `render_session_card` actually emits. Changing the emitted lines and the height table in separate commits leaves a window where they disagree — cards overlap or leave a blank row. Do M1 and M2 together.
- **Bottom-title collision with the border corners.** An over-long bottom title can eat the corner glyphs and make the card look broken. M3 is not optional polish; it is the guard against that.
- **Snapshot churn.** Every existing card snapshot changes. Review the regenerated `.snap` diffs by eye rather than accepting them wholesale — they are the only assertion that the border actually reads correctly.

## Open Questions

1. Bottom-**right** (mirroring the top-right status badge) or bottom-**left**? Right is the recommendation; either way pin it in a snapshot.
2. Should the shortened forms keep the `Last:` / `Tools:` labels, or is `2m · 14` self-evident enough at narrow widths given the position is fixed?
3. Do the counters belong on placeholder ("No agent") cards at all, where `Tools: 0` and an elapsed time carry no information?

## Work Log

### 2026-08-02 — Created

Split out of the #336 discussion. #336 (toggle the orchestration sidebar to 25%) surfaced that narrowing a card can flip it from wide to narrow layout and *lose* a density tier — more width for the pane column, less information in the sidebar. Tracing that back showed the `wide` axis exists solely to place two counters, and that the top border already solves the same problem for status. Removing the axis is a prerequisite for #336 and a simplification on its own. Related: [#336](https://github.com/vfarcic/dot-agent-deck/issues/336), and the separate not-yet-filed field-priority ladder PRD.
