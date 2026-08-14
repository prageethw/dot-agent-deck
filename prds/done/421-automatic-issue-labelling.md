# PRD #421: Automatic issue labelling — priority, size, and a machine-written `in-progress` claim that records its claimant

**GitHub Issue**: [#421](https://github.com/vfarcic/dot-agent-deck/issues/421)

**Priority**: Medium

**Status**: **Complete.** *(Status corrected 2026-08-14: the 2026-08-12 correction fixed M3.1's checkbox and body but left this line claiming "M3.1's rule 12 cross-version interop run has no record of having been performed" — stale the moment it was written, since the same commit added M3.1's own paragraph recording that the run *was* performed on 2026-08-09 and copying its evidence in-repo per CLAUDE.md rule 13.)* Merged 2026-08-09 as [PR #154](https://github.com/prageethw/dot-agent-deck/pull/154) (original merge commit `c0cd1c8`, superseded by a later fork-sync rebase; the equivalent commit on `main` today is `4edefb71`, confirmed by `git merge-base --is-ancestor`), first released in **v0.36.1**. All milestones M1.0–M3.1 done, including the cross-version interop run — see M3.1 for the full record. Upstream issue [#421](https://github.com/vfarcic/dot-agent-deck/issues/421) remains open — that is the upstream tracker, not this fork's own completion.

## Problem Statement

Issues carry no priority, no size, and no machine-written status. Triage is entirely manual, and the deck contributes nothing to it even though the deck is the thing doing the work.

**The product gap.** `issue_dispatch` (PRD #120) **reads** labels — `issue_list_argv` builds `gh issue list --label <x>` (`src/issue_dispatch.rs:118`) — but **never writes one**. When the deck dispatches an issue to a worker it leaves no mark on the issue. Nothing on GitHub shows that an agent is working on it.

That matters more than status display, because of how dispatch decides what to pick up. `dispatch_decision` (`src/issue_dispatch.rs:277`) uses exactly two idempotency signals: the per-issue worktree already exists on disk, or an open PR's head branch is `agent/issue-<n>`. **Both are inferred side-effects.** Neither is visible to a human scanning the issue list, and neither is checkable by a *second* deck instance before it starts work. There is no explicit claim anywhere in the system.

**Priority and size are never captured at all.** No labels exist for them on either this repo or the fork, so the only ordering information about a backlog lives in someone's head. Anything that later wants to prioritise — a scheduler, a maintainer, a report — has nothing to read.

**The human-process analogue shows the failure mode is real.** A downstream fork adopted an `in-progress` label as a manual claim convention and had two orchestrations collide on the same issue anyway: one claimed an issue and applied the label, and eight minutes later a second delegated a worker onto it without checking, because nothing *required* the check. A label a machine writes and reads does not have that failure mode. The fork also automated *removal* of the label on issue close and never automated adding it, which is the asymmetry this PRD closes.

## Solution Overview

Three parts, one theme: make the claim explicit, attributable, and machine-maintained.

1. **Write `in-progress` at dispatch and read it as a third idempotency signal.** Write it *after* the worktree and spawn succeed, so a failed dispatch never leaves a false claim. Then have `dispatch_decision` read it. The claim stops being an inferred side-effect.
2. **Record the claimant, and report it when skipping.** A bare label says *someone* claimed the issue, not *who*. The claim is honoured either way — provenance never changes the decision — but a skip that cannot name its claimant is an exclusion nobody can act on.
3. **Priority and size labels, applied by an agent, with an honest fallback for uncertainty.**

**The deck does not classify.** It already spawns agents with substituted prompts and those agents have `gh` on `PATH`, so the triage agent applies its own labels. The deck supplies the vocabulary, the prompt template, and the uncertainty rule — not a classifier, not an API client, not a parser for the answer. This keeps the Rust surface small and puts the judgement where judgement already lives.

**The fallback for uncertainty is a label, not a question.** When the agent is not confident it applies `needs-triage` and leaves priority unset rather than guessing. That works unattended *and* interactively — in a pane the agent can additionally just ask, but correctness never depends on someone watching. A wrong priority is worse than an absent one, because it is indistinguishable from a considered one.

### A claim is authoritative regardless of who made it

**Provenance never affects the skip decision.** An `in-progress` label set by a human, by another tool, by another deck, or by this deck is treated identically: the issue is claimed, so dispatch skips it. Generalised: **any status marker this deck did not just create is a reason to exclude the issue, and to say so.**

Two consequences follow, and both are simplifications:

**The claimant becomes reporting content, not a decision input.** There is no "is this my own claim?" comparison to implement, and therefore no subtle failure mode where a deck talks itself into re-dispatching something. The recorded identity exists purely to make the skip *legible* — "claimed by orchestration `X` (`id`) at `T`" versus "in-progress, no claimant recorded, so a human or an external tool set it". The self-resume case that comparison might have served is already covered by the existing worktree-exists signal.

**Skipping must be highlighted, never silent.** This is the load-bearing half: excluding an issue is only safe if the exclusion is visible, otherwise a stray label silently starves the backlog and looks like nothing happening. Today's event cannot express this — `IssueDispatchSkipped` (`src/scheduler.rs:76`) carries `{ task, repo, issue, branch }` and **no reason**, so every skip renders as the same line: `skipping already-claimed issue #N of <repo> (<branch>)`. **Three** distinct causes already collapse into it — the worktree exists, an open PR matches the head branch, or `create_worktree` lost a concurrent-creator race — and a label would be an indistinguishable fourth. The event needs a reason, which repairs the three existing cases as well as serving the new one.

### Why the classifier must be local

A GitHub Action cannot do this. CI has no LLM credentials — only `GITHUB_TOKEN` plus repo-specific publish/scan tokens — and more fundamentally **a CI job can never pause to ask a human**; it must guess or leave the issue unlabelled. Both constraints point classification at an agent running on a real machine.

### Claimant identity

The identity already exists and does not need inventing. `mint_orchestration_id()` (`src/agent_pty.rs:383`) mints a per-tab token, and `OrchestrationIdentity::Instance { id, name }` (`src/state.rs`) carries it alongside the orchestration's config name. Its own doc states the property this needs: *"two tabs of the SAME orchestration in the SAME directory are two distinct routing groups."* So record **`name` + `id`** — the **name alone is insufficient**, ambiguous in exactly the same-name-same-directory case. `NameCwd` is the legacy fallback for clients predating PRD #140 that carry no token.

A label cannot carry that identity — a label per orchestrator is unusable — so split the two jobs:

- **Label** — the machine-readable claim, cheap to read, what `dispatch_decision` gates on.
- **Comment** — the human-readable claimant: orchestration name, instance id, host, timestamp.

The two write points have different claimants and both must be representable: scheduler-side dispatch claims under `ScheduledTask.name` (documented unique per daemon, `src/config.rs:609`); a human orchestration claims as `Instance { id, name }`.

## Scope

### In Scope

- Writing `in-progress` on successful dispatch in the fire-time flow (`src/issue_dispatch_run.rs`), placed so a failed dispatch leaves the issue unmarked.
- Reading the label as a third signal in `dispatch_decision`, **independent of who applied it** — human, external tool, another deck, or this one.
- Adding a **reason** to `IssueDispatchSkipped` (`src/scheduler.rs:76`) and surfacing it, so an excluded issue always says why it was excluded. This also disambiguates the three existing skip causes, which currently render identically.
- A claimant comment carrying orchestration name, instance id, host and timestamp.
- The label vocabulary: `priority:high|medium|low`, `size:high|medium|low`, `needs-triage` — created idempotently rather than assumed to exist.
- An agent-driven triage path that applies priority/size labels itself, with `needs-triage` as the uncertainty outcome.
- Opt-in configuration on `IssueDispatchConfig` (`src/config.rs:586`), which already carries `#[serde(default)]` optional fields.
- Tests per CLAUDE.md rule 4 and docs for the vocabulary and the claim semantics.

### Out of Scope

- **Changing `DelegateSignal`.** It carries no issue reference (`src/event.rs:674`) and does not need one — the dispatch path already knows the issue number. Keeping it untouched is what makes this a no-protocol-change feature.
- **Sorting or prioritising dispatch by the new labels.** Recording priority is not the same as acting on it; that is separate work with its own ordering question (see Open Questions).
- **Classifying in CI.** Ruled out above.
- **Retro-triaging the existing backlog.** A one-off sweep is an operational task, not a product behaviour.
- **Removing `in-progress` on close.** Already solved by a repo-side workflow in the fork; the product does not need to watch for closes.

## Success Criteria

- Dispatching an issue marks it `in-progress` and records the claimant; a **failed** dispatch leaves the issue completely unmarked.
- A second dispatch of an already-claimed issue skips it, and the skip is driven by the label — demonstrable with the worktree and PR signals both absent.
- An issue labelled `in-progress` **by a human or an external tool** is skipped exactly as one claimed by a deck, and the skip **names the reason**.
- Every skip reports which of the four causes fired; no two causes render identically.
- An agent triaging an issue applies a priority and a size, or applies `needs-triage` and no priority — never a guessed priority.
- Priority and size are visible and filterable on GitHub via `gh issue list --label`.
- No `PROTOCOL_VERSION` bump and no change to `DelegateSignal`.
- `cargo test-fast` green per task; `cargo test-e2e` green pre-PR.

## Milestones

### Phase 1: The claim

- [x] **M1.0** — Write `in-progress` on successful dispatch, after worktree creation and spawn both succeed. The per-issue error boundary is where this is won or lost. *Landed as `claim_issue`, called after `spawn` returns `Ok` and after the `IssueDispatched` notify; a `gh` failure logs a warning and is never propagated, so it cannot turn a successful dispatch into a spurious `IssueDispatchFailed`.*
- [x] **M1.1** — Post the claimant comment (orchestration name, instance id, host, timestamp), with the scheduler claiming under `ScheduledTask.name`. *See "Only one write point exists" below — `Claimant::Instance` is implemented and unit-tested but has no call site, because the product has no human-orchestration entry into issue dispatch.*
- [x] **M1.2** — Read the label as a third signal in `dispatch_decision`, regardless of provenance. *Read off the existing `gh issue list` enumeration (`--json number,labels`), so it costs no extra `gh` invocation.*
- [x] **M1.3** — Add a reason to `IssueDispatchSkipped` and render it, covering all four causes (worktree exists, open PR, concurrent-creator race, label present). *`SkipReason` enum; the label cause renders two ways depending on whether a claimant was recorded.*
- [x] **M1.4** — Tests: labels-on-success, no-label-on-failed-dispatch, label-as-skip-signal, externally-applied-label-also-skips, and each skip reason rendering distinguishably. *`scheduler/dispatch/010`, `014`, `015`, `016`, `017` plus two pure-data unit tests — see "Coverage of the fourth skip cause" below.*

### Phase 2: Triage

- [x] **M2.0** — Label vocabulary, created idempotently and documented as canonical. *Seven hyphenated labels created via `gh label create --force`, once per run; a per-label failure warns and continues.*
- [x] **M2.1** — The agent-driven triage path and its prompt template, including the `needs-triage` uncertainty rule. When a human **is** present, the agent asks a specific, bounded question — "priority for #N: high, medium, or low?" — rather than prose. The unattended path is unchanged: apply `needs-triage` and move on, never block a scheduled run on a prompt. *Scoped to **triage-on-dispatch**: the instruction is appended to the prompt of each **dispatched** agent. Not a backlog sweep — see "Triage scope" below.*
- [x] **M2.2** — Tests for the triage path's label application and its uncertainty outcome. *`scheduler/dispatch/018` (enabled) and `019` (off by default, a guard against the feature leaking into the default path).*

### Phase 3: Ship

- [x] **M3.0** — Docs: the vocabulary, what a claim means, and how to read the claimant. *`docs/scheduled-tasks.md`. The pre-existing section "Idempotency: the worktree is the ledger" was **falsified** by this PRD and was rewritten as "Idempotency: three signals, one explicit claim".*
- [x] **M3.1** — Changelog fragment; cross-version check per CLAUDE.md rule 12 (touches the daemon and `issue_dispatch`); PR, review, merge, close #421. *Changelog fragment `changelog.d/421.feature.md` landed. Protocol classification settled (see below). PR #154 reviewed and merged 2026-08-09.* **The cross-version interop run was performed on 2026-08-09, against a released v0.36.0 daemon with the new TUI.** Both directions were exercised (old daemon / new TUI, and the reverse), `delegate` and `work-done` delivered cleanly in both, and the conclusion reached was that no `PROTOCOL_VERSION` bump and no `.breaking.md` fragment are required — consistent with the protocol classification below. The full record is `.dot-agent-deck/xversion-421-findings.md` plus its companion summary `report-coder-fae53952-xversion-421-check.md`, but those are gitignored scratch files, not part of the repo's durable record — per CLAUDE.md rule 13, project knowledge belongs in-repo, so **that was the actual defect: the evidence existed but was never copied into this PRD, PR #154, the fork's issues, or the changelog fragment.** This paragraph is that copy.

### Also delivered, beyond the original milestones

- **A `--triage` flag on `schedule add`.** Without it the feature was opt-in via a config field that the CLI hardcoded to `false`, so the only way to enable it was hand-editing `schedules.toml` — an opt-in feature with no supported way to opt in.

## Decisions taken during implementation

These settle items that were open when this PRD was written. Recorded here so they are not re-litigated.

- **Label naming: hyphenated.** `priority-high|medium|low`, `size-high|medium|low`, `needs-triage` — matching the existing house style (`in-progress`, `ci-cd`) rather than the `priority:high` colon form. *(Settles the "Label naming" open question.)*
- **Claim comment shape: append, one per dispatch — never edit in place.** The PRD's "comment noise" risk turns out not to be reachable: once M1.2 lands, the only path that re-runs the dispatch success flow for the same issue is a deliberate un-claim (label removed **and** worktree gone), which normally means a *different* claimant is taking over. Editing in place would overwrite the previous claimant's record and destroy exactly the provenance this PRD exists to add. *(Settles the "Claim comment shape" open question, and supersedes the "Comment noise" mitigation under Risks.)*
- **Only one write point exists.** This PRD assumed two claimants — scheduler-side (`ScheduledTask.name`) and a human orchestration (`OrchestrationIdentity::Instance { id, name }`). In fact `run_issue_dispatch` has exactly one caller (`src/daemon.rs`, the scheduler); there is **no** human-orchestration entry into issue dispatch. The `ui.rs` worktree-creation path is fork #122's orchestration-tab feature and is unrelated to GitHub issues. `Claimant::Instance` is therefore implemented and unit-tested but deliberately unwired, ready for a second write point if one ever exists.

  > **Confirmed by PRD fork#235** (`prds/235-issue-claim-lock.md`). The second write point arrived as `dot-agent-deck issue claim` — a standalone CLI verb outside the scheduler entirely, not the human-orchestration entry this PRD anticipated — and it came with concrete requirements about what identity it carries, exactly as predicted here. The requirement turned out to be an **instance** identity: the worktree's own absolute path plus its git branch (plus, added during the round-3 hardening pass, its hostname), not the `name` + `id` pair `Claimant::Instance` implemented above. Three designs were tried before that was clear (`prds/235-issue-claim-lock.md`'s "Identity, round 2" section, also recorded in CLAUDE.md rule 23): a worktree ownership marker (round 1 — almost never present, since rule 1's mandated `git worktree add` flow writes none), `DOT_AGENT_DECK_PANE_ID` (round 2 — a small daemon-scoped integer that recycles across a daemon restart), and finally the worktree's path plus branch itself (round 3, the one that shipped, matching CLAUDE.md rule 23's own pre-existing hand-made anchor). `Claimant::Instance` was not reused for the new write point — it defines its own `Identity::Worktree`/`Identity::Human` instead, because neither the name+id shape built here nor the scheduler's own claimant shape carried the per-worktree-instance anchor round 3 needed.
- **Coverage of the fourth skip cause.** The concurrent-creator race (`WorktreeCreation::AlreadyClaimed`, a `git worktree add` TOCTOU) has no deterministic black-box trigger, so `scheduler/dispatch/017` covers three of the four causes end-to-end. Rather than adding a production test seam purely to force a race, a **pure-data unit test** asserts all four reasons render distinguishably — exhaustive over the variants, and therefore stronger evidence for the "no two causes render identically" success criterion than the e2e, which only samples three.
- **Triage scope: triage-on-dispatch.** Triage applies only to the issues actually dispatched (≤ `max_per_run`, default 3), not to the whole backlog. Deliberately narrow because this PRD defers "should dispatch sort by priority" to an open question — so priority labels have **no consumer yet**, and broad backlog coverage would buy nothing today. Widening the coverage later is purely additive.
- **No `experimental` feature flag** (CLAUDE.md rule 9). The feature is already opt-in through `IssueDispatchConfig`, and its surfaces are not TUI — a label write, a claim comment, stderr skip text and a config/CLI knob. A config opt-in is the stronger and more appropriate gate.
- **Protocol classification: no bump** (CLAUDE.md rule 12). Verified rather than assumed: `NotifyEvent` derives only `Debug, Clone, PartialEq, Eq` — no serde — and its only production impl is `StderrNotifier`, so it never crosses the TUI↔daemon wire. Neither `IssueDispatchConfig` nor `ScheduledTask` appears anywhere in `src/daemon_protocol.rs`. The new `triage` field is additive under `#[serde(default)]`, so an older daemon ignores it and a newer one defaults it. No `PROTOCOL_VERSION` bump and no `.breaking.md`. The manual cross-version interop run still applies, since that is what catches a semantic break behind a stable wire.

## Key Files

- `src/issue_dispatch_run.rs` — fire-time flow; the module doc (`:1-40`) lists all seven steps. Dispatch and spawn are step 4; the per-issue error boundary is step 6.
- `src/issue_dispatch.rs` — `dispatch_decision` (`:277`), `DispatchDecision` (`:265`), `issue_list_argv` (`:118`) — the existing `--label` *read* path to mirror for writes.
- `src/state.rs` — `OrchestrationIdentity` and its `Instance { id, name }` variant; `pane_orchestration_map` (`:509`).
- `src/agent_pty.rs` — `mint_orchestration_id` (`:383`); `TabMembership::Orchestration::orchestration_id` (`:364`).
- `src/config.rs` — `IssueDispatchConfig` (`:586`); `ScheduledTask.name` (`:609`).
- `src/scheduler.rs` — `NotifyEvent::IssueDispatchSkipped` (`:76`) and its rendering (`:135`); the two emit sites are `src/issue_dispatch_run.rs:279` and `:320`.
- `src/event.rs` — `DelegateSignal` (`:674`), deliberately untouched.

## Risks and Mitigations

- **A false claim on a failed dispatch.** Marking too early would make an issue permanently un-dispatchable. Mitigation: write only after worktree and spawn both succeed, and make the no-label-on-failure case an explicit test rather than an assumption.
- **A stale claim outlives the deck that made it.** A crashed deck leaves its label behind. Mitigation: the recorded claimant makes a stale claim *diagnosable* rather than anonymous; expiry policy is an open question, deliberately not guessed here.
- **A stray label silently starves the backlog.** Since any `in-progress` excludes an issue regardless of provenance, a label left by a human or a tool stops dispatch indefinitely. Mitigation: this is *why* the reason-carrying skip report is in scope rather than optional — an exclusion nobody can see is the actual hazard, not the exclusion itself.
- **Guessed priorities look considered.** An LLM will always produce an answer if asked for one. Mitigation: `needs-triage` is a first-class outcome, and the prompt makes declining the *expected* behaviour under uncertainty rather than a failure.
- **`gh` calls on the dispatch path can fail.** Labelling is now a step that can error. Mitigation: it runs inside the existing per-issue error boundary — a labelling failure must not abort the run or the other issues.
- **Comment noise on re-dispatched issues.** ~~Mitigation: edit one claim comment in place rather than appending per dispatch.~~ **Resolved as not reachable** — the dispatch success path cannot run twice for the same issue once the label is read back, so comments do not accumulate. Appending is the deliberate choice; see "Decisions taken during implementation".

## Open Questions

Still open — deliberately **not** decided by this work:

- **Do claims expire?** A deck that dies mid-work leaves its claim behind, and only issue *close* currently clears the label. Is there a staleness window, and who acts on it? The recorded claimant makes a stale claim *diagnosable*, not self-healing.
- **Priority of what, exactly?** Priority to a maintainer and priority to a dispatch scheduler are not the same ordering. If dispatch ever sorts by priority, that distinction has to be settled first. Nothing sorts by priority today.
- **Re-triage on change.** If an issue is substantially edited after triage, is size/priority revisited, or is triage once-only?

Resolved during implementation — see "Decisions taken during implementation" above:

- ~~**Label naming.**~~ Settled: hyphenated.
- ~~**Claim comment shape.**~~ Settled: append one per dispatch, never edit in place.
