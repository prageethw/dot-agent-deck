# PRD #358: Scope work-done delivery to orchestration identity, not bare pane_id

**GitHub Issue**: [#358](https://github.com/prageethw/dot-agent-deck/issues/358)

**Priority**: High

**Status**: Not started.

## Problem Statement

PRD #140 scoped the *routing decisions* — `delegate_targets` and `orchestrator_for_worker` (`src/state.rs:3878-3929`) — to `OrchestrationIdentity`, keyed via `pane_orchestration_map: HashMap<String, OrchestrationIdentity>` (`src/state.rs:572`). That fix is correct and still holds.

`handle_work_done` (`src/state.rs:4156-4366`) — the function that actually *delivers* a worker's report — was never brought into that same scoping. It resolves everything it needs purely from the bare `pane_id` carried on `WorkDoneSignal`:

- The report's destination directory: `self.pane_cwd_map.get(&signal.pane_id)` (`:4256`).
- The role label used in the write and in the orchestrator's feedback: `self.pane_role_map.get(&signal.pane_id)` (`:4225-4231`).
- The report filename: `work_done_file_name(role, pane_id)` → `pane_digest_hex(pane_id)` (`:797-818`), a hash of the bare pane_id string alone.

`register_orchestration_role` (`:3640-3662`) overwrites `pane_cwd_map`, `pane_role_map`, `pane_orchestration_map` and `orchestrator_pane_ids` together whenever a pane_id is re-registered — which happens routinely, because pane ids are small daemon-scoped integers that recycle (across a daemon restart, or whenever a worktree is torn down and a new one's role pane lands on the same slot). Nothing on `WorkDoneSignal`, and nothing in `handle_work_done`'s lookups, distinguishes "the tenant this signal was produced for" from "whichever tenant currently holds this pane_id." A `work-done` produced under one registration and delivered (or delayed) past the point where the pane_id is re-registered for a different orchestration is written into the new tenant's worktree, under the new tenant's role label, and reported to the new tenant's orchestrator — silent cross-delivery. `unregister_pane` (`:3790-3796`) clears all four maps together on a *clean* close, but there is no barrier against a late signal racing a fresh registration.

Fork PR #361 (merged) fixed an adjacent asymmetry in `orchestrator_pane_ids` only (a start-role registration inserted with no corresponding removal) — it does not touch the `pane_cwd_map`/`pane_role_map` keying this issue is about, and the issue was explicitly re-scoped to stay open after that PR landed.

Upstream PR #501 (`vfarcic/dot-agent-deck`) is adjacent, not overlapping: it fixes a *same-orchestration* stale-file-reuse bug (a failed write leaving the orchestrator pointed at an old report at the same role-keyed path) via a `WorkDoneProvenance` commission ledger. It does not validate the pane's *current* tenant against the tenant the signal was produced for, so it does not close this issue.

## Solution Overview

**Bind a `WorkDoneSignal` to the specific registration it was produced under, and refuse delivery — rather than silently rerouting it — when that registration is no longer current.**

Concretely:

1. Give each pane registration a generation token. The natural home is alongside the existing four maps `register_orchestration_role` already writes together — e.g. a `pane_registration_generation: HashMap<String, u64>`, incremented on every `register_orchestration_role` call for a given pane_id (including same-identity re-registration, so a torn-down-and-recreated worktree that happens to reuse both the same pane_id *and* the same `OrchestrationIdentity` still gets a fresh generation).
2. Capture that generation at the point a delegation is issued for a pane (wherever the worker's `work-done` invocation gets its context — likely at spawn/delegate time, so the generation travels with the worker process rather than being re-derived later), and have `worker-agent-deck work-done` echo it back on `WorkDoneSignal`.
3. In `handle_work_done`, before resolving `pane_cwd_map`/`pane_role_map`/`pane_orchestration_map` for delivery, compare the signal's carried generation against the pane's *current* `pane_registration_generation`. A mismatch means the pane has been re-registered since this signal's work began — the original tenant is gone and the current tenant is not who this report belongs to. Refuse to write into the current tenant's worktree or notify the current tenant's orchestrator.
4. Decide and implement what "refuse" means observably: at minimum, log the stale signal (pane_id, expected vs. actual generation, role) at a level that is triageable, and do not write any file or notification into the current tenant's context. Whether a stale report is discarded entirely or written to an orchestration-agnostic location for forensic purposes is an implementation decision to make explicit in the PRD's Open Questions before M1, not one to guess mid-implementation.

**Do not attempt to make the stale signal "arrive at the right orchestration instead."** By the time a pane_id has been reassigned, the original orchestration's worktree may already be gone (this is exactly the #358 repro: the worktree that would receive a correctly-attributed report may no longer exist). Refusing delivery to the *wrong* recipient is the fix; delivering to the *right* one, if it's even still around, is not in scope — see Out of Scope.

## Scope

### In Scope

- A generation token per pane registration, incremented on every `register_orchestration_role` call.
- Threading that generation from delegation/spawn time through to `WorkDoneSignal`.
- `handle_work_done` validating the signal's generation against the pane's current generation before using `pane_cwd_map`/`pane_role_map` to resolve delivery, and refusing (not rerouting) on mismatch.
- Observable, triageable logging for a refused stale delivery.
- A regression test that reproduces the actual failure mode: register pane P for orchestration A, capture a work-done signal's context, re-register pane P for orchestration B (simulating a worktree teardown + reuse), then deliver A's stale signal and assert it is refused rather than landing in B's worktree/role/orchestrator feedback.
- Updating the stale-asymmetry test area (`state.rs` ~6511-6560, ~6772-6794) if the new generation field changes what those tests need to set up.

### Out of Scope

- **Recovering or re-delivering a stale report to its original orchestration.** If that orchestration's worktree is already gone, there is nothing to deliver to; if it still exists, redelivery is a separate, harder feature (would need to resolve the *current* pane for that orchestration, which may not exist either). This PRD's bar is "don't misdeliver," not "always deliver correctly."
- **Upstream PR #501's stale-file-reuse fix.** Different bug, different code path in its finished form (`WorkDoneProvenance` ledger); not reused or blocked on here.
- **Changing `pane_orchestration_map`'s existing routing role** for `delegate_targets`/`orchestrator_for_worker` — those are already correctly scoped by PRD #140 and are not touched beyond whatever the generation field needs alongside them.
- **Preventing pane_id reuse itself.** Reuse is normal and expected (daemon restarts, worktree churn); the fix is detecting a stale tenant at delivery time, not eliminating reuse.

## Success Criteria

1. A `work-done` signal produced under registration generation N for pane P, delivered after pane P has been re-registered (generation N+1, any identity, including the same orchestration re-registering), is refused — never written into the current tenant's worktree, never attributed to the current tenant's role, never surfaces in the current tenant's orchestrator feedback.
2. A `work-done` signal delivered while its registration generation is still current is delivered exactly as today — no regression to the ordinary, non-racing path.
3. The refusal is observable (logged with enough detail — pane_id, role, expected vs. actual generation — to triage after the fact), not a silent drop indistinguishable from a signal that never arrived.
4. `orchestration_route_isolation` (`tests/e2e_orchestration_route_isolation.rs`) and the existing `state.rs` unit tests for `delegate_targets`/`orchestrator_for_worker`/`orchestrator_pane_ids` reuse continue to pass unmodified — this PRD adds a new axis of protection, it does not change the ones PRD #140 and fork PR #361 already established.

## Milestones

### M1 — generation token and refusal gate

- [ ] `pane_registration_generation: HashMap<String, u64>` added alongside the other pane-scoped maps in `AppState`, incremented in `register_orchestration_role`.
- [ ] The generation is captured and threaded through to `WorkDoneSignal` (exact mechanism — at spawn, at delegate, or read fresh by the worker CLI at `work-done` invocation time — is a design decision for the implementing worker to make and document, since it depends on how `worker-agent-deck work-done` currently learns its own pane_id).
- [ ] `handle_work_done` compares signal generation to current generation before resolving `pane_cwd_map`/`pane_role_map`, and refuses on mismatch per the Solution Overview.
- [ ] New regression test reproducing the pane-reuse-across-orchestrations race (see In Scope), asserting refusal.

### M2 — observability and existing-test parity

- [ ] Stale-refusal logging in place and manually verified to carry enough context to triage.
- [ ] `tests/e2e_orchestration_route_isolation.rs` and the `state.rs` #140/#361-era unit tests pass unmodified.
- [ ] Changelog fragment describing the behavior change (a stale cross-tenant work-done report is now refused rather than silently delivered).

### M3 — close honestly

- [ ] Issue #358 closes with the actual mechanism fixed, not a rescope.
- [ ] Rule 12 cross-version question answered explicitly: this changes daemon-internal state only (no wire/frame shape change), but changes *behavior* on a hook-adjacent path (a report that used to arrive now doesn't, for the stale case) — record whether a `.breaking.md` fragment is warranted or whether "adds a refusal for cases that were previously silent misdelivery, never previously-correct delivery" is sufficient justification for none.

## Key Files

- `src/state.rs:493-500` — `OrchestrationIdentity`
- `src/state.rs:548-572` — `pane_role_map`, `pane_cwd_map`, `pane_orchestration_map`
- `src/state.rs:3640-3662` — `register_orchestration_role`
- `src/state.rs:3790-3796` — `unregister_pane`
- `src/state.rs:3878-3929` — `delegate_targets`, `orchestrator_for_worker` (the already-correctly-scoped precedent to follow)
- `src/state.rs:3960-4140` — `handle_delegate`
- `src/state.rs:4156-4366` — `handle_work_done` (the function to fix)
- `src/state.rs:797-818` — `work_done_file_name`, `pane_digest_hex`
- `src/state.rs` ~6307-6560, ~6772-6794 — existing unit tests for this area
- `tests/e2e_orchestration_route_isolation.rs` — PRD #140 M5.1 coverage to preserve

## Risks and Mitigations

| Risk | Mitigation |
|---|---|
| Threading the generation through to the worker CLI touches the delegate/spawn/work-done contract | Keep the wire shape unchanged if possible (daemon-internal comparison only); if `WorkDoneSignal` must carry a new field, treat it as additive and answer M3's rule-12 question explicitly rather than assuming no bump is needed. |
| A legitimate same-identity re-registration (same orchestration, pane torn down and recreated) gets treated as "stale" and refuses a real report | Generation increments on every registration regardless of identity, which is intentional — a report that started before *any* re-registration, including same-identity, genuinely raced a restart and should not silently land as if nothing happened. If this proves too strict in practice, that's a finding for review, not an assumption to bake in up front. |
| Fix is implemented as a workaround beside the existing maps rather than a real gate | Success criterion 1 requires an actual reproduction test to pass, not a grep for an absent identifier — model this PRD's own M1 test after that discipline (see fork#256's own retrospective on gameable success criteria). |

## Open Questions

- **What does "refuse" mean observably beyond logging?** Discard entirely, or write to a non-orchestration-scoped forensic location? Decide before M1's test is written, since the test needs to assert on the actual behavior.
- **Where does the worker CLI currently learn its own pane_id**, and is the cleanest seam for carrying the generation at spawn time or at `work-done` invocation time? This needs a few minutes reading `src/agent_pty.rs`/the `work-done` CLI path before M1 starts — flagged here rather than guessed in this PRD.
