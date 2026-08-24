# PRD #586 — Orchestrator watchdog: silent delegations and wrong-content `work-done` reports

**Issue:** [prageethw/dot-agent-deck#586](https://github.com/prageethw/dot-agent-deck/issues/586)
**Priority:** Medium
**Status:** M1+M2 shipped (PR #589, merged `03b305a7`); M3 (design) complete below; M4-M6 pending
**Related:** PRD #126 (idle-worker watch, `OutstandingDelegation`), PRD #249 (delegate-silence watch, `SilenceWatchRecord`), issue #448 (`DelegationCommission`), PRD fork#465 (silent-exit signal — the closest prior art for "an existing daemon-side signal nobody surfaces"), upstream [#590](https://github.com/vfarcic/dot-agent-deck/issues/590) (`DelegationCommission` never expires — folded into this PRD's scope), upstream [#447](https://github.com/vfarcic/dot-agent-deck/issues/447) (`WaitingForInput` never routed to the orchestrator — related, explicitly not folded in)
**Fork-only?** No — this is a general `dot-agent-deck` limitation. The daemon-side tracking this PRD exposes (PRD #126/#249, issue #448) is core, unmodified upstream infrastructure. Build here first, offer upstream once shipped, per CLAUDE.md rule 19.

## Problem Statement

The orchestrator has (at least) three structurally different ways a delegation signal can go wrong, and no reliable way to detect any of them proactively — it depends entirely on the daemon's own fixed internal timers, or on the orchestrator noticing something is off while reading a signal it already received.

### Problem 1 — silent/stalled delegations (the issue as originally filed)

When the orchestrator delegates a task, it has no way to ask "what's still outstanding, and for how long" — it can only wait for a `work-done` report or the daemon's internal 120-minute idle-worker timeout to fire. A long-running orchestration juggling several concurrent delegations has no systematic way to notice a worker that has gone quiet early, apply a bounded retry/escalation policy, or make a decision informed by real elapsed time instead of conversational memory.

**This isn't a missing capability — it's an existing one that's invisible.** The daemon already tracks exactly this, in `AgentPtyRegistry` (`src/agent_pty.rs`):

1. **`OutstandingDelegation`** (PRD #126) — armed on delegate, fires a timeout prompt (`worker_response_timeout_minutes`, default 120min) if `work-done` never arrives.
2. **`SilenceWatchRecord`** (PRD #249) — a separately-clocked, shorter watch: did the worker emit *anything* after receiving its task pointer at all?
3. **`DelegationCommission`** (issue #448) — a per-worker-pane outstanding count, used to classify a later `work-done` as `Solicited`/`Unsolicited`.

Worker process exit without `work-done` is also already detected (PRD fork#465 / upstream issue #205, merged) and pushes an immediate notice rather than waiting out the timeout.

**None of this state is queryable.** `daemon status --json` (`src/daemon_status.rs`) is a thin, deliberately privacy-scrubbed projection (`agent_id`, `pane_id`, `cwd`, `role`, `status`, `active_tool.name`) with zero fields for any of the above. The tracking is real and correct; it's just invisible to any query the orchestrator (or a human operator) could make.

### Problem 2 — wrong-content `work-done` reports (a stale-session symptom, folded in 2026-08-24)

A structurally different failure that Problem 1's fix does not address at all, because the worker **does** call `work-done` — no silence, no timeout, nothing Problem 1's detectors would ever flag.

**Observed directly, repeatedly, in one orchestration session** (2026-08-24, a fix-bugs sweep across fork issues #571/#259/#281/#344/#373): `reviewer` and `auditor` panes, given a fresh, verified-correct task pointer (`worker-task-<role>.md` checked and confirmed correct every time), would sometimes call `work-done` with a full, well-formed report about a **completely different, already-merged PRD** (fork#544/PR #545, merged 2026-08-22 — two days before the session that observed this). The report was not garbled or truncated — it was a coherent, detailed report about the *wrong task*, as if the pane's agent session had reverted to (or replayed) an old completed turn instead of processing the new one.

**Measured cost, that one session:** roughly 12–13 occurrences across the two roles combined, recurring on nearly every PR in the sweep. Every occurrence required the orchestrator to read the report's opening, recognize the subject didn't match the delegation, and re-delegate with an explicit "confirm you are looking at PR #NNN, not fork#544" instruction — sometimes needing 2–3 retries before the genuine report arrived. Rough estimate: 10,000–14,000 tokens spent on this workaround loop within that one session, separate from and larger than Problem 1's own cost (one STUCK escalation, resolved by an unrelated disk-space fix).

**What was ruled out:** the daemon routing itself. The task file handed to the pane was correct every single time this was checked — the mismatch originates inside the worker's own agent session, not in how the daemon delivers the task pointer.

**What this PRD does NOT claim:** a root cause. Unlike Problem 1 (where the exact code path is known and cited above) or PRD fork#465 (where `pump_reader`'s EOF handling was traced precisely), this session did not have the means to inspect the underlying agent harness's session-resumption/context behavior that produced this. It may not even be fixable from `dot-agent-deck`'s own codebase — if the cause lives in how the harness hands a new prompt to an already-used agent process, that's outside this daemon's control. What *is* in this daemon's control is **detecting** the mismatch before the orchestrator trusts it.

### Problem 3 — misattributed notification delivery (independent corroboration, same day, a different orchestration session)

A third, related-but-distinct symptom in the same family — not folded into Problem 2's wording, because the failure shape differs: Problem 2 is a *solicited* report arriving with fresh-looking but wrong content; Problem 3 is a notification arriving that does not belong to this orchestration's own delegation state at all, regardless of content.

**Observed directly, 7 times, in a separate 2026-08-24 orchestration session** (the same day as Problem 2's observation, a different orchestration entirely — PR #573/#582 review work): a `coder`-role completion notification arrived **6 times** as a stale re-delivery of a report already read and acted on moments earlier — each one caught only because the harness itself flagged it: *"Worker coder reported completing a task, but you have no outstanding delegation to that worker."* A 7th instance was more serious in framing: a notification stating *"A delegated worker has not responded with work-done... delegated 2 hours ago"* for a `reviewer` role, which that orchestration had no outstanding delegation for at all — on its face indistinguishable from a genuine stall, the exact case Problem 1's fix is meant to help detect. It was cross-talk from a different orchestration's `reviewer` identity, not a real stall.

**What this adds to the case for Problem 1's fix, specifically:** exposing `OutstandingDelegation`/`SilenceWatchRecord` state (Problem 1's fix, item A) only helps if the orchestrator can use it to **independently verify** an arriving signal rather than trusting the signal's own framing. All 7 instances here were correctly handled — but only because the orchestrator manually cross-checked "do I actually have this outstanding?" against its own conversational memory each time, which does not scale and does not survive compaction. A watchdog built on Problem 1's exposed state should treat it as the **source of truth to check an arriving notification against**, not an alternative notification channel to trust on its own.

**Not claiming a root cause here either** — same posture as Problem 2. Whatever routes a daemon-level signal into a specific Claude Code session sits above `dot-agent-deck`'s own codebase (this daemon's `DelegationCommission` already carries an `orchestrator_pane_id` field internally, i.e. it *does* track ownership — so this may be a routing-layer bug entirely outside this repo, not a `dot-agent-deck` daemon defect). Recorded here as corroborating evidence for the PRD's general thesis, not as a fourth milestone of its own.

**A fourth, smaller instance surfaced during M1+M2's own review round (2026-08-24, PR #589's final confirmation pass):** the `reviewer` agent's first read of its task pointer resolved to a stale copy in a *different sibling checkout* (`/home/prageeth/workspaces/dot-agent-deck/.dot-agent-deck/worker-task-reviewer.md`, a leftover from an unrelated, already-merged PRD #544/PR #545 review two days earlier) rather than the correct file in the intended checkout, because the task pointer was given as a bare relative path and this machine has five sibling `dot-agent-deck*` checkouts. Caught immediately (the timestamp and subject were obviously wrong) and re-read correctly, with no downstream effect — but it is a second, independently-observed mechanism by which a worker can end up looking at content belonging to fork#544/PR#545 specifically, the exact same "phantom PRD" that contaminated Problem 2's original 12-13 occurrences. This raises (without proving) the possibility that Problem 2's root cause is this same class of ambiguous-path resolution rather than agent-harness session-resumption — worth a cheap check during M4/M6 (grep the affected session's task-delegation commands for bare relative paths) but not a re-scope of this PRD's root-cause-agnostic posture above.

## Decisions

| Question | Decision |
|---|---|
| Fold Problem 2 into this PRD, or track separately? | **Fold in.** Same operator (the orchestrator watching delegations), same watchdog framing, and Problem 2's fix — content verification on receipt — is a natural extension of "don't trust a `work-done` report at face value," not a separate feature area. |
| Does Problem 1's fix (exposing existing state) address Problem 2? | **No.** Problem 2's reports arrive with no silence and no timeout — none of `OutstandingDelegation`/`SilenceWatchRecord`/`DelegationCommission` would ever flag them. They need an independent mechanism. |
| Should the daemon try to decide "this worker is stuck" and act automatically? | **No**, for either problem. Verifying a completion claim against real evidence (a CI result, a file, a PR, a subject match) requires actual tool use and judgement a Rust daemon can't perform. The daemon's job is to expose ground truth; deciding what to do with it stays the orchestrator's. |
| Root-cause Problem 2 before shipping a detector? | **No** — out of scope for this PRD. Ship the detection/verification layer against the observed symptom; a root-cause fix (if one exists within this codebase) is separate follow-up work once the symptom is at least caught reliably. |
| Does Problem 3 get its own fix milestone? | **No.** It's corroborating evidence, not a separate feature — it shapes how (A) should be *used* (as a verification source, not a trust source) rather than adding new scope. If a genuine `dot-agent-deck`-side routing defect is found later, file it as its own issue; this PRD does not claim to fix notification-layer attribution. |
| M3: is daemon-side generation/sequence matching viable for Problem 2? | **No — investigated and dropped.** It already exists (`pane_registration_generation`/`daemon_boot_id`) for stale-pane routing correctness, which is a different failure shape than Problem 2's content correctness. A worker-echoed subject tag, checked by the orchestrator, is the chosen mechanism instead — see (C) above. |

## What we're building

### A. Expose existing delegation-tracking state (Problem 1)

Add optional fields to `StatusAgent` reporting (`src/daemon_status.rs`) per pane: whether an `OutstandingDelegation`/`SilenceWatchRecord` is currently armed, and since when (a timestamp, not raw internal generation counters — keep the same privacy-conscious scoping the rest of this surface already follows). Purely additive fields; no `SCHEMA_VERSION` bump needed under the existing "additive fields tolerate unknown keys" contract.

### B. Close the adjacent, already-filed `DelegationCommission` expiry bug

[vfarcic/dot-agent-deck#590](https://github.com/vfarcic/dot-agent-deck/issues/590): the commission counter never expires, so a much-later genuine `work-done` can be mislabeled `Solicited`. Narrow, same code area as (A), worth closing alongside the exposure work.

### C. Content-verification layer for `work-done` receipt (Problem 2) — M3 design decision (2026-08-24)

**Decided: worker-echo + orchestrator-check, combined.** Every task file states an explicit subject tag (the issue/PR number, or a short opaque token when there is no natural one). The worker's `work-done` payload is required to echo that tag back. Before treating a report as authoritative, the check (formalized in the standard delegation protocol — see M5/(D)) compares the echoed tag against what was actually delegated and refuses to trust a mismatch silently.

Two of the three originally-listed candidates map onto this:
- *Worker echo* — adopted as the structural signal. It's the only piece of this that's verifiable without relying on the orchestrator's own memory (which the PRD's Problem 3 section already established doesn't scale and doesn't survive compaction).
- *Orchestrator-side convention* — adopted as the enforcement half, but **formalized**, not left as the ad hoc discipline this session already improvised. Cheap on its own but insufficient alone: it depends on the orchestrator remembering to check every time, exactly the failure mode Problem 3 documents.
- *Daemon-side generation/sequence matching* — **investigated and dropped.** The daemon already has this mechanism for a *different* problem: `pane_registration_generation` + `daemon_boot_id` (`src/state.rs:5600` onward, `handle_work_done`) already validates that a `work-done` signal comes from the pane's *current* tenant, catching stale-pane-reuse misattribution — a routing-correctness check. It cannot help here: Problem 2's report is correctly routed (right worker pane, right orchestrator) — the *content* is wrong because the worker's own agent session produced the wrong content. No sequence number the daemon could pass through would let it detect that; the daemon has no semantic understanding of what content is "correct" for a given task. This class of check is fully covered by the existing infrastructure (and improved by M1+M2's commission-expiry fix) for Problem 1/3's routing-shaped failures; Problem 2's content-shaped failure needs the echo instead.

Concretely for M4: `work-done`'s CLI payload gains a required (or defaulted-but-checked) subject field; the orchestrator's delegation task-file template states the subject explicitly; a mismatch is surfaced to the orchestrator as a hard flag (not silently trusted) rather than the daemon attempting to auto-reject (per the Decisions table: the daemon exposes ground truth, the orchestrator decides).

### D. Update default orchestrator-prompt surfaces to use the new state

`assets/config_gen_prompt.md` (the meta-prompt driving interactive config-gen for every user) and `docs/orchestration.md`'s illustrative example should describe a periodic-polling, bounded-retry watchdog pattern built on the fields from (A), and the subject-verification discipline from (C), so new users get this by default rather than every project reinventing it independently (as this fork's own `.dot-agent-deck.toml` orchestrator prompt currently has to).

## Explicitly out of scope

- [vfarcic/dot-agent-deck#447](https://github.com/vfarcic/dot-agent-deck/issues/447) (`WaitingForInput` never routed to the orchestrator) — same family of gap (internal signal exists, doesn't reach the orchestrator), but a genuinely different mechanism (push notification vs. exposed pollable state) and its own existing issue. Related, not folded in here.
- Root-causing Problem 2's underlying session-resumption/context behavior — see Decisions above.
- Any daemon-side automatic "decide this worker is stuck/wrong and act" logic — stays the orchestrator's job for both problems.

## Milestones

- [x] **M1** — Design the exact `StatusAgent` field additions for (A); confirm the privacy-scoping bar with a spot-check against the existing fields. Per Problem 3, design it as a **verification source** (something an orchestrator checks an arriving signal against) rather than an additional notification channel in its own right. *(Shipped in PR #589.)*
- [x] **M2** — Implement (A) + (B), tests, ship. *(PR #589, merged `03b305a7`. Went through 3 review rounds — round 1's expiry design was found to violate upstream #590's own constraints and was redesigned to a per-arm `VecDeque<Instant>` with a fixed, config-independent window. Upstream offer tracked as fork issue #590 — note the number collision with the upstream issue this closes; always write `vfarcic/dot-agent-deck#590` in full from here on.)*
- [x] **M3** — Design phase for (C): evaluate the candidate mechanisms above against the observed session's actual failure shape (11ish fork#544 replays); pick one. *(Decided 2026-08-24: combined worker-echo + orchestrator-check. See (C) above for the full reasoning, including why daemon-side generation matching was investigated and dropped.)*
- [ ] **M4** — Implement (C), tests, ship.
- [ ] **M5** — (D): update `assets/config_gen_prompt.md` and `docs/orchestration.md` with the watchdog pattern built on (A)+(C).
- [ ] **M6** — Validate on a real long-running orchestration session; confirm the watchdog pattern actually reduces the retry-loop cost measured in Problem 2's provenance section.

## Provenance

Problem 1 surfaced while designing an orchestrator-side watchdog workflow; re-scoped after a search turned up the existing detection infrastructure and the adjacent open upstream issue (#590). Problem 2 surfaced and was measured directly during a 2026-08-24 fix-bugs sweep orchestration session (fork issues #571/#259/#281/#344/#373), folded into this PRD by maintainer decision the same day rather than tracked as a separate, unrelated watchdog effort. Problem 3 surfaced independently the same day in a *different* orchestration session (PR #573/#582 review work), corroborating the same general thesis — a delegation/notification signal cannot be trusted at face value — from a third angle; folded in as supporting evidence during reconciliation of this PRD with its own earlier, narrower draft (originally scoped to Problem 1 alone before this document existed).
