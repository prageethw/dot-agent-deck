# PRD fork#257: Make the retry backoff floor overridable, so M4's confirmation-retry can be proved instead of hoped for

**GitHub Issue**: [fork #257](https://github.com/prageethw/dot-agent-deck/issues/257)

**Priority**: Medium

**Status**: Planning

**Parent**: [fork #197](https://github.com/prageethw/dot-agent-deck/issues/197) — the seed/prompt delivery state machine, merged as PR [#219](https://github.com/prageethw/dot-agent-deck/pull/219). This PRD carries the M4 verification gap that PR deliberately deferred.

**Related**: fork [#194](https://github.com/prageethw/dot-agent-deck/issues/194) (the duplicate-seed bug M4 fixes) · fork [#254](https://github.com/prageethw/dot-agent-deck/issues/254) (the wrapper-strategy confirmation signal — natural to sequence alongside, but **not** a dependency; see Sequencing) · fork [#256](https://github.com/prageethw/dot-agent-deck/issues/256) (the mode-seed path's surviving silent-loss half) · [upstream #424](https://github.com/vfarcic/dot-agent-deck/issues/424) (the delivery-confirmation mechanism all of this extends)

**Fork-only?** **No — this is upstream-worthy.** `send_retry_delay`, `confirmation_grace_period` and the whole confirmation cycle are upstream #424's machine, living in upstream code. Per CLAUDE.md rule 19 the default is to branch from `upstream/main` and offer it there. See [Milestone M3](#m3--offer-upstream).

## Problem Statement

PRD fork#197's **M4** is the fix for fork #194: a confirmation-retry now sends a **bare CR** instead of re-writing the prompt text, so a prompt already sitting in the composer cannot be duplicated by the attempt to confirm it. The PRD calls M4's lost-write half *"the single most important thing for review and audit to probe."*

`tests/e2e_orchestration_seed_retry_real.rs` is named for that retry and presented as the L2 real-agent proof of it. **It cannot prove it, and its own module doc says so** — corrected during review, and commendably honest:

> the retry usually does NOT fire. Measured directly: the retry fired at +503ms in one run and not at all in another. […] this file proves no duplication was observed in WHICHEVER branch actually ran on a given execution, not that the confirmation-retry's standalone-CR dedup path was positively exercised.

So most executions verify only the no-retry happy path — which `orchestration/seed/011` already covers. **M4's bare-CR branch has no real-agent verification at all.** What it has is the L1 doubles (`orchestration/seed/005`, `/014`) and a mutation-verified unit test on `prompt_text_confirms`.

### Why it cannot fire deterministically today

The retry is gated on **two** conditions, and the existing test override moves only one:

| Gate | Overridable? | Effective value under test |
|---|---|---|
| `now - awaiting.since >= confirmation_grace_period()` | **yes** — `DOT_AGENT_DECK_TEST_CONFIRMATION_GRACE_PERIOD_MS` | 250 ms |
| `!backed_off`, where `send_retry_delay(1)` is a fixed floor | **no** | 500 ms |

`send_retry_delay`'s floor is `const BASE_MS: u64 = 500` (`src/ui.rs:2287`). The earliest a retry can fire is therefore `max(250, 500) = 500 ms`, which means **`DOT_AGENT_DECK_TEST_CONFIRMATION_GRACE_PERIOD_MS` cannot change retry timing at all** — any value at or below 500 ms is a no-op.

Meanwhile the confirming event (`UserPromptSubmit` / `session.prompt`) fires at *submission* time, before any inference — a local hook round trip of tens of milliseconds. So on a healthy run confirmation lands well inside 500 ms and the retry never happens. The test is not flaky; it is structurally unable to reach the branch it is named for.

### The deferral reason did not hold

PR #219 recorded this rationale for not fixing it:

> Making that positive proof deterministic would require the backoff floor itself to become overridable, **which is production code and out of scope for a test-only change.**

`confirmation_grace_period()` **is** production code, and the same PR changed it on exactly those terms — `cfg(any(test, debug_assertions))`-gated, clamped to a documented range, warning once on an out-of-range value, compiled out of release builds entirely. Same idiom, same size, same release-build property.

**Either both are in scope or neither is.** The inconsistency is the finding, not the deferral.

## Solution Overview

**Extend the override idiom the codebase already has** — do not invent a second one.

`src/ui.rs:2239-2270` is the template, and it is followed literally:

1. A `#[cfg(any(test, debug_assertions))]` `send_retry_delay` reading `DOT_AGENT_DECK_TEST_SEND_RETRY_BASE_MS`, clamped, warning **once** through a `static AtomicBool`.
2. A `#[cfg(not(any(test, debug_assertions)))]` twin returning today's schedule unchanged.
3. A documented clamp range, with the ceiling justified in the doc comment the way `CONFIRMATION_GRACE_PERIOD_MAX`'s is.

The override moves only the **base**; the exponential shape (`BASE << (attempts-1)`, capped at `SEND_RETRY_BACKOFF_CAP`) and the cap itself are untouched, so a test that lowers the base still exercises the real backoff curve rather than a flattened one.

With the floor movable, the real-agent test stops asserting the retry-agnostic `submissions.len() == 1` and asserts **positively that a retry occurred** — by counting `pane_write` trace events, or by asserting the `"orchestrator prompt: write applied"` log line appears twice.

### The stronger test, which is the actual point

Proving the retry *fires* is necessary and not sufficient. To verify M4's real mechanism — **that a standalone CR submits what is already in the composer** — the test needs a scenario where the original write's CR is **suppressed or lost**, so the retry's CR is the only thing that could have produced the submission. Otherwise the two are unobservable apart, which is precisely why today's test cannot discriminate.

That also retires a risk PRD fork#197 records as accepted-but-unverified:

> If a standalone CR does not submit on those harnesses, M4 fixes duplicate delivery on Claude while making the retry a no-op there, which is arguably worse than the duplication it replaces.

## Scope

### In Scope

- The `send_retry_delay` override pair and its clamp constants, in `src/ui.rs`.
- A unit test pinning the clamp and the warn-once behaviour.
- An L1 test proving the retry fires deterministically once the floor is lowered.
- Rewriting `tests/e2e_orchestration_seed_retry_real.rs`'s assertions from retry-agnostic to positive, including the suppressed-CR scenario.
- Correcting the module doc and the `orchestration/seed/015`/`016` catalog entries, which currently describe a proof the file does not provide.

### Out of Scope

- **Changing the production schedule.** 500 ms / 1 s / 2 s stays exactly as it is in release builds. This PRD adds a test seam, not a retune.
- **The wrapper-strategy confirmation signal** — that is fork #254, and this PRD must not quietly become a second attempt at it.
- **The mode-seed path** — fork #256.
- **Removing LEVEL.** Blocked on #254; untouched here.

## Milestones

### M1 — the override seam

- [ ] `send_retry_delay` splits into the `cfg`-gated pair, mirroring `confirmation_grace_period` line for line.
- [ ] `DOT_AGENT_DECK_TEST_SEND_RETRY_BASE_MS` is read, parsed, and **clamped** to a documented range. Zero is legitimate (retry as soon as the grace period allows). The ceiling is pinned below `AUTOMATIC_PROMPT_DEADLINE` for the same reason `CONFIRMATION_GRACE_PERIOD_MAX` is: past that point the override is not a longer backoff, it is a silently disabled retry.
- [ ] Out-of-range values warn **once** via a `static AtomicBool`, not once per call.
- [ ] The release build is byte-identical in behaviour — the override cannot be reached without `debug_assertions` or `cfg(test)`.

### M2 — the proof

- [ ] `orchestration/seed/017` — unit: the override is honoured, clamped at both ends, and warns once. Must be mutation-checked: changing the clamp bound has to fail it.
- [ ] `orchestration/seed/018` — L1: with the floor lowered, a landed-but-unconfirmed write **deterministically** produces a second write whose payload is the empty string. This is the assertion that currently cannot be made.
- [ ] `orchestration/seed/015` (real-agent) asserts **positively** that a retry fired, rather than `submissions.len() == 1`.
- [ ] A scenario in which the original CR is suppressed, so the retry's CR is the **only** possible source of the submission. If this proves infeasible on a real harness, record why in this PRD rather than quietly dropping it — the risk it retires is a real one.
- [ ] The module doc and catalog entries stop claiming what the file does not prove.

### M3 — offer upstream

- [ ] Branch from `upstream/main` and open the PR there, per rule 19. This touches upstream #424's machine and is a test-determinism fix of the kind upstream takes.

## Success Criteria

1. A developer can make the confirmation-retry fire on demand, in a test, without editing production constants.
2. Release builds are unchanged — provable by the `cfg` gate, not by inspection.
3. At least one test fails if M4's bare-CR mechanism regresses to sending prompt text again.
4. No test claims a proof it does not deliver.

## Key Files

- `src/ui.rs:2286-2291` — `send_retry_delay`, the floor being made overridable.
- `src/ui.rs:2239-2270` — `confirmation_grace_period`, the idiom to mirror exactly.
- `src/ui.rs:2276` — `SEND_RETRY_BACKOFF_CAP`, untouched.
- `src/ui.rs:2299` — `AUTOMATIC_PROMPT_DEADLINE`, the clamp ceiling's reference point.
- `tests/e2e_orchestration_seed_retry_real.rs` — `orchestration/seed/015`, `/016`.
- `tests/CATALOG.md` — entries for `/015`, `/016`, plus new `/017`, `/018`.

## Sequencing

**This does not depend on #254**, despite the issue listing it as related. #254 concerns whether the *confirmation signal* can return false; this concerns whether the *retry branch* can be reached in a test. A sound signal is not needed to prove a retry fires — indeed the suppressed-CR scenario is easier to reason about while LEVEL is still in place, because the test controls the confirming event directly.

Doing #257 **first** is deliberate: it is the smallest of the three fork#197 follow-ups, and it produces the deterministic retry harness that #254 and #256 will both want when they come to prove their own changes.

## Rule 12 — cross-version contract

**No `PROTOCOL_VERSION` bump and no `.breaking.md` fragment.** This adds a `cfg`-gated environment override to a timing constant. It touches no TUI↔daemon frame, no handler contract, and no field meaning on a stable wire. Release builds do not read the variable at all, so no peer's behaviour changes.

The manual cross-version run is therefore **not** required. Recorded here explicitly rather than left implied — PR [#250](https://github.com/prageethw/dot-agent-deck/pull/250) exists because a milestone ticked on neither a run nor a waiver is the state rule 12 is designed to prevent.

## Risks and Mitigations

| Risk | Mitigation |
|---|---|
| The override widens a test-only surface into a shipped binary | `cfg(any(test, debug_assertions))` — the same gate already trusted for `confirmation_grace_period`. Release builds compile the reader out entirely. |
| Lowering the base flattens the backoff curve and the test proves something unrealistic | Only the base moves; the shift and `SEND_RETRY_BACKOFF_CAP` are untouched, so the curve keeps its shape. |
| The suppressed-CR scenario turns out to be infeasible against a real agent | Record the finding in this PRD and ship M2's other four items. A partial proof honestly described beats a full proof claimed and absent — which is the defect this PRD exists to correct. |
| Two overrides interact confusingly (grace period **and** base) | The L1 test sets both explicitly and the doc comments cross-reference each other, stating that the effective earliest retry is `max` of the two. |
