# PRD #499: Show external-wait monitoring as active work, not idle

**GitHub Issue**: [#499](https://github.com/prageethw/dot-agent-deck/issues/499)

**Priority**: Medium (filed `needs-triage` — genuinely uncertain; this is an observability/trust improvement, not a functional defect, so it competes with other work rather than blocking anything)

**Created**: 2026-08-19

**Status**: Not started

## Problem Statement

When a role is waiting on an external dependency — CI, a GitHub check, another agent/worker, an external build/deploy, an approval or status transition — it should not read as idle while it is actively responsible for noticing that dependency resolve. A user glancing at the dashboard mid-wait should be able to tell "still waiting on something" from "genuinely done."

**The mechanism already exists, and partially covers this — investigated before writing this PRD, not assumed.** The deck derives `Working` from live process activity, not from any tracked concept of "this pane has outstanding external work": `descendant_shell_activity` (`src/platform/proc/scan.rs`) scans a pane's shell for a live descendant process matching `CLAUDE_BASH_TOOL_SHAPE`, sampled by a poll loop (`run_shell_activity_monitor`/`run_shell_activity_monitor_with`, `src/daemon.rs:1355`/`:1377`) that classifies each pane via `AgentPtyRegistry::shell_activity_candidates` + `classify_shell_activity` (`src/agent_pty.rs:5769`/`:5810`) and sets `crate::state::SessionStatus::Working`/`::Idle` accordingly. This is PRD fork#370/#386 (descendant-scan shell-activity signal), already shipped.

**What it covers today:** a pane blocked inside one sustained foreground command — an agent running `gh run watch`, or a poll loop kept in the foreground — genuinely shows `Working` for the whole duration, because that command is a live descendant process the whole time. This is real, already-shipped behavior, confirmed by reading the code rather than assumed from memory.

**What it misses — verified against a real incident, not hypothetical:**

1. **Discrete/gapped polling.** During a fork↔upstream sync (`docs/develop/fork-sync-workflow.md`) on 2026-08-19, the orchestrator dispatched `ci.yml`/`e2e.yml` via `gh workflow run` and then polled `gh run list` for the result — but as **separate, short-lived Bash calls** interleaved with other work (answering questions, filing an issue), not as one sustained blocking command. Each individual poll finished in well under a second, so there was never a live descendant process between polls for the deck to see. `worker-agent-deck daemon status --json` showed every agent `Idle` for the several minutes CI actually ran.
2. **Work that outlives the pane's own task.** Once a delegated worker (coder, tester, …) finishes its own task and calls `work-done`, its pane has nothing running at all — even when something that worker triggered (a CI run it dispatched, a review it requested) is still in flight and someone is still waiting on the outcome. There is no local process left to be a proxy for that wait.

Both gaps share a root cause: `Working` is inferred from **"is there a live foreground process right now,"** not from **"is this pane/role still responsible for an outstanding external outcome."** Those two things usually correlate (an agent that cares about a result often blocks on it), but they are not the same fact, and the difference is exactly where this PRD sits.

### Expected behaviour

```text
Agent starts external operation
        ↓
External work still running
        ↓
Agent polls/checks status
        ↓
Agent remains WORKING
        ↓
External work completes
        ↓
Agent continues next task
```

The role responsible for monitoring the external dependency should continue to appear as **working**, regardless of whether it happens to be blocked in one long foreground call or checking back in discrete steps — and regardless of whether its own delegated task has technically already been reported done.

## Scope

Focused on **status/activity classification** only. Do **not** redesign the task scheduler, the daemon's shell-activity poll loop's sampling strategy, or any external integration architecture (CI dispatch, GitHub API usage, etc.) — those are out of scope; this PRD only changes what counts as "working" and how that's signaled and cleared.

### In Scope

- An explicit, first-class **monitored-wait** state (or equivalent activity signal) distinct from the existing process-derived `Working`/`Idle` — something a role can enter/exit deliberately rather than relying on incidental shell activity.
- A mechanism for a role to declare "I am waiting on external outcome X" and later clear it (success, failure, cancellation, timeout — all four terminal cases, not just success).
- Attribution: the monitored-wait state is associated with the role/pane actually responsible for the dependency, not a global flag.
- Coexistence with the existing `descendant_shell_activity` signal — a pane can be `Working` from either source; this PRD adds a second source, it does not replace the first.
- Status/tab indicator updates so this reads consistently wherever `Working`/`Idle` is currently shown.
- Regression coverage for at least two distinct external-dependency kinds (CI polling is the concrete motivating case; pick a second — e.g. waiting on another delegated worker — to prove the mechanism generalizes rather than being CI-specific).

### Out of Scope

- Any change to how CI is dispatched, polled, or read (rule 8's "read the job log, not the run conclusion" guidance is unaffected).
- A general task-scheduler redesign.
- Surfacing *what* the external thing is in detail (e.g. a CI run URL) in the status model — that's a display/UX decision for implementation, not a scope commitment here.

## Technical Approach

Not designed here — left for the implementer, per the milestone list below. Two shapes worth naming as starting candidates, neither chosen:

- **A CLI verb** (analogous to `worker-agent-deck issue claim`) a role invokes to declare/clear a monitored wait, recorded daemon-side and read the same way `SessionStatus` is read today.
- **An extension of the existing shell-activity classification** so a role can register a *logical* wait (not tied to a specific process) that the same poll loop consults alongside `descendant_shell_activity`.

Whichever is chosen, it must handle all four terminal outcomes of an external wait (success, failure, cancellation, timeout) — clearing only on success would leave the status wedged exactly the way a stale claim wedges dispatch in PRD #421/#464's design, which is worth reading for the "derive, don't maintain" caution about state that has to be remembered to be cleared.

### Key Files

| File | Why |
| --- | --- |
| `src/platform/proc/scan.rs` | `descendant_shell_activity`, `MEASURED_SHELL_TOOL_SHAPES`, `CLAUDE_BASH_TOOL_SHAPE` — the existing process-activity signal this PRD adds a second source alongside. |
| `src/agent_pty.rs` | `AgentPtyRegistry::shell_activity_candidates` / `classify_shell_activity` (`:5769`/`:5810`) — where per-pane classification happens today; likely where a second signal gets merged in. |
| `src/daemon.rs` | `run_shell_activity_monitor` / `run_shell_activity_monitor_with` (`:1355`/`:1377`) — the poll loop driving the classification; `crate::state::SessionStatus::{Working, Idle}` is the status enum being set. |
| `src/state.rs` | Wherever `SessionStatus` is defined/transitioned — the natural home for a new state variant if that's the chosen shape. |

## Milestones

- [ ] **M1** — Confirm the current `Working`→`Idle` transition logic (the poll loop + classification path named above) and write down precisely where a new signal would plug in.
- [ ] **M2** — Confirm what, if any, external-wait/polling state already exists in the runtime beyond `descendant_shell_activity` (check for anything from PRD #370/#386 or elsewhere that this might already partially cover) before adding a new mechanism.
- [ ] **M3** — Design and add the explicit monitored-wait signal (CLI verb, extended classification, or another shape — pick one and justify it against the two candidates above).
- [ ] **M4** — Wire declare/clear into at least the CI-polling case (the motivating incident) so a role checking CI status in discrete steps reads as `Working` throughout.
- [ ] **M5** — Attribute the monitored-wait to the correct role/pane — never a global "something somewhere is working" flag.
- [ ] **M6** — Confirm genuinely passive/inactive agents are unaffected and still classify as `Idle`.
- [ ] **M7** — Handle all four terminal outcomes (success, failure, cancellation, timeout) — the wait must clear on every one, not just success, or a failed/cancelled/timed-out wait leaves the status wedged.
- [ ] **M8** — Status/tab indicators reflect the new state consistently everywhere `Working`/`Idle` is currently shown.
- [ ] **M9** — Regression tests for CI polling plus at least one other external-dependency kind (e.g. waiting on another delegated worker), so the mechanism is proven general rather than CI-specific.

## Success Criteria

```text
actively monitoring external dependency → WORKING
doing nothing / no active responsibility → IDLE
```

Concretely: a role that dispatches an external check and polls for it in discrete steps (not one blocking call) reads as `Working` for the whole wait, on all four terminal outcomes, attributed to the correct pane, with no change to genuinely idle agents.

## Risks and Mitigations

| Risk | Mitigation |
| --- | --- |
| A monitored-wait flag that isn't reliably cleared wedges a role as permanently `Working` — the same shape as PRD #421/#464's stale-claim hazard, just in the status model instead of dispatch. | M7 explicitly requires all four terminal outcomes to clear it, not just success. Consider whether a TTL/self-healing fallback (per #421/#464's "derive, don't maintain" lesson) is warranted here too. |
| Overlap/conflict between the new signal and the existing `descendant_shell_activity` signal produces flapping or contradictory status. | M1/M2 require understanding both mechanisms before adding a third; M3's design should state explicitly how the two compose (e.g. `Working` if *either* signal is active). |
| Scope creep into the task scheduler or external-integration architecture. | Explicitly out of scope (see above) — flag and stop rather than widen the PRD if implementation reveals a temptation to touch either. |

## Open Questions

1. **Declare/clear mechanism shape** — CLI verb vs. extended classification vs. something else. Left to the implementer (Technical Approach above); pick one and record why here once decided.
2. **Does this want a TTL/self-healing fallback**, mirroring PRD #421/#464's answer to the same class of "state that must be remembered to be cleared" problem? Worth deciding deliberately rather than defaulting to "no" by omission.
3. **Should the status surface *what* the external wait is** (e.g., a CI run URL) or just that one exists? Affects M8's display design; not committed to either answer here.
