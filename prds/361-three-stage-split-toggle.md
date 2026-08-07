# PRD #361: Orchestration pane UX — split toggle, input-status clearing, auto-return focus, and command-entry lock

**Status**: Superseded — retained for historical reference
**Priority**: Medium
**Created**: 2026-08-03
**Updated**: 2026-08-04 — expanded from the original three-stage split-toggle scope to cover three more related Orchestration-tab UX items (status-clearing bug, auto-return focus, command-entry lock), all four living in this one PRD.
**GitHub Issue**: [#361](https://github.com/vfarcic/dot-agent-deck/issues/361)
**Related**: `src/ui.rs` (`DASHBOARD_LEFT_PERCENT`/`DASHBOARD_PANES_PERCENT`, `ORCHESTRATION_LEFT_PERCENT`/`ORCHESTRATION_PANES_PERCENT`, `split_cards_area`, `compute_frame_layout`, `handle_key_event`, `handle_pane_input_key`, `key_action_for_mode`, `global_action_for_mode`, `Action::ForwardToPane`), `src/tab.rs` (`Tab::Dashboard`, `Tab::Orchestration`, `auto_focus_waiting_pane` — fork-only), `src/keybindings.rs` (`Action`/`ActionSpec`), `src/state.rs` (`AppState::apply_event`'s `SessionStatus` transitions), `src/hook.rs` (`map_event_type`)

> **Superseded.** This PRD was never implemented under its own number: the four items below were split out into PRDs #371, #372 and #374, all three of which have shipped. It is kept here as the historical record of how the scope was framed before that split — read it for context on the original reasoning, not as a description of work still outstanding or of how the shipped behavior actually works.

## Overview

This PRD covers four related Orchestration/Dashboard-tab UX improvements, developed together because they all touch the same pane-focus/pane-status/keystroke-routing seams:

1. **Item 1** (both tabs is N/A — this one is Dashboard+Orchestration, wherever `SessionStatus` renders) — a genuine bug fix: a pane's "Needs Input" status can outlive the actual permission prompt, staying stuck while the pane is visibly back to working.
2. **Item 2** (Orchestration tabs only) — auto-return focus to the orchestrator pane, both on an "all clear" transition and after 30s of human inactivity following a manual focus move.
3. **Item 3** (Orchestration tabs only) — lock direct keystroke entry to the orchestrator pane by default, with `Ctrl+e` toggling the lock.
4. **Item 4** — the original three-stage `Ctrl+l` split-toggle (Default / Narrow / Hidden), covering both Dashboard and Orchestration tabs. Kept below exactly as originally written.

Items 2 and 3 are new, closely-related Orchestration-tab behaviors and are written up together where they intersect (the human-activity signal, the pane-focus/keystroke-routing code path). Item 1 is a bug fix independent of the other three. Item 4 (the split toggle) is unchanged from the original PRD content.

---

## Item 1 — Fix: stale "Needs Input" status after a permission prompt is answered

### Problem Statement

Reported symptom: a pane's dashboard/orchestration-sidebar status stays on "Needs Input" (`SessionStatus::WaitingForInput`, rendered as the bold "Needs Input" label — `src/ui.rs:15987`) even though the pane is visibly working again (the agent's own PTY output shows it running).

Root cause, confirmed against git history rather than guessed:

`SessionStatus` is purely hook-event-driven (`AppState::apply_event`, `src/state.rs:3526-3568`) — there is no PTY-content pattern-matching path that independently detects "the input form went away"; the deck cannot see the agent's own terminal UI, only the hook events the agent process emits. So the *only* way `WaitingForInput` clears is one of these event-type arms:

- `EventType::ToolStart` (`src/state.rs:3535-3543`): `if session.status != SessionStatus::WaitingForInput { session.status = Working }` — i.e. **while status is `WaitingForInput`, a `ToolStart` never clears it**, it only refreshes `active_tool`.
- `EventType::ToolEnd` (`src/state.rs:3544-3550`): unconditionally sets `Thinking` if status was `WaitingForInput`.
- `EventType::Thinking` (from a `UserPromptSubmit` hook, `src/hook.rs:100`) unconditionally sets `Thinking` (`src/state.rs:3531-3534`).

For a plain idle/notification-driven wait (Claude Code's `Notification` hook, mapped to `WaitingForInput` at `src/hook.rs:103`), the human types a reply and hits enter, `UserPromptSubmit` fires, and `Thinking`'s unconditional arm clears the status immediately — **this path is not broken**.

The broken path is the **permission-approval** flow: a `PermissionRequest` hook sets `WaitingForInput` (`src/state.rs:3551-3553`); the human answers via `Action::SendPermissionResponse` (`src/ui.rs:7490-7509`), which writes a raw `"y"`/`"n"` keystroke straight into the pane's PTY (there is no separate closeable "form" widget to detect — the prompt lives inside the agent's own rendered terminal content) and does **not** touch `session.status` itself. The next hook event Claude Code sends is the approved tool's own `PreToolUse` → `ToolStart` — and that is exactly the event the `ToolStart` guard above refuses to use to clear `WaitingForInput`. So the status stays "Needs Input" for the entire duration of the now-running, human-approved tool, only flipping to `Thinking` once *some* `ToolEnd` (not necessarily the same tool) fires.

That guard is not a bug in isolation — it exists on purpose, added in `4d31103` ("fix: preserve WaitingForInput status when concurrent subagent fires ToolStart", #86, 2026-05-12) to stop a *different*, real regression: a concurrent subagent's unrelated `PreToolUse` flipping a genuine, still-outstanding "Needs Input" card back to "Working" before the human had answered. The problem is what it replaced: a one-day-older `pending_permissions` queue (added `7ea3a11`/#43, refined `15bef44`, `87f3708`, all 2026-04-05) that *did* discriminate "this ToolStart is the approved tool" from "this is some other concurrent tool" — matched first by `tool_use_id`, then by tool name after `PermissionRequest`'s synthetic id turned out not to match the real `tool_use_id` on `ToolStart`. That whole queue was deleted the very next day in `29384bb` ("fix: remove permission queue to fix stuck 'Needs Input' status", 2026-04-06) because the queue itself could get stuck non-empty. `4d31103` then re-added the *preserve* half of the guard five weeks later without restoring any discriminator — so today's code has the coarse "never clear on ToolStart while waiting" behavior with none of the matching logic that used to make it selective.

### Solution Overview

Fix scoped narrowly to the permission-approval path (the idle/notification path already self-heals via `UserPromptSubmit → Thinking`). Reintroduce a **single-slot** marker, not the old FIFO queue — Claude Code only ever shows one outstanding permission prompt on a given pane at a time (the turn blocks synchronously on it), so a queue's ordering semantics were never actually needed; the previous implementation's complexity (matching by id, then by name, across a multi-entry queue) is what made it fragile enough to be reverted twice. Concretely: record the tool name from the triggering `PermissionRequest`/`WaitingForInput` event in a single `Option<String>` on the session; on `ToolStart`, clear `WaitingForInput` when either (a) there's no pending marker (a plain notification wait — any tool starting must mean the human's reply set it off), or (b) the incoming event's tool name matches the marker. Clear the marker itself whenever `WaitingForInput` clears by any path.

### Scope

**In Scope**: the `ToolStart`/`ToolEnd`/`PermissionRequest` arms in `AppState::apply_event` (`src/state.rs:3526-3568`); a new single-slot pending-permission field on the session (not a queue); unit tests covering approve → tool starts → status clears, and the concurrent-subagent-preserve case `4d31103` was written for (still must not regress).

**Out of Scope**: the plain notification/idle-wait path (already correct); any change to how permission responses are sent to the pane (`Action::SendPermissionResponse`, `src/ui.rs:7490-7509`) — this is a status-tracking fix only, not a change to the approve/deny mechanism.

### Technical Approach

See Solution Overview above for the mechanism. One genuine ambiguity that this exact code seam has flip-flopped on twice already (`7ea3a11`/`15bef44`/`87f3708` → `29384bb` → `4d31103`), so it is called out rather than silently re-guessed a third time:

**Open Question (item 1)**: is name-matching (this PRD's proposed single-slot approach) actually reliable enough this time, or does Claude Code's hook payload need to be checked directly for a `tool_use_id` (or equivalent) that *does* survive from `PermissionRequest` to the matching `ToolStart` — which `87f3708`'s commit message says was tried and found not to match ("PermissionRequest uses synthetic IDs that differ from the real tool_use_id on ToolStart")? If a reliable id-based match is available today (hook payload formats may have changed since April), prefer it over name-matching, which breaks on two identically-named concurrent tool calls. Resolve this by inspecting a live `PermissionRequest`→`ToolStart` hook payload pair before implementation, not by assumption.

### Success Criteria

- Approving a permission prompt clears `WaitingForInput` on the *next* `ToolStart` for that tool, not on some later, unrelated `ToolEnd`.
- The `4d31103`/#86 regression (a concurrent subagent's `ToolStart` prematurely clearing a genuinely-still-outstanding permission prompt) does not reappear — covered by a regression test mirroring that commit's scenario.
- The plain notification/idle-wait path is untouched and continues to clear via `UserPromptSubmit → Thinking`.

---

## Item 2 — Auto-return focus to the orchestrator pane (Orchestration tabs only)

### Problem Statement

Confirmed with the user: **scoped to Orchestration tabs only**, not Dashboard or Mode tabs. Two related behaviors:

1. When every role pane in the active Orchestration tab that was `WaitingForInput` has resolved (none remain waiting), focus should move to the orchestrator pane specifically.
2. If the human manually moves focus to a different pane in that tab, and 30 seconds pass with no further human activity, focus automatically snaps back to the orchestrator pane.

### Relationship to existing auto-focus behavior

`src/tab.rs`'s `Tab::Orchestration` already carries `role_pane_ids: Vec<String>`, `focused_role_pane_id: Option<String>`, and `start_role_index: usize` — "the orchestrator" is `role_pane_ids[start_role_index]` (`src/tab.rs:96-117`); `focused_role_pane_id == None` already means "default to the start/orchestrator role pane on switch-in" (`src/tab.rs:104-106`), an existing precedent for orchestrator-targeted focus this PRD's item 2 extends to a *new trigger* rather than only tab switch-in.

The fork-only (not upstream) `TabManager::auto_focus_waiting_pane` (`src/tab.rs:409-432`, wired once per render frame at `src/ui.rs:9756-9758`) is a **related but distinct** behavior: it fires on the *waiting* condition (steers focus to the lowest-`role_pane_ids`-order pane that currently *is* `WaitingForInput`), continuously re-evaluated every frame from the live `pane_status_for_tabs` join (`build_pane_status(&snapshot)`, same join PRD #333's tab-label coloring already reads). Item 2's first behavior fires on the opposite, *all-clear* condition and always targets the orchestrator role specifically, not "whichever pane needs attention." **The two must coexist on this fork**: `auto_focus_waiting_pane` continues to steer focus toward a newly-waiting pane while one exists; item 2's all-clear behavior takes over the instant the last one resolves. They are not in tension as event-driven states — a pane cannot be simultaneously "some pane is waiting" and "no pane is waiting" — but both need to keep being called from the same per-frame site (`src/ui.rs:9750-9758`), with the all-clear check naturally gated to run only when `auto_focus_waiting_pane` finds nothing to steer toward.

Because both behaviors evaluate every frame from a live status snapshot, the all-clear focus move must be **edge-triggered** (fires once, on the frame where "some pane was waiting" transitions to "none are"), not level-triggered (which would fight the second behavior below — if focus were pinned back to the orchestrator on every frame merely because nothing is currently waiting, the human could never manually look at another pane at all, defeating the point of the 30-second grace window). This requires a small piece of new per-tab state — e.g. `had_waiting_pane: bool`, refreshed each frame alongside the existing waiting-pane check — that the current `auto_focus_waiting_pane` doesn't need (it's naturally edge-driven already: it only ever moves focus toward a *specific new* waiting pane and no-ops once already there, per its own doc comment).

### Solution Overview — the 30-second inactivity timer

**Activity definition, resolved rather than left vague**: the codebase already tracks exactly the right signal for this. `UiState::last_pane_keystroke_at: Option<std::time::Instant>` (`src/ui.rs:1725`) is updated at the single choke point where a keystroke is actually forwarded into a focused pane's PTY — `Action::ForwardToPane`'s dispatch arm (`src/ui.rs:8243-8267`) and the paste-handling path (`src/ui.rs:11200-11225`) — and nowhere else (it is not touched by agent/hook activity, only by human keystrokes actually reaching a pane). Since only one pane can hold focus at a time and only the active tab's focused pane can receive forwarded keystrokes, this single, already-global timestamp is precisely "the human is actively typing into the currently-focused pane in the active tab." Proposed definition: **"activity" = a forwarded keystroke into the focused pane's PTY**, i.e. an update to `ui.last_pane_keystroke_at`. The 30-second timer starts when focus lands on a non-orchestrator pane in an Orchestration tab and resets on every such update; if it elapses with no update, focus snaps back to the orchestrator pane (`role_pane_ids[start_role_index]`).

**Open Question (item 2) — genuinely ambiguous, flagged rather than guessed**: this activity definition interacts with item 3's command-entry lock. If item 3's lock is engaged (its default state) while focused on a non-orchestrator pane, keystrokes typed there are — by item 3's own design — dropped *before* they reach `Action::ForwardToPane`, so `last_pane_keystroke_at` never updates no matter how much the human fiddles with the locked pane, and the 30-second timer would elapse almost immediately regardless of genuine attention. Two readings are both defensible: (a) that's fine — there's nothing productive to do on a locked pane without unlocking it first, so a fast snap-back is arguably correct; or (b) the timer should also reset on *attempted* (blocked) keystrokes, since the human is still clearly engaged with that pane even though nothing was forwarded. This needs a product decision, not an engineering guess — surfaced here rather than assumed either way.

### Scope

**In Scope**: `TabManager` gains the all-clear focus-move (new method alongside `auto_focus_waiting_pane`, called from the same per-frame site, `src/ui.rs:9750-9758`) plus the edge-trigger state it needs; a 30-second inactivity timer keyed off `ui.last_pane_keystroke_at`, scoped to the active Orchestration tab, that refocuses the orchestrator pane; unit tests for the edge-trigger (no repeated snap-back once already on the orchestrator with nothing waiting) and the timer (elapses → snaps back; resets on activity per whichever answer the Open Question above resolves to).

**Out of Scope**: Dashboard and Mode tabs (per user confirmation); changing `auto_focus_waiting_pane` itself.

### Success Criteria

- The moment the last `WaitingForInput` role pane in the active Orchestration tab clears, focus moves to that tab's orchestrator pane exactly once (not repeatedly every frame).
- Manually focusing a non-orchestrator pane in an Orchestration tab, then leaving it untouched (per the resolved activity definition) for 30 seconds, snaps focus back to the orchestrator pane.
- No effect on Dashboard or Mode tabs, and no fighting between this behavior and `auto_focus_waiting_pane` (fork-only) — verified by a test exercising both in sequence (a pane starts waiting → gets steered to → resolves → all-clear steers back to orchestrator).

---

## Item 3 — Lock command entry to the orchestrator pane, `Ctrl+e` to unlock (Orchestration tabs only)

### Problem Statement

Confirmed with the user: **scoped to Orchestration tabs only**. By default, keystrokes typed by the human should not reach any non-orchestrator pane's PTY within an Orchestration tab — only the orchestrator pane accepts direct input. `Ctrl+e` toggles the lock; it starts **locked** (direct command entry to worker panes disabled by default).

### Investigation: `Ctrl+e` availability

Verified free: `src/keybindings.rs`'s `ACTIONS` table (`src/keybindings.rs:109` onward) has no entry using `e`, and a direct search of `src/ui.rs` for any hardcoded (non-`Action`-registry) `KeyModifiers::CONTROL` + `Char('e')` check found none. `Ctrl+e` is available as the default chord, same verification method the existing split-toggle item used for `Ctrl+l`.

### Investigation: where PTY keystroke forwarding lives

Confirmed there is exactly one choke point for all pane-forwarded keystrokes, in any tab type: `UiMode::PaneInput` is the mode a focused pane puts the UI into; `handle_key_event` (`src/ui.rs:8706`) resolves global chords first (`global_action_for_mode`, `src/ui.rs:6416-6424`, itself calling `global_action` — the small set of always-available `Ctrl+`-chord `Action`s: dashboard, new_pane, close_pane, toggle_layout, and now cycle_split_stage), and only if nothing claimed the key does `UiMode::PaneInput` fall through to `handle_pane_input_key` (`src/ui.rs:4178-4184`), which converts the key to raw bytes and returns `Action::ForwardToPane(bytes)`; that action is dispatched at `src/ui.rs:8243-8267`, which resolves the focused pane id via `EmbeddedPaneController::focused_pane_id()` and calls `write_raw_bytes`. `key_action_for_mode` (`src/ui.rs:6434-6445`) documents this exact two-seam composition (global chords, then PTY-forward fallback) and is the seam production and tests already share.

One consequence worth stating precisely because the task asked what should still work: plain tab-cycling keys (`h`/`l`/Tab/Shift-Tab/arrows, `cycle_tab_action`, `src/ui.rs:6451-6459`) are **only** resolved in `UiMode::Normal` (`src/ui.rs:8777`) — they are *not* checked at all while a pane has focus (`UiMode::PaneInput`), today, for any tab type, locked or not. So "does tab-switching still work while a worker pane is focused" is already "no" pre-existing behavior for every pane in the app, not something item 3 changes. What *is* already global regardless of focus is the small `Ctrl+`-chord set resolved by `global_action_for_mode` before the PTY-forward fallback is ever reached — item 3's gate sits entirely inside that fallback and never touches `global_action_for_mode`, so those chords (dashboard, new pane, close pane, toggle layout, split-cycle, and the new unlock chord itself) are unaffected by the lock by construction.

### Solution Overview

Gate the PTY-forward fallback itself: when the active tab is `Tab::Orchestration`, the focused pane is *not* `role_pane_ids[start_role_index]` (the orchestrator), and the tab's lock is engaged, `handle_pane_input_key`'s caller returns `Action::Continue` instead of `Action::ForwardToPane(bytes)` — i.e. the keystroke is silently dropped before it reaches the PTY, exactly like `key_action_for_mode`'s existing "nothing to forward" case. A brief status-message ("Pane locked — Ctrl+e to unlock") on the drop follows this codebase's existing no-op-with-feedback convention (e.g. `RequestConfigGen`'s "No active agent session to send prompt to.", `src/ui.rs:7483-7488`).

**Open Question (item 3)**: is the lock per-Orchestration-tab state (a new field on `Tab::Orchestration`, following the `split_stage`/`focused_role_pane_id` precedent already on that variant) or one global toggle shared across every open Orchestration tab? The task background didn't specify either way, and both are defensible — per-tab matches the "each orchestration tab is independent" pattern everywhere else on `Tab::Orchestration`, but a single global lock is arguably simpler to reason about for a human juggling several orchestration tabs at once ("is command entry locked" as one fact, not N facts). Recommend per-tab for consistency with the rest of this variant's state, but this is a real product choice, not decided here.

### Scope

**In Scope**: new `Action` (e.g. `ToggleOrchestrationLock`), `Section::Global`, default chord `Ctrl+e`, in `src/keybindings.rs`'s `ACTIONS` table; the lock-state field (shape per the Open Question above); the gate inside the PTY-forward fallback (`handle_pane_input_key`'s call site needs the tab/lock context threaded in — it currently takes only the raw `KeyEvent`, `src/ui.rs:4178`); a status message on a dropped keystroke; unit tests covering: locked pane drops keystrokes, orchestrator pane never gates regardless of lock state, `Ctrl+e` toggles the lock, and global chords (e.g. `Ctrl+d`) still work while a non-orchestrator pane is focused and locked.

**Out of Scope**: Dashboard and Mode tabs; changing `cycle_tab_action`'s existing Normal-mode-only scoping (unrelated pre-existing behavior, not part of this item).

### Success Criteria

- By default, typing while focused on a non-orchestrator pane in an Orchestration tab does not reach that pane's PTY.
- Typing while focused on the orchestrator pane always reaches its PTY, lock state notwithstanding.
- `Ctrl+e` toggles the lock; while unlocked, non-orchestrator panes accept direct input normally.
- Global chords (`Ctrl+d`, `Ctrl+n`, `Ctrl+w`, `Ctrl+t`, the split-cycle chord, `Ctrl+e` itself) keep working regardless of focus or lock state.

---

## Item 4 — Three-stage `Ctrl+l` pane-split toggle (Default / Narrow / Hidden)

*(Original PRD content, unchanged.)*

### Problem Statement

Both the Dashboard tab and Orchestration tabs render a horizontal split between a left sidebar (deck cards / role list) and a right pane column (agent terminals), and both are fixed-ratio constants today:

- Dashboard: `DASHBOARD_LEFT_PERCENT = 33` / `DASHBOARD_PANES_PERCENT = 67` (`src/ui.rs:1948-1949`)
- Orchestration: `ORCHESTRATION_LEFT_PERCENT = 34` / `ORCHESTRATION_PANES_PERCENT = 66` (`src/ui.rs:1950-1951`)

Both tabs already route through the same `split_cards_area(main_area, pane_ids, left_percent, panes_percent)` helper (`src/ui.rs:11362-11376`), called from `compute_frame_layout`'s `ActiveTabView::Dashboard` and `ActiveTabView::Orchestration` arms (`src/ui.rs:11289-11330`) — so the two tabs are structurally identical on this seam, differing only in which two constants they pass in.

On a laptop-sized terminal, roughly a third of the width goes to a sidebar that is often just a short list of cards or roles, leaving the working pane column narrower than it could be. There is no way to reclaim that width, or to go briefly full-width on the pane column (e.g. to read a wide log or diff) without permanently losing the sidebar, short of editing the source constant and rebuilding.

### Solution Overview

Add a keybinding action (default `Ctrl+l`, remappable through the existing `keybindings.rs` `Action`/`ActionSpec` system — `Ctrl+l` is unbound today, verified against the `ACTIONS` table in `src/keybindings.rs`) that cycles a tab's sidebar/pane-column split through **three** stages, looping back to the first:

1. **Default** — today's fixed ratio for that tab type (33/67 Dashboard, 34/66 Orchestration).
2. **Narrow** — 25/75 (sidebar shrinks to roughly a quarter width).
3. **Hidden** — sidebar collapsed to 0 width; the pane column takes the full tab area.

Pressing the chord again returns to Default, and the cycle repeats. State is **per-tab**: cycling one tab's stage never affects another open tab's stage — including tabs of the other type (toggling a Dashboard tab's stage doesn't move an open Orchestration tab, and vice versa).

**Both tab types are in scope from the start**, sharing one `Action` and one stage-cycle resolver function — investigation (below) found the two tabs' layouts are already structurally identical on this seam, so this is a clean symmetric extension, not two separate features bolted together.

No persistence across restarts for v1 (every tab resets to Default on relaunch) — see Open Questions for why this isn't proposed as trivial.

### Scope

#### In Scope

- One new `Action` (e.g. `CycleSplitStage`), section `Global`, default chord `Ctrl+l`, registered in `src/keybindings.rs`'s `ACTIONS` table.
- A `SplitStage` enum (`Default`, `Narrow`, `Hidden`) and a pure "next stage" resolver function (`Default → Narrow → Hidden → Default → …`), unit-tested independently of rendering.
- Per-tab state: a `split_stage: SplitStage` field added to both `Tab::Dashboard` and `Tab::Orchestration` in `src/tab.rs` (alongside their existing per-tab fields, e.g. `selected_session_id`, `role_pane_ids`), defaulting to `SplitStage::Default`. Threaded through to the corresponding `ActiveTabView::Dashboard`/`ActiveTabView::Orchestration` variants in `src/ui.rs` for the layout pass to read.
- The two `split_cards_area` call sites in `compute_frame_layout` (`src/ui.rs:11289-11330`) resolve the active stage's percentages instead of the fixed constants directly.
- L1 snapshot coverage pinning all three stages' geometry, for both an Orchestration tab and a Dashboard tab.
- L2 (PTY/vt100) coverage driving the chord through the full 3-stage cycle on both tab types, and asserting cross-tab and cross-tab-type isolation.
- `docs/keyboard-shortcuts.md` updated with the new binding and its 3-stage, both-tab-types scope; changelog fragment.

#### Out of Scope

- More than three stages, or a user-configurable ratio for the Narrow stage — this is a fixed 3-stage cycle, matching the scope of the original request.
- Persisting the toggled stage across restarts (see Open Questions).
- Mode tabs — their 50/50 agent/side-pane split (`src/ui.rs:11262-11275`) is a different layout shape (no sidebar/pane-column split, no `split_cards_area` call) and is not part of this toggle.
- Changing the Default ratios themselves (33/67, 34/66) — those stay exactly as they are today; this only adds two additional stages reachable by cycling.

### Technical Approach

#### Dashboard/Orchestration parity — investigated, confirmed symmetric

The task background asked whether Dashboard tabs have an analogous split at all, or whether the layout there is structurally different. Verified directly against `upstream/main`: it does, and it isn't. `DASHBOARD_LEFT_PERCENT`/`DASHBOARD_PANES_PERCENT` and `ORCHESTRATION_LEFT_PERCENT`/`ORCHESTRATION_PANES_PERCENT` are defined together (`src/ui.rs:1948-1951`) specifically so a shared helper (`split_cards_area`) and shared per-tab-type dims helpers (`dashboard_pane_dims`/`orchestration_role_pane_dims`, both delegating to `right_column_pane_dims`) can't drift apart. `compute_frame_layout`'s `ActiveTabView::Dashboard` and `ActiveTabView::Orchestration` arms are near-identical — both filter `all_pane_ids`, both call `split_cards_area` with their own two constants, both call `cards_pane_rects` on the result. This means extending the toggle to Dashboard tabs is a clean symmetric extension of the same resolver and the same call-site change, not a second feature — there is no Open Question here, contrary to what the task background speculated might be needed.

#### Stage resolver

```rust
enum SplitStage { Default, Narrow, Hidden }

fn next_split_stage(current: SplitStage) -> SplitStage {
    match current {
        SplitStage::Default => SplitStage::Narrow,
        SplitStage::Narrow => SplitStage::Hidden,
        SplitStage::Hidden => SplitStage::Default,
    }
}

/// `default_left`/`default_panes` are the tab type's own Default-stage
/// constants (33/67 for Dashboard, 34/66 for Orchestration) so one resolver
/// serves both tab types without hardcoding either ratio.
fn split_stage_percents(stage: SplitStage, default_left: u16, default_panes: u16) -> (u16, u16) {
    match stage {
        SplitStage::Default => (default_left, default_panes),
        SplitStage::Narrow => (25, 75),
        SplitStage::Hidden => (0, 100),
    }
}
```

Both `ActiveTabView::Dashboard` and `ActiveTabView::Orchestration` arms in `compute_frame_layout` call `split_stage_percents(view_stage, DASHBOARD_LEFT_PERCENT, DASHBOARD_PANES_PERCENT)` / `(…, ORCHESTRATION_LEFT_PERCENT, ORCHESTRATION_PANES_PERCENT)` respectively, then pass the result into the existing `split_cards_area` call unchanged.

#### Hidden stage rendering

`split_cards_area` already handles a `Constraint::Percentage(0)` sidebar chunk correctly — `Layout::horizontal` produces a zero-width `Rect` for the sidebar and gives the remainder to the panes chunk, and a zero-width `Rect` renders nothing. This means Hidden needs **no new branch or widget-suppression logic** — it reuses the exact same code path as Default and Narrow with different numbers, which is the simplest option and keeps the three stages mechanically identical at the call site. Confirm this holds in practice as part of M1 (the L1 snapshot for the Hidden stage is the regression guard).

#### Where the per-tab state lives

`src/tab.rs`'s `Tab::Dashboard` and `Tab::Orchestration` variants already carry per-tab UI state alongside their structural fields (e.g. `selected_session_id`, `focused_role_pane_id`) — `split_stage: SplitStage` follows that existing pattern rather than introducing a new tab-keyed side-table. The `Tab` enum has no `Serialize`/`Deserialize` derive today (session snapshots are built from `Tab` state into separate `Saved*` structs at snapshot time, not derived automatically), which is why persistence is not free — see Open Questions.

### Success Criteria

- In an Orchestration tab, pressing the chord cycles the sidebar/pane-column split Default → Narrow (25/75) → Hidden (sidebar gone, pane column full-width) → Default, looping indefinitely.
- The same cycle works identically on a Dashboard tab, using that tab's own Default ratio (33/67) as stage 1.
- The cycle is scoped per tab: toggling one tab's stage does not change another open tab's stage, whether that other tab is the same type or the other type.
- No effect on Mode tabs.
- The chord is remappable through the same config mechanism as every other keybinding.

---

## Cross-cutting: experimental-flag question (CLAUDE.md rule 9)

Per CLAUDE.md rule 9, whether each new user-visible surface ships behind the `experimental` flag is confirmed at `/prd-start`, not decided in this PRD. Judgment per item, to inform that conversation rather than pre-empt it:

- **Item 1 (status-clearing bug fix)**: **no**, and not really a candidate at all — it's a correctness fix to an existing, already-shipped status indicator, not a new surface. Flag-gating a bug fix would mean the bug persists by default until a user opts into the flag, which inverts the point of fixing it.
- **Item 2 (auto-return focus)**: candidate for **yes** — it's a new, potentially surprising behavior (focus moves out from under the user without a keypress) scoped to Orchestration tabs, similar in shape to why item 4 leans "no" (cheap to verify, no cross-user/cross-agent state) but with a materially different risk profile: unlike a keybinding the user presses on purpose, this one *acts on its own* via a timer, which is exactly the kind of thing worth letting users opt into first. Recommend flagging it and letting `/prd-start` confirm.
- **Item 3 (command-entry lock)**: candidate for **yes**, and probably the strongest case of the three — it changes default keyboard behavior in a way a user could easily not expect (typing "does nothing" on a pane they can see) until they learn about `Ctrl+e`. A flag lets it ship without surprising existing users mid-session.
- **Item 4 (split toggle)**: unchanged from the original PRD — candidate answer **no**, same reasoning as PRD #341 (a keybinding-driven layout affordance with no new state that persists or affects other users/agents, cheap to verify with L1/L2 coverage) — confirmed at `/prd-start`, not assumed here.

## Milestones

- [ ] **M1 — `SplitStage` enum and resolver.** Pure-data type and `next_split_stage`/`split_stage_percents` functions, unit-tested (including the Hidden-stage zero-width rendering assumption).
- [ ] **M2 — Per-tab state added.** `split_stage` field on both `Tab::Dashboard` and `Tab::Orchestration`, defaulting to `SplitStage::Default`; threaded through to `ActiveTabView`.
- [ ] **M3 — `CycleSplitStage` action wired.** New `Action` registered with default chord `Ctrl+l`; pressing it in either tab type advances that tab's stage.
- [ ] **M4 — Layout call sites resolve the active stage.** Both `compute_frame_layout` arms use `split_stage_percents` instead of the fixed constants directly.
- [ ] **M5 — L1 snapshot coverage.** `insta` render tests pin all three stages' geometry for an Orchestration tab and a Dashboard tab (per CLAUDE.md rule 4).
- [ ] **M6 — L2 coverage.** A vt100 test drives the chord through the full 3-stage cycle on an Orchestration tab, asserting the visible transitions and that a second open Orchestration tab is unaffected; a second vt100 test does the same for a Dashboard tab and additionally asserts cross-tab-type isolation against an open Orchestration tab.
- [ ] **M7 — Docs and changelog.** `docs/keyboard-shortcuts.md` updated with the new binding and its scope; changelog fragment added.
- [ ] **M8 — Item 1: single-slot pending-permission marker.** Resolve the Open Question (name-matching vs. an id that survives `PermissionRequest → ToolStart`) against a live hook payload pair, then implement the discriminated clear in `AppState::apply_event`.
- [ ] **M9 — Item 1: regression coverage.** Unit tests for approve → tool starts → status clears, and a repeat of the `4d31103`/#86 concurrent-subagent scenario (must still preserve `WaitingForInput` in that case).
- [ ] **M10 — Item 2: all-clear focus move.** New edge-triggered `TabManager` method alongside `auto_focus_waiting_pane`, wired at the same per-frame site (`src/ui.rs:9750-9758`); unit tests for the edge-trigger (fires once, not every frame) and coexistence with `auto_focus_waiting_pane`.
- [ ] **M11 — Item 2: 30-second inactivity snap-back.** Timer keyed off `ui.last_pane_keystroke_at`, scoped to the active Orchestration tab; resolve the Open Question (locked-pane "attempted activity") before finalizing the reset condition.
- [ ] **M12 — Item 3: `ToggleOrchestrationLock` action wired.** New `Action`, default chord `Ctrl+e`, registered in `src/keybindings.rs`; lock-state field added per the resolved per-tab-vs-global Open Question.
- [ ] **M13 — Item 3: PTY-forward gate.** `handle_pane_input_key`'s call site gated on active tab / focused-pane identity / lock state; dropped keystrokes surface a status message; global chords verified unaffected.
- [ ] **M14 — Items 2+3: L1/L2 coverage.** Unit + vt100 coverage for the interaction between the lock and the inactivity timer, once the item-2 Open Question above is resolved.
- [ ] **M15 — Docs and changelog (items 1-3).** `docs/keyboard-shortcuts.md` updated with `Ctrl+e`; changelog fragments for the bug fix and the two new behaviors.

## Risks

- **Hidden-stage assumption needs verification.** The plan relies on a zero-width `Rect` from `split_cards_area` rendering nothing extra (no stray border, no panic on a 0-width inner area in downstream widgets like `cards_pane_rects`). Confirm early in M1/M4 with a real render; if any downstream helper assumes a non-zero sidebar width, the Hidden stage will need an explicit skip-rendering branch instead.
- **Snapshot churn.** Existing full-frame Dashboard/Orchestration snapshots are unaffected in content (Default stage is numerically identical to today), but any snapshot that happens to pin the *type* of `ActiveTabView` variant fields will need updating once `split_stage` is added.
- **Chord conflicts.** `Ctrl+l` is free today (verified against the `ACTIONS` default table on `upstream/main`); re-check before landing in case another default binding was added upstream in the meantime. Same re-check needed for `Ctrl+e` (item 3) before landing.
- **Item 1: name-matching fragility.** If the resolved discriminator (M8) ends up being tool-name matching rather than a stable id, two identically-named concurrent tool calls (main turn + a subagent both calling the same tool) could still mismatch — this is exactly the class of edge case that made the original `pending_permissions` queue unreliable. Keep the M9 regression test broad enough to catch it, not just the single-tool-call happy path.
- **Item 2/3: cross-item coupling.** Item 2's inactivity timer and item 3's lock share the same keystroke-forwarding choke point (`Action::ForwardToPane`); a change to one that doesn't account for the other (see the Open Question under item 2) could silently make the timer nonfunctional whenever the lock is engaged. M14 exists specifically to catch this at the seam rather than in each item's isolated tests.

## Open Questions

1. **Persistence across restarts (item 4).** Not proposed as in-scope: `Tab` has no `Serialize`/`Deserialize` derive, and session snapshots are built into separate `Saved*` structs rather than mirroring `Tab` automatically, so wiring `split_stage` through snapshot/restore is real (if probably small) additional work, not a free field default. Left for a follow-up if users want it.
2. **Experimental flag gating (all items, CLAUDE.md rule 9)** — to be settled at `/prd-start`, not here (see the cross-cutting section above for per-item judgment).
3. **Action naming (item 4)** — `CycleSplitStage` vs `ToggleSplitStage` (the latter reads oddly for a 3-state cycle vs. a 2-state toggle) vs `CyclePaneSplit`. Settle at implementation time; does not affect behavior.
4. **Item 1: name-matching vs. a stable id.** See Technical Approach — needs a live hook payload check before implementation, not an assumption either way.
5. **Item 2: does "activity" include blocked keystrokes on a locked pane?** See Solution Overview — a genuine product decision, not an engineering default.
6. **Item 3: per-tab lock state vs. one global lock.** See Solution Overview — both are defensible; recommend per-tab for consistency with the rest of `Tab::Orchestration`'s state, but not decided here.
