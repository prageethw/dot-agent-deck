# PRD fork#405 — Role name on its own row under the LLM name, and a highlighted orchestrator tab

**Issue:** [prageethw/dot-agent-deck#405](https://github.com/prageethw/dot-agent-deck/issues/405)
**Predecessors:** fork #339 (agent-type badge toggle), fork #378 (model in the badge) for M1; issue #306 and fork #377 (active-tab cue) for M2.
**Experimental flag:** No (CLAUDE.md rule 9 — M1 modifies an existing surface rather than adding one, and gating it would make card height flag-dependent; M2 changes an existing cue).

Two independent milestones, one PRD. They share nothing but the deck-chrome readability theme and can land in either order.

---

## M1 — Move the role name onto its own row, beneath the LLM/model name

### Problem

Fork #378 grew the agent-type badge to carry the active model, so a card's border title row now reads:

```
┌ 1 ClaudeCode (Opus) · Orchestrator ───────── ● Working ┐
```

Everything identifying the agent is on one line. The role name — the most useful label on an orchestration card — is also the segment that disappears first: `truncate_styled_segments` (`src/ui.rs:6821`) ellipsizes left-to-right, so a long model id eats the role. `dashboard/agent-badge/005` exists precisely because an unbounded model could push identity off the title budget.

### Outcome

```
┌ 1 ClaudeCode (Opus) ───────────────────────  ● Working ┐
│ Orchestrator                                           │   ← Color::Indexed(130)
│ Dir:  workspace                                        │
│ Prmt: inspect the repository                           │
└────────────────────────── Last: 0s  Tools: 0 ──────────┘
```

The title row becomes purely the type/model line; the first body row becomes purely identity.

### Decisions

| Question | Decision |
|---|---|
| Surface | **Deck cards only** (`render_session_card`). Not the embedded terminal pane. |
| Toggle interaction | **The role row is unconditional.** Only the model line above it responds to `Ctrl+D` → `Ctrl+M`. |
| Colour | **`Color::Indexed(130)`** (`#af5f00`), as `palette::ROLE_NAME`. |
| Badge-off title | The name **moves down**, it is not duplicated. |
| Experimental flag | **No.** |

**Why deck cards only.** An embedded pane's inner area *is* the PTY size, asserted by the PRD #84 invariant-3 contract guard (`src/terminal_widget.rs:194-214`). Taking a body row there would shrink the terminal every agent runs in and trip that guard. The pane keeps its single-line role title.

**Why the role row is unconditional.** Role is identity, not a debugging detail — an orchestrator pane should say `Orchestrator` whether or not you care which model backs it. This choice also has a structural payoff: because the row always exists, `card_height()` stays a function of density alone. Had the row ridden the toggle, height would have become toggle-dependent and `choose_density`, `visible_rows`, the scroll clamp and both L1 seams would all need to handle two heights.

**Why `Color::Indexed(130)`, and why this needs justifying at all.** `src/palette.rs` documents a named-ANSI-or-`Color::Reset` policy, and named ANSI has no orange. The candidates, measured:

| Colour | vs black | vs white |
|---|---|---|
| `Indexed(208)` `#ff8700` | 8.72:1 | 2.41:1 |
| `Indexed(166)` `#d75f00` | 5.53:1 | 3.80:1 |
| **`Indexed(130)` `#af5f00`** | **4.46:1** | **4.71:1** |

130 is the only candidate balanced across both themes. 166 fails WCAG AA (4.5:1) on light terminals, which would add a second instance of the bug class [#312](https://github.com/prageethw/dot-agent-deck/issues/312) is already open about. `Color::Yellow` and `Color::LightRed` are unavailable — they already mean `STATUS_WAITING` and `STATUS_ERROR`, and a card cannot have one hue meaning two things.

This makes `ROLE_NAME` **the palette's first non-named colour**, so `src/palette.rs`'s policy paragraph must be amended in the same commit. A 256-cube index is nominally remappable but effectively fixed on most terminals — one tier weaker than the `Color::Rgb` ban PRD #13 enforces, and accepted deliberately here rather than drifted into. The `Color::Rgb` ban itself is unchanged.

### Design

1. **`src/palette.rs`** — add `pub const ROLE_NAME: Color = Color::Indexed(130);` beside the accent roles, and amend the "named ANSI only" paragraph to record the exception and its reasoning.
2. **`src/ui.rs:104-125`** — `card_height()` becomes `(1 + 1 + prompts + separator + tools) + 2`. Its doc comment already mandates this ("Any future line `render_session_card` gains must be added here in the same change"). Update the doc comment and the variant comments at `:98-100`.
3. **`src/ui.rs:19520-19574`** — remove `label_after_badge` from the badge-on branch and `identity_text` from the badge-off branch. `id_display` stays; it moves to step 4.
4. **`src/ui.rs:19654`** — push the identity row as the first body `Line`, `fg(palette::ROLE_NAME)`, using the same `display_name → id_display` ladder and `truncate_with_ellipsis`. Push **unconditionally**, emitting `Line::from("")` when both are empty, so the emitted line count never diverges from reserved height. Reference `palette::ROLE_NAME`, never an inline `Color::Indexed`, so `theme/guard/003`'s single-source-of-truth clause stays honest.

### Costs, accepted knowingly

1. **Scroll capacity drops ~17%.** Heights go 5/8/10 → 6/9/11, so Compact shows 8 decks instead of 9 at the 48-row reference and 9 instead of 11 at 56.
2. **`orchestration/layout/001` has no margin left.** It passes at 48/6 = 8 ≥ 7, but 48/7 = 6 < 7. **Height 6 is the largest value that satisfies it — the next row anyone adds to a card breaks that test.** Recorded here so the next person to reach for a card row finds out before they spend it.
3. **Density degrades one tier at specific card counts** near the old boundaries — e.g. 2 cards at 20 available rows goes Spacious → Normal. It does not collapse everything to Compact; `choose_density` still returns Compact unconditionally as its floor.
4. **The default title row becomes a bare card number.** The badge is off by default, so most users see `┌ 1 ──── ● Working ┐`. Coherent under "title = type row, body = name row", but it is a real change to the default rendering.
5. ~~**The role row is hidden while idle art is showing**~~ — **this cost was accepted at design time and then withdrawn in review.** As originally written, the idle-art overlay `Clear`ed the whole inner area, so the identity row disappeared behind the art on Spacious cards. The reviewer challenged it by *multiplying* it with cost #4, which the PRD had recorded separately and never combined: with the badge off — the default — an idle Spacious card showing art would have been identified by **nothing but its card number**, where before this PRD it still read `┌ 1 example-coder ─── ● Idle ┐`. That is the opposite of what M1 exists to do, so the fix was taken rather than the cost: `Clear` and the art `Paragraph` now target `inner` offset down one row, leaving the identity row untouched. The art loses one row of nine.

   Kept here rather than deleted, because the *reasoning* is the durable part: two costs can each be individually acceptable and jointly unacceptable, and a costs list that only ever records them separately will never show that. The lesson generalises past this PRD — when adding a cost to such a list, check it against the ones already there.

---

## M2 — Highlight the selected orchestrator tab in its own text colour

### Problem

The selected tab is cued by `Modifier::BOLD` alone (`src/ui.rs:14620-14640`). On a tab bar where several tabs already carry a status tint, bold-only is a weak signal for which one is actually selected.

### Outcome

The selected **Orchestration** tab renders `REVERSED`, so its own text colour becomes its background. Dashboard and Mode tabs keep the plain BOLD cue.

```
unselected orchestration tab → normal appearance
selected   orchestration tab → highlighted in its text colour
selected   Mode / Dashboard  → BOLD, unchanged
```

### Decisions

| Question | Decision |
|---|---|
| Which tab | **`Tab::Orchestration`** in the top tab bar. The three kinds are Dashboard / Mode / Orchestration (`src/tab.rs:101`); there is no "worker" kind. |
| Mechanism | **`Modifier::REVERSED`**, applied only to the active orchestration tab. |
| Other tabs | Untouched. |

**What "its text colour" resolves to.** A tab's label colour is its aggregate status colour — `palette::status_color(&palette::highest_priority_status(statuses))` at `src/ui.rs:14666-14672` — not a fixed orchestrator hue. So the highlight tracks status: green while working, red on error, DarkGray at rest.

**This is a deliberate, scoped partial reversal of [#306](https://github.com/vfarcic/dot-agent-deck/issues/306).** That issue removed `REVERSED` from the tab bar for exactly the effect being reinstated here — the in-source comment at `src/ui.rs:14644-14649` records that REVERSED "would invert an absolute fg into the label's background". Scoping it to the orchestration tab is the intent. **A future upstream sync must not silently restore the blanket removal**, in the same way the fork-only Idle-tint decision at `src/ui.rs:14653-14665` is protected.

`REVERSED` is the sanctioned terminal-relative highlight: it paints no absolute background, so it passes `theme/guard/002`, which bans `bg(Color::Rgb` and three named palette bg fields. It is already used this way at `src/ui.rs:16077`, `:17128`, `:17145` and `:19178`.

### Design

1. **`src/ui.rs:15334-15357`** — derive a per-tab "is orchestration" flag at the call site, the same way `closeable` is already derived from the `Tab` enum, and pass it to `render_tab_strip`. No new mechanism.
2. **`src/ui.rs:14636-14672`** — add `REVERSED` when `i == active_index && is_orchestration[i]`. **Order matters:** it must be applied *after* the status fg overwrite, since it inverts whatever fg is present.

**This requires a signature change, and that has a sequencing consequence.** `render_tab_strip` and its L1 seam `render_tab_bar_to_buffer` (`src/ui.rs:21091`) take `labels`, `closeable`, `active_index`, `width`, `tab_statuses` — and **nothing in that set distinguishes a Mode tab from an Orchestration tab**: both are `closeable = true` and both carry `Some(..)` status data. So M2 needs a new `is_orchestration: &[bool]` parameter, appended after `tab_statuses` (the same append-at-the-end convention `tab_statuses` itself followed).

A missing *parameter* has no literal-substitution workaround the way a missing *constant* does, so M2's tests cannot compile before it exists — and because the `build` job runs `clippy` before `nextest`, that compile failure suppresses the **entire fast tier**, M1's tests included. The RED round therefore landed in three steps rather than two: tests → an interface-only stub adding the parameter with no behaviour → the real implementation. Recorded because the shape recurs: whenever a new test needs a new production *signature*, the RED round needs an interface stub first, or it produces no readable result at all.

### Cost, accepted

An idle orchestration tab highlights as DarkGray-on-terminal-background — low contrast on a dark theme. This is the same class as, and downstream of, the fork-only maintainer decision recorded at `src/ui.rs:14653-14665` that made an idle tab paint `STATUS_IDLE` at all.

---

## Test plan

L1 only. Both milestones are pure widget/layout changes, so CLAUDE.md rule 4 requires no L2 PTY test. Every `#[spec]` needs a `/// Scenario:` comment (rule 7), and `tests/CATALOG.md` needs matching entries.

### New

| Catalog ID | Covers |
|---|---|
| `dashboard/pane/011` | Badge ON: title carries `ClaudeCode (Opus)` and **not** the name; the first body row carries the name in `palette::ROLE_NAME`. Badge OFF: title carries neither and the body row still carries the name — the unconditional half, and the half most likely to regress. Plus fg distinctness from every status role and every registry `badge_color`, and the `id_display` fallback. Colour-aware `insta` snapshot for the on/off pair. |
| `tabs/orchestration/016` | Unselected orchestration tab is normal. Selected orchestration tab is REVERSED with its status fg intact. A selected Mode tab and a selected Dashboard tab in the same buffer are BOLD and **not** REVERSED. |

`/016`, not `/015`: this PRD originally named `/015`, which is **already taken** by a pre-existing L2 test in `tests/e2e_orchestration_pane_column.rs` (`orchestration_015_active_tab_bold_status_color_no_underline`, issue #313's real-terminal BOLD/no-UNDERLINED guard). Note that `/015` is itself an assertion about the active tab's modifiers, so the two are neighbours in subject as well as in ID — `/016` cross-references it.

### Existing tests that break and must be repaired

| Where | What |
|---|---|
| `src/ui.rs:28651` `card_height_001` | 5/8/10 → 6/9/11 |
| `src/ui.rs:28604` `test_choose_density` | `(2,2,10)` Spacious→Normal; `(4,2,16)` Normal→Compact; the comment banner at `:28605` |
| `src/ui.rs:28630` `test_choose_density_boundaries` | `(1,1,10)` Spacious→Normal; `(2,1,17)` Normal→Compact; the banner at `:28632` |
| `tests/render_dashboard.rs` `agent_badge_001`, `/004` | Split each `"<Label> · <name>"` needle into two **row-scoped** assertions — badge in the title, name on the body row — so the test proves the placement this PRD is about rather than merely tolerating it |
| `tests/render_dashboard.rs` `agent_badge_005` | Its premise — identity must not be pushed off the title budget — is now **impossible by construction**. Rewrite its Scenario and CATALOG entry to say identity is structurally immune, rather than tweaking the needle; otherwise it becomes a test of nothing |
| `tests/render_dashboard.rs` `layout_001` | Doc comment "48 / 5 = 9" → "48 / 6 = 8", plus a line recording that the margin is now one row |
| `tests/e2e_orchestration_pane_column.rs:343` | `has_role_status` requires the role name and status on the **same** line, and its 17-line doc comment says so. Rewrite for name-on-row-N+1 within the same card box, and rewrite the comment with it |
| `tests/e2e_dispatcher_mode.rs:934` | `card_label` builds `"ClaudeCode · {role}"`. Rewrite for the first body row. Note it may already be latently wrong, since the badge is off by default |
| `tests/render_tab_strip.rs` `tabs/orchestration/010`, `/012`, `/014`, `tabs/label/002` | Each asserts REVERSED is **absent** on the active tab. Re-scope so the absence assertion holds for non-orchestration tabs and the new behaviour is asserted for orchestration ones |

### Snapshots

Nine card-render snapshots move, all because of the extra row: `render_dashboard__pane_004`, `__pane_005`, `__pane_006`, `__pane_008_codex_card_omits_agent_type_badge`, `__pane_008_named_agent_badges`, `__agent_badge_001_codex_on`, `__card_stats_001`, `__card_stats_002`, `mode_indication__mode_deck_001_selected_card_styles`. Review each diff individually rather than blanket-accepting — the last two are where the title visibly loses its identity.

There are **no** tab-bar snapshots; `tests/render_tab_strip.rs` asserts cells, fg and modifiers directly.

### Watch in CI

`tests/e2e_card_layout.rs:22` runs at `RECORDING_ROWS = 16`, where a 2-card grid at the new Compact height fits in exactly 12 rows with zero margin. If the hints bar wraps to two rows, one card scrolls off. Raise `RECORDING_ROWS` if it flakes.

## Out of scope

Role semantics and naming, the `Ctrl+D` → `Ctrl+M` toggle itself, the embedded terminal pane, pane styling, shortcuts, and status logic. Render path only — no daemon, protocol, orchestration or hook change, so CLAUDE.md rule 12 does not apply and no `PROTOCOL_VERSION` bump or `.breaking.md` fragment is expected.
