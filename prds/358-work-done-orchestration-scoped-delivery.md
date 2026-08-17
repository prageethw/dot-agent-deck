# PRD #358: Scope work-done delivery to orchestration identity, not bare pane_id

**GitHub Issue**: [#358](https://github.com/prageethw/dot-agent-deck/issues/358)

**Priority**: High

**Status**: M1, M2 and M4 done — CI-confirmed on commit `93e983d2` (PR #440): `handle_work_done_refuses_a_stale_signal_from_before_a_daemon_restart` now passes (`build` job, `cargo nextest run --workspace`, `3382 tests run: 3382 passed, 0 skipped`), and the informational `e2e` job is also green (`9624 tests run: 9624 passed, 0 skipped`). M3 (close honestly) is done — see below.

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

- [x] `pane_registration_generation: HashMap<String, u64>` added alongside the other pane-scoped maps in `AppState`, incremented in `register_orchestration_role` (via the `reserve_registration_generation`/`confirm_orchestration_role` split — see M2 below — which performs the identical `.or_insert(0) += 1` arithmetic the original inline version did).
- [x] The generation is captured and threaded through to `WorkDoneSignal`. Redesigned mid-implementation (see M2): the first cut had the `work-done` CLI ask the daemon for the pane's *current* generation immediately before sending (`DaemonMessage::GetRegistrationGeneration`), which by construction could never disagree with itself and so never actually caught a re-registered pane. The generation is now reserved **before spawn** and injected into the worker's environment (`DOT_AGENT_DECK_REGISTRATION_GENERATION`, sibling to `DOT_AGENT_DECK_PANE_ID`), so the CLI reports the registration it was genuinely spawned under.
- [x] `handle_work_done` compares signal generation to current generation before resolving `pane_cwd_map`/`pane_role_map`, and refuses on mismatch per the Solution Overview (`src/state.rs` ~4334, unchanged by the M2 redesign).
- [x] New regression test reproducing the pane-reuse-across-orchestrations race (see In Scope), asserting refusal — `handle_work_done_refuses_a_stale_cross_orchestration_signal_after_pane_reuse` in `src/state.rs`.

### M2 — observability and existing-test parity

- [x] Stale-refusal logging in place (see `handle_work_done`'s refusal branch, `src/state.rs` ~4334-4360) and carries pane_id, role, and expected-vs-actual generation for triage. Mechanism redesigned to spawn-time capture (see M1) once the original read-at-send-time approach was found not to close the actual race: `state.rs` gained `reserve_registration_generation`/`confirm_orchestration_role` (split out of `register_orchestration_role`), `agent_pty.rs` gained `DOT_AGENT_DECK_REGISTRATION_GENERATION`, `main.rs`'s `work-done` CLI now reads that env var directly instead of round-tripping to the daemon, and `spawn.rs`/`daemon_protocol.rs` reserve the generation before spawn and inject it into the child's env at both production spawn call sites. The now-unused `DaemonMessage::GetRegistrationGeneration` variant and its request/response types were removed from `event.rs`, and the dead handler removed from `daemon.rs`.
- [x] `tests/e2e_orchestration_route_isolation.rs` and the `state.rs` #140/#361-era unit tests pass unmodified — no changes made to that test area; the redesign only touched the generation-carrying mechanism, not the routing maps PRD #140/#361 scoped.
- [x] Changelog fragment describing the behavior change: `changelog.d/358.breaking.md` (the original `358.bugfix.md` was folded into it as one combined narrative — see M3 for why this is `.breaking.md` rather than `.bugfix.md`).

### M3 — close honestly

- [x] Issue #358 closes with the actual mechanism fixed, not a rescope. M4's compound generation/boot-id key, the cross-restart test, and the B2 ordering fix are all in place and merged into this branch. **CI-confirmed on commit `93e983d2` (PR #440):** the previously-RED `handle_work_done_refuses_a_stale_signal_from_before_a_daemon_restart` passes (`build` job's `cargo nextest run --workspace`: `3382 tests run: 3382 passed, 0 skipped`); the informational `e2e` job is also green (`9624 tests run: 9624 passed, 0 skipped`). Read from the CI log's literal `Summary [...] N tests run: ...` line, not from the run's `conclusion` field (CLAUDE.md rule 8).
- [x] Rule 12 cross-version question answered explicitly: **no wire/frame shape change** — this rides the existing unversioned hook socket, and no `PROTOCOL_VERSION` bump is needed. It **is** a real behavioral break, though, which is why `changelog.d/358.breaking.md` exists rather than a `.bugfix.md`: a `work-done` CLI built before this change has no code path that populates a registration generation at all, so `WorkDoneSignal::generation` decodes via `#[serde(default)]` as `0` — and `0` never matches a live pane's real registration (those start at `1`, per `reserve_registration_generation`'s `.or_insert(0) += 1`). Concretely, this means **every** report from an old CLI is refused as stale by a new daemon, not only genuinely stale ones: the fail-closed design cannot distinguish "old binary, no field" from "delayed report from a torn-down orchestration" — both look identical on the wire (`generation: 0`). That is a real compatibility break for a mixed old-CLI/new-daemon pairing (silently drops every worker completion report), even though no field or frame shape moved, which is exactly the "same-wire, different-meaning" semantic-break case CLAUDE.md rule 12 calls out as `.breaking.md`-worthy on its own.

### M4 — redesign for cross-restart survival, and prove the real wiring in CI (added after independent reviewer + auditor findings on M1/M2, 2026-08-17)

Both reviewer and auditor, reviewing the whole diff independently, converged on the same two structural problems with M1/M2's mechanism — different reasoning paths, same conclusion:

1. **The gate cannot fire in production, and specifically cannot catch the scenario this PRD names as the real repro.** `confirm_orchestration_role` has exactly two production callers (`spawn.rs`, `daemon_protocol.rs`), each fed by a process-unique pane_id source (`next_pane_id`'s process-global counter, `mint_pane_id`'s per-process nonce+seq) — so within one daemon process, a pane_id is confirmed at most once and `pane_registration_generation[pane]` never exceeds `1`. The actual reuse mechanism this PRD's Problem Statement names is pane_id recycling **across a daemon restart** — but `pane_registration_generation` is an in-memory `HashMap`, so it resets to empty on restart exactly like `PANE_COUNTER`/the nonce source do. A pre-restart worker's signal carries generation `1`; the post-restart pane that reused its pane_id also confirms at generation `1`. They match. The stale signal is delivered. **Issue #358's own motivating scenario is not fixed by M1/M2 as implemented**, even though the mechanism is real and does add a same-process protection layer (both reviewers agree this is "still worth having").
2. **No test that runs in CI exercises the actual spawn→env→CLI→daemon wiring the fix depends on.** Every fast-tier test hand-seeds both the env var (or map entry) AND the signal's carried generation as the same literal value, so they pass by construction regardless of whether the real threading (`reserve_registration_generation` → env injection → CLI reads env → `WorkDoneSignal.generation` → `handle_work_done` comparison) is wired correctly at all. The only tests that spawn a real worker and would exercise that seam (`tests/e2e_orchestration_route_isolation.rs`, `e2e_codex_worker.rs`, `e2e_delegate_work_done_chain.rs`, `e2e_pi_orchestrator.rs`, `e2e_pi_worker.rs`) are `skip_unless!` credential-gated and self-skip in CI (CLAUDE.md rule 5's "19 real-agent files" carve-out). Auditor confirmed directly: deleting both the `env.push` injection and the `std::env::var` read in `main.rs` leaves every CI check green.

A third, related finding (auditor B2): a refused stale signal still cancels the current tenant's silence-watch/outstanding-delegation bookkeeping (`retire_silence_watch`/`retire_outstanding_delegation` in `agent_pty.rs`'s registry) **before** `handle_work_done`'s generation check runs — this was already known and filed as issue #444 for the general case, but this PRD's own fix makes the mixed-version case (an old `worker-agent-deck` binary on a worker's `$PATH` during a rolling upgrade — the documented normal state during an upgrade, since `generation: 0` never matches) a **guaranteed, undetectable hang on every such delegation**: no report is written, no orchestrator feedback fires, and the idle-worker nudge that would otherwise catch a silent worker was already cancelled by the very signal that then got refused.

**Scope for M4:**

- [x] Make the generation survive a daemon restart, or make it otherwise impossible for a pre-restart and post-restart registration to compare equal. **Chose the compound-key approach** (over persisting `pane_registration_generation` to disk): a new `DaemonBootId` newtype (`src/state.rs`) minted fresh every time one is constructed, stored as `AppState::daemon_boot_id` (private field, `pub fn daemon_boot_id(&self) -> &str` accessor). Deliberately hand-implements `Default` (not derived) using the same recipe as `agent_pty::mint_pane_id`/`mint_orchestration_id` (pid + epoch nanos hashed, plus a monotonic per-process `AtomicU64` sequence) but WITHOUT their `OnceLock` caching — each call mints a genuinely fresh value, because `AppState::default()` is called once per real daemon process but many times across the test suite (including twice within the SAME real OS process to model two different daemon restarts — see the new test below), and a process-cached nonce would hand every `AppState` in one test binary the same value, silently defeating the whole key. No new dependency (no `uuid` crate) — reused the existing pid+nanos+seq idiom already established in `agent_pty.rs`. Threaded exactly like the generation: reserved/read alongside `reserve_registration_generation` (`AppState::daemon_boot_id()`), injected into the child's env as `DOT_AGENT_DECK_DAEMON_BOOT_ID` (sibling to `DOT_AGENT_DECK_REGISTRATION_GENERATION`) at both production spawn sites (`spawn.rs`'s `pane_env`/`spawn_one`, `daemon_protocol.rs`'s `StartAgent` handler), read back by `main.rs`'s `work-done` CLI, and carried on `WorkDoneSignal::daemon_boot_id` (`#[serde(default)]`, defaults to `""`, which no real boot id is ever minted as). `handle_work_done` now refuses unless BOTH the generation AND the boot id match.
- [x] Add at least one fast-tier test (CI-executing, not credential-gated) that exercises the REAL chain — calls `reserve_registration_generation`, then the actual env-injection path (or as close to it as a fast test can reach), then `confirm_orchestration_role`, then `handle_work_done` — rather than hand-writing the same literal on both sides of the comparison. This is what closes reviewer's F1 / auditor's B3. `handle_work_done_delivers_when_signal_carries_the_reserved_generation` (already existed for the generation) now also reads back `state.daemon_boot_id()` rather than hand-typing it. `pane_env_injects_the_daemon_boot_id_when_present` (new, `spawn.rs`) mirrors the existing `pane_env_injects_the_registration_generation_when_present` for the env-injection seam itself.
- [x] Fix the watchdog-cancellation-before-refusal ordering (auditor B2) so a refused signal does not silently disarm the current tenant's own silence-watch/outstanding-delegation tracking — at minimum for the mixed-version case this PRD's own fail-closed design makes newly common. **Landed a narrower fix here with a comment cross-referencing #444** (implementer's call): the compound generation/boot-id check now runs BEFORE `retire_silence_watch`/`retire_outstanding_delegation`, so a signal refused for a mismatched registration never touches the current tenant's bookkeeping. Deliberately left the "unknown pane" check (`pane_role_map` lookup) positioned AFTER the retire calls, unchanged from before — that's a different, narrower residual (a pane already unregistered but whose generation is unchanged) which is #444's own remaining scope; #444 stays open for it, not duplicated here.
- [x] Re-verify PRD Success Criterion 1 (the actual repro test) against the NEW mechanism, since M1's existing pinning test (`handle_work_done_refuses_a_stale_cross_orchestration_signal_after_pane_reuse`) hand-registers the same pane_id twice within one test — a sequence reviewer/auditor both confirmed no production path produces. Added `handle_work_done_refuses_a_stale_signal_from_before_a_daemon_restart` (`src/state.rs`), which builds two SEPARATE `AppState::default()` instances (a real cross-restart, not a same-process re-registration), confirms via a sanity assertion that the two instances' generations DO collide (both start pane "P" at generation 1 — the exact gap M1/M2 left open) AND that their `daemon_boot_id`s do NOT collide (the actual mechanism that closes it), then asserts a signal produced under the first instance is refused when delivered to the second. The M1 test is kept — it still pins the intra-process reuse case (same `AppState`, re-registered pane, generation changes but boot id does not) — this new test is the one that pins the actual cross-restart scenario Success Criterion 1 is about.
- [x] Once M4 lands, M3's "close honestly" box can be checked for real. Implementation and local gates (`cargo fmt --check`, `cargo clippy --workspace --all-targets --features e2e -- -D warnings`, `cargo xtask linkage-check`) are done; per this fork's CLAUDE.md rule 5 all test runs happen in CI, so M3's box is checked once CI confirms the previously-RED `handle_work_done_refuses_a_stale_signal_from_before_a_daemon_restart` now passes.

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
