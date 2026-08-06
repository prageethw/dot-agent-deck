# PRD #393: A command-mode-scoped, deck-global command-entry lock — and focus that follows the lock

**Status**: Test plan approved by the user. Implementation starting on branch `prd-393-command-mode-lock-and-lock-governed-focus`, in a dedicated worktree at `../dot-agent-deck-prd-393`, cut from `origin/main` at `7747e6a`.
**Priority**: Medium-High (the lock and the focus timer are both in the deck's primary daily path; the timer half removes a behaviour the user actively does not want)
**Created**: 2026-08-07
**GitHub Issue**: [#393](https://github.com/vfarcic/dot-agent-deck/issues/393), filed upstream on `vfarcic/dot-agent-deck`. **Note the number**: this PRD was drafted as `#388` on the assumption that it would take the next number after the then-highest `#387`; upstream had several issues in flight and it landed as `#393`. Every internal reference has been renumbered.
**Related**: [#374](https://github.com/vfarcic/dot-agent-deck/issues/374) (`prds/374-command-entry-lock.md`) — **this PRD reverses two of its recorded decisions**; the lock mechanism itself is kept. [#373](https://github.com/vfarcic/dot-agent-deck/issues/373) (`prds/373-auto-return-focus.md`) — **this PRD deletes its M2 and M3 outright** and keeps M1. [#387](https://github.com/vfarcic/dot-agent-deck/issues/387) (`prds/387-unified-deck-global-split-toggle.md`) — **the direct structural precedent**: the same two moves (command-mode scoping + deck-global state) applied to the sibling chord `Ctrl+L`; its `scope_split_stage` is the shape this PRD's scoping function follows, and its decisions 1 and 2 are the reasoning this PRD inherits rather than relitigates. [#241](https://github.com/vfarcic/dot-agent-deck/issues/241) M1 / [#218](https://github.com/vfarcic/dot-agent-deck/issues/218) — `close_pane`'s command-mode scoping (`Ctrl+W` closing a pane while typing in a shell), the original precedent for decision 1. **[#369](https://github.com/vfarcic/dot-agent-deck/issues/369) — the rationale thread, and required reading before any upstream work**: it asks whether workers should accept direct human input at all, and carries vfarcic's detailed reply declining the "deliberate act" framing, naming the accidental-misdirection framing he would accept, and raising the fail-safe objection. See the Upstream section. [#361](https://github.com/vfarcic/dot-agent-deck/issues/361) — the parent issue #371/#372/#373/#374 were all split out of. [#383](https://github.com/vfarcic/dot-agent-deck/issues/383) — the blocked-keystroke reset, folded into #373 and now deleted by this PRD. [#330](https://github.com/vfarcic/dot-agent-deck/issues/330) — `delegate` exits 0 with no output, so a confirmatory re-run silently arms a duplicate delegation; relevant because the "check the worker panes" diagnostic the fail-safe objection rests on runs through that same path. Code: `src/ui.rs` (`handle_key_event`, `handle_pane_input_key`, `gate_pane_input_key`, `global_action`, `global_action_for_mode`, the per-frame auto-focus chain, `UiState::last_pane_keystroke_at`), `src/tab.rs` (`Tab::Orchestration::command_entry_locked`, `last_role_pane_activity_at`, `had_waiting_pane`, `all_clear_pending`, `auto_focus_waiting_pane`, `auto_focus_all_clear`, `auto_focus_after_inactivity`, `observe_waiting_panes`, `inactivity_timeout_from_env`), `src/keybindings.rs` (`Action::ToggleOrchestrationLock`).

## Why the lock exists at all — the rationale, stated because it was never written down

#374 built the lock and described *what* it does, but never recorded *why* anyone would want it. That omission matters twice over: it is very likely part of why the feature was declined upstream as not-planned, and without it a future reader sees only an odd restriction on typing and reasonably asks to have it removed. Recorded here, from the user, as the load-bearing motivation for everything in this PRD:

**An orchestration deck is not a set of terminals that happen to share a window. It is one workflow with a single coordinator.** When every pane accepts human keystrokes, the human becomes a second, uncoordinated actor inside that workflow. Interrupting a worker directly — typing into it, answering its question, redirecting it — mutates state the orchestrator believes it owns, and the orchestrator has no way to learn that it happened. The deck still looks coherent; the model behind it has quietly diverged.

The second half is about the human, not the machine. **Open panes invite a reflex.** A prompt appears in a worker pane and the natural response is to answer it immediately, right there, because the cursor is already available and the question is right in front of you. That reflex is fast and almost always wrong at the workflow level: it bypasses the coordinator, it commits to an answer before the wider context is considered, and it is taken without pausing to ask whether the orchestrator should be handling this instead. **The lock's purpose is to convert that reflex into a decision** — the deliberate `Ctrl+D`, `Ctrl+E` is not overhead, it is the pause. The friction is the feature.

This reframes the entire design. The lock is not a safety guard against stray keystrokes, and it is not a modal editor affectation. **It is a workflow-integrity mechanism**, and the default has to be locked for it to mean anything — a lock you must remember to engage protects nothing.

### The tension this creates with decision 4 — named, not smoothed over

Decision 4 (a `WaitingForInput` pane is not gated) was settled on usability grounds before this rationale was written down, and the two are **not fully consistent**. The rationale above says the reflex to "just jump in and answer the agent's prompt" is precisely what the lock should interrupt. Decision 4 makes exactly that prompt answerable with no pause at all.

The narrower reading that keeps both is that the lock distinguishes two different acts: **interrupting a working agent** (unsolicited, always requires a deliberate unlock) versus **answering an agent that has explicitly stopped and asked** (solicited, and arguably not an interruption at all, since the workflow is already blocked on a human). Under that reading decision 4 is a refinement, not a hole — the pause is preserved where it protects the workflow and relaxed where it only obstructs.

**Resolved by the user: that narrower reading is the intended one, and decision 4 stands.** The lock's subject is the *unsolicited* interruption — typing at an agent that is working, on the human's initiative, while the orchestrator believes it owns that state. An agent that has stopped and asked is a different situation: the workflow is already blocked on a human, the orchestrator is not mid-flight on that pane, and answering is a response to a request rather than an intrusion into one. The pause is therefore preserved exactly where it protects workflow integrity and relaxed where it would only obstruct.

So the rationale is narrowed, deliberately and in writing: **the friction exists to convert an unsolicited interruption into a decision, not to add ceremony to a question the agent itself asked.** Recorded this way so that a future reader — or an upstream reviewer — sees a considered boundary rather than an inconsistency. Tracked as resolved Open Question 4.

## Problem Statement

Four problems, all in the same input/focus path. They are stated separately because each stands on its own — the first three are things the lock gets wrong, the fourth is a behaviour that should not exist at all.

### Problem 1 — `Ctrl+E` is claimed mode-independently, so it is both hard to reach and swallowed where it matters

PRD #374 deliberately made `Ctrl+E` a global chord resolved from any mode, reasoning that the lock "actually matters" in `UiMode::PaneInput` and so the chord must work from there. The consequence is the same conflict class `Ctrl+W` hit in PRD #241 and `Ctrl+L` hit in PRD #387: on an Orchestration tab the deck eats the chord unconditionally, so a focused role pane's PTY never receives `0x05` and **readline's `end-of-line` never reaches the agent**. #374's own docs acknowledge this and describe non-orchestration tabs falling through "as ordinary input (e.g. readline's end-of-line binding)" — which is a precise admission that on orchestration tabs, where interactive agents actually run, the binding is gone.

This is the third instance of one pattern. #241 resolved it for `Ctrl+W`, #387 resolved it for `Ctrl+L`, and both landed on the same trade: scope the chord to command mode, cost the user one extra `Ctrl+D`, and give the chord back to the program they are typing into. `Ctrl+E` is the remaining hold-out, and it is the one where the argument for mode-independence was made most explicitly — so it is worth saying why that argument does not survive. It rested on "you need to unlock while focused on a pane"; but unlocking is a deliberate, infrequent act, not a per-keystroke one, and `Ctrl+D` then `Ctrl+E` is exactly the ritual the user already performs for `Ctrl+W` and (post-#387) `Ctrl+L`. Uniformity across the three chords is worth more than one saved keystroke on a rare action.

### Problem 2 — lock state is per-tab, so unlocking has to be repeated in every Orchestration tab

#374 resolved the lock to be **per-orchestration-tab**, on the stated grounds that "each orchestration tab is independent." In use that means someone who wants direct pane access unlocks tab 1, switches to tab 2, and is locked again, with no indication that the state they set moments ago does not apply here. This is the identical complaint #387 records for the split stage, and the identical resolution applies: the lock reflects **how someone is working right now, not which tab they happened to open**. There is the same shape precedent — `UiState::pane_layout` is already a deck-global UI preference driven by a global chord (`Ctrl+T`), and #387 moves `split_stage` alongside it for exactly this reason.

### Problem 3 — a locked pane cannot answer a question its own agent asked

The lock gates every keystroke to a non-orchestrator role pane. When Claude Code (or any agent) presents an interactive prompt in a worker pane — a permission request, a numbered option list, a yes/no — the human can see the prompt and cannot answer it. The only remedies today are unlocking the whole tab or routing the answer through the orchestrator, and neither is what the moment calls for. The deck already knows this state: `SessionStatus::WaitingForInput` is joined per pane every frame by `build_pane_status`, and it already drives both PRD #333's tab-label colouring and the fork-only `auto_focus_waiting_pane` steering. The information needed to fix this is present and unused at the gate.

### Problem 4 — the 30-second inactivity snap-back is unwanted, and it was never fully sound

PRD #373 M2 snaps focus back to the orchestrator after 30 seconds of inactivity on a non-orchestrator pane. The user's direction is to remove it: focus should simply stay where it is put. Beyond the preference, the mechanism carries known defects on record in #373 itself:

- Its wall-clock "time since last stamp" check is **racy against render-loop stalls**. A frame that takes a few hundred ms to drain queued keystrokes makes the last stamp look stale even while the human is typing steadily — measured at `elapsed=4.86s` against a 2s threshold, 100% reproducible, and caught only by the PTY-attached test. The shipped mitigation was to arm the branch only while `command_entry_locked` is true, which narrows the race rather than closing it.
- It required **six** distinct activity-stamp sites to be kept in sync (`Action::ForwardToPane`, paste, `Action::SelectCard`/`FocusCard`/`Focus`, `dispatch_normal_mode_key`, `send_config_gen_prompt`, tab switch-in), and four review findings on #373 were each a missed or over-broad stamp at one of them. That is a maintenance surface with no remaining justification once the behaviour is gone.

The mitigation above is also the seed of this PRD's focus model: **#373 already established that the lock governs whether the focus machinery runs.** This PRD generalises that from one branch to the whole chain.

## The five decisions — settled, with reasoning

Decided by the user before this PRD was written. Recorded as **settled**, not open, with reasoning preserved so a future reader does not relitigate them.

**1 — `Ctrl+E` is command-mode only.** The deck claims the chord only in `UiMode::Normal`; in pane input it passes through to the agent untouched. Follows #387 decision 1 and #241 M1 verbatim. Reverses #374's "global chord, any mode" decision.

**2 — One lock value across every Orchestration tab.** All Orchestration tabs share a single lock state: toggling it on **any Orchestration tab** changes it on **every Orchestration tab**, and a newly opened Orchestration tab adopts the current value. Follows #387 decision 2; reverses #374's per-tab decision. Resolves the user's "all panes in all tabs will be editable" as *all Orchestration tabs*, not as widening the gate to new tab types — see decision 3.

**State this precisely, because "deck-global" is easy to over-read.** What goes deck-global is *where the value is stored* — one field on `UiState` instead of one per `Tab::Orchestration`. It is **not** a claim that the chord works everywhere or that the lock affects anything outside Orchestration tabs. Decisions 1 and 3 together mean `Ctrl+E` is only ever *claimed* on an Orchestration tab in command mode; on a Dashboard or Mode tab, `Ctrl+D` followed by `Ctrl+E` toggles nothing and the chord falls through as ordinary input, exactly as it does today. A single stored value is simply the mechanism that makes the setting stop being per-tab — it is the same shape `UiState::pane_layout` and (post-#387) `UiState::split_stage` already use.

**3 — The gate's reach is unchanged: Orchestration tabs only.** Dashboard and Mode tabs are not gated and behave exactly as they do today. Confirmed explicitly by the user ("orchestrator-only focus applies to orchestration tabs only"). This keeps the "orchestrator pane" concept meaningful — it exists only on `Tab::Orchestration`, via `role_pane_ids[start_role_index]` — and avoids inventing a per-tab-type "privileged pane" rule for Dashboard tabs, which hold arbitrary unrelated panes. **What goes deck-global is the state, not the reach.**

**4 — A pane that is `WaitingForInput` is not gated.** While a non-orchestrator role pane reports `SessionStatus::WaitingForInput`, the lock stops gating that pane entirely and every key reaches its PTY; the gate re-engages the instant the status clears. Chosen over an always-allowed navigation-key allowlist (arrows/Enter/Esc/digits/y-n) because it reuses a signal the deck already computes every frame, needs no per-agent key knowledge, and can answer a free-text prompt — which an allowlist cannot. **Accepted limitation:** an agent that never reports `WaitingForInput` gets no carve-out and still needs a deliberate unlock. That is the same limitation `auto_focus_waiting_pane` and #333's tab colouring already carry, so it adds no new class of blind spot.

**5 — Focus follows the lock.** While **locked** (the default), focus is pinned to the orchestrator pane: it steers to a `WaitingForInput` role pane in ascending `role_pane_ids` order while one exists, and returns to the orchestrator on the all-clear edge. While **unlocked**, no auto-focus branch fires at all — focus stays exactly where the human put it until the deck is locked again. The 30-second inactivity timer is deleted in both states. The user's words: *"it will stay focussed in same pane unless we lock panes again."*

## Solution Overview

Three changes, each small, sharing one theme: the lock becomes a single deck-wide mode that governs both key routing and focus.

**Key routing.** Add a pure scoping function beside #387's `scope_split_stage`, following its shape exactly:

```rust
/// Mirrors #387's `scope_split_stage` for the command-entry lock.
/// `is_orchestration_tab` is true only for `Tab::Orchestration`, whose
/// `role_pane_ids[start_role_index]` gives the chord something to mean.
fn scope_command_entry_lock(
    action: Option<Action>,
    is_orchestration_tab: bool,
    mode: UiMode,
) -> Option<Action> {
    match action {
        Some(Action::ToggleOrchestrationLock) if !is_orchestration_tab || mode != UiMode::Normal => None,
        other => other,
    }
}
```

Kept standalone and pure for #342/#387's stated reason: it is unit-testable without a PTY, whereas an inline `if` at the call site is only reachable through the full event loop. Note this *replaces* #374's existing un-resolution logic (which already returns `None` on non-orchestration tabs) rather than adding a second mechanism — the mode term is the only new condition.

**The gate.** `gate_pane_input_key` keeps its structure and changes two inputs: it reads the deck-global lock instead of `Tab::Orchestration::command_entry_locked`, and it consults the focused pane's live `SessionStatus`. It drops a keystroke only when the tab is an Orchestration tab, the lock is engaged, the focused pane is not the orchestrator, **and** that pane is not `WaitingForInput`. The status is read from the same per-frame `build_pane_status` join the auto-focus chain already uses, so no new data flow is introduced. The drop-path status message stays, but its wording is now wrong: "Pane locked — Ctrl+e to unlock" tells the user to press a chord that no longer works from where they are standing. It must name `Ctrl+D` first — see M6.

**The focus chain.** Today's per-frame chain is a three-way `else if`: `auto_focus_waiting_pane` → `auto_focus_all_clear` → `auto_focus_after_inactivity`, preceded by an unconditional `observe_waiting_panes`. The third branch is deleted, and the whole chain — including `observe_waiting_panes` — is gated on the deck-global lock being engaged. While unlocked the chain does not run, so no focus decision exists to fight the human.

One subtlety that must be handled deliberately rather than discovered: **`observe_waiting_panes` maintains the edge state (`had_waiting_pane` / `all_clear_pending`) that `auto_focus_all_clear` consumes.** Skipping it while unlocked means the deck stops tracking waiting episodes, so an episode that begins and ends during an unlocked stretch leaves a stale latch behind. On the locked→unlocked transition the latch must therefore be **cleared**, so that re-locking starts from a clean slate and does not fire an all-clear move for an episode the human already dealt with by hand. This is the only piece of genuinely new state logic in the PRD.

## Migration

**There is no on-disk migration.** Neither `command_entry_locked` nor any auto-focus state is persisted — `Tab` carries no `Serialize`/`Deserialize` derive and snapshots are built into separate `Saved*` structs (verified for `split_stage` by #387; the same holds here and must be re-verified at M2). Every deck starts locked on launch, exactly as today.

**In-memory state.** The per-tab field is **removed, not shadowed** — keeping a global default plus a per-tab override reintroduces the "which one wins" ambiguity decision 2 exists to delete, and nothing requires a tab to diverge from the deck.

- Delete `command_entry_locked: bool` from `Tab::Orchestration` — the declaration at `src/tab.rs:185` plus **exactly four** struct-literal initialisers, all in `src/tab.rs`: `:939` (`open_orchestration_tab`) and `:1076` (hydration/reconnect), both production, and `:1831` / `:1864`, fixtures in that file's own test module. **Correction, verified during M2:** this bullet previously said the initialisers include `src/ui.rs` test fixtures. There are none — every `src/ui.rs` occurrence of the field is a pattern-match *read*, never a struct-literal *write*.
- Add `command_entry_locked: bool` to `UiState`, defaulting to `true`, directly alongside `pane_layout` and (post-#387) `split_stage` — same field, same lifetime, same deck-global-UI-preference semantics.
- `Action::ToggleOrchestrationLock`'s handler collapses from a per-tab mutation to a single `ui.command_entry_locked = !ui.command_entry_locked`.

**Deletions from #373 M2/M3.** All of the following go, and the diff should be read as a subtraction:

- `TabManager::auto_focus_after_inactivity` and its `INACTIVITY_TIMEOUT` constant.
- `Tab::Orchestration::last_role_pane_activity_at` and **all six** stamp sites.
- `TabManager::inactivity_timeout_from_env` and the `DOT_AGENT_DECK_INACTIVITY_TIMEOUT_SECS` test seam #373 M5 introduced.
- The blocked-keystroke stamp inside the lock's drop site (#373 M3).
- `UiState::last_pane_keystroke_at` **only if** nothing else reads it — #373 describes it as pre-existing and used at the `ForwardToPane` and paste sites; the implementer must check for other consumers before removing it, and leave it if any exist.

**Kept, and must stay green.** `auto_focus_waiting_pane`, `auto_focus_all_clear`, `observe_waiting_panes`, `had_waiting_pane`, `all_clear_pending`, and the `poll(0ms)` pending-input guard on both surviving branches.

## Scope

### In Scope

- `scope_command_entry_lock`, replacing #374's un-resolution logic; `Ctrl+E` claimed only in `UiMode::Normal`, only on Orchestration tabs.
- One deck-global `command_entry_locked` on `UiState`; the per-tab field removed.
- The `WaitingForInput` carve-out inside `gate_pane_input_key`.
- Deleting #373 M2 and M3 in full, per Migration above.
- Gating the surviving two-branch focus chain (and `observe_waiting_panes`) on the lock, with the latch cleared on the locked→unlocked transition.
- Rewriting the tests whose assertions invert; deleting the tests whose subject is gone.
- `docs/keyboard-shortcuts.md` and `docs/orchestration.md`; changelog fragment; amendments to `prds/373-*.md` and `prds/374-*.md` so neither is left asserting behaviour that no longer exists.
- The CLAUDE.md rule 12 answer (see below).

### Out of Scope

- **Widening the gate to Dashboard or Mode tabs.** Decision 3.
- **Cross-tab focus switching.** Focus never leaves the active tab to chase a waiting pane elsewhere; the tab label's colour (#333) already flags it. Decision 5 is scoped to the active tab.
- **Ordering waiting panes by wait time.** Ascending `role_pane_ids` order is what `auto_focus_waiting_pane` already implements and what decision 5 keeps. A "longest-blocked first" ordering would need a new per-pane `waiting_since` timestamp and is a separate change.
- **The deferred input/focus race.** #373 records a residual window where a key arriving after the `poll(0ms)` check and before `focus_pane` completes lands in the orchestrator's PTY. The real fix is binding each input event to the pane focused when it arrived (a focus-generation handoff), which re-architects the input loop. **This PRD narrows the window substantially — deleting the timer removes the branch that fired unprompted, so focus now only moves on a status edge the human can see coming — but does not close it.** It stays deferred to its own PRD, and this PRD must not claim otherwise.
- **Persisting the lock across restarts.** Same reasoning #387 gives for the split stage: more attractive once the value is singular, but separate snapshot work.
- **An always-allowed navigation-key allowlist.** Considered and rejected at decision 4; recorded so it is not re-proposed as an obvious addition.

## Milestones

Each is independently testable; the test that proves each is named in the Test Plan.

- [ ] **M1 — `scope_command_entry_lock`, the chord fix.** The pure scoping function replaces #374's inline un-resolution. Orchestration tabs stop claiming `Ctrl+E` outside command mode. **Deliberately first and standalone**: it is the user-visible chord fix, independent of everything else, and the half that stands alone if the rest is rejected.
- [ ] **M2 — Deck-global lock state.** `command_entry_locked` moves to `UiState`; the per-tab field is deleted; the toggle handler collapses to one assignment. Re-verify no persistence path touches it.
- [ ] **M3 — The `WaitingForInput` carve-out.** `gate_pane_input_key` consults live pane status and stops gating a waiting pane; re-engages when the status clears.
- [ ] **M4 — Delete the inactivity timer and gate the chain.** #373 M2/M3 removed in full per Migration; the surviving two-branch chain plus `observe_waiting_panes` gated on the lock; the edge latch cleared on locked→unlocked. **Split during implementation into M4a (deletion) and M4b (chain gating) — see the sequencing note below.**

### Sequencing correction found during implementation — M2 and M4a are one pass

The milestone list above reads as if M2 and M4 are independent. They are not, and the coupling was only found once M2's RED tests existed. Recorded here because the milestone order is otherwise misleading to anyone picking this up.

`auto_focus_after_inactivity`'s gating (`src/ui.rs:10742-10751`) **pattern-matches `Tab::Orchestration::command_entry_locked`** — the very field M2 removes. So M2 cannot land without breaking it. Nor can the deletion simply run first: M2's RED tests reference `ui.command_entry_locked`, so the test binary does not compile until M2 lands either. The two are mutually blocking, and the only orderings that produce a compiling tree are "both together" or "patch one temporarily and undo it later" — the latter being throwaway work on code already condemned.

**Resolved: M2 and M4's deletion half ship in one coder pass** (as two commits where they split cleanly — deletion first, then the state move — so the wide subtraction stays reviewable apart from the state change). M1 remains the standalone commit the milestone ordering was really protecting, and it already shipped that way (`f854ef8`).

**M4 therefore splits in two:**
- **M4a — the deletion.** Pure subtraction: `auto_focus_after_inactivity`, `inactivity_timeout_from_env`, the `DOT_AGENT_DECK_INACTIVITY_TIMEOUT_SECS` seam, `last_role_pane_activity_at` and its six stamp sites, the blocked-keystroke stamp, the chain's third branch, and tests `tabs/orchestration/013`-`019`, `021`-`023` plus `orchestration/focus/001`. Ships with M2.
- **M4b — the chain gating.** Gating the surviving two-branch chain and `observe_waiting_panes` on the deck-global lock, plus clearing the edge latch on the locked→unlocked transition. This is the PRD's only genuinely new state logic, it depends on M2's deck-global value existing, and it keeps its own tester-RED → coder cycle with `tabs/orchestration/026` as its pin.
- [ ] **M5 — Real-pane proof.** The PTY-attached tests: `Ctrl+E` reaching a role pane's PTY, and the full locked/unlocked focus contract as a user sees it.
- [ ] **M6 — Docs, changelog, PRD amendments, rule 12 answer.** Including the status-message wording change to name `Ctrl+D` first.
- [ ] **M7 — Upstream contribution (optional, non-blocking, and NOT startable yet).** See "Upstream" below. This is a *net-new feature proposal*, not a port of this branch's diff, and it must not gate the fork work. **Gated on two things that are not code**: answering the three questions vfarcic left on [#369](https://github.com/vfarcic/dot-agent-deck/issues/369), and accumulating real usage evidence on the fork — he explicitly said that is the best possible argument for bringing it upstream.

## Upstream — there is nothing here to port, and that changes the pitch

**Verified against `upstream/main` (`c73f6d7`) before implementation started**, because the assumption that an upstream PR was available turned out to be wrong in an important way:

| Symbol | Occurrences on `upstream/main` |
|---|---|
| `command_entry_locked` | 0 |
| `auto_focus_waiting_pane` / `auto_focus_all_clear` / `auto_focus_after_inactivity` | 0 |
| `ToggleOrchestrationLock`, any `Ctrl+e` binding | 0 |
| `gate_pane_input_key` | 0 |
| `scope_orchestration_split` (#342) | 0 — still unmerged |

**The entire command-entry lock and the entire auto-focus chain are fork-only.** #373 and #374 were both closed upstream as not-planned and continued here independently, so upstream never received either. It follows that **this PRD has nothing to fix upstream**: you cannot scope a chord upstream does not bind, remove a timer upstream does not run, or add a carve-out to a gate upstream does not have. Every line of this branch's diff touches code that exists only on the fork.

That does not close the upstream door, it reframes it. The available contribution is the **whole feature, net-new, in its post-#393 shape** — one coherent "command entry is locked to the orchestrator, and focus follows the lock" model, arriving as a single proposal rather than as the four split-out issues (#371/#372/#373/#374) that were declined piecemeal. Two things make that a genuinely better pitch than the originals: it carries none of the mechanisms that were reverted here (no per-tab state to argue about, no 30-second wall-clock timer with a documented race), and the `Ctrl+E` scoping now matches the `Ctrl+W`/`Ctrl+L` precedent upstream already accepts.

**The third and most important thing is the argument, not the code.** Read #374 as an upstream maintainer sees it: it proposes that typing into a pane you can see should stop working by default, and it never says why. That is a hard sell on its own terms, and "closed as not-planned" is a reasonable response to it. The case only becomes compelling once the rationale in **"Why the lock exists at all"** above is on the table — that this is a **workflow-integrity mechanism**, not an input restriction. The two claims an upstream proposal has to lead with:

1. **A human typing into a worker pane is a second uncoordinated actor in a workflow that has a coordinator.** It mutates state the orchestrator believes it owns, with no path for the orchestrator to learn it happened. The deck looks fine; the model behind it has diverged. This is a correctness argument about multi-agent orchestration, not an ergonomics preference.
2. **The deliberate `Ctrl+D`, `Ctrl+E` is the point, not the cost.** An open pane invites the reflex of answering a prompt on the spot, bypassing the coordinator and committing before the wider context is considered. Converting that reflex into a decision is the entire value; the friction is the feature. A lock you must remember to engage protects nothing, so locked-by-default is load-bearing rather than an opinionated default.

Anything submitted upstream should therefore **lead with the rationale and treat the diff as the supporting detail** — the reverse of how #374 was presented. It should also be explicit that this is scoped to Orchestration tabs, since the objection "you have made my terminal read-only" is the obvious first reaction and is not what the change does. And it should carry the resolution of the decision-4 tension above rather than leaving a reviewer to find the inconsistency themselves.

### The upstream maintainer has already answered this argument — read [#369](https://github.com/vfarcic/dot-agent-deck/issues/369) before writing anything

This is the single most important input to M7, and it was found only after this PRD was drafted. Issue **#369** ("If idea is human to communicate via orchestrator, should we even allow command prompt of its workers at all?") raises exactly the rationale recorded above, and **vfarcic replied at length**. Three things in that reply change how M7 must be approached:

**1 — The "deliberate act" framing has already been declined.** His words: *"A keybinding for something you should rarely do adds UX surface for everyone, permanently, to guard against something that is — as you describe it — a deliberate act. 'Giving direct instructions to workers confuses the workflow' reads to me as an argument for don't do that rather than for make it impossible."* The rationale in "Why the lock exists at all" above is, as stated, that framing. **Leading upstream with it would re-run an argument that has already lost.**

**2 — He named the framing he *would* find compelling, and it is the accidental case.** *"The version of this I'd find much more compelling is the accidental one: has anyone ever sent input to a worker while believing they were talking to the orchestrator? Inspect a worker pane, get distracted, type your next instruction into the wrong pane. That's a real hazard, it's a different problem from the one in the title, and it would be worth fixing on its own terms."* That is a direct invitation, and it happens to fit this PRD's design far better than #374's did — misdirected input is exactly what a locked default prevents, and the `Ctrl+D`/`Ctrl+E` cost falls only on the deliberate case, which by definition is not the one being guarded against. **M7 should lead with misdirection, not with discipline.**

**3 — The fail-safe objection is the real obstacle, and it must be answered head-on rather than argued around.** He points out that reaching into a worker is a fail-safe the deck already depends on: a provider hiccup parks an agent and typing "try again" fixes in seconds what nothing else can reach; a weaker model never calls `work-done`; an agent mode waits for input somewhere unexpected. Most concretely: *"the 30-second 'delegate possibly not delivered' report exists precisely for a task that never landed, its text says 'check the worker panes', and it is written deliberately without an Enter so the orchestrator doesn't chase it — that diagnostic is addressed to the human. Removing worker input would strand its own remedy."*

The answer this design already has, which should be stated plainly rather than discovered by the reviewer:

- **The lock does not remove worker input. It gates it behind one deliberate act.** Every fail-safe he lists remains reachable; it costs `Ctrl+D`, `Ctrl+E`. The objection as written is against *removal*, and that is not what is proposed.
- **Decision 4 reaches the parked-agent cases at zero cost.** A pane reporting `WaitingForInput` is not gated at all, so "an agent mode waits for input where you didn't expect it" needs no unlock whatsoever — arguably it is *better* served than today, since the same signal also steers focus to that pane.
- **The "check the worker panes" diagnostic is the sharpest test of the design, and it must be verified rather than assumed.** If a worker that never received its task does *not* report `WaitingForInput`, then reaching it does require an unlock, and M7 must say so honestly instead of claiming the carve-out covers it. **Action for M7: verify what status such a pane actually reports before making any claim about it.** Note the deck's own duplicate-delegation hazard here too ([#330](https://github.com/vfarcic/dot-agent-deck/issues/330)) — this diagnostic path is already known to be fragile.

**4 — Fork-first was explicitly blessed, and the precondition is usage evidence, not a better PR.** *"No objection at all to the lock living in your fork in the meantime — if it turns out to earn its keystroke there, that's the best possible evidence for bringing it upstream."* So M7's gate is not "write a more persuasive description" — it is **having actually lived with this for a while and being able to report what happened.** Submitting before that evidence exists spends the invitation for nothing.

**The three questions have now been answered on #369, and the key one lands in this PRD's favour.** vfarcic asked (1) what the confusion actually looked like, (2) whether it was ever *accidental*, and (3) whether idle/not-delivered reporting changes the calculus. The reporter's answers:

1. *"It was mostly orchestrator and workers contradicting each other and becoming a deadlock situation. I quickly figured out I should not be interfering with it, but until I got familiar with it, it was hard, and I still sometimes try to answer workers directly, not answering the orchestrator."*
2. *"Most accidental, yes, we should allow people to intentionally intervene when required."*
3. *"Sometimes, but mostly agents asking me what's next step with the prompt filled in; it is tempting to answer it at that time."*

**Answer 2 is the one that matters, and it unblocks the framing.** vfarcic said the accidental case is the version he would find compelling; the reporter says it is *mostly* the accidental case. The compelling framing is therefore available, and it is the truthful one — M7 should lead with it. Answer 2 also endorses the exact shape this PRD builds rather than a hard prohibition: *"we should allow people to intentionally intervene when required"* is precisely a locked default with a deliberate unlock, and it is a direct answer to the fail-safe objection in the reporter's own words.

Answer 1 adds a failure mode worth carrying upstream: the observed harm was **orchestrator and worker contradicting each other into deadlock**, not merely a stale plan. It also names an onboarding cost — the behaviour had to be learned the hard way, and is still occasionally violated by someone who now knows better. That is a strong argument for a default rather than documentation, and it is exactly the "don't do that" counter-argument's weak point: a rule you must remember is not a rule new users get for free.

### Answer 3 is evidence against decision 4, and is recorded rather than buried

Answer 3 says the dominant real-world temptation is *"agents asking me what's next step with the prompt filled in; it is tempting to answer it at that time."* An agent asking what to do next, with a prompt rendered and waiting, is **exactly a pane reporting `WaitingForInput`** — which decision 4 deliberately leaves ungated. So the single situation the reporter most often wants protection from is the one situation the carve-out reopens.

This does not reverse decision 4 — the user resolved that question explicitly after the tension was raised, on the reading that a solicited answer is not an interruption, and that resolution stands. But the evidence arrived afterwards and points the other way, so it is recorded here rather than left to be rediscovered. Two consequences worth acting on:

- **This is the thing to watch during the fork-usage period that gates M7.** The concrete question is whether, in practice, the carve-out reintroduces the "answered the worker instead of the orchestrator" mistake. If it does, decision 4 is the first thing to revisit, and Open Question 5's visual cue becomes considerably more attractive as a middle path — a pane that is *temporarily* typeable looking different from a locked one would restore the pause without restoring the block.
- **M7 must not claim the carve-out is unambiguously an improvement.** The honest upstream framing is that it trades a small hole for the fail-safe objection's answer, with the trade-off named.

Sequencing note: such a PR would sit on top of `scope_split_stage`, which upstream does not have either (#387 is fork-only; #342, its ancestor, is still unmerged). So an upstream submission would need to either carry its own scoping helper or wait on #342. **That dependency is the reason M7 is optional and non-blocking — the fork work must not wait on it.**

## Test Plan

**#386's standard applies verbatim, and #387 restates why: *a green suite around a mechanism nothing feeds is indistinguishable from a working feature.*** #387's Defect 1 shipped precisely because a correct mechanism was attached to an untested path — #371 added a mode guard for Dashboard tabs with a passing test and left orchestration tabs unguarded with no test at all. This PRD touches the same resolver, so every entry below is stated as *what it would prove in a real pane*.

The full table, with catalog IDs, is the artefact under review. IDs marked **new** need creating; the rest already exist.

| Catalog ID | Tier | Action | Scenario |
|---|---|---|---|
| `orchestration/lock/007` **new** | L1 | create | Table-driven over `is_orchestration_tab` × every `UiMode` × `{ToggleOrchestrationLock, other}`: the action survives **only** at `(true, UiMode::Normal)`, and every other action passes through untouched in every cell. Mechanism test — proves nothing in a real pane on its own; it is here so a later failure localises. |
| `orchestration/lock/008` **new** | L2 PTY | create | **The chord fix as the user sees it.** A real interactive bash/readline role pane, focused; send `0x05`; assert readline's `end-of-line` genuinely moved the cursor. Then `Ctrl+D`, `Ctrl+E`, and assert the lock toggled instead. RED on `main` today — the chord is swallowed and the cursor never moves. Mirrors `orchestration_007`/`tabs/orchestration/024`. |
| `orchestration/lock/001` | L1 | modify | Default-locked state now reads deck-global `UiState`, not the per-tab field. |
| `orchestration/lock/002` | L1 | modify | `Ctrl+E` toggles from command mode only; in `PaneInput` it no longer toggles. |
| `orchestration/lock/003` | L1 | **invert** | Currently pins per-tab isolation. Now asserts the opposite: toggling in one Orchestration tab changes every Orchestration tab, and a newly opened one adopts the current value. The coupling it guards is still real — rewrite, do not delete. Name changes with it. |
| `orchestration/lock/004`–`006` | L2 | verify | Existing gate and global-chord regression tests. Must stay green under deck-global state; `006`'s global-chord set now excludes `Ctrl+E` from `PaneInput`. |
| `orchestration/lock/009` **new** | L1 | create | A locked non-orchestrator pane reporting `WaitingForInput` passes keys to its PTY; the gate re-engages the moment the status clears. Orchestrator pane and unlocked deck unaffected. (Decision 4) |
| `orchestration/lock/010` **new** | L2 PTY | create | **The carve-out as the user sees it.** A real agent pane presenting a prompt, deck locked: keys reach it and the prompt is answered on screen. Per CLAUDE.md rule 4 this is the strongest candidate for a real-agent (Haiku) test rather than a stand-in, since the whole point is that a genuine agent prompt is answerable. |
| `tabs/orchestration/013`–`018` | L1 | **delete** | The 30-second inactivity timer (#373 M2), including its four review-fix stamp-site tests. |
| `tabs/orchestration/021`–`023` | L1 | **delete** | The remaining M2 tests: waiting-pane safety, tab-switch-in grace, already-selected-card stamp. |
| `tabs/orchestration/019` | L1 | **delete** | Blocked-keystroke-resets-timer (#373 M3). No timer remains to reset. |
| `orchestration/focus/001` | L2 PTY | **delete** | The PTY timer test, together with the `DOT_AGENT_DECK_INACTIVITY_TIMEOUT_SECS` seam it introduced. |
| `tabs/orchestration/010`, `012`, `020` | L1 | keep | Waiting-pane steering, all-clear edge trigger, `observe_waiting_panes` — the surviving focus core. |
| `tabs/orchestration/011` | L1 | verify | Render-loop wiring, now a two-branch chain. |
| `tabs/orchestration/025` **new** | L1 | create | Three role panes go `WaitingForInput`; focus visits them in ascending `role_pane_ids` order, advancing as each resolves, then returns to the orchestrator on the all-clear. (Decision 5, locked half) |
| `tabs/orchestration/026` **new** | L1 | create | While **unlocked**, no auto-focus branch fires — manual focus survives a waiting pane appearing *and* an all-clear. Re-locking resumes pinning, and the episode that elapsed while unlocked does **not** fire a stale all-clear move. Pins the latch-clearing rule. |
| `orchestration/focus/002` **new** | L2 PTY | create | **Rule 4 headline test.** Real binary: locked, focus on the orchestrator; a worker goes waiting and visibly pulls focus; resolving returns it; `Ctrl+D`,`Ctrl+E` unlocks and manual focus then sticks across both events. This is the feature as a user actually experiences it, and it replaces `orchestration/focus/001` in the reel-eligible slot. |

**Deliberately not claimed.** No test here proves anything about a specific agent's prompt rendering beyond `orchestration/lock/010`'s single real-agent case. `Ctrl+E` reaching the PTY is the contract; what a given program does with `0x05` is that program's business, and asserting on Claude Code's own output would couple the suite to another product for no added confidence.

## Risks

- **Changing a keybinding's mode-scoping is muscle-memory-visible.** Anyone who today unlocks from a focused pane must now press `Ctrl+D` first. Accepted deliberately — it is the identical trade #241 M1 made for `Ctrl+W` and #387 makes for `Ctrl+L`, and after this PRD all three chords behave alike, which is itself worth something. The changelog fragment must call it out; a silent scoping change reads as a bug to the person who had the habit.
- **"One value across every Orchestration tab" is a behaviour change for anyone relying on per-tab locks.** #374 shipped per-tab isolation as an explicit, tested, user-resolved decision. Accepted per decision 2; it is the point of the PRD, not a side effect.
- **The carve-out widens the lock's hole exactly where the lock was most protective.** A pane that reports `WaitingForInput` becomes fully typeable, and status is agent-reported — a mis-reported or stuck status leaves a pane unlocked with no visual cue distinguishing it from a correctly-waiting one. Mitigation: the same status already drives auto-focus, so a stuck status is *already* user-visible as focus behaving oddly, and this makes it no more silent. Worth a docs sentence so the behaviour is not surprising.
- **Deleting six stamp sites is a wide, mechanical diff.** Compiler-guided once the field is gone, so the risk is not correctness but a large diff hiding a real change. Mitigation is the milestone ordering: M1 (chord fix) and M4 (the deletion) are separate commits.
- **The latch-clearing rule is the one piece of genuinely new logic** and has no precedent to copy. `tabs/orchestration/026` exists specifically to pin it. Getting it wrong is not catastrophic — a spurious one-time focus move on re-lock — but it is exactly the class of bug #373's review found four times.
- **~~Sequencing against #387.~~ Resolved before implementation started.** Both PRDs edit the same resolver seam and both add a `scope_*` function beside the other, so this PRD wanted to land after #387. Verified at worktree-creation time that **#387 is already merged into `origin/main`** (`scope_split_stage` present in `src/ui.rs`, `ACTIVE_SPLIT_STAGE` present, per-tab `split_stage` gone from the `Tab` variants). This branch is cut from `origin/main` at `7747e6a`, so `scope_command_entry_lock` is written next to a merged `scope_split_stage` exactly as intended. No sequencing constraint remains.

## CLAUDE.md rule 12 — cross-version contract

**Does this change the TUI↔daemon contract? Expected no, to be confirmed at M6.** This PRD touches key resolution, `UiState`, `Tab`, and focus — all client-side. It adds no wire field, changes no `EventType`, alters no handler, and should touch neither `src/daemon.rs` nor `src/hook.rs`. `Tab` is not serialized to the daemon and neither the lock nor the focus state is persisted. **No `PROTOCOL_VERSION` bump expected.**

**Is a `.breaking.md` fragment owed? No.** Rule 12 defines "breaking" narrowly as a TUI↔daemon interoperability break, including a semantic break behind a stable wire. Neither applies. The user-visible behaviour changes (mode scoping, shared lock, no timer) belong in the ordinary changelog fragment; filing them as `.breaking.md` would wrongly signal a compatibility break.

**Is the cross-version manual test required?** Rule 12 triggers on daemon/protocol/**orchestration**/hooks. This PRD touches orchestration behaviour, so **yes** — run it at M6: branch TUI against the previous release's daemon with an agent under it, confirm a delegate still routes and hooks still arrive. Note #387's procedural finding, which applies directly: you cannot test a version boundary by pointing a newer TUI at an older daemon, because the build-version handshake silently restarts a mismatched daemon when no agents are running. The procedure that works is old binary serves → its own TUI opens an orchestration and **detaches** → new TUI raises the mismatch warning → decline the restart.

**Bump policy** (while `0.x`): client-side behaviour changes plus a chord fix, no contract break → **patch**.

## CLAUDE.md rule 9 — experimental flag

**Asked and answered: no flag. Recorded rather than skipped, as the rule requires.**

Rule 9 triggers on a **new** user-visible surface — a pane, field, command, tab, footer entry, or keybinding. This PRD adds none. It changes *when an existing keybinding is claimed*, *where an existing value is stored*, and *removes* a behaviour. There is no new surface to gate and no `show_<feature>()` wrapper to add to `src/features.rs`.

The question was reconsidered once during planning, when the lock's reach was briefly read as extending to every tab type — that *would* have been a large enough default change to argue for a flag. Decision 3 settled the reach as unchanged, and the case went with it. Matches #374's own resolution for the same surface (no flag, visible by default) and #387/#371/#336/#333/#341 before it.

One asymmetry worth naming: gating the *chord fix* behind a flag would be actively wrong for the same reason #387 gives — shipping a build that still swallows `Ctrl+E` by default is the exact behaviour the PRD exists to remove, and a bug fix behind an opt-in flag is not a bug fix.

## Amendments owed to #373 and #374

Both PRDs are marked complete and both will assert behaviour that no longer exists. Leaving them stale is how a future reader relitigates a settled decision from a document that looks authoritative. At M6:

- **`prds/374-command-entry-lock.md`** — annotate the two reversed decisions (per-tab state; global-chord/any-mode resolution) as **superseded by #393**, in place, without rewriting the original reasoning. Its Risks section's experimental-flag entry stays as-is; it was correct for its scope.
- **`prds/373-auto-return-focus.md`** — annotate M2 and M3 as **removed by #393**, and update the "Known limitation (accepted, deferred)" residual-race entry to record that #393 narrowed the window but did not close it. M1 stays as shipped, live behaviour.

## Open Questions

1. ~~**The GitHub issue is not filed.**~~ **Resolved: filed as [#393](https://github.com/vfarcic/dot-agent-deck/issues/393)** on `vfarcic/dot-agent-deck`. It did *not* take the assumed `#388` — upstream had issues in flight and it landed five numbers later. The PRD file, branch, worktree and all internal references were renumbered to match. Given #373 and #374 were both closed upstream as not-planned, this one may go the same way; the fork's record has a stable reference either way.
2. **Should `UiState::last_pane_keystroke_at` be deleted?** It exists to serve the timer being removed, but #373 describes it as pre-existing. The implementer must grep for other consumers at M4 and leave it in place if any exist, rather than assuming the timer was its only reader.
4. ~~**Does decision 4's carve-out survive the rationale?**~~ **RESOLVED: yes, decision 4 stands.** Raised the moment the "Why the lock exists at all" rationale was written down, because the two read as inconsistent: the rationale says the reflex to answer an agent's prompt on the spot is what the lock should interrupt, while decision 4 makes that prompt answerable with no pause. Two alternatives were offered and declined — dropping the carve-out entirely (maximally faithful, but reopens Problem 3), and routing worker prompts through the orchestrator (most faithful to "one workflow, one coordinator", but a new mechanism rather than a gate tweak, and its own PRD if ever wanted).

   The user chose to **keep the carve-out on the narrow reading**: the lock's subject is the *unsolicited* interruption, and a solicited answer to an already-blocked agent is not one. The rationale section above now states that boundary explicitly rather than leaving it implied. M3 and tests `orchestration/lock/009`/`010` proceed exactly as planned in the Test Plan.

5. **Does the carve-out need a visual cue?** A pane that is temporarily typeable because it is `WaitingForInput` looks identical to one that is locked. A border or status-chip difference would make it obvious, but it is a new visible surface — which would re-trigger rule 9. Deliberately left out of scope; raised so it can be a follow-up rather than a surprise.
