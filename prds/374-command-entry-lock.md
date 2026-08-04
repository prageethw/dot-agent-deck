# PRD #374: Lock command entry to the orchestrator pane, `Ctrl+e` to unlock (Orchestration tabs only)

**Status**: Implementation complete — PR pending
**Priority**: Medium
**Created**: 2026-08-04
**GitHub Issue**: [#374](https://github.com/vfarcic/dot-agent-deck/issues/374) (closed upstream as not-planned; this fork continues the work independently)
**Related**: Split from #361, alongside #371/#372/#373 (siblings). [#373](https://github.com/vfarcic/dot-agent-deck/issues/373) (auto-return focus) structurally depends on this PRD for its combined interaction test — the "blocked keystroke on a locked pane resets the 30-second inactivity timer" case is deferred on #373's own M3 until this PRD's lock exists to attempt a blocked keystroke against. `src/tab.rs` (`Tab::Orchestration`, `role_pane_ids`, `start_role_index`), `src/keybindings.rs` (`Action`/`ActionSpec`), `src/ui.rs` (`handle_key_event`, `handle_pane_input_key`, `global_action`, `key_action_for_mode`, `Action::ForwardToPane`)

## Problem Statement

Confirmed with the user: **scoped to Orchestration tabs only**. By default, keystrokes typed by the human should not reach any non-orchestrator pane's PTY within an Orchestration tab — only the orchestrator pane accepts direct input. `Ctrl+e` toggles the lock; it starts **locked** (direct command entry to worker panes disabled by default).

### Investigation: `Ctrl+e` availability

Verified free: `src/keybindings.rs`'s `ACTIONS` table has no entry using `e`, and a direct search of `src/ui.rs` for any hardcoded (non-`Action`-registry) `KeyModifiers::CONTROL` + `Char('e')` check found none. `Ctrl+e` is available as the default chord, same verification method the existing split-toggle item (`Ctrl+l`, PRD #371) used.

### Investigation: where PTY keystroke forwarding lives

Confirmed there is exactly one choke point for all pane-forwarded keystrokes, in any tab type: `UiMode::PaneInput` is the mode a focused pane puts the UI into; `handle_key_event` resolves global chords first (`global_action_for_mode`, itself calling `global_action` — the small set of always-available `Ctrl+`-chord `Action`s: dashboard, new_pane, close_pane, toggle_layout, toggle_orchestration_split, and now toggle_orchestration_lock), and only if nothing claimed the key does `UiMode::PaneInput` fall through to `handle_pane_input_key`, which converts the key to raw bytes and returns `Action::ForwardToPane(bytes)`; that action is dispatched at its own match arm, which resolves the focused pane id via the pane controller and calls `write_raw_bytes`. `key_action_for_mode` documents this exact two-seam composition (global chords, then PTY-forward fallback) and is the seam production and tests already share.

One consequence worth stating precisely: plain tab-cycling keys (`h`/`l`/Tab/Shift-Tab/arrows, `cycle_tab_action`) are **only** resolved in `UiMode::Normal` — they are *not* checked at all while a pane has focus (`UiMode::PaneInput`), today, for any tab type, locked or not. So "does tab-switching still work while a worker pane is focused" is already "no" pre-existing behavior for every pane in the app, not something this item changes. What *is* already global regardless of focus is the small `Ctrl+`-chord set resolved by `global_action_for_mode` before the PTY-forward fallback is ever reached — this item's gate sits entirely inside that fallback and never touches `global_action_for_mode`, so those chords (dashboard, new pane, close pane, toggle layout, split-cycle, and the new unlock chord itself) are unaffected by the lock by construction.

## Solution Overview

Gate the PTY-forward fallback itself: when the active tab is `Tab::Orchestration`, the focused pane is *not* `role_pane_ids[start_role_index]` (the orchestrator), and the tab's lock is engaged, `handle_pane_input_key`'s caller returns `Action::Continue` instead of `Action::ForwardToPane(bytes)` — i.e. the keystroke is silently dropped before it reaches the PTY, exactly like `key_action_for_mode`'s existing "nothing to forward" case. A brief status message ("Pane locked — Ctrl+e to unlock") on the drop follows this codebase's existing no-op-with-feedback convention (e.g. `RequestConfigGen`'s "No active agent session to send prompt to.").

**Decision**: lock state is **per-orchestration-tab**, not global — resolved by the user. A new field on `Tab::Orchestration` (following the `split_narrow` precedent already on that variant), consistent with the "each orchestration tab is independent" pattern everywhere else on that variant. Toggling `Ctrl+e` in one Orchestration tab does not affect the lock state of any other open Orchestration tab.

## Scope

**In Scope**: new `Action` (`ToggleOrchestrationLock`), `Section::Global`, default chord `Ctrl+e`, in `src/keybindings.rs`'s `ACTIONS` table; a per-tab lock-state field on `Tab::Orchestration` (per the Decision above); the gate inside the PTY-forward fallback (`handle_pane_input_key`'s call site needs the tab/lock context threaded in — it previously took only the raw `KeyEvent`); a status message on a dropped keystroke; unit tests covering: locked pane drops keystrokes, orchestrator pane never gates regardless of lock state, `Ctrl+e` toggles the lock, and global chords still work while a non-orchestrator pane is focused and locked.

**Out of Scope**: Dashboard and Mode tabs; changing `cycle_tab_action`'s existing Normal-mode-only scoping (unrelated pre-existing behavior, not part of this item).

## Technical Approach

See Solution Overview above for the mechanism. `ToggleOrchestrationLock` mirrors `ToggleOrchestrationSplit`'s (PRD #371... actually #336-originated, carried into #371's three-stage successor) existing scoping pattern in `handle_key_event`: `global_action` resolves the chord from any mode (so `Ctrl+e` works from `UiMode::PaneInput`, where the lock actually matters), but `handle_key_event` un-resolves it back to `None` when the active tab is not `Tab::Orchestration` — this lets `Ctrl+e` fall through to the normal PTY-forward path on a Dashboard/Mode-tab pane instead of being silently swallowed (readline binds `Ctrl+e` to "end of line"; consuming the chord globally on non-orchestration tabs would have broken that).

The gate itself lives in a small helper (`gate_pane_input_key`) called from `handle_key_event`'s `UiMode::PaneInput` arm, right after `handle_pane_input_key` produces its candidate action: if the candidate is `Action::ForwardToPane`, the active tab is a locked `Tab::Orchestration`, and the focused pane (from the pane controller) is not `role_pane_ids[start_role_index]`, the candidate is replaced with `Action::Continue` and the status message is set. The orchestrator pane's own id is looked up fresh on every keystroke (no cached identity), so it can never go stale across a reconnect or role-pane respawn.

### Dependency note for #373 (auto-return focus)

#373's Decision that "a blocked keystroke on a locked pane counts as activity" (resetting its 30-second inactivity timer) needs this PRD's lock to exist before there is anything to block. #373's own M1/M2 land independently of this PRD; its M3 (the blocked-keystroke timer reset and the combined interaction test) is deferred until this PRD ships. This PRD's own scope does not depend on #373 in either direction.

## Success Criteria

- By default, typing while focused on a non-orchestrator pane in an Orchestration tab does not reach that pane's PTY.
- Typing while focused on the orchestrator pane always reaches its PTY, lock state notwithstanding.
- `Ctrl+e` toggles the lock; while unlocked, non-orchestrator panes accept direct input normally.
- Global chords (`Ctrl+d`, `Ctrl+n`, `Ctrl+w`, `Ctrl+t`, the split-cycle chord, `Ctrl+e` itself) keep working regardless of focus or lock state.

## Milestones

- [x] **M1 — `ToggleOrchestrationLock` action wired.** New `Action`, default chord `Ctrl+e`, registered in `src/keybindings.rs`; per-tab lock-state field (`command_entry_locked: bool`, default `true`) added on `Tab::Orchestration`.
- [x] **M2 — PTY-forward gate.** `handle_pane_input_key`'s call site gated on active tab / focused-pane identity / lock state via `gate_pane_input_key`; dropped keystrokes surface the "Pane locked — Ctrl+e to unlock" status message; global chords verified unaffected (`orchestration_lock_005`).
- [x] **M3 — L1/L2 coverage.** `orchestration_lock_001`-`003` (L1, `src/ui.rs`) pin the default-locked state, the `Ctrl+e` toggle, and per-tab isolation; `orchestration_lock_004`-`005` (L2, `tests/e2e_orchestration_lock.rs`) drive the real PTY-forward gate and the global-chord regression guard.
- [x] **M4 — Docs and changelog.** `docs/keyboard-shortcuts.md` updated with `Ctrl+e`; changelog fragment.

## Risks

- **Cross-item coupling with #373.** This item's lock and #373's inactivity timer share the same keystroke-forwarding choke point (`Action::ForwardToPane`); #373's Decision (a blocked keystroke on a locked pane still resets its timer) means #373's M3 gate needs to record a blocked-attempt signal once it lands here, not just observe a drop. Tracked on #373's side, not this PRD's.
- **Experimental-flag candidate — resolved: No.** Per CLAUDE.md rule 9, this was flagged as the strongest candidate for **yes** among the four split-out items pre-implementation, since it changes default keyboard behavior in a way a user could easily not expect (typing "does nothing" on a pane they can see) until they learn about `Ctrl+e`. **Resolved: No — visible by default.** Keybinding-driven affordance (a status message on every drop tells the user why nothing happened, and `Ctrl+e` is discoverable the moment it's tried), no persisted state, and existing L1/L2 coverage is sufficient verification. Matches precedent (PRD #336/#333/#341).

## Open Questions

1. **Experimental flag gating** — resolved: no, visible by default (see Risks above).

Resolved (previously open, decided by the user before implementation started):

- **Per-tab lock state vs. one global lock.** Resolved: **per-orchestration-tab**, not global (see Decision under Solution Overview).
