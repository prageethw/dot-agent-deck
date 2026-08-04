# PRD #372: Fix stale "Needs Input" status after a permission prompt is answered

**Status**: Implementation complete — PR pending
**Priority**: Medium
**Created**: 2026-08-04
**GitHub Issue**: [#372](https://github.com/vfarcic/dot-agent-deck/issues/372) (closed upstream as not-planned; this fork continues the work independently)
**Related**: Split from #361, alongside #371/#373/#374 (siblings). `src/state.rs` (`AppState::apply_event`'s `SessionStatus` transitions), `src/hook.rs` (`map_event_type`), `src/ui.rs` (`Action::SendPermissionResponse`, `SessionStatus` rendering)

## Problem Statement

Reported symptom: a pane's dashboard/orchestration-sidebar status stays on "Needs Input" (`SessionStatus::WaitingForInput`, rendered as the bold "Needs Input" label — `src/ui.rs:15987`) even though the pane is visibly working again (the agent's own PTY output shows it running).

Root cause, confirmed against git history rather than guessed:

`SessionStatus` is purely hook-event-driven (`AppState::apply_event`, `src/state.rs:3526-3568`) — there is no PTY-content pattern-matching path that independently detects "the input form went away"; the deck cannot see the agent's own terminal UI, only the hook events the agent process emits. So the *only* way `WaitingForInput` clears is one of these event-type arms:

- `EventType::ToolStart` (`src/state.rs:3535-3543`): `if session.status != SessionStatus::WaitingForInput { session.status = Working }` — i.e. **while status is `WaitingForInput`, a `ToolStart` never clears it**, it only refreshes `active_tool`.
- `EventType::ToolEnd` (`src/state.rs:3544-3550`): unconditionally sets `Thinking` if status was `WaitingForInput`.
- `EventType::Thinking` (from a `UserPromptSubmit` hook, `src/hook.rs:100`) unconditionally sets `Thinking` (`src/state.rs:3531-3534`).

For a plain idle/notification-driven wait (Claude Code's `Notification` hook, mapped to `WaitingForInput` at `src/hook.rs:103`), the human types a reply and hits enter, `UserPromptSubmit` fires, and `Thinking`'s unconditional arm clears the status immediately — **this path is not broken**.

The broken path is the **permission-approval** flow: a `PermissionRequest` hook sets `WaitingForInput` (`src/state.rs:3551-3553`); the human answers via `Action::SendPermissionResponse` (`src/ui.rs:7490-7509`), which writes a raw `"y"`/`"n"` keystroke straight into the pane's PTY (there is no separate closeable "form" widget to detect — the prompt lives inside the agent's own rendered terminal content) and does **not** touch `session.status` itself. The next hook event Claude Code sends is the approved tool's own `PreToolUse` → `ToolStart` — and that is exactly the event the `ToolStart` guard above refuses to use to clear `WaitingForInput`. So the status stays "Needs Input" for the entire duration of the now-running, human-approved tool, only flipping to `Thinking` once *some* `ToolEnd` (not necessarily the same tool) fires.

That guard is not a bug in isolation — it exists on purpose, added in `4d31103` ("fix: preserve WaitingForInput status when concurrent subagent fires ToolStart", #86, 2026-05-12) to stop a *different*, real regression: a concurrent subagent's unrelated `PreToolUse` flipping a genuine, still-outstanding "Needs Input" card back to "Working" before the human had answered. The problem is what it replaced: a one-day-older `pending_permissions` queue (added `7ea3a11`/#43, refined `15bef44`, `87f3708`, all 2026-04-05) that *did* discriminate "this ToolStart is the approved tool" from "this is some other concurrent tool" — matched first by `tool_use_id`, then by tool name after `PermissionRequest`'s synthetic id turned out not to match the real `tool_use_id` on `ToolStart`. That whole queue was deleted the very next day in `29384bb` ("fix: remove permission queue to fix stuck 'Needs Input' status", 2026-04-06) because the queue itself could get stuck non-empty. `4d31103` then re-added the *preserve* half of the guard five weeks later without restoring any discriminator — so the pre-fix code had the coarse "never clear on ToolStart while waiting" behavior with none of the matching logic that used to make it selective.

## Solution Overview

Fix scoped narrowly to the permission-approval path (the idle/notification path already self-heals via `UserPromptSubmit → Thinking`). Reintroduce a **single-slot** marker, not the old FIFO queue — Claude Code only ever shows one outstanding permission prompt on a given pane at a time (the turn blocks synchronously on it), so a queue's ordering semantics were never actually needed; the previous implementation's complexity (matching by id, then by name, across a multi-entry queue) is what made it fragile enough to be reverted twice. Concretely: record the tool name from the triggering `PermissionRequest`/`WaitingForInput` event in a single `Option<String>` on the session; on `ToolStart`, clear `WaitingForInput` when either (a) there's no pending marker (a plain notification wait — any tool starting must mean the human's reply set it off), or (b) the incoming event's tool name matches the marker. Clear the marker itself whenever `WaitingForInput` clears by any path.

## Scope

**In Scope**: the `ToolStart`/`ToolEnd`/`PermissionRequest` arms in `AppState::apply_event` (`src/state.rs:3526-3568`); a new single-slot pending-permission field on the session (not a queue); unit tests covering approve → tool starts → status clears, and the concurrent-subagent-preserve case `4d31103` was written for (must not regress).

**Out of Scope**: the plain notification/idle-wait path (already correct); any change to how permission responses are sent to the pane (`Action::SendPermissionResponse`, `src/ui.rs:7490-7509`) — this is a status-tracking fix only, not a change to the approve/deny mechanism.

## Technical Approach

See Solution Overview above for the mechanism. One genuine ambiguity that this exact code seam had flip-flopped on twice already (`7ea3a11`/`15bef44`/`87f3708` → `29384bb` → `4d31103`), so it is called out rather than silently re-guessed a third time:

**Decision**: match the approved permission tool by **name**, not a stable id — resolved by the user rather than by inspecting a live hook payload pair. `87f3708`'s commit message notes that a synthetic `PermissionRequest` id was tried in the past and found not to survive to the matching `ToolStart` ("PermissionRequest uses synthetic IDs that differ from the real tool_use_id on ToolStart"); name-matching is accepted as the discriminator despite the known edge case where two identically-named concurrent tool calls (main turn + a subagent calling the same tool) could still mismatch — see the corresponding entry under Risks.

### Experimental-flag question (CLAUDE.md rule 9)

**No**, and not really a candidate at all — it's a correctness fix to an existing, already-shipped status indicator, not a new surface. Flag-gating a bug fix would mean the bug persists by default until a user opts into the flag, which inverts the point of fixing it.

## Success Criteria

- Approving a permission prompt clears `WaitingForInput` on the *next* `ToolStart` for that tool, not on some later, unrelated `ToolEnd`.
- The `4d31103`/#86 regression (a concurrent subagent's `ToolStart` prematurely clearing a genuinely-still-outstanding permission prompt) does not reappear — covered by a regression test mirroring that commit's scenario.
- The plain notification/idle-wait path is untouched and continues to clear via `UserPromptSubmit → Thinking`.

## Milestones

- [x] **M1 — Single-slot pending-permission marker.** Implemented the discriminated clear in `AppState::apply_event` using name-matching (per the Decision above), not a `tool_use_id`.
- [x] **M2 — Regression coverage.** `tests/hooks_permission.rs::hooks_permission_001_tool_start_matching_the_approved_tool_clears_waiting_for_input` and `hooks_permission_002_tool_start_for_an_unrelated_tool_preserves_waiting_for_input` — the approve → tool starts → status clears case, and a repeat of the `4d31103`/#86 concurrent-subagent scenario (still preserves `WaitingForInput` in that case).
- [ ] **M3 — Docs and changelog.** `docs/keyboard-shortcuts.md` (if applicable) and a changelog fragment for the bug fix.

## Risks

- **Name-matching fragility.** The Decision above accepts tool-name matching (M1) over a stable id; two identically-named concurrent tool calls (main turn + a subagent both calling the same tool) could still mismatch — this is exactly the class of edge case that made the original `pending_permissions` queue unreliable. The M2 regression test is broad enough to catch the single-tool-call happy path and the concurrent-subagent-preserve case, but not this specific same-name-concurrent-call edge case; worth a follow-up test if it turns out to matter in practice.

## Open Questions

None outstanding — the one open question this item carried (name-matching vs. a stable id) was resolved by the user before implementation started; see the Decision under Technical Approach.
