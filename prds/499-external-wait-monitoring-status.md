# PRD #499: Show external-wait monitoring as active work, not idle

**GitHub Issue**: [#499](https://github.com/prageethw/dot-agent-deck/issues/499)

**Priority**: Medium (filed `needs-triage` — genuinely uncertain; this is an observability/trust improvement, not a functional defect, so it competes with other work rather than blocking anything)

**Created**: 2026-08-19

**Status**: Resolved — via workflow guidance, no product/daemon change. See "Resolution" below.

## Problem Statement

When a role is waiting on an external dependency — CI, a GitHub check, another agent/worker, an external build/deploy, an approval or status transition — it should not read as idle while it is actively responsible for noticing that dependency resolve. A user glancing at the dashboard mid-wait should be able to tell "still waiting on something" from "genuinely done."

**The mechanism already exists, and partially covers this — investigated before writing this PRD, not assumed.** The deck derives `Working` from live process activity, not from any tracked concept of "this pane has outstanding external work": `descendant_shell_activity` (`src/platform/proc/scan.rs`) scans a pane's shell for a live descendant process matching `CLAUDE_BASH_TOOL_SHAPE`, sampled by a poll loop (`run_shell_activity_monitor`/`run_shell_activity_monitor_with`, `src/daemon.rs:1355`/`:1377`) that classifies each pane via `AgentPtyRegistry::shell_activity_candidates` + `classify_shell_activity` (`src/agent_pty.rs:5769`/`:5810`) and sets `crate::state::SessionStatus::Working`/`::Idle` accordingly. This is PRD fork#370/#386 (descendant-scan shell-activity signal), already shipped.

**What it covers today:** a pane blocked inside one sustained foreground command — an agent running `gh run watch`, or a poll loop kept in the foreground — genuinely shows `Working` for the whole duration, because that command is a live descendant process the whole time. This is real, already-shipped behavior, confirmed by reading the code rather than assumed from memory.

**What it misses — verified against a real incident, not hypothetical:**

1. **Discrete/gapped polling.** During a fork↔upstream sync (`docs/develop/fork-sync-workflow.md`) on 2026-08-19, the orchestrator dispatched `ci.yml`/`e2e.yml` via `gh workflow run` and then polled `gh run list` for the result — but as **separate, short-lived Bash calls** interleaved with other work (answering questions, filing an issue), not as one sustained blocking command. Each individual poll finished in well under a second, so there was never a live descendant process between polls for the deck to see. `worker-agent-deck daemon status --json` showed every agent `Idle` for the several minutes CI actually ran. (The Resolution below closes the *waiting* tail of this incident — a pane whose only remaining responsibility is the wait. It does not, and cannot, address the *other* work the orchestrator was doing in the gaps — answering questions, filing an issue — reading `Idle` on a non-shell tool call; that is a different defect, out of scope here.)
2. **Work that outlives the pane's own task.** Once a delegated worker (coder, tester, …) finishes its own task and calls `work-done`, its pane has nothing running at all — even when something that worker triggered (a CI run it dispatched, a review it requested) is still in flight and someone is still waiting on the outcome. There is no local process left to be a proxy for that wait.

Both gaps share a root cause: `Working` is inferred from **"is there a live foreground process right now,"** not from **"is this pane/role still responsible for an outstanding external outcome."** Those two things usually correlate (an agent that cares about a result often blocks on it), but they are not the same fact, and the difference is exactly where this PRD sat.

## Resolution

**No product or daemon code change was made.** The gap was in *how panes waited*, not in the deck's status model — the existing `descendant_shell_activity` mechanism already reports `Working` correctly for the entire duration a pane is blocked inside one sustained foreground command, and that gate (`pane_hook_session_id`, `src/daemon.rs:1857`) applies uniformly to every agent pane with a hook session, including the orchestrator's own pane, not only delegated workers.

The fix is **CLAUDE.md rule 28** ("Wait on Any External Result With One Sustained Foreground Command, Never Discrete Gapped Polls"): a general, durable instruction that whenever a pane's only remaining responsibility is waiting on an external/async result, it waits via one sustained foreground blocking call — a purpose-built primitive when one exists (`gh run watch`, `gh pr checks --watch`), or a single-call `while`/`until` + `sleep` loop otherwise — never a sequence of separate short calls with gaps between them. `docs/develop/fork-sync-workflow.md` was updated to model the pattern at the exact point the originating incident happened.

This addresses both of the gaps recorded above, with different strength — one mechanically, one conventionally, and that distinction matters more than it looks:

- **Discrete/gapped polling** (gap 1) — closed directly: the responsible pane now blocks in one call instead of checking in gaps.
- **Work that outlives the pane's own task** (gap 2) — **reframed, not mechanically closed.** Once a worker reports `work-done`, its own pane still reads `Idle`; rule 28's claim is that responsibility for the wait moves to whichever pane picks it up next (usually the orchestrator), not that the deck guarantees it. That pane only reads `Working` once it *actually* issues a blocking call — nothing obliges that pickup to happen immediately, so a real window can exist, between `work-done` and the next blocking call, where nothing reads `Working` even though the wait is still genuinely outstanding. Correct as a workflow convention; not a system property.

**A genuine strength of this resolution, worth naming rather than only implying via "on any outcome" above**: it mechanically eliminates the failure mode the *original* (rejected) design most worried about. A `SessionStatus` provenance flag that has to be explicitly cleared on all four terminal outcomes (success/failure/cancellation/timeout) can always fail to clear and wedge a pane permanently `Working` — the exact stale-claim shape PRD #421/#464 already warned about for a different kind of state. This resolution has no such flag: the signal is a live process, and a process stops existing on every one of those outcomes alike, with nothing to remember to clear. That is the strongest argument for this route over the original design, not merely the cheapest one.

**What was explicitly considered and rejected:** a new `SessionStatus` provenance flag, a `worker-agent-deck monitor start/stop` CLI verb, wire-protocol additions (`DaemonMessage::MonitoredWaitStart`/`End`), and TTL self-healing for a "state that must be remembered to be cleared" — this was the PRD's original direction before the simpler resolution was identified (the full original scope is preserved in this file's own git history, one commit back — `git show <sha-before-this-commit>:prds/499-external-wait-monitoring-status.md`). None of that machinery is needed: the existing mechanism, used correctly, already satisfies the PRD's own Success Criteria below.

**Out of scope for this resolution, tracked separately:** `.claude/skills/dot-ai-prd-full/SKILL.md` has the same "poll `gh pr checks <n>`" gap this rule fixes elsewhere, but it is a synced mirror from the upstream `dot-ai` project (CLAUDE.md rule 13) — a direct edit here would be silently clobbered on the next sync. Tracked as [prageethw/dot-agent-deck#559](https://github.com/prageethw/dot-agent-deck/issues/559); fixing it belongs through `/dot-ai-request-dot-ai-feature`, not a patch in this repo.

**What this resolution does not provide, stated plainly rather than left to be inferred from a closed issue**: no lint, test, or CI check turns red when a pane polls in gaps instead of following rule 28 — the symptom (a pane reading `Idle` while genuinely waiting) is silent and indistinguishable from a genuinely idle pane. #499 itself was found by a human noticing the dashboard, and that remains the only detector for any future recurrence, including the already-known one above.

## Success Criteria (met by convention under rule 28, not enforced by any mechanism)

```text
actively monitoring external dependency → WORKING
doing nothing / no active responsibility → IDLE
```

This holds for any pane that follows rule 28. It does **not** hold — exactly as it did not before this PR — for a pane that waits via gapped polling despite the rule; nothing in the product prevents that pane from reading `Idle` while genuinely responsible for an outstanding result. "Met" describes the convention now being correct and documented, not a system guarantee.

Concretely: a role that waits on an external check via one sustained foreground command (rule 28) reads as `Working` for the whole wait, on any outcome, for whichever pane holds responsibility at the time, with no change to genuinely idle agents — because the mechanism producing `Working` (`descendant_shell_activity`) already has these properties for any pane blocked in a live foreground process.

## Original Scope (superseded — kept for history)

The PRD originally scoped an explicit, first-class **monitored-wait** state distinct from the existing process-derived `Working`/`Idle`, a declare/clear CLI mechanism, four-terminal-outcome handling, TTL self-healing, and regression tests for at least two external-dependency kinds. All of this is superseded by the Resolution above — the process-derived signal already had the needed properties once panes wait correctly, so no new state, CLI surface, or wire protocol was required.
