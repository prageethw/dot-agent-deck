# PRD fork#377: Drop the active-tab underline — bold plus a fully coloured name is the whole cue

**GitHub Issue**: [fork #377](https://github.com/prageethw/dot-agent-deck/issues/377)

**Priority**: Low

**Status** *(2026-08-16)*: **Complete.** PR [#382](https://github.com/prageethw/dot-agent-deck/pull/382) on branch `feat/377-drop-active-tab-underline`. RED (`e0395035`) and GREEN (`3a3413ae`) both confirmed from CI; `reviewer` and `auditor` both ran, and their findings were fixed in `eb02b811` (docs/src) and `4681bb09` (tests). All gates green on `4681bb09`: CI 3262/3262, 3261/3261, 1954/1954; e2e 9371/9371, zero failures. This fork has no rulesets, no required status checks and no required approval (`gh api repos/prageethw/dot-agent-deck/rulesets` returns empty) — the `main-protected` ruleset in CLAUDE.md rule 8 belongs to `vfarcic/dot-agent-deck`, not this fork.

**Fork-only?** **Yes — permanently.** This is a *preference*, not a bugfix, feature or enhancement upstream would want. Upstream chose `UNDERLINED | BOLD` deliberately in issue #306 as the replacement for `REVERSED`. Under CLAUDE.md rule 19's decision test the answer to "would upstream want this?" is **no**, so there is nothing to offer and no upstream-offer issue to file at merge time. It carries a **PERMANENT** row in [`fork-sync-workflow.md`](../docs/develop/fork-sync-workflow.md) instead.

**Why a PRD for a one-line change.** `work-type-check` R4 requires a `prds/` file behind any `.feature.md` fragment, and the fragment suffix is correct: this changes user-visible behaviour, so it is `prd`-type work under [`work-types.md`](../docs/develop/work-types.md), not `bug` or `chore`. The document is short because the change is small — that is the honest outcome, not an omission.

---

## Problem Statement

The request arrived as a bug report: *"the entire tab name used to be status-coloured; a later fix implemented only a coloured underline and left the text uncoloured."*

**Both halves of that premise turned out to be wrong**, and establishing that is most of this PRD's value.

### 1. The colouring was never lost

Fork issue #351 / PR #352 restored full-text status colouring and merged as `1408686e`, shipped in v0.38.2. It was intact the whole time: `render_tab_strip` (`src/ui.rs:14639`) applies `.fg(palette::status_color(highest_priority_status(...)))` to the label span for active and inactive tabs alike, and `tab_status_data` (`src/ui.rs:15122`) still carries the fork-only `Tab::Mode` arm.

Investigated under fork issue **#371**, which was closed as not-a-defect. `tester` drove a real pane through the unmodified hook socket and captured the tab strip's raw ANSI: `Thinking` → `38;5;4` (blue), `WaitingForInput` → `38;5;3` (yellow), `Idle` → `38;5;8` (dark gray). The maintainer then confirmed by direct observation that live `Tab::Orchestration` tabs carry the right colour too.

### 2. What made it *look* uncoloured

`palette::STATUS_IDLE` is `Color::DarkGray`. On a dark terminal an all-idle tab strip is visually indistinguishable from an uncoloured one — so **correct behaviour and the reported defect produce the same pixels**. That contrast cost is accepted explicitly by the `FORK-ONLY` comment above `src/ui.rs:14639`, a maintainer decision of 2026-08-15 that made colour a total function of status.

The residual signal in that state was the active tab's underline, which is what the report described.

### 3. What was actually wanted

Not a colour fix: **remove the underline.** The status colour should be the whole signal, with bold marking the active tab.

## Scope

| Tab | Modifier | Foreground |
|---|---|---|
| Active | `BOLD` only | full status colour |
| Inactive | none | full status colour |
| Dashboard (active) | `BOLD` only | `Color::Reset` — carries no status data, unchanged |

"Fully" is part of the ask: the label's padding spaces **and** the `[×]` close glyph carry the same colour as the name. They already did — both take the same `style` at `src/ui.rs:14660` and `:14668` — but nothing asserted it, so it is now pinned rather than assumed.

`.fg(Color::Reset)` is retained on `active_style`: the Dashboard tab depends on it, and `tabs/orchestration/012` asserts it.

## Out of scope

Two `UNDERLINED` sites are unrelated and deliberately untouched — the star-prompt hyperlink (`src/ui.rs:17562`) and vt100 attribute passthrough (`src/terminal_widget.rs:41`). They are named here because a global search-and-replace would catch both.

Revisiting `STATUS_IDLE`'s contrast is **not** in scope. It is a live question — fork issue [#312](https://github.com/prageethw/dot-agent-deck/issues/312) already tracks a palette-contrast problem on the same axis — but it reverses a deliberate maintainer decision and belongs to its own change.

## Milestones

- **M1 — RED.** Flip four active-tab assertions in `tests/render_tab_strip.rs` from `contains(UNDERLINED) && contains(BOLD)` to `contains(BOLD) && !contains(UNDERLINED)`, so the underline's *absence* is pinned rather than merely unmentioned. Add the padding/close-glyph colour assertions. Rename four functions off the old cue, catalog IDs unchanged. **Done — `e0395035`; CI: 3262 run, 3258 passed, 4 failed, identically on Linux/macOS/Windows.**
- **M2 — GREEN.** `Modifier::UNDERLINED | Modifier::BOLD` → `Modifier::BOLD` at `src/ui.rs:14595`; five comments rewritten to name BOLD while preserving the issue #306 reasoning for why `REVERSED` is unusable; `docs/orchestration.md`; `changelog.d/377.feature.md`. **Done — `3a3413ae`; CI: 3262/3262, 3261/3261, 1954/1954, zero failures.**
- **M3 — Sync protection.** PERMANENT row in `fork-sync-workflow.md`, stated as a *behaviour comparison* ("does an upstream active tab carry an underline?") rather than a symbol grep — both modifiers legitimately remain in the tree, so a bare grep proves nothing, and that file already records two cases where a name-based check returned a confident false negative. **Done — same commit.**
- **M4 — Review.** `reviewer` + `auditor`. **Done — findings fixed in `eb02b811` (docs/src) and `4681bb09` (tests); all gates green on `4681bb09`.**

## Test plan

| Catalog ID | Tier | Action | Scenario |
|---|---|---|---|
| `tabs/orchestration/010` | L1 widget | modify | An active tab with an `Error` pane renders Red as ordinary foreground, cued `BOLD` with no `UNDERLINED`; padding and `[×]` share that colour; an inactive Idle tab renders `STATUS_IDLE`. |
| `tabs/orchestration/012` | L1 widget | modify | An active Dashboard tab — the one tab carrying no status data — is cued `BOLD`, no `UNDERLINED`, no `REVERSED`, `Color::Reset`. |
| `tabs/orchestration/014` | L1 widget | modify | An active tab whose aggregate resolves to Idle renders `STATUS_IDLE` while cued `BOLD` only. |
| `tabs/label/002` | L1 widget | modify | An active tab carrying `Some(&[Working])` renders `STATUS_WORKING`, cued `BOLD` only. |

L1 is the right tier throughout: this is a pure widget-styling change with no daemon, hook, protocol or spawn involvement, so CLAUDE.md rule 4's L2 requirement does not apply and no `.cast` or reel clip arises.

## Verification

The automated gates cannot close this one. No check in this repo reads "is there an underline" — the four L1 tests assert the modifier bits, which is as close as automation gets. **Final confirmation is the maintainer's own eyes on a running deck**: no underline anywhere in the tab strip, the active tab bold, every tab name fully coloured including its padding and close glyph.

That gap is the whole lesson of the #371 detour: a green board was never going to answer the question being asked.

With **every tab idle**, confirm the active tab is still identifiable on the terminal actually in use. This is the accessibility gap the auditor raised, and the maintainer's chosen route is to ship as-is and verify by eye rather than add a fallback branch. ratatui maps `Color::DarkGray` to SGR 90 (bright black), so a terminal that implements bold as "use the bright variant" rather than a heavier face has nothing left to brighten once the foreground is already the bright variant of the palette — the active cue can render as nothing at all. `UNDERLINED` was immune to this, being geometric rather than a colour/weight cue. The existing checklist is most easily satisfied on a non-idle strip and never exercises the all-idle case — the one state this PRD itself identifies as risky (see "What made it *look* uncoloured" above). There is a direct precedent on this same colour: `src/palette.rs:22-27` records that for issue #442, colour and weight each failed to signal on their own in a real report, and only a third, non-colour cue (`▸ `) closed it.
