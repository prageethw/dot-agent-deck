# PRD #499: Show external-wait monitoring as active work, not idle

**GitHub Issue**: [#499](https://github.com/prageethw/dot-agent-deck/issues/499)

**Priority**: High (reopened 2026-08-26 — see "Reopened" note below)

**Created**: 2026-08-19

**Status**: M1-M10 complete (PR #617, 7 rounds — see Milestones below); ready to close

## Reopened 2026-08-26 — supersedes the rule-28-only resolution

This PRD was previously closed via PR #557, which added CLAUDE.md rule 28 ("wait via one sustained foreground blocking command") as a workflow-guidance-only resolution and explicitly rejected the mechanical approach below. That decision was reviewed twice (reviewer + auditor) and explicitly approved by the maintainer before merging.

The maintainer has now decided to pursue the mechanical fix after all. **Rule 28 stays in place** — it is a real, useful convention and remains correct advice for any pane that follows it. This PRD adds a **product-level guarantee** that does not depend on the calling agent knowing about or following a written rule: rule 28's own resolution text names the gap plainly — *"Nothing here is enforced or detected: no lint, no test, no CI check turns red when a pane polls in gaps instead of following this rule... a human noticing the dashboard remains the only detector."* That gap is what this PRD closes.

**Resolved from Open Question #1 (declare/clear mechanism shape):** a CLI verb, analogous to `worker-agent-deck issue claim`. Chosen over extending `descendant_shell_activity`'s classification because it's explicit and independently attributable per-pane without needing a live process to exist at all — the exact case ("work that outlives the pane's own task") rule 28 could only reframe, not close.

**A prior attempt at this same code path was started and abandoned** (branch `feat/499-monitored-wait-status`, claimed 2026-08-23, no PR ever opened) in favor of the simpler rule-28 resolution. No code from that attempt survives on disk; this is a fresh implementation, not a resume.

## Problem Statement

When a role is waiting on an external dependency — CI, a GitHub check, another agent/worker, an external build/deploy, an approval or status transition — it should not read as idle while it is actively responsible for noticing that dependency resolve. A user glancing at the dashboard mid-wait should be able to tell "still waiting on something" from "genuinely done."

**The mechanism already exists, and partially covers this — investigated before writing this PRD, not assumed.** The deck derives `Working` from live process activity, not from any tracked concept of "this pane has outstanding external work": `descendant_shell_activity` (`src/platform/proc/scan.rs`) scans a pane's shell for a live descendant process matching `CLAUDE_BASH_TOOL_SHAPE`, sampled by a poll loop (`run_shell_activity_monitor`/`run_shell_activity_monitor_with`, `src/daemon.rs:1355`/`:1377`) that classifies each pane via `AgentPtyRegistry::shell_activity_candidates` + `classify_shell_activity` (`src/agent_pty.rs:5769`/`:5810`) and sets `crate::state::SessionStatus::Working`/`::Idle` accordingly. This is PRD fork#370/#386 (descendant-scan shell-activity signal), already shipped.

**What it covers today:** a pane blocked inside one sustained foreground command — an agent running `gh run watch`, or a poll loop kept in the foreground — genuinely shows `Working` for the whole duration, because that command is a live descendant process the whole time. This is real, already-shipped behavior, confirmed by reading the code rather than assumed from memory. CLAUDE.md rule 28 (added by PR #557) documents this as the recommended convention.

**What it misses even when rule 28 is followed perfectly:**

1. **Work that outlives the pane's own task.** Once a delegated worker (coder, tester, …) finishes its own task and calls `work-done`, its pane has nothing running at all — even when something that worker triggered (a CI run it dispatched, a review it requested) is still in flight and someone is still waiting on the outcome. There is no local process left to be a proxy for that wait. Rule 28 can only say "responsibility moves to whichever pane picks it up next" — it cannot guarantee that pickup is immediate, or that any pane is actually watching in the gap.
2. **Non-compliance is invisible.** A pane that polls in gaps instead of following rule 28 reads exactly like a genuinely idle pane — no lint, test, or CI check catches it, and a human has to notice the dashboard to find out (as happened for the original #499 incident).

Both gaps share a root cause: `Working` is inferred from **"is there a live foreground process right now,"** not from **"is this pane/role still responsible for an outstanding external outcome."** Rule 28 makes the two facts correlate well when followed; it cannot make them the same fact.

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

- An explicit, first-class **monitored-wait** state distinct from the existing process-derived `Working`/`Idle` — a role enters/exits it deliberately via a CLI verb, rather than relying on incidental shell activity.
- A mechanism for a role to declare "I am waiting on external outcome X" and later clear it (success, failure, cancellation, timeout — all four terminal cases, not just success).
- Attribution: the monitored-wait state is associated with the role/pane actually responsible for the dependency, not a global flag.
- Coexistence with the existing `descendant_shell_activity` signal — a pane can be `Working` from either source; this PRD adds a second source, it does not replace the first.
- Status/tab indicator updates so this reads consistently wherever `Working`/`Idle` is currently shown.
- Regression coverage for at least two distinct external-dependency kinds (CI polling is the concrete motivating case; pick a second — e.g. waiting on another delegated worker — to prove the mechanism generalizes rather than being CI-specific).
- A self-healing fallback (TTL or equivalent) so a monitored-wait that is never explicitly cleared does not wedge a pane permanently `Working` — this is the risk PR #557's resolution named as its strongest argument against building this at all, and it must be answered here, not deferred.

### Out of Scope

- Any change to how CI is dispatched, polled, or read (rule 8's "read the job log, not the run conclusion" guidance is unaffected).
- A general task-scheduler redesign.
- Surfacing *what* the external thing is in detail (e.g. a CI run URL) in the status model — that's a display/UX decision for implementation, not a scope commitment here.
- Removing or weakening CLAUDE.md rule 28 — it remains correct, useful guidance and this PRD's mechanism is a backstop for when it isn't followed, not a replacement.
- **A `daemon status` provenance marker distinguishing a wait-promoted `Working` from any other kind** (round 2/3/4 review MEDIUM 8). `src/daemon_status.rs` renders the `Working*` marker only from `shell_synthetic_working`, so a wait-promoted `Working` currently renders unmarked rather than mislabeled — round 3 removed the harm an earlier attempt introduced (attaching the shell marker to a wait-caused `Working`), leaving this genuinely incomplete rather than misleading, and cosmetic in either state. Deliberately deferred a fourth time rather than silently dropped again: this is intentionally out of scope for PRD #499 and should be scoped as its own follow-up issue if/when it's worth doing.

## Technical Approach

**Chosen shape: a CLI verb**, analogous to `worker-agent-deck issue claim` — a role invokes `worker-agent-deck wait start <label>` to declare a monitored wait and `worker-agent-deck wait done <label>` (or an outcome-specific variant) to clear it, recorded daemon-side and read the same way `SessionStatus` is read today.

It must handle all four terminal outcomes of an external wait (success, failure, cancellation, timeout) — clearing only on success would leave the status wedged exactly the way a stale claim wedges dispatch in PRD #421/#464's design, which is worth reading for the "derive, don't maintain" caution about state that has to be remembered to be cleared. A TTL/self-healing fallback is required (see Scope above) precisely because this mechanism, unlike a live process, has no natural way to stop existing on its own.

### Key Files

| File | Why |
| --- | --- |
| `src/platform/proc/scan.rs` | `descendant_shell_activity`, `MEASURED_SHELL_TOOL_SHAPES`, `CLAUDE_BASH_TOOL_SHAPE` — the existing process-activity signal this PRD adds a second source alongside. |
| `src/agent_pty.rs` | `AgentPtyRegistry::shell_activity_candidates` / `classify_shell_activity` (`:5769`/`:5810`) — where per-pane classification happens today; likely where a second signal gets merged in. |
| `src/daemon.rs` | `run_shell_activity_monitor` / `run_shell_activity_monitor_with` (`:1355`/`:1377`) — the poll loop driving the classification; `crate::state::SessionStatus::{Working, Idle}` is the status enum being set. |
| `src/state.rs` | Wherever `SessionStatus` is defined/transitioned — the natural home for a new state variant. |
| `src/issue_claim.rs` | Closest existing precedent for a declare/claim-shaped CLI verb with daemon-side recorded state — read for the pattern, not reused directly. |

## Milestones

- [x] **M1** — Confirm the current `Working`→`Idle` transition logic and write down precisely where the new signal plugs in. *(Rounds 1-3; the plug-in point turned out to be `apply_event` itself, not a side-table — see round 3's architectural fix below.)*
- [x] **M2** — Design the `wait start`/`wait done` CLI verb: argument shape, daemon-side record shape, composition with `descendant_shell_activity`. *(Round 1: CLI shape and hook-socket transport decided; round 3 revised the composition mechanism after round 2's daemon-only state proved not client-visible.)*
- [x] **M3** — Implement `wait start`/`wait done`, wired into `SessionStatus`. *(Round 1, restructured in round 3 to route through `apply_event` via `SessionState`/`SessionSnapshot` fields rather than a daemon-only map, so the composition is replicated to every client instead of computed daemon-side only.)*
- [x] **M4** — Wire declare/clear into at least the CI-polling case so a role checking status in discrete steps reads as `Working` throughout, without needing rule 28's foreground-blocking convention. *(Pinned by `wait/monitored/002`.)*
- [x] **M5** — Attribute the monitored-wait to the correct role/pane, never a global flag. *(Pinned by `wait/monitored/003`; card-scoping specifically — surviving a pane respawn without leaking onto the new card — pinned by `017`.)*
- [x] **M6** — Confirm genuinely passive/inactive agents are unaffected and still classify as `Idle`. *(Pinned by `wait/monitored/004`.)*
- [x] **M7** — Handle all four terminal outcomes (success, failure, cancellation, timeout). *(Pinned by `wait/monitored/005`-`008`.)*
- [x] **M8** — TTL/self-healing fallback so a never-cleared wait doesn't wedge a pane `Working` forever. *(Pinned by `wait/monitored/009`; the clamp on caller-supplied TTL — closing a real unbounded-wedge finding — is auditor A1, fixed round 2.)*
- [x] **M9** — Status/tab indicators reflect the new state consistently everywhere `Working`/`Idle` is currently shown. *(Satisfied by construction: the mechanism composes into the existing `SessionStatus::Working` value rather than a new status variant, so every render path that already shows `Working`/`Idle` shows this correctly with no separate UI work needed. A distinguishing provenance marker — so a user could tell "Working because of a monitored wait" apart from "Working because of real activity" at a glance — was scoped out as MEDIUM 8 and deferred to its own follow-up; this PRD's own success criteria only require the status to read correctly, not to be visually distinguishable by cause.)*
- [x] **M10** — Regression tests proving the mechanism generalizes rather than being CI-specific, including all four terminal outcomes and the TTL fallback. *(The full `wait/monitored/001`-`023` suite exercises the generic `wait start`/`wait done` mechanism directly — none of it depends on CI-specific code, so genericity is demonstrated structurally rather than by a second named scenario. All four terminal outcomes: `005`-`008`. TTL fallback: `009`. Two additional rounds of real bugs — `019`-`023` — were found and closed after this milestone's original tests already passed, via reviewer/auditor re-derivation rather than new named dependency-kind scenarios.)*

**Total test count**: 23 new tests (`wait/monitored/001`-`023`), all in `tests/wait_monitored.rs`, none `e2e`-feature-gated (they run in the blocking `build`/`build-macos`/`build-windows` fast tier, not only the informational `e2e:` job). Seven rounds of tester/coder/reviewer/auditor iteration; five real bugs found and fixed after initial implementation looked complete and green (BLOCKER A/client-visibility, HIGH B/real-Working-clobbered, BLOCKER H/ownership-never-transfers, BLOCKER I/the-relocated-wedge, plus several MEDIUM/LOW findings) — see PR #617's review history for the full trail.

## Success Criteria

```text
actively monitoring external dependency → WORKING
doing nothing / no active responsibility → IDLE
```

Concretely: a role that dispatches an external check and polls for it in discrete steps (not one blocking call) reads as `Working` for the whole wait, on all four terminal outcomes, attributed to the correct pane, with no change to genuinely idle agents — and this holds **even if the role never heard of rule 28**, since the guarantee is mechanical, not conventional.

## Risks and Mitigations

| Risk | Mitigation |
| --- | --- |
| A monitored-wait flag that isn't reliably cleared wedges a role as permanently `Working` — the same shape as PRD #421/#464's stale-claim hazard, and the exact risk PR #557's resolution cited as its strongest argument against this design. | M7 requires all four terminal outcomes to clear it; M8 requires a TTL/self-healing fallback as a hard requirement, not an open question. |
| Overlap/conflict between the new signal and the existing `descendant_shell_activity` signal produces flapping or contradictory status. | M1/M2 require understanding both mechanisms before adding a third; M2's design states explicitly how the two compose (`Working` if *either* signal is active). |
| Scope creep into the task scheduler or external-integration architecture. | Explicitly out of scope (see above) — flag and stop rather than widen the PRD if implementation reveals a temptation to touch either. |
| Reintroducing exactly the risk profile rule 28's resolution was written to avoid, for a benefit rule 28 already delivers to any compliant caller. | Named directly in the "Reopened" section above — this is a deliberate, maintainer-approved trade of "no wedge risk" for "no dependency on convention compliance." Not re-litigated per-milestone. |

## Open Questions

1. ~~**Declare/clear mechanism shape**~~ — **Resolved 2026-08-26: CLI verb** (`worker-agent-deck wait start`/`wait done`), analogous to `issue claim`.
2. ~~**Does this want a TTL/self-healing fallback**~~ — **Resolved 2026-08-26: yes, required** (M8), not optional — this is the one risk PR #557's resolution explicitly weighed against building this mechanism at all.
3. **Should the status surface *what* the external wait is** (e.g., a CI run URL) or just that one exists? Affects M9's display design; not committed to either answer here.

## CLAUDE.md rule 9 (experimental flag) and rule 12 (cross-version) answers

**Rule 9 — should `wait start`/`wait done` ship behind the `experimental` feature flag? No, ungated.** This is a new user-visible CLI surface (two new subcommands), which is exactly what rule 9 asks about — but the reasonable default there (gate it) assumes a standalone, opt-in feature a user might want to try before committing to. This mechanism is different in kind: it is infrastructure other roles and PRDs immediately start depending on for correct dashboard status (the whole point is that a role's `work-done` no longer has to coincide with the pane going idle), and CLAUDE.md rule 28 already tells every role to reach for it. Gating it behind a flag that defaults off elsewhere would silently reintroduce the exact wedge/visibility gap this PRD exists to close for any deployment that hasn't separately opted in. Shipped ungated.

**Rule 12 — does this change the TUI↔daemon protocol/handler contract? No, and no `PROTOCOL_VERSION` bump is needed.** Reviewer and auditor both independently verified this across rounds 3/4 and agree: the four new `SessionSnapshot` fields (`monitored_wait_active`, `wait_synthetic_working`, `shell_descendant_busy`, `wait_deferred_revert`) and the two new `DaemonMessage`/event variants are all additive — `#[serde(default, skip_serializing_if = ...)]` on every field, no `#[serde(deny_unknown_fields)]` anywhere on `SessionSnapshot`, and `EventType`'s `#[serde(other)] Unknown` catch-all covers the two new event types for an older build reading a newer stream. An older daemon talking to a newer TUI (or vice versa) simply never sends/reads these fields, which decodes to `false`/no wait — exactly today's pre-PRD behavior — so an old/new pair interoperates safely with no wire-shape change and no semantic break behind a stable wire.

**Round 6 addendum — `shell_synthetic_working` is a fifth, pre-existing field whose *meaning* moved under this PRD, and it needs its own answer.** `shell_synthetic_working` (`SessionSnapshot`, `src/state.rs`) predates this PRD (PRD #370). Before round 6 it was `true` only when a synthesized `ShellBusy` event itself had promoted the current `Working`. Round 6's `MonitoredWaitDone` Direction-A hand-off can now also set it on a `Working` a **real agent event** emitted (e.g. a genuine `ToolStart`), once a monitored wait's revert obligation becomes unpayable except through shell. The wire shape is unchanged — still a plain `bool`, still additive/omit-when-false — but what a `true` value means has changed, which is exactly the same-wire/different-meaning case CLAUDE.md rule 12 calls a semantic break.

**No `.breaking.md` fragment and no `PROTOCOL_VERSION`/`SCHEMA_VERSION` bump is needed for this field either**, for two reasons specific to it (distinct from the "additive" reasoning above, which does not apply to a pre-existing field): first, the field was never part of any documented external contract beyond this repo's own `worker-agent-deck status` rendering — no third-party consumer decodes `status --json` against a published meaning of this flag. Second, its one consumer, `format_human` in `src/daemon_status.rs`, already handles both the narrow pre-round-6 case and the widened round-6 case identically: it appends `*` whenever the flag is `true`, with no branch on *why* it is `true`. An older TUI reading this field from a newer daemon still renders `Working*` and a paired `ShellIdle` still reverts correctly — the widened meaning does not change what any reader does with the value, only which situations produce it. Both the field's own doc comment (`src/state.rs`) and the `*` marker's rendering comment (`src/daemon_status.rs`) were updated in round 7 to state the widened meaning and name all three writers, rather than leaving this decision implicit.
