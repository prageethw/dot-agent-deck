# PRD #387: A unified, deck-global `Ctrl+L` split toggle — and a chord that stops being swallowed

**Status**: **CLOSED — fork-complete.** M1–M6 shipped and released. Merged 2026-08-06 as [prageethw/dot-agent-deck#19](https://github.com/prageethw/dot-agent-deck/pull/19) (merge commit `43a22bc`), reviewer and auditor clean, Greptile 5/5 with no findings, and **released in [v0.35.8](https://github.com/prageethw/dot-agent-deck/releases/tag/v0.35.8)** (published 2026-08-06, binaries for darwin/linux × amd64/arm64) alongside PRD #386.

**M7 is deliberately NOT tracked here any more.** It is gated on upstream PR [#342](https://github.com/vfarcic/dot-agent-deck/pull/342) — still OPEN, untouched since 2026-08-06 01:56, no review decision — and per decision 4 it must land as a *separate* upstream PR that deletes `split_narrow` in the same change. That is an upstream contribution on someone else's merge queue, not fork work, so keeping this PRD open on it would misrepresent finished work as unfinished. **Upstream issue [#387](https://github.com/vfarcic/dot-agent-deck/issues/387) was reopened and now owns that tracking.** Note that **upstream still swallows `Ctrl+L` on orchestration tabs** — only the fork is fixed.
**Priority**: Medium-High (the swallowed-chord half is a daily annoyance in the deck's primary use case; the unification half is a behaviour improvement)
**Created**: 2026-08-06
**GitHub Issue**: [#387](https://github.com/vfarcic/dot-agent-deck/issues/387) (filed upstream — the swallowing bug and the fix both live in upstream's own code)
**Related**: [#336](https://github.com/vfarcic/dot-agent-deck/issues/336) / upstream PR [#342](https://github.com/vfarcic/dot-agent-deck/pull/342) — the two-stage `split_narrow` toggle and the `scope_orchestration_split` fix this PRD generalises; **do not re-scope #342**. [#371](https://github.com/vfarcic/dot-agent-deck/issues/371) (`prds/371-three-stage-split-toggle.md`) — the three-stage `SplitStage` cycle this PRD makes deck-global; **supersedes its per-tab granularity, not its stages**. [#241](https://github.com/vfarcic/dot-agent-deck/issues/241) M1 — `close_pane`'s command-mode scoping, the precedent decision 1 follows. Code: `src/ui.rs` (`global_action`, `global_action_for_mode`, the inline `claims_ctrl_l` match, `split_stage_percents`, `ACTIVE_ORCHESTRATION_SPLIT_STAGE`/`ACTIVE_DASHBOARD_SPLIT_STAGE`, `UiState::pane_layout` as the shape precedent), `src/tab.rs` (`SplitStage`, `next_split_stage`, `Tab::Dashboard::split_stage`, `Tab::Orchestration::split_stage`), `src/keybindings.rs` (`Action::ToggleOrchestrationSplit`, `ACTIONS`).

## Problem Statement

Two problems, one chord. The first is a live bug that a user hits every day; the second is that the same feature now exists in two incompatible shapes across a fork boundary. They are stated separately because the bug is worth fixing even if the proposal is rejected.

### Defect 1 — `Ctrl+L` is claimed mode-independently on orchestration tabs, so a role pane's agent never receives it

`Ctrl+L` is a `Section::Global` binding resolved by the global keybinding resolver, and on an **orchestration tab** it is claimed *regardless of UI mode*. A focused role pane's PTY therefore never receives `0x0c`, and **Claude Code's own `Ctrl+L` (clear screen) never reaches the agent**. Running an interactive agent in a role pane is the deck's primary use case, so this is the worst possible place for the chord to be eaten.

Verified on the fork's `main` (`ad2599c`):

```
src/keybindings.rs   ToggleOrchestrationSplit → section: Section::Global, default "Ctrl+l"
src/ui.rs:6545       matched in the global resolver → Some(Action::CycleSplitStage)
src/ui.rs            scope_orchestration_split → NOT PRESENT
```

The claim decision is an inline `match` at `src/ui.rs:9077-9086`:

```rust
if matches!(action, Some(Action::CycleSplitStage)) {
    let claims_ctrl_l = match tab_manager.active_tab() {
        Tab::Orchestration { .. } => true,                   // mode-independent — the defect
        Tab::Dashboard { .. } => ui.mode == UiMode::Normal,   // already command-mode scoped
        Tab::Mode { .. } => false,
    };
    if !claims_ctrl_l {
        action = None;
    }
}
```

**State this precisely, because it is easy to get backwards: Dashboard tabs are already guarded.** PRD #371 scoped them to `UiMode::Normal`, and `tests/e2e_orchestration_pane_column.rs::orchestration_007_ctrl_l_forwards_to_pty_on_non_orchestration_tab` pins that with a real interactive bash pane, asserting readline's clear-screen actually runs and a sentinel line disappears. The unguarded case is **orchestration tabs**, and it has no equivalent test — which is exactly why it survived. Mode tabs claim nothing and are already correct.

The fix for this half already exists and is simply not on our `main`: `scope_orchestration_split(action, is_orchestration_tab, mode)`, written on upstream PR #342, un-resolves the chord outside command mode. Its own doc comment names the motivation — a role pane is "the most likely place to want a screen clear" — and it deliberately mirrors `close_pane`'s scoping from PRD #241 M1, where `Ctrl+W` had the identical conflict class (a global chord eating readline's word-delete). It never reached us because #342 is unmerged and #371 was developed independently of it.

### Defect 2 — the same feature exists in two incompatible shapes, and the fork-sync record understates it

- **#336 / upstream PR #342** — a two-stage `split_narrow: bool`, orchestration tabs only, plus `scope_orchestration_split`. Open upstream, CI green, made *global* this session at vfarcic's request. **`split_narrow` does not exist on the fork's `main` at all** (verified: `git show main:src/tab.rs | grep -c split_narrow` → `0`, and zero hits anywhere under `src/`).
- **#371** — a three-stage `SplitStage` enum on `Tab::Dashboard` *and* `Tab::Orchestration`, per-tab. Shipped on the fork via fork PR #10 (`30c5f79`). Verified: 14 `SplitStage` hits in `src/tab.rs`, 44 in `src/ui.rs`. Issue #371 was closed upstream as *not-planned*; the fork continued it.

So **#371 superseded #336's mechanism on the fork**, while #342 carries #336's mechanism upstream. The two are not parallel features that happen to overlap — they are two generations of one feature that diverged across the fork boundary.

`docs/develop/fork-sync-workflow.md`'s stack table lists **both** `ab71a28` (#336) and `30c5f79` (#371) as **PERMANENT** fork-only, which understates that relationship: #336's contribution is *superseded* on the fork, not parallel to it, and carrying both forward as independently-permanent invites a future sync to reconcile a field that no longer has a reason to exist. **Flagged here, deliberately not corrected in this PRD's authoring task** — see Open Questions.

## The four decisions — settled, with reasoning

These were decided by the user before this PRD was written. They are recorded as **settled**, not as open questions, and the reasoning is preserved so a future reader does not relitigate them.

**1 — Command-mode only, everywhere.** The deck claims `Ctrl+L` only in command mode, on every tab type; in pane input it passes through to the agent untouched. Generalise `scope_orchestration_split` rather than inventing a parallel mechanism. Consistent with `close_pane` (PRD #241 M1): a globally-bound chord that a pane's occupant also wants is scoped to command mode, and the user pays one extra keystroke (`Ctrl+D` first) instead of losing the chord entirely. Note that once the rule is uniform the tab-type dimension largely collapses — the predicate becomes "command mode, on a tab that has a sidebar split" — so this **simplifies** the current inline `match` rather than adding another special case.

**2 — One split stage across the entire deck.** Dashboard and every orchestration tab share a single value. Toggling anywhere changes everywhere, and a newly opened tab of any kind adopts the current stage. Reasoning on record, from vfarcic and accepted by the user: sidebar width reflects **how someone reads, not which tab they happened to open**, and per-tab state means anyone who prefers a narrow sidebar re-toggles forever. There is an exact shape precedent in the codebase — `pane_layout: PaneLayout` is already a deck-global field on `UiState` (`src/ui.rs:1566`) driven by a global chord (`Ctrl+T`), for the same reason.

**3 — Three stages.** Default (34/66) → Narrow (25/75) → Hidden (sidebar collapsed) → Default. This matches what the fork already ships via #371; upstreaming two stages and adding the third later would be two review cycles for one feature. Important nuance that makes decisions 2 and 3 compose cleanly: **the shared value is the *stage*, not the percentages.** `Default` resolves to each tab type's own ratio (Dashboard 33/67, Orchestration 34/66), which is already precisely how `split_stage_percents(stage, default_left, default_panes)` works today — so "one value across the deck" does not force one ratio across the deck.

**4 — Upstream path: land #342 first, then a follow-up PR.** PR #342 merges as the orchestration-only global toggle vfarcic has already reviewed twice and pushed to himself. This unified feature goes upstream as a **second** PR. **Do not re-scope #342** — reopening a twice-reviewed PR to widen it costs the reviewer their prior review and delays the `Ctrl+L` fix that #342 already carries.

## Solution Overview

Replace the two per-tab `split_stage` fields with **one deck-global `split_stage` on `UiState`**, and replace the inline `claims_ctrl_l` match with **one pure scoping function** that claims the chord only in command mode, only on tab types that have a sidebar split.

Everything else is kept as-is. The `SplitStage` enum, `next_split_stage`, `split_stage_percents`, the `Default`/`Narrow`/`Hidden` ratios, the `Action` and its `Ctrl+L` default binding, and the `split_cards_area` call sites all stay exactly as #371 built them. This PRD moves *where the value lives* and *when the chord is claimed*; it does not redesign the layout maths.

The scoping function generalises upstream's `scope_orchestration_split` with the one extra parameter needed to cover tab types beyond orchestration:

```rust
/// Generalises #342's `scope_orchestration_split` to every tab type.
/// `has_split_sidebar` is true for Dashboard and Orchestration tabs, false for
/// Mode tabs (whose 50/50 layout has no sidebar/pane-column split at all).
fn scope_split_stage(
    action: Option<Action>,
    has_split_sidebar: bool,
    mode: UiMode,
) -> Option<Action> {
    match action {
        Some(Action::CycleSplitStage) if !has_split_sidebar || mode != UiMode::Normal => None,
        other => other,
    }
}
```

Kept as a standalone pure function for the same reason #342 did: it is unit-testable without a PTY, whereas an inline `if` at the call site is only reachable through the full event loop.

## Migration

This is the fiddly part, and it has three independent axes: in-memory state, the fork's own tests, and fork-sync reconciliation. It also has one genuinely simplifying fact, which is worth stating first.

**There is no on-disk migration.** `split_stage` is **not persisted** — verified: zero `split_stage`/`SplitStage` hits in `src/config.rs` and `src/state.rs`. PRD #371 left persistence out of scope (its Open Question 1: `Tab` has no `Serialize`/`Deserialize` derive, and snapshots are built into separate `Saved*` structs). So no saved session carries a stage, no `Saved*` struct changes, and no user's on-disk state needs migrating or defaulting. Every deck still starts at `Default` on launch, exactly as today.

### In-memory state: how the deck-global value replaces the per-tab field

The per-tab field is **removed, not shadowed.** Keeping both a global default and a per-tab override would reintroduce exactly the "which one wins" ambiguity decision 2 exists to delete, and there is no requirement anywhere for a tab to diverge from the deck.

- Delete `split_stage: SplitStage` from `Tab::Dashboard` (`src/tab.rs:111`) and `Tab::Orchestration` (`src/tab.rs:152`), plus its ~8 `SplitStage::Default` initialisers in `src/tab.rs` and its initialisers in `src/ui.rs`'s test fixtures.
- Add `split_stage: SplitStage` to `UiState` (`src/ui.rs:1483`), directly alongside `pane_layout` (`:1566`) and defaulting to `SplitStage::Default` — same field, same lifetime, same "deck-global UI preference" semantics.
- `Action::CycleSplitStage`'s handler (`src/ui.rs:7117-7132`) collapses from a two-arm `match` on the active tab to a single `ui.split_stage = next_split_stage(ui.split_stage)`.
- The two thread-locals, `ACTIVE_ORCHESTRATION_SPLIT_STAGE` and `ACTIVE_DASHBOARD_SPLIT_STAGE` (`src/ui.rs:1999-2017`), collapse into **one** `ACTIVE_SPLIT_STAGE`, refreshed each frame from `ui.split_stage` rather than from the active tab (`src/ui.rs:10542-10573`). They exist only because `compute_frame_layout`'s signature is a fixed, widely-tested seam with no spare parameter slot; that constraint is unchanged, so the mechanism stays and only its arity and its source drop. Passing the stage as an explicit parameter instead is a reasonable alternative and is left to the implementer — see Open Questions.

**A whole bug class dissolves here, and that is the strongest argument for the shape.** `layout_003_new_orchestration_tab_spawns_at_default_split_even_when_another_tab_is_narrow` (`src/ui.rs`, spec `orchestration/layout/003`) guards a real spawn-order regression: a brand-new tab's role panes were spawned with `AgentSpawnOptions::cols` derived from the *previous* tab's narrow stage, because `orchestration_role_pane_dims` read a thread-local the render loop had not yet resynced for the new tab. With a single deck-global value, the thread-local **cannot be stale relative to a new tab** — there is one value, it is already correct, and a newly opened tab is supposed to adopt it. The staleness window that test defends closes by construction.

### The fork's own tests: three invert, one is the point of the PRD

Decision 2 reverses the isolation invariant #371 deliberately built, so these tests must be **rewritten to assert the new behaviour, not deleted** — the couplings they guard are still real.

- `layout_003_…_spawns_at_default_split_even_when_another_tab_is_narrow` (L1, `src/ui.rs`) — currently asserts a new tab B defaults to `Default` while A is narrow, and that B's spawned `cols` match A's *default-split* cols. Both assertions invert: B now adopts the deck stage, and B's `cols` must match the **narrow-derived** width. The spawn-order/`cols` coupling is the valuable part and must survive the rewrite; only the expected values change.
- `dashboard_001_ctrl_l_cycles_dashboard_split_stage_isolated_from_orchestration` (L2, `tests/e2e_dashboard_pane_column.rs:49`) — its cross-tab-type isolation assertion (`:112`) inverts to a **shared-stage** assertion. Its name changes with it.
- `orchestration_006_ctrl_l_cycles_pane_column_split_stages` (L2, `tests/e2e_orchestration_pane_column.rs:68`) — its cross-tab isolation step (`:141`) inverts likewise.
- `orchestration_007_ctrl_l_forwards_to_pty_on_non_orchestration_tab` (L2) — **unchanged and must stay green.** It is the existing proof for the Dashboard half of Defect 1, and it is the template for the new orchestration-tab test this PRD's test plan requires.

### Fork-sync reconciliation when #342 lands upstream

The sequencing matters, because #342 introduces upstream a field the fork has already superseded.

**Today:** upstream has neither `SplitStage` nor `split_narrow` on `main` (`split_narrow` lives only on the unmerged #342 branch). The fork has `SplitStage`, per-tab, and no `split_narrow`.

**When #342 merges upstream:** `upstream/main` gains `split_narrow: bool` on `Tab::Orchestration` plus `scope_orchestration_split`. The next fork sync will surface both against the fork's `SplitStage`. The reconciliation is **not** a three-way merge of two mechanisms — it is a supersession, and it should be resolved deliberately:

- **Take the fork's `SplitStage`**; drop upstream's `split_narrow` field and its call sites entirely. A `bool` and a three-variant enum are the same feature at two granularities, and the enum is strictly the later generation.
- **Take upstream's `scope_orchestration_split` as the seed** for `scope_split_stage`, rather than reinventing it — it is the reviewed artefact, and decision 1 explicitly says generalise rather than parallel-invent.
- **Correct the fork-sync stack table** at the same time: `ab71a28` (#336) stops being independently PERMANENT once its mechanism is gone, and the table should record that `30c5f79` (#371) supersedes it. This PRD flags that; it does not perform it.

**The upstream follow-up PR (decision 4) is authored against post-#342 upstream and deletes `split_narrow` in the same PR.** Leaving both a `split_narrow` bool and a deck-global `SplitStage` alive upstream, even briefly, would recreate this PRD's Defect 2 on the other side of the boundary. The PR's story is therefore "replace the field #342 just added with its three-stage, deck-global successor", and it should say so plainly rather than presenting itself as an unrelated addition.

**Asymmetry worth naming:** on the fork this is a *change to existing behaviour* users will notice (per-tab → shared). Upstream it is a *replacement of a field that just landed* and that no user has built a habit around yet. Same diff, different risk profile, different PR description.

## Scope

### In Scope

- One deck-global `split_stage: SplitStage` field on `UiState`; the per-tab fields on `Tab::Dashboard` and `Tab::Orchestration` removed.
- `scope_split_stage`, a pure generalisation of #342's `scope_orchestration_split`, replacing the inline `claims_ctrl_l` match — claiming `Ctrl+L` only in `UiMode::Normal`, only on tab types with a sidebar split.
- Collapsing `ACTIVE_ORCHESTRATION_SPLIT_STAGE` + `ACTIVE_DASHBOARD_SPLIT_STAGE` into one thread-local sourced from `UiState`.
- Rewriting the three tests whose isolation assertions invert, preserving the couplings they guard.
- A new L2 test proving a focused **orchestration role pane** receives `Ctrl+L` — the reported bug, currently untested.
- `docs/keyboard-shortcuts.md` updated for the deck-global, command-mode-only scope; changelog fragment.
- The CLAUDE.md rule 12 cross-version manual check (see below).

### Out of Scope

- **#372, #373, #374** — sibling splits from #361, unrelated mechanisms; nothing here touches them.
- **The `pane_hook_session_id` work in #386** — a different subsystem entirely. This PRD is queued behind #386 for release sequencing, not because they interact.
- **Re-scoping PR #342.** Decision 4. It merges as-is.
- **Persisting the stage across restarts.** Still out of scope, as in #371. Arguably *more* attractive once the value is deck-global and singular — but it is a separate change with its own snapshot work, and folding it in would widen the diff and the review.
- **Changing the Default ratios themselves** (33/67, 34/66) or the Narrow/Hidden constants.
- **Mode tabs gaining a split.** They have no sidebar/pane-column split; they simply never claim the chord.
- ~~**Correcting the fork-sync stack table.** Flagged in Defect 2; performed separately.~~ **Superseded during implementation.** Open Question 1 was resolved in favour of "now", and the correction landed as a standalone docs commit (`d1391d8`, `docs/develop/fork-sync-workflow.md`) *before* any implementation commit on this branch — so it is deliberately in the branch's diff rather than deferred. Kept as a separate commit precisely so it stays reviewable and revertible independently of the feature.

## Milestones

Each is independently testable; the test that proves each one is named in the Test Plan.

- [x] **M1 — `scope_split_stage`, the chord fix.** The pure scoping function replaces the inline `claims_ctrl_l` match. Orchestration tabs stop claiming `Ctrl+L` outside command mode. **Deliberately first and standalone**: it is the user-visible bug fix, it is independent of the state move, and it is the half that stands alone if the rest is ever rejected.
- [x] **M2 — Deck-global state.** `split_stage` moves to `UiState`; the per-tab fields are deleted; the `CycleSplitStage` handler collapses to one assignment.
- [x] **M3 — One thread-local.** The two mirrors collapse into one sourced from `UiState`; both `split_cards_area` call sites read it. The layout maths is unchanged.
- [x] **M4 — Invert the three isolation tests.** `layout_003`, `dashboard_001`, `orchestration_006` rewritten to assert shared-stage behaviour, preserving the spawn-order/`cols` coupling in `layout_003`.
- [x] **M5 — Real-pane proof.** The new L2 test that a focused orchestration role pane genuinely receives `Ctrl+L`, plus the deck-global cycle driven through a real PTY.
- [x] **M6 — Docs, changelog, cross-version check.** `docs/keyboard-shortcuts.md`; changelog fragment; CLAUDE.md rule 12 manual cross-version test. The cross-version check was run and **partially** confirmed — see the Work Log entry below for the delegate leg that stayed unverified and why.
- [ ] **M7 — Upstream follow-up PR.** Authored against post-#342 upstream, deleting `split_narrow` in the same PR (decision 4). **Gated on #342 actually merging — not startable from this branch, and no longer tracked by this PRD.** Ownership moved to upstream issue [#387](https://github.com/vfarcic/dot-agent-deck/issues/387) when this PRD was closed fork-complete on 2026-08-06.

## Test Plan

**#386 states the standard this plan is held to, and it applies verbatim here: *a green suite around a mechanism nothing feeds is indistinguishable from a working feature.*** That is not a borrowed slogan in this case — it is the precise reason Defect 1 shipped. #371 added a mode guard for Dashboard tabs and a passing test for it (`orchestration_007`), and left orchestration tabs unguarded with **no test at all**. The suite was green, the feature was half-broken, and the gap was invisible because nothing exercised the one path that mattered most. Every entry below is therefore stated as *what it would prove in a real pane*.

**M1 — `scope_split_stage` (fast tier, L1, pure).** Table-driven over the cross product of `has_split_sidebar` × every `UiMode` × `{CycleSplitStage, some other action}`. Assert `CycleSplitStage` survives **only** at `(true, UiMode::Normal)`, and that every other action passes through untouched in every cell. *Proves in a real pane*: nothing on its own — this is a mechanism test, and #371's failure was precisely a correct mechanism attached to an untested path. It is here so a later failure localises.

**M1b — the reported bug, in a real pane (e2e tier, PTY-attached, user-visible). This is the test that must exist.** Mirror `orchestration_007` exactly, but on an **orchestration tab**: bring up a real interactive bash/readline role pane, echo a uniquely-named sentinel, send `0x0c`, and assert the sentinel disappears within a few seconds because readline's `clear-screen` actually ran. **RED on `main` today** — the chord is swallowed by the global resolver and the sentinel survives. *Proves in a real pane*: **the reported bug, as the user sees it.** CLAUDE.md rule 4 requires validating a feature as a user actually uses it, and for this PRD the user-visible reality is "my agent receives the keystroke I typed". A test that asserts `scope_split_stage` returns `None` proves the function; only this one proves the byte reached the PTY.

**M2/M3 — deck-global state (fast tier, L1).** Dispatch `CycleSplitStage` once and assert **both** an open Dashboard tab and an open Orchestration tab resolve their percentages from the new stage — Dashboard through 33/67 and Orchestration through 34/66 at `Default`, and both through the shared 25/75 and 0/100 at `Narrow`/`Hidden`. That last part is the assertion that pins decision 3's nuance: one shared *stage*, per-tab-type *ratios*. Plus an `insta` snapshot per stage for each tab type. *Proves in a real pane*: that toggling on one tab visibly moves the other, and that "shared" did not accidentally flatten the two tab types onto one ratio.

**M4a — new-tab adoption and spawn `cols` (fast tier, L1).** The rewritten `layout_003`: cycle to `Narrow`, spawn a brand-new orchestration tab, assert it renders at `Narrow` **and** that its role panes' recorded `AgentSpawnOptions::cols` match the narrow-derived width. *Proves in a real pane*: that a newly opened tab adopts the current stage and that its agents are spawned with a terminal width matching what is actually on screen — a mismatch here means an agent wrapping its output to the wrong column, which is user-visible and ugly. This is the one place the state move could plausibly regress something real, which is why the coupling is preserved rather than the test replaced.

**M4b — cross-tab sharing through a real PTY (e2e tier).** The rewritten `dashboard_001` / `orchestration_006`: open a Dashboard tab and a real Orchestration tab, cycle on one, switch tabs, assert the other is at the same stage. *Proves in a real pane*: decision 2's actual user-visible promise — "toggle anywhere, changes everywhere" — against the real render loop rather than against a state field.

**M5 — the full cycle, deck-global, in a real pane (e2e tier, PTY-attached).** Drive `Ctrl+L` through Default → Narrow → Hidden → Default in command mode on a real orchestration tab, asserting the rendered sidebar geometry at each stage, including that `Hidden` genuinely renders no sidebar. *Proves in a real pane*: the feature as shipped, end to end.

**Regression guard that must stay green.** `orchestration_007` is unchanged by this PRD and must keep passing — it is the Dashboard half of Defect 1 and the reason the fix must not be implemented by *widening* the claim rather than narrowing it.

**Deliberately not claimed.** No test here proves anything about agents other than through readline's `clear-screen`. `Ctrl+L` reaching the PTY is the contract; what a given agent does with it is that agent's business, and asserting on Claude Code's specific clear-screen rendering would couple the suite to another product's output for no added confidence.

## Risks

- **Changing a keybinding's mode-scoping is muscle-memory-visible.** Anyone who today toggles the split from an orchestration tab *while a pane is focused* must now press `Ctrl+D` first. That is a real, if small, regression in keystroke count for an existing habit — accepted deliberately, because the alternative is permanently eating a chord the agent needs, and because it is exactly the trade PRD #241 M1 already made for `Ctrl+W`. The changelog fragment must call it out; a silent scoping change is the kind of thing that reads as a bug to the person who had the habit.
- **"One value across every tab" is a behaviour change for anyone relying on per-tab stages today.** #371 shipped per-tab isolation as an explicit, tested feature on this fork. Anyone who kept one tab wide and another narrow loses that. Accepted per decision 2; it is the point of the PRD, not a side effect.
- **Dashboard and Orchestration sidebars show different content, so one width may suit them unequally.** The Dashboard sidebar lists deck cards; an orchestration sidebar lists roles. A width that reads well for one may crowd or waste space on the other. **This tension was considered and the user chose one shared value anyway — record it as an accepted trade-off, not an open question.** It is materially softened by decision 3's nuance: `Default` still resolves per tab type, so the two only converge at `Narrow` and `Hidden`, which are deliberate "I want more pane" states where the sidebar is secondary by definition.
- **Fork-sync collision with #342.** The reconciliation is a supersession, not a merge, and a mechanical three-way resolution would happily leave both `split_narrow` and `SplitStage` alive. Mitigated by writing it down in Migration above and by M7 deleting the field in the same upstream PR — but it needs a human to actually read the conflict.
- **Deleting the per-tab field touches many initialisers.** Roughly eight `split_stage: SplitStage::Default` sites in `src/tab.rs` plus test fixtures in `src/ui.rs`. Mechanical and compiler-guided — the risk is not correctness but a large, noisy diff that hides a real change inside it. Keeping M1 (the bug fix) as a separate, earlier commit from M2 (the state move) is the mitigation, and is why the milestones are ordered that way.
- **Snapshot churn.** Any `insta` snapshot pinning `ActiveTabView` variant *shape* will move when the field leaves the `Tab` variants, exactly as #371 noted when it added them. Content is unchanged at `Default`.
- **`Ctrl+L` chord availability upstream.** #371's own risk list flagged re-checking that `Ctrl+L` is still free in upstream's `ACTIONS` table before landing. Re-check again at M7 time; upstream may have added a default binding since.

## CLAUDE.md rule 12 — cross-version contract

**Does this change the TUI↔daemon contract? No.** This PRD touches key resolution, `UiState`, `Tab`, and layout — all client-side. It adds no wire field, changes no `EventType`, alters no handler, and touches neither `src/daemon.rs` nor `src/hook.rs`. `Tab` is not serialized to the daemon, and `split_stage` is not persisted at all. **No `PROTOCOL_VERSION` bump is owed.**

**Is a `.breaking.md` fragment owed? No.** Rule 12 defines "breaking" narrowly as a TUI↔daemon interoperability break, *including* a semantic break behind a stable wire. Neither applies: there is no wire involved. The user-visible behaviour changes (mode scoping, shared stage), and both belong in the ordinary changelog fragment — but a user-visible behaviour change is explicitly **not** what `.breaking.md` is for, and misfiling it there would wrongly signal a compatibility break to anyone reading the release notes.

**Is the cross-version manual test required?** Rule 12 triggers it when a PRD touches the daemon, the protocol, orchestration, or hooks. **This PRD touches none of the four**, so the check is not strictly owed. Run the abbreviated version anyway at M6 — branch TUI against the previous release's daemon, confirm a delegate still routes and hooks still arrive — because the change sits in the key-dispatch path that *precedes* every one of those flows, and the cost is minutes. If it is skipped, say so explicitly in the PR rather than leaving it ambiguous.

**Bump policy** (while `0.x`): a bugfix plus a client-side behaviour change, no contract break → **patch**.

## CLAUDE.md rule 9 — experimental flag

**Asked and answered: the flag does not apply. Recorded rather than skipped, as the rule requires.**

Rule 9 triggers on a **new** user-visible surface — a pane, field, command, tab, footer entry, or keybinding. This PRD adds none. It changes *when an existing keybinding is claimed* and *where an existing value is stored*. The chord, the action, the three stages, and both tab types' layouts all already ship. There is no new surface to gate, and no `show_<feature>()` wrapper to add to `src/features.rs`.

There is also a specific reason gating would be actively wrong here. Rule 9 calls the flag a *presentation* switch, and Defect 1 is a **bug in key dispatch**: flagging the fix would mean shipping a build that still swallows `Ctrl+L` by default, which is the exact behaviour the PRD exists to remove. A bug fix behind an opt-in flag is not a bug fix.

Matches the precedent #371 set for the same surface (resolved: no, visible by default), and #336/#333/#341 before it. The user can still overrule this if they want the *behaviour change* half (decision 2) opt-in for one release; the default reading is that it does not apply.

## Open Questions

**1 — Should the fork-sync stack table be corrected now or at the next sync? — RESOLVED: now.** This PRD flagged that `docs/develop/fork-sync-workflow.md` lists `ab71a28` (#336) and `30c5f79` (#371) as independently PERMANENT when the second supersedes the first. Correcting it now is a one-line docs change and keeps the record honest; correcting it at the next sync means doing it with the actual conflict in front of you, when `split_narrow` is real and the resolution is concrete. Recommendation was **now**, as a standalone docs commit — the table's purpose is to be correct *before* someone relies on it during a sync — and that is what happened: `d1391d8` records #371's `SplitStage` as superseding #336's `split_narrow`, and adds the supersession procedure. The Scope section's "out of scope" bullet is annotated accordingly. Whoever runs the next sync should still re-read the table against the then-current text rather than assuming it is complete.

**2 — Keep the thread-local, or pass the stage as a parameter?** The two thread-locals exist only because `compute_frame_layout`'s signature is a fixed, widely-tested seam. Collapsing to one is the minimal change and is what this PRD assumes. But a deck-global value is a much better candidate for an explicit parameter than a per-tab one was — there is exactly one, and the "read fresh every frame from the active tab" justification disappears with the per-tab field. Deferred to the implementer at M3, with a bias toward the minimal change: widening a widely-tested seam is a bigger diff than this PRD's actual subject.

**3 — Does persistence become more attractive once the value is singular?** Out of scope here, and stated as such. Noting it because #371's Open Question 1 deferred persistence partly on the grounds that per-tab state made it fiddly; a single deck-global stage is a much simpler thing to persist, so the follow-up is worth revisiting on its own merits rather than inheriting #371's "not proposed as trivial" verdict.

## Work Log

### 2026-08-06 — M1–M6 implemented, tested, reviewed and audited

Shipped across eight commits: `6c7647f` / `1147a0d` (M1 RED), `731806c` (M1 GREEN), `31577ec` (test repair for M1's scoping), `ee4892f` (M2/M3/M4 RED batch), `84a0d1e` (M2/M3 GREEN), `0ee97f1` (M4 repair + M5 extension), `e053ab9` (M6 docs/changelog/cross-version). Reviewer and auditor both returned **no blockers**; the reviewer's two suggestions were the PRD-record corrections applied in this same update.

**The staleness window really did close by construction, and two independent arguments confirm it.** `orchestration_role_pane_dims` lost its `narrow: bool` parameter and now reads the single `ACTIVE_SPLIT_STAGE` directly — necessary, because M4a asserts a brand-new tab's role panes record a *Narrow*-derived `cols`, which a hardcoded `narrow: false` could never produce. Review confirmed both production callers (live spawn `src/ui.rs:7917`, saved-session restore `:9898`) run on the TUI thread, and that `UiState::split_stage` *and* the thread-local both initialise to `SplitStage::Default`, so no un-synced non-default state can exist. The audit added a second, independent argument: command-mode key events break the input drain and render before another command-mode event, so a toggle cannot be followed by a keyboard-triggered spawn reading a stale mirror.

**Two test-side defects were found *after* the RED batch, and the reason matters.** `layout_003` compared `compute_frame_layout`'s `panes_area.width` (the **outer** pane-column width) against `AgentSpawnOptions::cols` (the PTY's **inner** width) — off by exactly the 2 border columns. It survived the RED batch because that batch's RED was a *compile* failure, so the assertion arithmetic never executed even once. **Lesson worth carrying forward: a compile-level RED does not validate assertions.** The fix was test-side only; resolving it in production by making `cols` the outer width would have satisfied the test while reintroducing the F3 spawn-vs-render drift the six `orchestration_role_pane_dims_*` seam guards exist to prevent. Separately, `dashboard_001` passed a *panicking* helper directly as a `wait_for_grid_predicate_within` predicate; the harness evaluates predicates at t=0 before any sleep, so the first sample still showed the outgoing tab and the helper aborted instead of retrying. Fixed with the `wait_for_string` guard the same file already used one call earlier.

**M5 needed extending, not just re-labelling.** `orchestration_006` already drove the full `Default → Narrow → Hidden → Default` cycle through a real PTY and asserted geometry at every stage — but its `Hidden` check was `pane_column_left_edge == 0`, which proves where the pane box *starts*, not that the sidebar renders *nothing*; stray sidebar fragments elsewhere on the grid would go unseen. A direct content-absence assertion was added, reusing the file's existing `· <role>` needle. Note that `orchestration_024` contributes nothing to M5 — it proves M1's mode-scoped forwarding, not split geometry.

**Cross-version check: hooks confirmed, delegate unverified-not-failed.** Run against the newest pre-#387 build (`66f2c8d`, protocol 7) rather than the literal previous release (v0.35.7, protocol 6) — that protocol bump is PRD #370's and landed unreleased on `main`, so a v0.35.7 daemon would be refused at the handshake for a reason predating this branch, telling us nothing about #387. Hooks demonstrably round-tripped across the boundary. The delegate leg was **not** confirmed: the fixture's worker role runs `bash`, which the deck does not recognise as an agent, so there was nothing to inject a task into. No evidence of breakage, and the branch touches no daemon/protocol/hook code — recorded as a stated partial skip, and it must be said plainly in the PR rather than left ambiguous.

**Procedural finding worth reusing: you cannot test a version boundary by simply pointing a newer TUI at an older daemon.** The build-version handshake (`src/build_version_handshake.rs`, PRD #103/#161) *silently restarts* a mismatched daemon whenever no agents are running, so the obvious procedure tests a freshly-spawned current-build daemon while appearing to test a boundary. This is exactly why rule 12 words it "with an agent under it". The procedure that works: the old binary serves; its own TUI opens an orchestration and **detaches** (not stops), leaving panes alive; the new TUI then raises `⚠ Daemon version mismatch (N agent(s) running)` and you decline the restart.

**Hand-exercised, and the single most convincing observation was a ratio.** With the orchestration tab left at `Narrow`, switching to the Dashboard and pressing `Ctrl+L` **once** produced `Split: 0/100` — continuing the cycle into `Hidden` rather than restarting from the Dashboard's own `Default`. The next press gave `Split: 33/67`, a value the code only ever produces for `Tab::Dashboard`. That one number simultaneously proves the tab switch happened, the stage is shared, and `Default` still resolves per tab type rather than flattening onto one ratio — decision 3's nuance, confirmed on screen rather than only in a test. Separately, the reported bug itself was reproduced fixed three times: sentinel echoed in a focused role pane, `0x0c` sent, **no** `Split:` status appeared *and* the sentinel vanished, so the deck declined the chord and readline's clear-screen genuinely ran.

### 2026-08-06 — PRD authored; the task's stated diagnosis corrected against the code

The authoring task described the defect as "PRD #371 extended `Ctrl+L` to Dashboard tabs **with no command-mode guard**", with the fix framed as covering a Dashboard gap that "nothing covers".

**Verification found the tab types inverted.** All three greps the task supplied are accurate — `ToggleOrchestrationSplit` is `Section::Global`/`Ctrl+l`, it is matched in the global resolver at `src/ui.rs:6545`, and `scope_orchestration_split` is absent from the fork. What does not follow is the inference drawn from them. #371 **did** add a guard: the inline `claims_ctrl_l` match at `src/ui.rs:9077-9086` scopes **Dashboard** to `UiMode::Normal` and leaves **Orchestration** claiming mode-independently. `orchestration_007` already pins the Dashboard half with a real interactive pane. So the swallowing bug is real and the user's daily pain is real, but it lives on **orchestration tabs** — which is, if anything, a stronger case for the fix, since role panes are where interactive agents actually run and are exactly where #342's own doc comment predicted the conflict would hurt.

Nothing about the four decisions changes; decision 1 fixes it either way, and the correction makes the "generalise `scope_orchestration_split`" instruction fit better than expected — with the rule uniform, the tab-type dimension collapses to "does this tab have a sidebar split", which is one parameter, as the task anticipated. The PRD is written against the verified behaviour throughout, and the required real-pane test (M1b) is aimed at the orchestration path rather than the already-covered Dashboard one.

**Also verified while writing**: `split_narrow` has zero occurrences under `src/` on the fork's `main`; `SplitStage` has 14 in `src/tab.rs` and 44 in `src/ui.rs`; `split_stage` appears nowhere in `src/config.rs` or `src/state.rs`, confirming there is no persisted state to migrate; and `pane_layout: PaneLayout` on `UiState` (`src/ui.rs:1566`) is an existing deck-global-UI-preference field with a global chord, which is the precedent decision 2's shape follows.
