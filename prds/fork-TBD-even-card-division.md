# PRD fork#TBD — Divide the dashboard grid evenly so every card is visible

**Issue:** _not yet filed_ — file against `prageethw/dot-agent-deck`, then rename this file to `prds/fork-<issue>-even-card-division.md` and fill this link.
**Predecessors:** fork [#437](https://github.com/prageethw/dot-agent-deck/issues/437) / upstream [#588](https://github.com/vfarcic/dot-agent-deck/issues/588) (joint `(cols, density)` fit-seeking) — this is its direct follow-on. PRD #147 set the original "7 decks fit without scrolling" goal; PRD #339 and fork#405 each spent part of the margin that goal depended on.
**Experimental flag:** No (CLAUDE.md rule 9 — this changes how an existing surface lays itself out rather than adding one, and gating it would make card height flag-dependent, exactly the coupling fork#405 avoided when it made the role row unconditional).

---

## Problem

An orchestration tab with 7 roles paints 5 cards. The other two are reachable only by
scrolling.

This is reproducible and fully diagnosed. Config and runtime are both correct — the
`intent-sdd-agent-flow` orchestration's 7 roles are present in the project toml, in the
saved session, and as 7 live panes in `daemon status`, with the right names in the right
order. Nothing is drifting. The grid simply refuses to paint them.

`fit_grid` (`src/ui.rs:19574`) searches column count and density jointly, which is what
fork#437 added. But both axes are bounded:

- columns cap at `max_cols_for_width(width) = width / 40` (`src/ui.rs:19557`)
- density bottoms out at `Compact`, whose `card_height()` is a fixed **6 rows**
  (`src/ui.rs:116-125`)

The reported dashboard column is under 80 wide, so `max_cols` is 1 and the joint search has
exactly **one** candidate to consider. With ~32 rows available,
`visible_rows = 32 / 6 = 5`, and the render loop slices the row list to that
(`src/ui.rs:15743-15745`). Everything past row 5 is dropped into the scroll region.

Because 6 rows per card is a hard floor, the outcome does not depend on how many roles
exist: 7 roles show 5, 8 roles show 5, 12 roles show 5. Adding a role makes the deck
strictly less informative. There is no terminal resize that fixes it while the right-hand
panes keep the dashboard column narrow.

### This was the original design, and it eroded

PRD #147 (`37205237`) landed precisely this goal — *"fitting 7 decks without scrolling in
the single-column orchestration tab"* — and guarded it with
`layout_001_seven_decks_fit_single_column` (`tests/render_dashboard.rs:3208`). That test
still exists and still passes. It does not protect the real deck, for two reasons:

1. **It pins the property at exactly one size.** `AVAILABLE = 48` rows, described in the
   test as "a ~50-row card column". The reported deck has ~32. Nothing covers shorter.
2. **Its slack is gone.** The test's own doc comment records the erosion: fork#405 M1
   *"spends this test's last margin: 48 / 7 = 6 < 7, so height 6 is now the LARGEST Compact
   height that still satisfies it — the next content row anyone adds to a card breaks this
   test, and there is no more slack to absorb it."*

So the intent was never reverted. The *mechanism* — one fixed card height, verified at one
assumed terminal size — was always one content row away from collapse, and two features
(PRD #339 moving counters onto the border, fork#405 giving the role name its own row) spent
what was left.

### Why scrolling is the wrong fallback

This is an observability defect, not a cosmetic one.

Tab-level status is aggregated (PRD #78). So the tab reports **working** while the card
naming *which role* is working sits scrolled out of view. The deck asserts that something
is happening and simultaneously hides its source. For status purposes a card you have to
scroll to is not rendered at all — the operator sees activity they cannot attribute.

The card count is the human's decision. They chose those roles in the toml. The grid's job
is to show all of them; silently picking a subset substitutes the layout's judgement for
the operator's.

Two constraints follow, and they drive every decision below:

1. **No card is ever hidden.** Scrolling stops being something the layout reaches for.
2. **Role name and status survive every degradation.** Whatever height a card is squeezed
   to, it keeps the two things it exists to answer: *which role*, and *what it is doing*.
   Height comes out of prompts, tool lines and `Dir:` — never those two.

## Outcome

The dashboard column divides into as many equal parts as there are card rows. Cards get
shorter; they never get fewer.

Seven roles in a ~32-row column — four rows of 5, three of 4, summing to exactly 32:

```
┌ 1 ClaudeCode (Opus) ──────────────── ● Working ┐
│ orchestrator                                   │
│ Dir:  intent                                   │
└──────────────────────── Last: 0s  Tools: 3 ────┘
┌ 2 ClaudeCode ────────────────────────── ● Idle ┐
│ developer                                      │
│ Dir:  intent                                   │
└──────────────────────── Last: 4m  Tools: 0 ────┘
             … five more, all painted …
```

Twelve roles in the same column fall to 2 rows each and degrade rather than disappear:

```
┌ 1 ClaudeCode (Opus) ──────────────── ● Working ┐
│ orchestrator                                   │
┌ 2 ClaudeCode ────────────────────────── ● Idle ┐
│ developer                                      │
             … ten more, all painted …
```

At one row per card, the border goes and the line carries identity and status alone:

```
 1 orchestrator                         ● Working
 2 developer                              ● Idle
```

In every one of those shapes the operator can answer "who is working?" without touching a
key.

## Decisions

| Question | Decision |
|---|---|
| When does even division engage? | **Only when the grid would otherwise truncate.** `fit_grid` keeps winning whenever the cards genuinely fit. |
| Floor on card height | **1 row.** Not a comfortable minimum. |
| What gives way as cards shrink | `Prmt:` lines, tool lines, then `Dir:`. **Never** the role name or the status. |
| Column count in the fallback | **`max_cols_for_width`** — maximise columns first, then divide. |
| Where the layout lives | **Per-frame locals.** No new `UiState` field, nothing persisted. |
| `layout_001`'s "scrolling must still engage" guard | **Deliberately inverted.** See below. |
| Experimental flag | **No** (rule 9). |

**Why only when it would otherwise truncate.** The existing density ladder produces genuinely
better cards when there is room for them — prompts and tool lines are worth showing. Even
division is a degradation, not an improvement, so it must never pre-empt a layout that fits.
Concretely: `fit_grid`, `grid_columns` and `choose_density` are not modified at all. This
composes with them, exactly as fork#437's `fit_grid` composed rather than replaced.

**Why the floor is 1 row and not something readable like 3.** A comfortable floor is just
truncation with extra steps — below it, cards vanish again and the observability defect
returns at a higher role count. Whether a 1-row card is *useful* is a rendering question,
answered by the tiers below, not a reason to hide it. The only case with no answer is
`available_height < total_rows`: fewer rows in the pane than cards, where no layout can show
them all and the existing scroll path stays.

**Why maximise columns before dividing.** Even division only runs when the alternative is
dropping cards, so the usual argument for keeping cards wide (readability) is outranked.
Halving the row count doubles the height available to every card, which buys back a whole
degradation tier before any squeezing happens. At the reported width (<80) this changes
nothing — `max_cols_for_width` is 1 — but it matters as the pane widens or roles multiply.

**Why role name and status are the two survivors.** They are the question the deck exists to
answer. A card showing `Working` with no visible role reproduces the original defect at card
scale — activity you cannot attribute. A card showing a role with no status is inert. The
status badge is already short and right-aligned (`src/ui.rs:19687-19692`), so both fit on one
line even at narrow widths.

**Why per-frame and never stored.** The main loop already draws once per iteration
(`src/ui.rs:13454`) and that call autoresizes the buffer to the live terminal
(`src/ui.rs:13401`), so a derived layout has no reason to persist. Keeping it local is what
makes the behaviour dynamic for free: a role added to the toml, a pane spawned or closed
mid-session, a resize, or a change to the dashboard/panes split all re-divide on the next
frame with no restart and no cache to invalidate.

## Design

### M1 — Even division

A helper beside `fit_grid` in `src/ui.rs`:

```rust
/// Row heights dividing `available_height` across `total_rows` as evenly as
/// possible — the first `available_height % total_rows` rows get one extra row,
/// so the heights sum to exactly `available_height` and leave no blank tail.
/// `None` only when `available_height < total_rows`, i.e. there is not even one
/// row per card and no layout can show them all.
fn even_row_heights(total_rows: usize, available_height: u16) -> Option<Vec<u16>>
```

At the render site (`src/ui.rs:15632` for the `fit_grid` call, `:15760-15767` for the
constraint vector), when `total_rows * card_height > available_for_density`, enter
**fit-all mode**:

- recompute columns as `max_cols_for_width(dashboard_area.width)` and re-chunk;
- `Some(heights)` → `visible_rows = total_rows`, force `ui.scroll_offset = 0`, and build the
  vertical constraints from `heights` rather than `total_rows` copies of
  `Constraint::Length(card_height)`;
- `None` → today's slice-and-scroll, unchanged.

`density` still selects card *content*; only the reserved height changes.

Two existing behaviours fall out correctly and **must not be special-cased**:

- `hidden = showing - displayed_cards` (`src/ui.rs:15753-15757`) becomes 0, so the
  `" (N more — scroll to see)"` title suffix disappears on its own.
- `clamp_scroll_offset` (`src/ui.rs:170`) already returns 0 once `visible_rows >= total_rows`.

**One state field needs care.** `ui.columns` is written from `grid_columns` in the main loop
(`src/ui.rs:13271`) and again at the render site (`:15633`), and it drives keyboard
navigation. Fit-all mode recomputes columns, so it must write that same value into
`ui.columns` — otherwise navigation indexes a grid shape different from the one painted. The
test at `src/ui.rs:24848` already pins exactly this class of disagreement between the two
`grid_columns` callers; **extend it**, do not add a parallel test.

### M2 — Height-tiered card rendering

This is what makes "any N" real rather than aspirational. Today `render_session_card`
(`src/ui.rs:19670`) is unconditionally `Borders::ALL` plus a body `Paragraph`
(`:19837-19861`), so 2 rows go to border before any content: at 2 rows a card is all border,
at 1 row a single border line. Branch on `area.height`:

| Height | Rendering |
|---|---|
| `>= 3` | Today's bordered card, body clipped naturally — **no new code** |
| `2` | `Borders::TOP` only. The top border already carries the shortcut/badge title and the right-aligned status, so the single inner row goes to the role name. |
| `1` | No block. One line straight into `area`: selection prefix + card number + role name, status right-aligned. |

The `>= 3` tier needs nothing because the body renders as one `Paragraph` into
`block.inner(area)` and ratatui clips lines past the area height — the idle-art comment at
`src/ui.rs:19942` already depends on this. A `Compact` card painted into 4 rows renders
border, role name and `Dir:`, dropping prompt and tool lines.

Content priority is already correct for squeezing, which is why the clipping does the right
thing rather than an arbitrary thing: `lines[0]` is the role name (`src/ui.rs:19880`),
`lines[1]` is `Dir:` (`:19893`), prompts and tools follow.

M1 and M2 are separable and can land in either order, but M1 alone is only correct down to
3-row cards. Ship both before closing the issue.

## Tests

- `even_row_heights` sums to its input exactly, across several `(total_rows, available)`
  pairs including non-zero remainders — e.g. `(7, 32)` → `[5, 5, 5, 5, 4, 4, 4]`.
- Returned heights differ by at most 1 across rows.
- `even_row_heights(n, n) == Some(vec![1; n])` — the floor case.
- `even_row_heights(n, n - 1) == None`.
- Painted-output at each tier boundary (card heights 1, 2, 3): **both** the role name and
  the status text appear. Assert on status, not only the name — that is the regression guard
  for the defect this PRD is about.
- A `Working` card at every tier paints its status, so the tab's "working" claim always has
  a visible owner.
- Painted-output parameterised over role counts (7, 10, 16) in a narrow-and-short dashboard:
  **every** role name appears, and the title carries no `more — scroll to see`.
- Dynamic re-division: render the same session set into two different area heights; both
  fill completely and hide nothing.
- `ui.columns` matches the painted column count in fit-all mode (extends `src/ui.rs:24848`).

Reuse the narrow-and-short painted-output harness at `src/ui.rs:24742` and `:24887`, which
already pins the fork#437 case.

### An existing guard is inverted on purpose

`layout_001_seven_decks_fit_single_column` ends with an "over-correction guard", assertion
(3) at `tests/render_dashboard.rs:3252-3261`: with 20 decks *"scrolling must still engage —
the fix right-sizes the cards, it does not remove scrolling for genuinely too-many decks."*

That is the one place the old intent is asserted, and it states the opposite of this PRD's
first constraint. It is superseded deliberately:

- Replace assertion (3) with its inverse: at 20 decks the column still divides evenly and
  all 20 render (2 rows each at `AVAILABLE = 48`), exercising M2's tiers.
- Generalise assertions (1) and (2) beyond `AVAILABLE = 48`, parameterising over several
  heights, so the property no longer holds at exactly one terminal size.
- Retire the "no more slack to absorb it" warning in its doc comment — card height stops
  being a fixed constant that a new content row can overflow. That warning was a standing
  trap for the next person adding a card row; this change removes the trap, not just the
  warning.

Call the inversion out in the PR body and the changelog entry with the observability
reasoning above. A reviewer meeting a named guard assertion flipped needs the argument, not
a silent diff.

## Out of scope

- The `reviewer` pane reporting a blank status rather than `Idle` in `daemon status` — a
  separate question about Codex panes not emitting agent-events. Unrelated to card count.
- The hydration-drift defect (upstream #554 / fork #314). Different symptom: **wrong role
  names**, not a short count. Settled and parked; do not re-diagnose.
- Any change to `fit_grid`, `grid_columns` or `choose_density` behaviour when the cards fit.
