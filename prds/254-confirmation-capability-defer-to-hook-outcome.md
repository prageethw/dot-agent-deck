# PRD #254: Don't latch `ConfirmationCapability::Reports` before the native hook install/trust outcome is known

**GitHub Issue**: [#254](https://github.com/prageethw/dot-agent-deck/issues/254)

**Priority**: High

**Status**: Implemented and merged (PR #441) for the TUI-attached `delivery_capability` path. **Third attempt on this issue — the first two targeted a defect that no longer exists (see the issue's rescoping comment).** Issue #254 stays open: the dispatch/scheduler path (`drain_pre_write_events`, `src/spawn.rs`) still classifies from agent type alone and carries the same symptom — tracked separately as [#459](https://github.com/prageethw/dot-agent-deck/issues/459).

## Problem Statement

This issue originally described `LEVEL`'s false-positive confirmation (a generic stdout-activity classifier substituting for a real submit signal) and two failed attempts to fix it by changing retry timing. **That defect is gone** — an unrelated rewrite (PRD fork#197) deleted `LEVEL` and the fixed grace-period retry entirely; confirmation on current `main` is TEXT+identity-only (`prompt_submission_evidence`, `src/ui.rs:4077`), already proven falsifiable by an existing deterministic test (`pane_input_026_only_matching_pane_and_prompt_confirm_delivery`, `src/ui.rs:36083`). No fix is needed for that half, and this PRD does not attempt one.

**What remains is a different, narrower bug, found during this attempt's verification pass:**

`ConfirmationCapability` (`Reports`/`CannotReport`/`Unknown`, `src/prompt_delivery.rs:357`) governs what happens at the 60s delivery deadline (`AUTOMATIC_PROMPT_DEADLINE`, `src/prompt_delivery.rs:62`) if no confirmation ever arrives: a `Reports` pane takes `abandon_orchestrator_prompt`, which inserts the tab into `orchestration_remit_abandoned` — **documented as permanent for the tab's lifetime**. A `CannotReport`/`Unknown` pane instead finalizes honestly, trusting the single write.

That capability is decided from `Emitter::emit_fork_session_start` — a `SessionStart` the wrapper emits **unconditionally, the instant `cmd.spawn()` returns** — before Codex's own native hook install/trust (`codex_spawn_prep`: `auto_install()` + `trust_deck_hooks_in()`) has had any chance to succeed or fail. A failure there is real, anticipated, and non-crashing — spawn continues, and per the code's own comment, events "degrade to stdout classification." On that degraded path `Emitter::emit_with_metadata` hardcodes `user_prompt: None` unconditionally, so TEXT confirmation is structurally impossible for that pane, forever. But `delivery_capability` resolves capability purely from agent *type* (`agent_reports_submitted_prompt(Codex) == true` → `Reports`), latched sticky at the first write attempt (`delivery.can_report_prompts |= ...`) and never revisited.

**Net effect:** a Codex pane whose native hook genuinely failed to install/trust is misclassified as `Reports`. It burns the full 60s escalating-backoff retry cycle (bare-Enter probes into a live interactive Codex TUI — bounded, but real visible-glitch risk), then permanently abandons that tab's remit — a strictly worse, user-visible outcome than the honest single-write finalization a correctly-classified pane would get.

## Solution Overview

**Resolve `ConfirmationCapability` from whether the native hook installation/trust actually succeeded, not from agent type alone — and defer the resolution, rather than latching it at spawn.**

Concretely:

1. Surface the `codex_spawn_prep` install/trust outcome (success/failure) somewhere `delivery_capability` can read it — it's currently only `tracing::warn!`'d and discarded. This likely means threading a result (not just a log line) out of `codex_spawn_prep` into whatever per-pane/per-session state `delivery_capability` already consults.
2. Change capability resolution so a type that *can* report (`Codex`, `ClaudeCode`, `OpenCode`, `Devin`) is only actually treated as `Reports` once the hook outcome is known-successful for that specific pane — not merely because the type is capable in general. Until that's known, resolve to `Unknown` (already an existing variant) rather than assuming `Reports`.
3. Decide explicitly what "known" means for the sticky-latch behavior: does capability get re-evaluated on later frames until the install/trust outcome is available (bounded — install/trust presumably completes quickly after spawn), or is it resolved once, synchronously, before the first delivery attempt can even begin? The existing sticky-OR latch (`can_report_prompts |= ...`) is a **should-only-improve** ratchet — reusing it correctly (never latching `true` from stale/unknown state) is likely simpler than replacing it, but confirm against the actual read/write sites before assuming.
4. Preserve the deadline-abandon behavior for panes that genuinely can't report (unchanged) — this PRD narrows *when* a pane is classified `Reports`, it does not change what happens once it legitimately is.

## Scope

### In Scope

- Threading the native hook install/trust outcome from `codex_spawn_prep` to wherever `delivery_capability`/`ConfirmationCapability` resolution reads pane capability.
- Changing capability resolution so `Reports` requires a *known-successful* hook, not merely a capable agent type.
- A regression test: simulate (or directly force) a hook install/trust failure for a `Codex`-type pane, and assert `ConfirmationCapability` resolves to something other than `Reports` (and that the deadline path finalizes honestly rather than abandoning the remit).
- A regression test for the unaffected path: a Codex pane whose hook installs/trusts successfully still resolves `Reports` exactly as today — no behavior change for the healthy case.
- Changelog fragment describing the behavior change (a Codex pane with a broken native hook no longer gets its remit permanently abandoned; it finalizes honestly like a non-reporting agent).

### Out of Scope

- **Anything from the original #254 framing (LEVEL, `CONFIRMATION_GRACE_PERIOD`, retry timing).** That defect doesn't exist on `main`; not touched here.
- **fork#256's M2 (mode-seed retry/confirmation parity).** Independent PRD; unblocked by this issue's rescoping (see the note in `prds/fork-256-modeseed-phase-machine.md`'s Sequencing section), not something this PRD needs to coordinate with beyond that.
- **Issue #439 (macOS e2e harness `/var` symlink defect).** Filed separately; unrelated to this fix, and this PRD's own real-agent test coverage may be affected by it — see Risks.
- **Any other agent type's capability resolution** (`ClaudeCode`, `OpenCode`, `Devin`, `Pi`). This PRD's evidence is Codex-specific (its hook install/trust is the concrete failure mode found); if the same "resolved from type alone" pattern applies to another type's own hook mechanism, that's a follow-up, not silently bundled in here.

## Success Criteria

1. A Codex pane whose native hook install/trust fails is **not** classified `Reports`; at the delivery deadline it finalizes honestly (trusts the single write) rather than permanently abandoning the tab's remit.
2. A Codex pane whose native hook install/trust succeeds is classified `Reports` exactly as before — no regression to the healthy path's behavior or timing.
3. `pane_input_023`, `pane_input_026`, `pane_input_033` (the existing confirmation/capability test coverage this PRD's investigation relied on) continue to pass unmodified.
4. The new regression test is a genuine reproduction (forces or simulates an actual hook-install failure), not an assertion against a mocked-in capability value that begs the question.

## Milestones

### M1 — thread the real outcome

- [x] `codex_spawn_prep`'s install/trust result is surfaced to whatever state `delivery_capability` reads, not only logged (`AppState::codex_hook_trust_failed`).
- [x] `delivery_capability`/`ConfirmationCapability` resolution for `Codex` requires a known-successful hook outcome for `Reports`, resolving to `Unknown` otherwise — for the **TUI-attached path** (`delivery_capability`, `src/ui.rs`). The dispatch/scheduler path (`drain_pre_write_events`, `src/spawn.rs`) still classifies from agent type alone — tracked as [#459](https://github.com/prageethw/dot-agent-deck/issues/459), out of this PRD's scope.
- [x] RED test: forced hook-install failure → capability is not `Reports` → deadline path finalizes honestly (`codex_spawn_prep_ok_zero_hooks_is_not_confirmed` and the full plumbing-chain coverage added during review).

### M2 — no regression on the healthy path

- [x] GREEN test: successful hook install/trust → capability resolves `Reports` exactly as today (`delivery_capability_still_reports_a_codex_pane_with_no_recorded_hook_failure`).
- [x] `pane_input_023`/`026`/`033` pass unmodified.

### M3 — close honestly

- [ ] Issue #254 does **not** close — the TUI-attached path is fixed, but the dispatch/scheduler path ([#459](https://github.com/prageethw/dot-agent-deck/issues/459)) still carries the symptom. Issue #254 stays open until that lands too.
- [x] Changelog fragment (`changelog.d/254.bugfix.md`).
- [x] Rule 12: answered during review — no `.breaking.md` needed. This is internal Codex-pane classification logic within a single build (no `PROTOCOL_VERSION` change, no wire/frame shape change, and the new state key is not on `main` in any released build yet, so no released peer's values can be reinterpreted).

## Key Files

- `src/prompt_delivery.rs:62` — `AUTOMATIC_PROMPT_DEADLINE`
- `src/prompt_delivery.rs:112` — `MAX_PAYLOAD_SUBMISSIONS`
- `src/prompt_delivery.rs:336` — `agent_reports_submitted_prompt`
- `src/prompt_delivery.rs:357` — `ConfirmationCapability`
- `src/prompt_delivery.rs:479` — `unconfirmed_retry_delay`
- `src/ui.rs:4077` — `prompt_submission_evidence`
- `src/ui.rs:4693` — `deliver_orchestrator_prompt`
- `src/ui.rs:4793-4799` — the deadline abandon-vs-finalize branch, gated on `ConfirmationCapability`
- `src/ui.rs` `abandon_orchestrator_prompt`, `orchestration_remit_abandoned`
- `src/wrap.rs` — `Emitter::emit_fork_session_start` (unconditional spawn-time emission), `Emitter::emit_with_metadata` (the `user_prompt: None` hardcode on the degraded path)
- Codex spawn prep — `auto_install()` / `trust_deck_hooks_in()` (module confirmed during investigation as `src/codex_hooks_manage.rs`; verify exact function name/location at implementation time)
- `src/ui.rs:36083` — `pane_input_026_only_matching_pane_and_prompt_confirm_delivery`
- `src/ui.rs:34532` — `pane_input_023_orchestrator_write_is_provisional_until_confirmation`

## Risks and Mitigations

| Risk | Mitigation |
|---|---|
| Forcing a real hook-install failure for the RED test is hard to do deterministically (it's an external `codex` CLI installation step) | Prefer a unit-level test that injects the failure at the seam between `codex_spawn_prep`'s outcome and capability resolution, rather than an end-to-end real-Codex test — matches how `pane_input_026` already tests this area (deterministic, no real agent). A real-Codex e2e confirmation is nice-to-have, not required, and is additionally blocked right now by issue #439 on macOS. |
| The sticky-OR latch (`can_report_prompts |= ...`) is reused incorrectly and ends up permanently latching `Unknown`→`Reports` from a stale read | Read every write site of `can_report_prompts` before changing resolution logic — this PRD's M1 should not assume the existing latch composes correctly with a deferred outcome without checking. |
| Threading the hook outcome touches spawn-time state shared with other agent types | Scope the change to the Codex-specific resolution path only (see Out of Scope); do not generalize to other types speculatively. |

## Open Questions

- **Where exactly does `codex_spawn_prep`'s result need to land** for `delivery_capability` to read it — a new field on existing per-pane state, or a new event? Needs a few minutes reading the actual call graph between the two before M1 starts (flagged rather than guessed here, per this PRD's own investigation not having traced that specific wiring yet — the investigation confirmed *that* the gap exists and *why*, not the exact plumbing for the fix).
