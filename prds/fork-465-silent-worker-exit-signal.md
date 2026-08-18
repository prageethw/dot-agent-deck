# PRD fork#465 — A worker that exits without calling `work-done` must produce a signal, not silence

**Issue:** [prageethw/dot-agent-deck#465](https://github.com/prageethw/dot-agent-deck/issues/465)
**Priority:** High
**Status:** Planning
**Related:** upstream [#205](https://github.com/vfarcic/dot-agent-deck/issues/205) (original ask, commented with this fork's repro), upstream [#590](https://github.com/vfarcic/dot-agent-deck/issues/590) (adjacent — commission-ledger expiry, separate unmerged work), PRD #126 (idle-worker watch), PRD #249 (delegate-silence watch)
**Fork-only?** No — this is a general `dot-agent-deck` limitation (CLAUDE.md rule 19). Fix here first, offer upstream once shipped, per the incident's own filing rationale.

## Problem Statement

The orchestration model in `.dot-agent-deck/orchestrator-context.md` depends on one guarantee: a delegated worker either calls `worker-agent-deck work-done`, or the orchestrator eventually hears about it going idle/silent. On 2026-08-18, a delegated `coder` worker completed real work — committed, pushed, opened a real PR — twice, and its agent session ended both times without ever calling `work-done`. The orchestrator received **no notification of any kind**. The only way the work was discovered was a human asking "why is no agent active?" and the orchestrator manually inspecting worktree/git/`gh` state.

This is not a missing feature; it is two existing detectors reaching the wrong conclusion in a case they were never built to distinguish.

### Root cause: the daemon already knows the process died — the delegation trackers don't ask

Two existing mechanisms are supposed to catch a worker going quiet:

- **PRD #126's idle-worker watch** (`src/state.rs:1386` `arm_idle_worker_watch_for_delegation`, `src/agent_pty.rs:3048` `arm_outstanding_delegation`) arms an `OutstandingDelegation` record with a `worker_response_timeout_minutes` sleep — **120 minutes by default**.
- **PRD #249's silence watch** (`src/state.rs:1871` `arm_delegate_silence_watch`, `src/agent_pty.rs:3105` `arm_silence_watch`) arms a `SilenceWatchRecord` on a shorter window (`min(worker_response_timeout, 30s)` by default), listening for hook events proving a turn happened.

Both are cancelled the same two ways: `handle_work_done` (`src/state.rs:4197`/`4178`, via `retire_outstanding_delegation`/`retire_silence_watch`), or `begin_pane_close`/`finish_pane_close` (`src/agent_pty.rs:3301`/`3329`) — reachable **only** from an explicit client `StopAgent` request (`src/daemon_protocol.rs:1582/1639/1744`).

Meanwhile, the daemon has an immediate, unconditional signal that a worker's process is gone: `pump_reader` (`src/agent_pty.rs:1378-1394`) observes `Ok(0)` (EOF) on the PTY read and sets `exited.store(true, ...)` (`:1392`) the moment the child process dies — clean exit, crash, or otherwise. This flag's **only** consumer is the daemon's own auto-shutdown idle monitor (`src/daemon.rs:1000-1060`), which asks "should the whole daemon exit," not "does any outstanding delegation need to be told its worker is gone."

**The two subsystems never talk to each other.** A worker that exits naturally — finishes its task, session ends — without calling `work-done` and without a client sending `StopAgent`, leaves its `OutstandingDelegation`/`SilenceWatchRecord` armed and sleeping for the full window, indistinguishable from a worker that is merely slow. That is exactly the reported symptom, and the mechanism is now fully explained rather than merely observed.

### What this is *not*

- **Not the commission ledger.** Upstream #501 (still open, unmerged as of this PRD) proposes a `WorkDoneProvenance`/commission-ledger mechanism to solve a different problem — "was this `work-done` solicited or not." Upstream #590 flags that ledger's own gap (no time-based expiry). Neither addresses process-exit detection; this PRD does not depend on either landing.
- **Not "forward the worker's last message."** Upstream #205 (the original ask this issue extends) proposed exactly that, and the maintainer's own comment on #205 explicitly declined it: a worker's trailing PTY output is "often not the deliverable — it can be a tool result, a partial thought, or unrelated to the task," and forwarding it risks the orchestrator mistaking noise for a report. PRD #249's own silence notice was deliberately built the opposite way — `compose_delegate_silence_notice` (`src/state.rs:1666`) carries **fixed daemon-authored text only**, no interpolated worker content, specifically to avoid that risk (and an injection concern raised in #249's own review). This PRD follows that precedent rather than reopening a considered-and-declined design.

## Decisions

| Question | Decision |
|---|---|
| Detect exit, or synthesize a report? | **Detect and signal, not synthesize.** Matches PRD #249's own precedent and upstream's stated reasoning on #205. |
| Where does the exit hook live? | **`pump_reader`'s EOF path** (`src/agent_pty.rs:1392`), the earliest and only unconditional "process is gone" signal already in the codebase. |
| What happens to an armed `OutstandingDelegation`/`SilenceWatchRecord` on exit? | **Retire immediately** (reuse `retire_outstanding_delegation`/`retire_silence_watch`, `src/state.rs:4197`/`4178`) instead of letting the timer run out, and fire a new fixed-text notice in the same place `compose_delegate_silence_notice` fires today. |
| New notice content | A third fixed daemon-authored variant, e.g. *"worker `<role>` (pane `<id>`) exited without calling work-done — its last known work: `<delegated task summary>`."* No PTY content interpolated, matching #249's injection-safety precedent. |
| Does this replace the timeout watches? | **No.** A worker can also hang *without* exiting (stuck, waiting on something). The timeout watches stay as the fallback; this PRD adds a fast, exit-triggered path alongside them, not a replacement. |

## Design

1. **`src/agent_pty.rs`'s `pump_reader`** (`:1378-1394`): on the `Ok(0)` EOF branch, after setting `exited`, look up the pane's outstanding delegation/silence-watch records (keyed the same way `begin_pane_close` already does) and, if either is armed, drive the same retirement + notice path `begin_pane_close`/`finish_pane_close` (`:3301`/`:3329`) already implement for the explicit-close case — reusing that logic rather than duplicating it.
2. **New notice composer** in `src/state.rs`, alongside `compose_idle_worker_prompt` (`:1314`) and `compose_delegate_silence_notice` (`:1666`): fixed text, no worker-authored content, states the role and the delegated task (the task text the orchestrator itself wrote — safe to echo back, since the orchestrator authored it).
3. **Delivery**: reuse the existing guarded-write path (`write_and_submit_guarded`/`write_notice_guarded`, `src/agent_pty.rs`) that both existing notices already use, so the same identity-guard and LF-vs-Enter-safety properties apply to the new notice.
4. **Race with a late, legitimate `work-done`**: `pump_reader` observing EOF and a nearly-simultaneous `work-done` call are both plausible near a worker's natural exit. Retirement must be idempotent (both paths already call `retire_*`, which should be a no-op on an already-retired record) so whichever arrives first wins cleanly, with no double notice and no lost `work-done`.

## Milestones

- [ ] M1 — `pump_reader`'s EOF path retires any armed `OutstandingDelegation`/`SilenceWatchRecord` for that pane and fires the new notice, reusing `begin_pane_close`/`finish_pane_close`'s existing retirement logic rather than duplicating it.
- [ ] M2 — New fixed-text "exited without reporting" notice composer, delivered via the existing guarded-write path; no worker-authored PTY content interpolated (upstream #205's declined design stays declined).
- [ ] M3 — Idempotent retirement confirmed under the late-`work-done`-vs-exit race (a record already retired by one path is a safe no-op for the other).
- [ ] M4 — Tests covering: a worker that exits after real work with no `work-done` call produces the new notice promptly (not after the full timeout window); a worker that calls `work-done` immediately before exiting produces no spurious second notice; the existing timeout-based paths (PRD #126/#249) are unaffected for a worker that merely hangs without exiting.
- [ ] M5 — CLAUDE.md's orchestrator-facing docs (`orchestrator-context.md` generation source in `.dot-agent-deck.toml`, or wherever the "wait for work-done" contract is documented) updated to describe the new signal, so an orchestrator reading its own seed prompt knows to expect it.
- [ ] M6 — Offer upstream per rule 19 once merged here (this is a general `dot-agent-deck` limitation, not fork-specific divergence) — comment on upstream #205 with the shipped approach and a link, since #205's own thread is where the "forward vs. signal" design choice was discussed.

## Test plan

L2 (PTY/real-process) is required here — this is fundamentally about detecting a real process's exit, which an L1 widget test cannot exercise. Candidate scenario: spawn a worker with a directive prompt and a cheap model (per CLAUDE.md rule 4's real-agent-test guidance), let it complete and exit its session without calling `work-done`, and assert the new notice lands in the orchestrator's pane within a bounded, short time — not the full 120-minute/30-second window. A synthetic/stand-in PTY test (a script that writes output and exits, no real agent) is also worth adding for the mechanical retirement-and-notice logic, faster and more deterministic than a real-agent run, with the real-agent test as the "as a user actually experiences it" bar rule 4 requires.

## Out of scope

- The commission ledger (upstream #501/#590) — separate, unmerged, solves a different question.
- Synthesizing or forwarding any worker-authored content in the notice — declined per upstream #205's own reasoning, not reconsidered here.
- Changing the timeout-based watches' default windows — unrelated to this fix.
