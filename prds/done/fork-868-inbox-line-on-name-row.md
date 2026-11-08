# PRD fork#868: Show the inbox line on the worker-name row so the card title keeps its width

**GitHub Issue**: [fork #868](https://github.com/prageethw/dot-agent-deck/issues/868)

**Priority**: Medium

**Status** *(2026-11-08)*: **Complete.** PR [#869](https://github.com/prageethw/dot-agent-deck/pull/869) merged as `e1926774` (squash, 2026-11-08) and closed issue #868. The branch `feat/inbox-line-name-row-868` had six commits and its final head was `92cb3f8a`: CI `test` 9428/9428, `build-macos` 9048/9048, the four e2e shards plus `e2e (version)` and the full e2e job 10183 tests, all passed; `reviewer` approved and `auditor` found it clean in their third round on that head. Two earlier heads were red and are superseded: `79e44f57` (`build-release` failed `work-type-check` R4 for want of this file) and `3f55ca3b` (`orchestration/roles/002` failed because its redraw wait looked for a `…` that a 39-column name never shows at 120 columns; `build-macos` also failed on an unrelated timing flake, `issue_dispatch_run::tests::create_001_concurrent_attach_never_double_creates`, which did not recur). CI on `main` at `e1926774` was green too.

**Fork-only?** **Yes, as a layout preference.** When this was written upstream drew the inbox line in the card's top-right chip, and the fork had already moved the worker's identity onto its own body row (fork#405), so the name row was the natural home for the line here. Carried as a **PERMANENT** row in [`fork-sync-workflow.md`](../../docs/develop/fork-sync-workflow.md). **It was offered upstream as a different implementation, and upstream merged that.** This layout cannot be ported, because upstream's title carries the worker's name and it has no name row, so the offer was a fresh implementation of the same idea for upstream's layout: issue [vfarcic/dot-agent-deck#2143](https://github.com/vfarcic/dot-agent-deck/issues/2143), PR [vfarcic/dot-agent-deck#2145](https://github.com/vfarcic/dot-agent-deck/pull/2145), squash `5464223d`, merged by the maintainer on 2026-11-08 after Qodo, Greptile and upstream's review bot found nothing. Upstream keeps the line in the chip while the whole title fits beside it and otherwise moves it to the right end of the `Dir:` row, so on the next sync it conflicts with this PRD's placement in `render_session_card`. The fork keeps the name row; the PERMANENT row in `fork-sync-workflow.md` says how to resolve it and which catalog ids to renumber.

**Why a PRD for a card-layout change.** `work-type-check` R4 requires a `prds/` file behind any `.feature.md` fragment, and the fragment suffix is correct: this changes user-visible behaviour, so it is `prd`-type work under [`work-types.md`](../../docs/develop/work-types.md), not `bug` or `chore`. The document is short because the change is small.

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
- The 3-row Minimal card, whose single role row also carries `Dir:` (issue #847) or a status reason, spends the row in the order name, inbox line, then `Dir:`. When the card's first status row is a reason (Blocked, orphaned), a delivery failure or an inbox notice, that row outranks the inbox line: the line is drawn only if the row still fits whole after it, and is dropped otherwise, so a reason is never cut short to make room for a delivery receipt. Background work is informational and does not outrank the line: with only a `Background:` row the order stays name, inbox line, then that row in what is left.
- No card gains or loses a row. The 1-row tier is unchanged. The 2-row tier did not draw the inbox line before and does not now; its title is simply no longer shortened by a line it never drew.

## Known limits

Decided with the maintainer on 2026-11-07 and recorded here rather than fixed in this change.

- **The name never shrinks, so a long name hides the line.** The line needs `name + 2 + text` columns inside the card's border, and the card's inner width changes with the terminal (a 120-column terminal gives a 39-column inner width, not 38). With `inner` the card's inner width, `✉2 queued` shows for names up to `inner − 11` columns, `✉ delivered` up to `inner − 13` and the bare count `✉2` up to `inner − 4`; beyond that nothing is drawn, not even the pending count. Shipped role names are 12 columns or shorter, but a long name still arrives by a pane rename, by a role name in the user's own `.dot-agent-deck.toml` (this change's own `orchestration/roles/002` fixture uses one), or by a display name a producer sets. A stalled wake hidden this way has no notice row behind it, so it can vanish while the card's status reads Idle.
- **The long states need a wide card.** `✉ delivery uncertain` (20 columns), `✉ may not have reached the agent` (32) and `✉2 waiting to wake: send it a prompt` (36) show for names up to `inner − 22`, `inner − 34` and `inner − 38` columns. On a 40-column card (38 inside) the stalled wake never fits beside a name and falls back to `✉2`, which reads the same as queued, and the not-started line fits only beside names up to 4 columns. This is not a regression: the old title chip had less room still, and uncertain now shows where it used to vanish.
- **The bare count `✉2` is ambiguous** between queued and stalled; `CardInbox::short_text` is the pending count only.

The fix for the second and third is a one-word set for the card line, tracked as [#870](https://github.com/prageethw/dot-agent-deck/issues/870), which changes the shared wording in both clients and so is its own change after this one merges. This change deliberately does not add a count to the title chip as a fallback: a bare count there is the same ambiguity, and it would split the line across two places again.

## Out of scope

- Any change to the inbox wording, its states, or the notice row beneath the card (the one-word set is [#870](https://github.com/prageethw/dot-agent-deck/issues/870)).
- The desktop app.
- The `(delegated)` status suffix, which lengthens the status label itself and is a separate surface.
- The two documentation-site screenshots that still show the old placement: tracked by [#863](https://github.com/prageethw/dot-agent-deck/issues/863).

## Milestones

- **M1: Implementation.** `render_session_card` draws the inbox line on the name row; `max_title` no longer subtracts any inbox width; L1 tests and five insta snapshots pin the new placement; docs reworded. **Done in `79e44f57`.**
- **M2: Sync protection.** PERMANENT row and register row in `fork-sync-workflow.md`. **Done in `79e44f57`.**
- **M3: Review.** `reviewer` and `auditor`, three rounds, each on the head it names. **Done: approve and clean on `92cb3f8a`.**
- **M4: Merge.** CLAUDE.md rule 25's checklist on the final head SHA, then a squash merge pinned to it. **Done: `e1926774`, with the merge report on the PR.**
- **M5: Offer upstream.** A fresh implementation for upstream's layout, from issue vfarcic/dot-agent-deck#2143. **Done: merged upstream as `5464223d` (PR #2145), 2026-11-08.**

## Follow-ups

- [#870](https://github.com/prageethw/dot-agent-deck/issues/870): one word per state on the card line, so every state fits a 40-column card. Changes the shared wording in both clients.
- [#872](https://github.com/prageethw/dot-agent-deck/issues/872): decide whether the name row needs the protection upstream's #2145 gave its `Dir:` row against a name that imitates the line.
- [#863](https://github.com/prageethw/dot-agent-deck/issues/863): regenerate the two documentation-site screenshots. Its text covers the `ORCHESTRATOR` badge they still show; they also still show the inbox line in the top-right chip, which this change moved.
- [#871](https://github.com/prageethw/dot-agent-deck/issues/871): this status, and the sync rows in `fork-sync-workflow.md`, brought up to date with upstream's #2145.
