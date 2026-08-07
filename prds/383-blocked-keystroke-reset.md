# PRD #383: Blocked-keystroke reset for the Orchestration inactivity timer (Orchestration tabs only)

> **Delivered via PRD [#373](https://github.com/vfarcic/dot-agent-deck/issues/373), not as a separate PR.** This PRD was carved out of #373's M3 while #374 was still in flight. Once #374's command-entry lock landed (`936a603`), the user decided the blocked-keystroke reset should ship in #373's own PR rather than separately, so it was folded back in as #373's M3: the lock's drop site in `handle_key_event`'s `UiMode::PaneInput` arm stamps `Tab::Orchestration::last_role_pane_activity_at` before returning `Action::Continue`, pinned by `tabs/orchestration/019`. See `prds/373-auto-return-focus.md` (M3) for what shipped. Everything below is retained as the original design record; no separate implementation work remains here.

**Status**: Delivered via PRD #373 (see the note above) — no separate work remains
**Priority**: Medium
**Created**: 2026-08-05
**GitHub Issue**: [#383](https://github.com/vfarcic/dot-agent-deck/issues/383) (closed upstream as not-planned; this fork continues the work independently)
**Related**: Split from [#373](https://github.com/vfarcic/dot-agent-deck/issues/373) (auto-return focus to the orchestrator pane), specifically #373's M3, deferred until [#374](https://github.com/vfarcic/dot-agent-deck/issues/374) (command-entry lock) landed. #374 shipped on `main` (`936a603`). #373 M1+M2 shipped without M3, documented as a known limitation in #373's own Risks section. `src/tab.rs` (`Tab::Orchestration::last_role_pane_activity_at`, `command_entry_locked`), `src/ui.rs` (`gate_pane_input_key`, the `UiMode::PaneInput` PTY-forward drop path, the per-frame `auto_focus_after_inactivity` call site in `run_tui`)

## Problem Statement

#373 shipped a 30-second inactivity timer that snaps focus back to the orchestrator role pane in an Orchestration tab once the human stops interacting with a non-orchestrator pane. #374 shipped a per-tab lock (`command_entry_locked`, default **on**) that drops keystrokes aimed at a non-orchestrator pane before they reach its PTY, unless `Ctrl+e` has unlocked that tab.

These two features currently don't talk to each other: a keystroke dropped by #374's lock does **not** reset #373's inactivity timer, because the timer only reads `Tab::Orchestration::last_role_pane_activity_at`, which is stamped at every site that actually reaches a pane's PTY or moves focus — but the #374 drop path (`gate_pane_input_key`'s `Action::Continue` substitution) returns before any of those sites run. Net effect: a human actively typing at a **locked** pane looks idle to #373's timer. After 30 seconds they get auto-snapped to the orchestrator pane — which #374 never locks — and their next keystrokes land there instead of being silently dropped as the lock intends.

#373's own Decision (recorded before either PRD shipped) already resolved that this should count as activity: *"a blocked/attempted keystroke to a locked pane does count as activity — it resets the 30-second inactivity timer, since the human is still clearly engaged with that pane even though nothing was forwarded to the PTY."* This PRD implements that decision, now that #374's lock exists to test against.

## Solution Overview

Stamp `Tab::Orchestration::last_role_pane_activity_at` on the **blocked** path too, not just the forwarded one. `gate_pane_input_key` (`src/ui.rs`) already has everything it needs in scope — the active tab, the lock state, the focused pane id — at the exact point it decides to substitute `Action::Continue` for a dropped `Action::ForwardToPane`. Add the same gated stamp used everywhere else this session (`Action::ForwardToPane`, `Action::SelectCard`, `Action::FocusCard`, `Action::Focus`, the paste path, `dispatch_normal_mode_key`, `send_config_gen_prompt`): immediately before returning `Action::Continue` for the drop, if `tab_manager.active_tab_mut()` matches `Tab::Orchestration { last_role_pane_activity_at, .. }`, set `*last_role_pane_activity_at = Some(Instant::now())`.

No new state, no new field — this is purely closing the last unstamped site in an already-established pattern.

## Scope

**In Scope**: stamping `last_role_pane_activity_at` on `gate_pane_input_key`'s drop path; the combined lock+timer interaction test #373 always deferred (attempt a keystroke against a locked non-orchestrator pane, confirm it's still dropped — #374's existing behavior, unaffected — **and** confirm the inactivity clock resets, i.e. the pane is not auto-snapped away from even after 30+ seconds with no keystroke ever reaching the PTY); a changelog fragment.

**Out of Scope**: any change to #374's lock/drop behavior itself (the keystroke still doesn't reach the PTY — only the timer's bookkeeping changes); any change to #373's M1/M2 mechanics; Dashboard and Mode tabs (both parent PRDs are Orchestration-only).

## Technical Approach

Land the stamp inside `gate_pane_input_key`, at the same point the drop decision is made, mirroring the gated-stamp pattern already used at every other site (`if let Tab::Orchestration { last_role_pane_activity_at, .. } = tab_manager.active_tab_mut() { *last_role_pane_activity_at = Some(Instant::now()); }`). `gate_pane_input_key` needs `tab_manager: &mut TabManager` in scope for this — check its current signature; if it only takes `&TabManager` or doesn't have `tab_manager` at all today, that's the one piece of mechanical plumbing this PRD may need (mirrors the `dispatch_normal_mode_key` parameter-threading #373 M2 needed for the same reason).

Test: an L1 (or L2 if the PTY-forward gate can't be exercised faithfully at L1) test that opens a locked Orchestration tab, seeds `last_role_pane_activity_at` as stale, drives a keystroke at the locked non-orchestrator pane through the real `gate_pane_input_key` path, confirms the keystroke was dropped (no PTY write — matching #374's existing `orchestration_lock_004` technique) **and** confirms `last_role_pane_activity_at` is now fresh. A second assertion: replaying #373's `auto_focus_after_inactivity` chain immediately after must NOT fire, proving the reset actually prevents the snap-back end to end.

## Success Criteria

- A keystroke blocked by #374's lock (dropped, never reaching the PTY) resets #373's 30-second inactivity clock for that Orchestration tab.
- The orchestrator pane is not auto-snapped to while a human is actively (if unsuccessfully) typing at a locked non-orchestrator pane.
- #374's lock behavior itself is unchanged — the keystroke still never reaches the PTY while locked.
- #373's known-limitation note (Risks section) is removed or updated once this ships.

## Milestones

- [ ] **M1 — Stamp the blocked-keystroke path.** `gate_pane_input_key`'s drop branch stamps `last_role_pane_activity_at` on the active `Tab::Orchestration`, same pattern as every other gated stamp site.
- [ ] **M2 — Combined interaction test.** The lock+timer test described in Technical Approach: blocked keystroke → still dropped, clock resets, subsequent inactivity check does not fire.
- [ ] **M3 — Update #373's Risks note.** Remove or update the known-limitation entry in `prds/373-auto-return-focus.md` once this lands, so the PRD trail doesn't describe a stale limitation.
- [ ] **M4 — Changelog fragment.**

## Risks

- **Low — this is a narrow, well-understood gap.** Both parent features are already shipped and independently tested; this PRD only adds one more stamp site to an established pattern and one test. The main risk is scope creep into re-litigating #373/#374's own mechanics, which is explicitly out of scope.

## Open Questions

None outstanding — the Decision this PRD implements was already resolved on #373 before either parent PRD shipped.
