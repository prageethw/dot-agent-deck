# PRD #373: Auto-return focus to the orchestrator pane (Orchestration tabs only)

**Status**: Not Started
**Priority**: Medium
**Created**: 2026-08-04
**GitHub Issue**: [#373](https://github.com/vfarcic/dot-agent-deck/issues/373) (closed upstream as not-planned; this fork continues the work independently)
**Related**: Split from #361, alongside #371/#372/#374 (siblings). Structurally depends on [#374](https://github.com/vfarcic/dot-agent-deck/issues/374) (command-entry lock) for its combined interaction test — see Technical Approach and Open Questions. `src/tab.rs` (`Tab::Orchestration`, `role_pane_ids`, `focused_role_pane_id`, `start_role_index`, `auto_focus_waiting_pane` — fork-only), `src/ui.rs` (`UiState::last_pane_keystroke_at`, `Action::ForwardToPane`, per-frame call site around `src/ui.rs:9750-9758`)

## Problem Statement

Confirmed with the user: **scoped to Orchestration tabs only**, not Dashboard or Mode tabs. Two related behaviors:

1. When every role pane in the active Orchestration tab that was `WaitingForInput` has resolved (none remain waiting), focus should move to the orchestrator pane specifically.
2. If the human manually moves focus to a different pane in that tab, and 30 seconds pass with no further human activity, focus automatically snaps back to the orchestrator pane.

### Relationship to existing auto-focus behavior

`src/tab.rs`'s `Tab::Orchestration` already carries `role_pane_ids: Vec<String>`, `focused_role_pane_id: Option<String>`, and `start_role_index: usize` — "the orchestrator" is `role_pane_ids[start_role_index]` (`src/tab.rs:96-117`); `focused_role_pane_id == None` already means "default to the start/orchestrator role pane on switch-in" (`src/tab.rs:104-106`), an existing precedent for orchestrator-targeted focus this PRD extends to a *new trigger* rather than only tab switch-in.

The fork-only (not upstream) `TabManager::auto_focus_waiting_pane` (`src/tab.rs:409-432`, wired once per render frame at `src/ui.rs:9756-9758`) is a **related but distinct** behavior: it fires on the *waiting* condition (steers focus to the lowest-`role_pane_ids`-order pane that currently *is* `WaitingForInput`), continuously re-evaluated every frame from the live `pane_status_for_tabs` join (`build_pane_status(&snapshot)`, same join PRD #333's tab-label coloring already reads). This PRD's first behavior fires on the opposite, *all-clear* condition and always targets the orchestrator role specifically, not "whichever pane needs attention." **The two must coexist on this fork**: `auto_focus_waiting_pane` continues to steer focus toward a newly-waiting pane while one exists; this PRD's all-clear behavior takes over the instant the last one resolves. They are not in tension as event-driven states — a pane cannot be simultaneously "some pane is waiting" and "no pane is waiting" — but both need to keep being called from the same per-frame site (`src/ui.rs:9750-9758`), with the all-clear check naturally gated to run only when `auto_focus_waiting_pane` finds nothing to steer toward.

Because both behaviors evaluate every frame from a live status snapshot, the all-clear focus move must be **edge-triggered** (fires once, on the frame where "some pane was waiting" transitions to "none are"), not level-triggered (which would fight the second behavior below — if focus were pinned back to the orchestrator on every frame merely because nothing is currently waiting, the human could never manually look at another pane at all, defeating the point of the 30-second grace window). This requires a small piece of new per-tab state — e.g. `had_waiting_pane: bool`, refreshed each frame alongside the existing waiting-pane check — that the current `auto_focus_waiting_pane` doesn't need (it's naturally edge-driven already: it only ever moves focus toward a *specific new* waiting pane and no-ops once already there, per its own doc comment).

## Solution Overview — the 30-second inactivity timer

**Activity definition, resolved rather than left vague**: the codebase already tracks exactly the right signal for this. `UiState::last_pane_keystroke_at: Option<std::time::Instant>` (`src/ui.rs:1725`) is updated at the single choke point where a keystroke is actually forwarded into a focused pane's PTY — `Action::ForwardToPane`'s dispatch arm (`src/ui.rs:8243-8267`) and the paste-handling path (`src/ui.rs:11200-11225`) — and nowhere else (it is not touched by agent/hook activity, only by human keystrokes actually reaching a pane). Since only one pane can hold focus at a time and only the active tab's focused pane can receive forwarded keystrokes, this single, already-global timestamp is precisely "the human is actively typing into the currently-focused pane in the active tab." Proposed definition: **"activity" = a forwarded keystroke into the focused pane's PTY**, i.e. an update to `ui.last_pane_keystroke_at`. The 30-second timer starts when focus lands on a non-orchestrator pane in an Orchestration tab and resets on every such update; if it elapses with no update, focus snaps back to the orchestrator pane (`role_pane_ids[start_role_index]`).

**Decision**: this activity definition interacts with #374's command-entry lock. If #374's lock is engaged (its default state) while focused on a non-orchestrator pane, keystrokes typed there are — by #374's own design — dropped *before* they reach `Action::ForwardToPane`, so `last_pane_keystroke_at` never updates no matter how much the human fiddles with the locked pane. Resolved by the user: a blocked/attempted keystroke to a locked pane **does** count as activity — it resets the 30-second inactivity timer, since the human is still clearly engaged with that pane even though nothing was forwarded to the PTY. This means the reset point cannot be `last_pane_keystroke_at` alone (that timestamp only updates on an actually-forwarded keystroke); the gate inside #374's PTY-forward fallback needs to also record an attempt — e.g. update a `last_pane_activity_at`-style timestamp (or reuse/extend `last_pane_keystroke_at` itself to be set on both the forwarded and the blocked-drop path) before returning `Action::Continue` for the dropped key.

## Scope

**In Scope**: `TabManager` gains the all-clear focus-move (new method alongside `auto_focus_waiting_pane`, called from the same per-frame site, `src/ui.rs:9750-9758`) plus the edge-trigger state it needs; a 30-second inactivity timer keyed off pane-forwarded and pane-blocked keystroke activity (see Decision above), scoped to the active Orchestration tab, that refocuses the orchestrator pane; unit tests for the edge-trigger (no repeated snap-back once already on the orchestrator with nothing waiting) and the timer (elapses → snaps back; resets on both a forwarded keystroke and a blocked keystroke on a locked pane).

**Out of Scope**: Dashboard and Mode tabs (per user confirmation); changing `auto_focus_waiting_pane` itself.

## Technical Approach

The edge-trigger and timer mechanics are described in Solution Overview above. One dependency worth stating precisely for sequencing this branch's work:

**Dependency on #374 (command-entry lock)**: the "blocked keystroke counts as activity" half of this PRD's Decision only has an observable effect once #374's lock exists — before that lands, every keystroke on a focused non-orchestrator pane is always forwarded (there is nothing to block), so the timer's reset behavior collapses to the single `last_pane_keystroke_at`-only case. This PRD's own scope (the edge-trigger, the timer, the orchestrator-targeted refocus) does not require #374 to be implemented first and can land independently. But the **combined interaction test** — verifying a blocked keystroke on a locked pane resets the timer — is structurally blocked on #374's lock existing, since there is no locked pane to attempt a blocked keystroke against without it. That test should be **deferred until #374 lands**, tracked as a follow-up on this PRD rather than blocking M1/M2 below.

## Success Criteria

- The moment the last `WaitingForInput` role pane in the active Orchestration tab clears, focus moves to that tab's orchestrator pane exactly once (not repeatedly every frame).
- Manually focusing a non-orchestrator pane in an Orchestration tab, then leaving it untouched (per the resolved activity definition) for 30 seconds, snaps focus back to the orchestrator pane.
- No effect on Dashboard or Mode tabs, and no fighting between this behavior and `auto_focus_waiting_pane` (fork-only) — verified by a test exercising both in sequence (a pane starts waiting → gets steered to → resolves → all-clear steers back to orchestrator).

## Milestones

- [ ] **M1 — All-clear focus move.** New edge-triggered `TabManager` method alongside `auto_focus_waiting_pane`, wired at the same per-frame site (`src/ui.rs:9750-9758`); unit tests for the edge-trigger (fires once, not every frame) and coexistence with `auto_focus_waiting_pane`.
- [ ] **M2 — 30-second inactivity snap-back.** Timer scoped to the active Orchestration tab, reset by a forwarded keystroke (per the Decision above); unit test for elapse → snap-back and reset-on-activity.
- [ ] **M3 — Blocked-keystroke reset (deferred until #374 lands).** Extend the timer reset to also fire on a blocked keystroke attempt against a locked pane, once #374's lock exists to attempt one against; combined-interaction test per Technical Approach.
- [ ] **M4 — Docs and changelog.** `docs/keyboard-shortcuts.md` updated if any new binding is introduced (none expected — this is timer/focus behavior, not a new chord); changelog fragment.

## Risks

- **Cross-item coupling with #374.** This item's inactivity timer and #374's lock share the same keystroke-forwarding choke point (`Action::ForwardToPane`); the Decision above (a blocked keystroke on a locked pane still resets the timer) means the gate inside #374's PTY-forward fallback must record that attempt before dropping the key, not just short-circuit. M3 exists specifically to catch this at the seam once #374 lands, rather than assuming it in isolation.
- **Experimental-flag candidate.** Per CLAUDE.md rule 9, candidate for **yes** — it's a new, potentially surprising behavior (focus moves out from under the user without a keypress) scoped to Orchestration tabs. Unlike a keybinding the user presses on purpose, this one *acts on its own* via a timer, which is exactly the kind of thing worth letting users opt into first. Recommend flagging it; to be confirmed at `/prd-start` for this branch.

## Open Questions

1. **Experimental flag gating** — to be settled at `/prd-start` for this branch (see Risks above for judgment).
2. **M3 sequencing** — this PRD's M1/M2 can land and ship independently of #374; M3 (the blocked-keystroke reset and its combined test) is explicitly deferred until #374 lands. Track #374's status before scheduling M3.

Resolved (previously open, decided by the user before implementation started):

- **Does "activity" include blocked keystrokes on a locked pane?** Resolved: yes — a blocked/attempted keystroke to a locked pane counts as activity and resets the 30-second timer (see Solution Overview's Decision).
