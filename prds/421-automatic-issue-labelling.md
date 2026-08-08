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
2. **Record the claimant.** A bare label says *someone* claimed the issue, not *who* — so a second instance cannot tell "another orchestration owns this" from "this is my own earlier claim, resume it".
3. **Priority and size labels, applied by an agent, with an honest fallback for uncertainty.**

**The deck does not classify.** It already spawns agents with substituted prompts and those agents have `gh` on `PATH`, so the triage agent applies its own labels. The deck supplies the vocabulary, the prompt template, and the uncertainty rule — not a classifier, not an API client, not a parser for the answer. This keeps the Rust surface small and puts the judgement where judgement already lives.

**The fallback for uncertainty is a label, not a question.** When the agent is not confident it applies `needs-triage` and leaves priority unset rather than guessing. That works unattended *and* interactively — in a pane the agent can additionally just ask, but correctness never depends on someone watching. A wrong priority is worse than an absent one, because it is indistinguishable from a considered one.

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
- Reading the label as a third signal in `dispatch_decision`, including comparing a recorded claimant against the reader's own identity so a deck can distinguish its own claim from another instance's.
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
- A deck can tell **its own** claim from **another instance's**, using the recorded id rather than the name.
- An agent triaging an issue applies a priority and a size, or applies `needs-triage` and no priority — never a guessed priority.
- Priority and size are visible and filterable on GitHub via `gh issue list --label`.
- No `PROTOCOL_VERSION` bump and no change to `DelegateSignal`.
- `cargo test-fast` green per task; `cargo test-e2e` green pre-PR.

## Milestones

### Phase 1: The claim

- [ ] **M1.0** — Write `in-progress` on successful dispatch, after worktree creation and spawn both succeed. The per-issue error boundary is where this is won or lost.
- [ ] **M1.1** — Post the claimant comment (orchestration name, instance id, host, timestamp), with the scheduler claiming under `ScheduledTask.name`.
- [ ] **M1.2** — Read the label as a third signal in `dispatch_decision`, comparing the recorded id against the reader's own.
- [ ] **M1.3** — Tests: labels-on-success, no-label-on-failed-dispatch, label-as-skip-signal, own-claim-vs-foreign-claim.

### Phase 2: Triage

- [ ] **M2.0** — Label vocabulary, created idempotently and documented as canonical.
- [ ] **M2.1** — The agent-driven triage path and its prompt template, including the `needs-triage` uncertainty rule.
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
- `src/event.rs` — `DelegateSignal` (`:674`), deliberately untouched.

## Risks and Mitigations

- **A false claim on a failed dispatch.** Marking too early would make an issue permanently un-dispatchable. Mitigation: write only after worktree and spawn both succeed, and make the no-label-on-failure case an explicit test rather than an assumption.
- **A stale claim outlives the deck that made it.** A crashed deck leaves its label behind. Mitigation: the recorded claimant makes a stale claim *diagnosable* rather than anonymous; expiry policy is an open question, deliberately not guessed here.
- **A human-applied label blocks the scheduler.** Once the label is a skip signal, a person marking an issue `in-progress` stops dispatch. Mitigation: this is arguably correct — it is the same claim — but it must be documented, not discovered.
- **Guessed priorities look considered.** An LLM will always produce an answer if asked for one. Mitigation: `needs-triage` is a first-class outcome, and the prompt makes declining the *expected* behaviour under uncertainty rather than a failure.
- **`gh` calls on the dispatch path can fail.** Labelling is now a step that can error. Mitigation: it runs inside the existing per-issue error boundary — a labelling failure must not abort the run or the other issues.
- **Comment noise on re-dispatched issues.** Mitigation: see Open Questions — edit one claim comment in place rather than appending per dispatch.

## Open Questions

- **Does the skip-signal apply to human-applied labels?** Probably yes — it is the same claim — but it means a person can block dispatch with a label, which needs documenting.
- **Do claims expire?** A deck that dies mid-work leaves its claim behind, and only issue *close* currently clears the label. Is there a staleness window, and who acts on it?
- **Priority of what, exactly?** Priority to a maintainer and priority to a dispatch scheduler are not the same ordering. If dispatch ever sorts by priority, that distinction has to be settled first.
- **Re-triage on change.** If an issue is substantially edited after triage, is size/priority revisited, or is triage once-only?
- **Label naming.** `priority:high` groups and filters better; the existing house style is hyphenated (`in-progress`, `ci-cd`). Pick one and apply it to all six new labels.
- **Claim comment shape.** Edit a single claim comment in place, or append one per dispatch and accept the timeline?
