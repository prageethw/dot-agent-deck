# PRD #421: Automatic issue labelling — priority, size, and a machine-written `in-progress` claim that records its claimant

**GitHub Issue**: [#421](https://github.com/vfarcic/dot-agent-deck/issues/421)

**Priority**: Medium

**Status**: Not started

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

- [ ] **M1.0** — Write `in-progress` on successful dispatch, after worktree creation and spawn both succeed. The per-issue error boundary is where this is won or lost.
- [ ] **M1.1** — Post the claimant comment (orchestration name, instance id, host, timestamp), with the scheduler claiming under `ScheduledTask.name`.
- [ ] **M1.2** — Read the label as a third signal in `dispatch_decision`, regardless of provenance.
- [ ] **M1.3** — Add a reason to `IssueDispatchSkipped` and render it, covering all four causes (worktree exists, open PR, concurrent-creator race, label present).
- [ ] **M1.4** — Tests: labels-on-success, no-label-on-failed-dispatch, label-as-skip-signal, externally-applied-label-also-skips, and each skip reason rendering distinguishably.

### Phase 2: Triage

- [ ] **M2.0** — Label vocabulary, created idempotently and documented as canonical.
- [ ] **M2.1** — The agent-driven triage path and its prompt template, including the `needs-triage` uncertainty rule. When a human **is** present, the agent asks a specific, bounded question — "priority for #N: high, medium, or low?" — rather than prose. The unattended path is unchanged: apply `needs-triage` and move on, never block a scheduled run on a prompt.
- [ ] **M2.2** — Tests for the triage path's label application and its uncertainty outcome.

### Phase 3: Ship

- [ ] **M3.0** — Docs: the vocabulary, what a claim means, and how to read the claimant.
- [ ] **M3.1** — Changelog fragment; cross-version check per CLAUDE.md rule 12 (touches the daemon and `issue_dispatch`); PR, review, merge, close #421.

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
- **Comment noise on re-dispatched issues.** Mitigation: see Open Questions — edit one claim comment in place rather than appending per dispatch.

## Open Questions

- **Do claims expire?** A deck that dies mid-work leaves its claim behind, and only issue *close* currently clears the label. Is there a staleness window, and who acts on it?
- **Priority of what, exactly?** Priority to a maintainer and priority to a dispatch scheduler are not the same ordering. If dispatch ever sorts by priority, that distinction has to be settled first.
- **Re-triage on change.** If an issue is substantially edited after triage, is size/priority revisited, or is triage once-only?
- **Label naming.** `priority:high` groups and filters better; the existing house style is hyphenated (`in-progress`, `ci-cd`). Pick one and apply it to all six new labels.
- **Claim comment shape.** Edit a single claim comment in place, or append one per dispatch and accept the timeline?
