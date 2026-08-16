# PRD fork#257: Make the retry backoff floor overridable, so M4's confirmation-retry can be proved instead of hoped for

**GitHub Issue**: [fork #257](https://github.com/prageethw/dot-agent-deck/issues/257)

**Priority**: Medium

**Status** *(2026-08-14)*: **Merged into the fork** — [PR #268](https://github.com/prageethw/dot-agent-deck/pull/268) (`eecb8576`); on `main` but not yet in a release tag. M1 and M2 complete, reviewed and mutation-proven. **M3 (offer upstream) is discharged as [fork #419](https://github.com/prageethw/dot-agent-deck/issues/419)** *(filed 2026-08-16, at archive time)* — rule 19 debt, not unfinished code.

**M1 — the override seam.** `send_retry_base()` extracted as a directly-callable accessor with the `cfg(any(test, debug_assertions))` pair, clamped to `SEND_RETRY_BACKOFF_CAP`, warn-once. The extraction and that ceiling were **decisions taken mid-flight**, not the PRD's original wording — see the Decision subsection under M1. A shared un-gated `SEND_RETRY_BASE_MS` keeps the release floor and the test fallback from drifting.

**M2 — the proof.** `orchestration/seed/017` asserts the clamp **against the accessor**, mutation-verified (mutating `SEND_RETRY_BASE_MAX` went RED on exactly that one test, run `31666931880`, then GREEN on revert). `/018` proves the retry fires deterministically once the floor is lowered. `orchestration/seed/015` is now a **positive** proof: a PTY relay suppresses the original write's CR — with byte counters proving exactly one byte differs between what was read and what was forwarded — so the retry's own bare CR is the sole possible cause of the single observed submission. Verified across five local real-agent runs including a mutation check.

**The standalone-CR-is-a-no-op risk is retired for Claude.** fork#197 recorded it as accepted-but-unverified; the retry's bare CR reliably submits the composer's already-typed text.

**`orchestration/seed/016` (Codex) is unchanged and still carries only the weaker "no duplication observed" claim.** The relay technique bypasses `dot-agent-deck wrap`, and Codex then never receives the seed at all — investigated across three real runs and filed as fork [#280](https://github.com/prageethw/dot-agent-deck/issues/280) with a concrete next attempt. Recorded rather than papered over, per this PRD's own rule that a partial proof honestly described beats a full one claimed and absent.

**Two findings this work produced, both filed:** fork [#284](https://github.com/prageethw/dot-agent-deck/issues/284) — the relay fixture has no regression net, and two real bugs in it (signal-handler re-entrancy, an unbounded final `waitpid`) were invisible to `fmt`, `clippy` and CI, surfacing only under manual signalling. And the observation that a test asserting against the constant a mutation would move is no test at all, caught twice in this PRD's own line of work.

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

`src/ui.rs:2239-2270` is the template, and it is followed literally — **with one supersession recorded in the "Decision" subsection under M1 below**: the override pair is not `send_retry_delay` itself, but a new directly-callable accessor, `send_retry_base()`, that `send_retry_delay` calls. `send_retry_delay`'s trailing `.min(SEND_RETRY_BACKOFF_CAP)` would otherwise make the clamp unobservable for any override at or above the cap, which is not true of `confirmation_grace_period()` (nothing caps it afterwards).

1. A `#[cfg(any(test, debug_assertions))]` `send_retry_base()` reading `DOT_AGENT_DECK_TEST_SEND_RETRY_BASE_MS`, clamped, warning **once** through a `static AtomicBool`.
2. A `#[cfg(not(any(test, debug_assertions)))]` twin returning today's fixed 500ms floor unchanged.
3. A documented clamp range — ceiling `SEND_RETRY_BACKOFF_CAP`, not `AUTOMATIC_PROMPT_DEADLINE` (see the Decision below M1) — justified in the doc comment the way `CONFIRMATION_GRACE_PERIOD_MAX`'s is.

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

- [x] `send_retry_delay`'s floor is extracted into its own directly-callable accessor, `send_retry_base()`, which splits into the `cfg`-gated pair, mirroring `confirmation_grace_period` line for line. **Superseded during the GREEN round** (below) from the PRD's original plan of gating `send_retry_delay` itself — the extraction is what makes the clamp observable at all.
- [x] `DOT_AGENT_DECK_TEST_SEND_RETRY_BASE_MS` is read, parsed, and **clamped** to a documented range. Zero is legitimate (retry as soon as the grace period allows). **Superseded**: the ceiling is `SEND_RETRY_BACKOFF_CAP`, not a value pinned below `AUTOMATIC_PROMPT_DEADLINE` — see "Decision: the clamp ceiling is `SEND_RETRY_BACKOFF_CAP`, not `AUTOMATIC_PROMPT_DEADLINE`" below.
- [x] Out-of-range values warn **once** via a `static AtomicBool`, not once per call.
- [x] The release build is byte-identical in behaviour — the override cannot be reached without `debug_assertions` or `cfg(test)`.

#### Decision: the clamp ceiling is `SEND_RETRY_BACKOFF_CAP`, not `AUTOMATIC_PROMPT_DEADLINE`

This supersedes the M1 wording above, which by analogy with `CONFIRMATION_GRACE_PERIOD_MAX` proposed pinning the base's clamp ceiling just below `AUTOMATIC_PROMPT_DEADLINE`. The tester writing `orchestration/seed/017` found the analogy does not hold, and correctly refused to decide it, since it is a production-code shape decision:

> `send_retry_delay`'s existing `.min(SEND_RETRY_BACKOFF_CAP)` (2 s) is applied *after* the base, for every `attempts` value — so for any out-of-range override the result is `2s` regardless of whether the override's clamp landed correctly, incorrectly, or was omitted entirely. A broken ceiling clamp and a correct one are indistinguishable through `send_retry_delay`'s return value alone.

`CONFIRMATION_GRACE_PERIOD_MAX`'s ceiling works as a testable clamp because `confirmation_grace_period()` **is** the directly-callable accessor, with nothing capping it afterwards. Pinning the base's clamp to `AUTOMATIC_PROMPT_DEADLINE` while leaving it buried inside `send_retry_delay` would copy that idiom's shape without its testability: `send_retry_delay(1)` for any override at or above `SEND_RETRY_BACKOFF_CAP` (2s) — whether the override is 3s or 3 minutes — always returns exactly `SEND_RETRY_BACKOFF_CAP`, so a broken clamp would be unobservable through it. That is the same class of defect as the seven surviving mutations filed upstream as fork #537.

The fix has two parts:

1. **Extract the clamped base into its own directly-callable accessor**, `send_retry_base()`, carrying the `cfg` pair, the env read, the clamp and the warn-once. `send_retry_delay` calls it and applies the shift and `SEND_RETRY_BACKOFF_CAP` as before — so the clamp itself is now callable and assertable independent of the shift/cap that sits downstream of it.
2. **Set the ceiling to `SEND_RETRY_BACKOFF_CAP` itself, not `AUTOMATIC_PROMPT_DEADLINE - 1ms`.** A base above `SEND_RETRY_BACKOFF_CAP` is unreachable by construction, since `send_retry_delay`'s `.min(SEND_RETRY_BACKOFF_CAP)` always wins regardless of how large the base is. Clamping the base to the cap is the honest bound — the largest value that can ever actually change `send_retry_delay`'s result — rather than a value borrowed by analogy from a sibling override whose downstream shape is different.

### M2 — the proof

- [x] `orchestration/seed/017` — unit: the override is honoured, clamped at both ends, and warns once. Mutation-checked: the upper-bound clamp is asserted directly against `send_retry_base()` (not only through `send_retry_delay`, whose trailing `.min(SEND_RETRY_BACKOFF_CAP)` made a broken or removed clamp indistinguishable from a correct one — reviewer P1), and changing `SEND_RETRY_BASE_MAX` was confirmed on CI to fail the test before being reverted.
- [ ] `orchestration/seed/018` — L1: with the floor lowered, a landed-but-unconfirmed write **deterministically** produces a second write whose payload is the empty string. This is the assertion that currently cannot be made.
- [x] `orchestration/seed/015` (real-agent, Claude) asserts **positively** that a retry fired, rather than `submissions.len() == 1`.
- [x] A scenario in which the original CR is suppressed, so the retry's CR is the **only** possible source of the submission — for **Claude only**. Implemented as a test-only PTY relay (`tests/fixtures/cr_suppressing_wrapper.py`, hosted via `orchestrator_command`) that drops the first `\r` seen on the daemon-to-agent direction and marks that it did so via a marker file `seed/015` asserts on — not the `pane_write`-trace-count or doubled-log-line approach this milestone originally proposed (both were checked and are structurally unavailable through this harness: no `pane_write`-level trace event exists to count, and `DOT_AGENT_DECK_LOG` does not reach a harness-spawned deck at all — `TuiDeckBuilder` calls `cmd.env_clear()` and forwards only `PATH` plus a pinned list, so the suggested "log line appears twice" assertion has no log to read). Confirmed on two consecutive real Claude runs (~22s each, deterministic — suppressing the original CR removes the race against the confirming hook event entirely, since nothing can ever confirm the suppressed write).
  - **Review-round hardening (P1/P1b, reviewer + auditor):** the marker originally proved only "the drop branch ran", not "a byte actually went missing" — a relay mutation that stopped removing the byte while leaving the marker-creation call intact would still pass. Fixed by making the evidence independent of the drop branch (Option 1 of the two the review offered, over narrowing the claim): the relay now tracks two cumulative byte counters, outside the drop branch, of bytes actually read from the daemon and bytes actually forwarded to the agent (the latter via a write-all loop that reports genuine completion rather than a single best-effort `os.write`, closing the separate short-write/EINTR gap P1b raised), and writes those counts into the marker instead of a flag. The assertion in `seed/015` now requires their difference to equal exactly 1. Mutation-checked locally: reverting the byte-removal slice while leaving `dropped_cr`/the marker write intact reproducibly fails with a reported difference of 0 (CI cannot witness this — `seed/015` self-skips there for lack of credentials — so it is rule 5 exception (a), local-only).
  - **Review-round hardening (P2a/P2b/P2c, reviewer + auditor):** the marker moved from a fixed global path keyed only on the sentinel to a fresh, private (0700), per-run `common::harness_tempdir()`, so no two runs can share a path (P2a); the relay validates `CR_SUPPRESS_MARKER` (required, absolute, parent directory must exist) before spawning the agent and creates the marker with `O_CREAT|O_EXCL`, closing a same-user symlink-race window a predictable path left open (P2c); relay teardown now signals the agent's whole process group and `waitpid`s for it instead of signalling only the direct child and exiting immediately, so no descendant of the real agent can survive relay teardown (P2b). Verifying this locally surfaced two real bugs in the fix itself, not caught by writing it, only by exercising it against a genuine agent process:
  1. A second SIGTERM/SIGHUP landing while `reap_process_tree` was still in its own bounded wait re-entered the handler and restarted cleanup on top of the still-running outer call, extending (not eliminating) the time to exit. Fixed by having the handler stop handling both signals (`SIG_IGN`) as its first action, before doing anything else; reproduced and confirmed with an ad hoc double-SIGTERM harness outside the cargo test suite (buggy: ~6.7s and growing with each extra signal; fixed: ~5.7s regardless of extra signals).
  2. Even with (1) fixed, a genuinely SIGKILLed real `claude` process was observed sitting in macOS's own "trying to exit" kernel teardown (`E` ps state) for well over a minute — a hardened-runtime/sandbox characteristic of the binary itself, nothing userspace can hurry along. The original unconditional final `waitpid` would have blocked the relay on this indefinitely, trading the orphan P2b exists to fix for a hung relay (and therefore a hung test). `reap_process_tree` now bounds the post-SIGKILL wait too: once elapsed, it gives up waiting on that specific child and exits anyway — the kill cannot be un-sent, so init/launchd reparents and reaps it whenever the kernel actually finishes, without the relay needing to observe that. The real agent's `argv[0]` is also resolved to an absolute, executable path before `pty.fork()` so a reordered `PATH` cannot substitute a different binary at the point the relay holds real credentials (audit), and the child closes every fd above stderr before `execvp` (audit P3, done since it was cheap).
  - **Codex gap, recorded per this milestone's own infeasibility clause:** the same relay technique was attempted against `orchestration/seed/016` and abandoned. Codex is the one `IntegrationStrategy::Wrapper` agent (`src/agent_registry.rs`) — its production path always runs through `dot-agent-deck wrap --agent codex -- codex …` (`wrap_launch_command`, `src/wrap.rs`), and routing `orchestrator_command` through the relay instead requires bypassing that rewrite (the relay must be the literal command the daemon spawns). With the wrapper bypassed, the relay was independently confirmed (ad hoc instrumentation) to forward the seed text and both CRs byte-for-byte, identically to the working Claude case — yet Codex never visibly reacted to any of it: no text appeared in its composer, no hook event fired, no sentinel. Codex's `Wrapper` strategy evidently provides something beyond PTY hosting and `CODEX_HOME` pinning (`run_wrap_pty`) that this round did not identify. A positive proof for Codex would mean nesting the relay INSIDE `dot-agent-deck wrap`'s own spawn (so its setup stays intact) rather than bypassing it — assessed as materially more real-agent iteration risk (each attempt costs a real, paid, ~2-minute Codex boot) than this round's budget covered, and not attempted. `orchestration/seed/016` is unchanged and still only proves the older, weaker "no duplication observed, whichever branch ran" claim.
- [x] The module doc and catalog entries stop claiming what the file does not prove — corrected for both `seed/015` (now claims the stronger, positive proof it has) and `seed/016` (still correctly claims only the weaker one, now cross-referencing why).

### M3 — offer upstream

- [ ] Branch from `upstream/main` and open the PR there, per rule 19. This touches upstream #424's machine and is a test-determinism fix of the kind upstream takes.

## Success Criteria

1. A developer can make the confirmation-retry fire on demand, in a test, without editing production constants.
2. Release builds are unchanged — provable by the `cfg` gate, not by inspection.
3. At least one test fails if M4's bare-CR mechanism regresses to sending prompt text again.
4. No test claims a proof it does not deliver.

## Key Files

- `src/ui.rs` — `send_retry_base()`, the extracted directly-callable accessor carrying the override, and `send_retry_delay`, which now calls it.
- `src/ui.rs:2239-2270` — `confirmation_grace_period`, the idiom mirrored (with the ceiling decision above superseding the analogy for the clamp bound).
- `src/ui.rs:2276` (pre-change line numbers) — `SEND_RETRY_BACKOFF_CAP`, untouched by the shift/cap logic and now also the clamp ceiling's reference point (see the Decision above; `AUTOMATIC_PROMPT_DEADLINE` is not used here).
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
| The suppressed-CR scenario turns out to be infeasible against a real agent | **Materialized for Codex, not for Claude.** Claude's positive proof shipped (`seed/015`); Codex's did not — recorded above with the concrete evidence gathered, and `seed/016` stayed on the older, honestly-described weaker proof rather than claiming more. A partial proof honestly described beats a full proof claimed and absent — which is the defect this PRD exists to correct. |
| Two overrides interact confusingly (grace period **and** base) | The L1 test sets both explicitly and the doc comments cross-reference each other, stating that the effective earliest retry is `max` of the two. |
