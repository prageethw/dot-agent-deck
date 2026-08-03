# PRD #333: Orchestration tab label reflects highest-priority pane activity

**Status**: Complete
**Priority**: Medium
**Created**: 2026-08-03
**GitHub Issue**: [#333](https://github.com/vfarcic/dot-agent-deck/issues/333)
**Related**: `src/palette.rs` (`status_color()`, the single source of truth for status colors), `src/state.rs` (`SessionStatus` enum), `src/ui.rs` (`render_tab_bar_to_buffer`, `TabBarInfo`), `src/tab.rs` (`Tab::Orchestration`, per-tab pane collection)

## Problem Statement

A user with several orchestration tabs open has no way to tell, without switching to each one, whether anything inside it needs attention. Each pane already carries a live status (Working/Thinking/Needs Input/Error/Idle) rendered via the shared palette in `src/palette.rs`, but that signal is invisible until the tab is focused. The more orchestration tabs someone runs in parallel, the more this matters — it's exactly the "what needs me right now" question a multi-agent dashboard should answer at a glance.

## Solution

Color the orchestration tab's label text using the same per-pane status palette already in `src/palette.rs` — no new colors, no new palette module. The color reflects the single highest-priority state among that tab's panes, evaluated in this fixed order (confirmed with the user):

| Priority | State | Color | Notes |
|---|---|---|---|
| 1 (highest) | Error | Red | `SessionStatus::Error` |
| 2 | Needs Input | Yellow | `SessionStatus::WaitingForInput` |
| 3 | Working | Green | `SessionStatus::Working` |
| 4 | Thinking | Blue | `SessionStatus::Thinking`, and `Compacting` (already aliases to Thinking's color in `palette.rs`) |
| 5 (lowest) | Idle | DarkGray | `SessionStatus::Idle`, and `Unknown` (already aliases to Idle's color in `palette.rs`) |

Scope decisions, confirmed with the user during PRD creation:
- **Orchestration tabs only.** Single-pane/mode tabs already show their own status directly and don't need an aggregate signal.
- **Renders as the tab label's text color** (not a dot/icon prefix, not a border/underline) — consistent with how deck cards already color their status badge text.

## Decisions

- **Reuse `palette::status_color()`, do not introduce a 6th color.** `palette.rs`'s own doc comments note all four "loud" named-ANSI slots (green/blue/yellow/red) are already assigned and cyan/magenta are reserved for the `FOCUSED`/`SELECTED` accents — there is no free slot for a distinct 6th status color without colliding with an existing role. This feature is a new *aggregation* over the existing five colors, not a new palette entry.
- **No experimental flag.** Per CLAUDE.md rule 9, this is a small, low-risk enhancement to an existing user-visible surface (the tab bar already renders labels; this only changes their color under specific conditions), not a new pane/field/command/tab/keybinding — falls outside the flag's intended scope, and the change is trivially reversible if it doesn't read well in practice.
- **No cross-version contract impact.** Purely a rendering change in the TUI; touches neither the daemon, the TUI↔daemon protocol, orchestration routing, nor hooks. No `PROTOCOL_VERSION` bump, no `.breaking.md` fragment (CLAUDE.md rule 12).

## Milestones

- [x] **M1 — Aggregate-priority resolver.** A pure function that takes an orchestration tab's pane statuses and returns the single highest-priority `SessionStatus` per the table above (or a defined "no panes" fallback). Unit-testable in isolation from rendering.
- [x] **M2 — Wire into tab-label rendering.** `render_tab_bar_to_buffer` colors an orchestration tab's label using `palette::status_color()` on the resolver's output instead of the current neutral/fixed label color. Non-orchestration tabs are unaffected.
- [x] **M3 — Tests.** L1 widget/snapshot coverage (`insta`) for the resolver's priority ordering (including the Compacting→Thinking and Unknown→Idle aliasing) and for the rendered tab-bar color under representative multi-pane mixes. Per CLAUDE.md rule 4, this is a functional TUI change and needs harness coverage, not just the pure-function unit test in M1.
- [x] **M4 — Documentation.** Note the new tab-label behavior wherever the tab bar / orchestration tabs are currently documented (check `docs/` for an existing orchestration-tabs page before adding a new one).

## Success Criteria

- With multiple orchestration tabs open and panes in different states, the non-focused tabs' label colors correctly surface the single most urgent state per tab without opening it.
- A tab with a panicking/errored pane always shows Red regardless of what else is happening in that tab, per the fixed priority order.
- No new color is introduced; every rendered color is one already emitted by `palette::status_color()` today.

## Risks

- **Priority collisions reading as "wrong."** If a tab is mostly Idle with one pane transiently Thinking, the label will flip to Blue even though "nothing important" is happening from the user's point of view. Mitigation: this is the explicitly requested behavior (highest-priority-wins), not a bug — revisit only if real usage shows it's noisy.
- **Aggregate color may be mistaken for a single pane's status when a tab is later focused and only one pane is visible.** Mitigation: this PRD only changes the *tab bar* label; it does not touch how any individual pane or deck card renders its own status, so there's no ambiguity once the user is inside the tab.

## Work Log

### 2026-08-03 — M1-M4 complete

The aggregate-priority resolver, its wiring into `render_tab_bar_to_buffer`, L1 `insta` snapshot coverage for the priority ordering and aliasing, and the `docs/orchestration.md` note all landed. `tests/render_tab_strip.rs` also gained a real zero-pane orchestration tab case for `layout_004`. Implementation complete.
