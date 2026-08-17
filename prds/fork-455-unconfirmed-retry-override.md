# PRD fork#455: Make `unconfirmed_retry_delay`'s floor overridable, so the confirmation-retry can finally be proved on the path that actually gates it

**GitHub Issue**: [fork #455](https://github.com/prageethw/dot-agent-deck/issues/455)

**Priority**: High

**Status** *(2026-08-17, filed)*: Not started.

**Parent**: [fork #257](https://github.com/prageethw/dot-agent-deck/issues/257) — closed once this PRD exists; carries the remaining half of #257's original ask.

**Related**: fork [#443](https://github.com/prageethw/dot-agent-deck/issues/443) (the sync audit that found PR #268's content had been silently dropped) · PR [#450](https://github.com/prageethw/dot-agent-deck/pull/450) (restored the *other* half — `send_retry_base()` and `orchestration_seed_017`) · [upstream #424](https://github.com/vfarcic/dot-agent-deck/issues/424) (the delivery-confirmation rewrite that is the actual reason this PRD exists)

**Fork-only?** Undetermined — likely upstream-worthy under CLAUDE.md rule 19, same reasoning as fork#257's own M3 (this extends upstream #424's machine, living in upstream code). Not evaluated yet; do this at M3 as fork#257 did.

## Problem Statement

Fork issue #257's original title — *"M4's confirmation-retry has no real-agent proof: the 500ms backoff floor is not overridable, so the retry branch is never deterministically exercised"* — is still true today, for the case that actually matters.

PR #268 (fork#257) fixed this once, by making `send_retry_delay`'s floor overridable (`send_retry_base()`) and adding two proof tests (`orchestration_seed_017`, `/018`). The 2026-08-15 upstream sync silently dropped that PR's content despite its merge commit landing (found during #443's audit, PR #445). PR #450 restored `send_retry_base()` and `seed/017` — but investigation during that restoration (documented in fork#257's own PRD, "Restoration note") found the fix no longer applies to the right function:

Since PR #268 (2026-08-13), **issue #424 rewrote the whole delivery-confirmation model.** The specific case #257's title is about — a write that landed but was never confirmed — is no longer gated by `send_retry_delay`/`send_retry_base` at all. Tracing `deliver_orchestrator_prompt` (`src/ui.rs:4693`): the `Applied`/`Queued`-but-unconfirmed retry is now scheduled by `schedule_unconfirmed_retry`, which calls `crate::prompt_delivery::unconfirmed_retry_delay` (`src/prompt_delivery.rs:479`) — a **separate function** with its own hardcoded schedule (500ms/1s/2s/4s/8s, capped 15s) and no override of any kind. Its own doc comment states explicitly it is *"Deliberately NOT `crate::ui::send_retry_delay`'s 2s-capped schedule … an unconfirmed write is the opposite case."*

`send_retry_base()` (now restored) genuinely serves a real, different purpose — the case where a target flatly **refused** the write (`HistoryOnly`/`Stale`/`NoLiveTarget`/`Err`). That override and its `seed/017` proof are correct and complete for that case. This PRD is about the other one.

The companion field `seed/018` originally asserted against — `ui.orchestration_awaiting_confirmation` — no longer exists on `UiState` either; it's been replaced by `ui.prompt_delivery: HashMap<String, PromptDelivery>` (keyed by pane id, not tab id), part of the same #424 rewrite. And the `confirmation_grace_period()` override `seed/018`'s doc comment said it "mirrors … line for line" is also gone — `SPAWN_TIME_READINESS_BUFFER` (a fixed `pub const`, `src/ui.rs:1825`) replaced it, with no env-var lever at all.

**Net effect**: nobody can make the unconfirmed-write confirmation-retry fire deterministically in a test today. It has L1 doubles proving the no-retry happy path (same shape as fork#197's `seed/005`/`/014` before it), and nothing else.

## Solution Overview

**Extend the override idiom fork#257 already restored — do not invent a second one.** `send_retry_base()` (`src/ui.rs`) is the current, live template: `cfg(any(test, debug_assertions))` pair, an env-var read, a documented clamp, a warn-once `AtomicBool`. Give `unconfirmed_retry_delay` (`src/prompt_delivery.rs`) the same treatment.

1. Extract `unconfirmed_retry_delay`'s floor into a directly-callable, `cfg`-gated accessor (mirroring `send_retry_base()`'s own shape — decide during M1 whether `unconfirmed_retry_delay`'s specific downstream cap logic requires the same "extract the base so the clamp is observable independent of what's applied after it" reasoning fork#257's own M1 Decision subsection recorded, or whether this function's shape makes that unnecessary).
2. A new env var (name TBD at M1 — something in the `DOT_AGENT_DECK_TEST_*` family, matching `DOT_AGENT_DECK_TEST_SEND_RETRY_BASE_MS`'s naming), read, parsed, clamped to a documented range, warning once on out-of-range.
3. Release build behavior unchanged — the override cannot be reached without `debug_assertions` or `cfg(test)`.
4. A new deterministic proof test, driving `deliver_orchestrator_prompt`'s actual unconfirmed-write retry path (via `ui.prompt_delivery`, not the removed `orchestration_awaiting_confirmation` field) and asserting the retry fires once the floor is lowered — the functional equivalent of what `seed/018` was for, adapted to the current model.

### Open question this PRD does not resolve up front: catalog numbering and the broader `orchestration/seed` gap

`tester`, while restoring `seed/017` for PR #450, found the **entire** `orchestration/seed` catalog category — `seed/001` through `seed/016`, not just `/017`/`/018` — is missing from current `main`'s `tests/CATALOG.md` and `src/ui.rs`/`tests/`. This is a broader loss than #443's audit caught (that audit was scoped to fork-only PRs merged before the 2026-08-15 sync; the `seed/001`-`016` family may predate even that, from fork#197 directly). Before starting M2 below, confirm whether:
- a) that broader loss is being tracked/restored elsewhere (check for a filed issue first, per rule 20, before assuming it isn't), in which case this PRD's new proof test should slot into the restored numbering, or
- b) it is not, in which case this PRD's new test gets fresh numbering independent of the old `seed/018` slot, and the broader gap gets its own follow-up issue if one doesn't already exist.

Do not let this question block M1 (the production override is independent of test numbering) — resolve it before M2 starts.

## Scope

### In Scope

- The `unconfirmed_retry_delay` override pair and its clamp constants, in `src/prompt_delivery.rs`.
- A unit test pinning the clamp and the warn-once behavior (mirroring `seed/017`'s own mutation-checked pattern — assert against the accessor directly, not only through the function that applies a cap afterward, per fork#257's own M1 Decision learning).
- A test proving the unconfirmed-write retry fires deterministically once the floor is lowered, driving the current `ui.prompt_delivery`-based model.
- Correcting `tests/CATALOG.md` and any relevant module docs to describe what actually exists, not what fork#257's original PR #268 assumed.

### Out of Scope

- **Changing the production schedule.** 500ms/1s/2s/4s/8s (capped 15s) stays exactly as it is in release builds.
- **Restoring `send_retry_base()`/`seed/017`** — already done, PR #450.
- **Restoring the real-agent e2e proof** (`orchestration/seed/015`/`016`, the CR-suppressing relay, `tests/e2e_orchestration_seed_retry_real.rs`) — a separate, larger gap (see "Open question" above); only pick this up here if scoping confirms it's the same restoration and cheap to fold in, otherwise it gets its own PRD.
- **The wrapper-strategy confirmation signal** (fork #254) or **the mode-seed path** (fork #256) — unrelated, do not conflate.

## Milestones

### M1 — the override seam on `unconfirmed_retry_delay`

- [ ] `unconfirmed_retry_delay`'s floor (or whichever internal constant actually gates its earliest-retry timing) is extracted into a directly-callable, `cfg`-gated accessor, mirroring `send_retry_base()`.
- [ ] The new env var is read, parsed, and clamped to a documented range with a stated rationale for the ceiling (do not blindly reuse `SEND_RETRY_BACKOFF_CAP`'s reasoning without checking whether `unconfirmed_retry_delay`'s own downstream cap — 15s — makes the same "clamp to the cap itself" argument apply, or a different ceiling is correct here).
- [ ] Out-of-range values warn once via a `static AtomicBool`, matching the existing idiom.
- [ ] Release build is byte-identical in behavior — the override cannot be reached without `debug_assertions` or `cfg(test)`.

### M2 — the proof

- [ ] Resolve the catalog-numbering open question above.
- [ ] A unit test pinning the new override, clamped at both ends, warns once — mutation-checked the way `seed/017` was (assert against the accessor directly).
- [ ] A test proving that with the floor lowered, a landed-but-unconfirmed write deterministically produces the retry — driving `deliver_orchestrator_prompt`'s actual current retry path through `ui.prompt_delivery`, not the removed `orchestration_awaiting_confirmation` field `seed/018` used to assert against.
- [ ] `tests/CATALOG.md` and any module docs updated to describe the current, real mechanism — not carry forward stale references to `confirmation_grace_period()`/`CONFIRMATION_GRACE_PERIOD_MAX` (both gone, per PR #450's own review findings on this exact class of mistake) or to the removed `orchestration_awaiting_confirmation` field.

### M3 — offer upstream (if applicable)

- [ ] Determine whether this is upstream-worthy per CLAUDE.md rule 19 (likely yes, per the same reasoning fork#257's own M3 used — this extends upstream #424's machine). If yes, branch from `upstream/main` and offer there; if the fork has already diverged enough that a clean offer isn't possible, follow rule 19's "fix on the fork, then offer" path and file the upstream-offer tracking issue at merge time (fork#257's own precedent: `#419`).

## Success Criteria

1. A developer can make the unconfirmed-write confirmation-retry fire on demand, in a test, without editing production constants.
2. Release builds are unchanged — provable by the `cfg` gate, not by inspection.
3. At least one test fails if the unconfirmed-write retry path regresses (e.g., stops firing, or fires on the wrong condition).
4. No test or doc comment claims a proof it does not deliver — the exact defect class this PRD exists to close, and the same class PR #450's review caught in the adjacent restoration (dangling references to removed code).

## Key Files

- `src/prompt_delivery.rs:479` — `unconfirmed_retry_delay`, the function to make overridable.
- `src/ui.rs:4693` — `deliver_orchestrator_prompt`, and `schedule_unconfirmed_retry`, which calls `unconfirmed_retry_delay`.
- `src/ui.rs` — `send_retry_base()` (restored by PR #450), the live template to mirror.
- `src/ui.rs` — `ui.prompt_delivery: HashMap<String, PromptDelivery>`, the current delivery-confirmation state (replaced `orchestration_awaiting_confirmation`).
- `tests/CATALOG.md` — wherever the `orchestration/seed` category ends up living once the numbering question is resolved.
- `prds/fork-257-retry-backoff-override.md` — full historical context on the override idiom, the clamp-ceiling reasoning, and the "Restoration note" documenting exactly why this PRD exists.

## Sequencing

Independent of fork#254 and fork#256, same reasoning as fork#257's own Sequencing section: this is about whether a *retry branch* can be reached deterministically in a test, not about the *confirmation signal*'s soundness. No dependency either direction.

## Rule 12 — cross-version contract

Expected **no** `PROTOCOL_VERSION` bump and **no** `.breaking.md` fragment — same shape as fork#257's own M1 (a `cfg`-gated environment override to a timing constant, touching no TUI↔daemon frame, no handler contract, no field meaning on a stable wire). Confirm this holds once M1's actual implementation is known, rather than assuming it verbatim from the precedent.

## Risks and Mitigations

| Risk | Mitigation |
|---|---|
| The override widens a test-only surface into a shipped binary | `cfg(any(test, debug_assertions))` — the same gate already trusted for `send_retry_base()`/`confirmation_grace_period` (historical). Release builds compile the reader out entirely. |
| `unconfirmed_retry_delay`'s clamp-ceiling reasoning doesn't transfer cleanly from `send_retry_base()`'s (different downstream cap value, different shape) | Don't copy the ceiling decision blindly — re-derive it against `unconfirmed_retry_delay`'s actual downstream logic during M1, the same way fork#257's own M1 Decision subsection had to re-derive its ceiling rather than copying `confirmation_grace_period()`'s by analogy. |
| The `orchestration/seed` catalog-numbering question turns into unscoped archaeology | Time-box it: check for an existing tracking issue (rule 20) before starting M2; if none exists and the scope of that broader loss looks large, file it as its own follow-up issue rather than letting this PRD's M2 absorb it. |
| This PRD's M2 rediscovers the same "test asserts against a constant a mutation would move" defect fork#257's own PRD flagged twice | Mutation-check the new unit test the same way `seed/017` was — assert against the new accessor directly, confirm a mutation to the clamp constant actually fails it before considering M2 done. |
