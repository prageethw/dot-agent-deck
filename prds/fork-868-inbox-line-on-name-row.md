# PRD fork#868: Show the inbox line on the worker-name row so the card title keeps its width

**GitHub Issue**: [fork #868](https://github.com/prageethw/dot-agent-deck/issues/868)

**Priority**: Medium

**Status** *(2026-11-07)*: **In progress.** PR [#869](https://github.com/prageethw/dot-agent-deck/pull/869) on branch `feat/inbox-line-name-row-868`. First push `79e44f57`: `test` 9425/9425 and every e2e shard green; `build-release` red on `work-type-check` R4 (this file was missing). `reviewer` and `auditor` have both returned on `79e44f57`; their findings are being resolved in one fix round.

**Fork-only?** **Yes, as a layout preference.** Upstream still draws the inbox line in the card's top-right chip; the fork already moved the worker's identity onto its own body row (fork#405), so the name row is the natural home for the line here. Carried as a **PERMANENT** row in [`fork-sync-workflow.md`](../docs/develop/fork-sync-workflow.md). Whether to offer it upstream is the maintainer's call after it merges; nothing is filed yet.

**Why a PRD for a card-layout change.** `work-type-check` R4 requires a `prds/` file behind any `.feature.md` fragment, and the fragment suffix is correct: this changes user-visible behaviour, so it is `prd`-type work under [`work-types.md`](../docs/develop/work-types.md), not `bug` or `chore`. The document is short because the change is small.

---

## Problem Statement

A session card's inbox line (`✉2 queued`, `✉ delivered`, `✉ delivery uncertain`, and the other forms `CardInbox::full_text` produces) was drawn in the card's top-right title chip, beside the status: ` ✉ delivered  ● Idle `. The title's width budget was what the chip left over, so every column of the inbox line came out of the title. The title carries the shortcut number and, with the agent badge on (`m`), the `claude (opus-4.7)` badge, so both were cut with `…` as soon as a message had been delivered, even though the worker's name sat on its own row below with plenty of room.

## Scope

The inbox line moves onto the worker-name row, directly after the name, separated by two spaces. The top-right chip carries the status alone, and the title no longer gives up any width to the inbox.

| Row | Before | After |
|---|---|---|
| Top border | `▸ 1 claude (op…  ✉ delivered ● Idle` | `▸ 1 claude (opus-4.7)         ● Idle` |
| Name row | `worker-1` | `worker-1  ✉ delivered` |

- The words are unchanged: `inbox_display::CardInbox::full_text` and `short_text` stay the single source, so the desktop tile and `tests/fixtures/inbox-display.json` are untouched.
- The line degrades as the row narrows: the full text, then the bare `✉N` count, then nothing. Width is measured in display columns, not characters.
- The 3-row Minimal card, whose single role row also carries `Dir:` (issue #847) or a status reason, spends the row in the order name, inbox line, then `Dir:` or the reason.
- No card gains or loses a row. The 1-row tier is unchanged. The 2-row tier did not draw the inbox line before and does not now; its title is simply no longer shortened by a line it never drew.

## Out of scope

- Any change to the inbox wording, its states, or the notice row beneath the card.
- The desktop app.
- The `(delegated)` status suffix, which lengthens the status label itself and is a separate surface.
- The two documentation-site screenshots that still show the old placement: tracked by [#863](https://github.com/prageethw/dot-agent-deck/issues/863).

## Milestones

- **M1: Implementation.** `render_session_card` draws the inbox line on the name row; `max_title` no longer subtracts any inbox width; L1 tests and five insta snapshots pin the new placement; docs reworded. **Done in `79e44f57`.**
- **M2: Sync protection.** PERMANENT row and register row in `fork-sync-workflow.md`. **Done in `79e44f57`.**
- **M3: Review.** `reviewer` and `auditor` on `79e44f57`. **Returned; findings in one fix round.**
- **M4: Merge.** CLAUDE.md rule 25's checklist on the final head SHA, then a squash merge pinned to it.
