# PRD #422: Reclaim merged worktrees automatically — gate on PR state and a clean tree, not git ancestry

**GitHub Issue**: [#422](https://github.com/vfarcic/dot-agent-deck/issues/422)

**Priority**: Medium

**Status**: Not started

## Problem Statement

Worktrees accumulate. The convention is already "remove it once its branch is merged" — it is written down — but it is prose, so it is forgotten. Measured on one working copy: **8 worktrees sitting on MERGED PRs, every one of them clean.** Eight that could have been reclaimed with zero risk and were not.

Once a PR is merged the code is in `main`. There is nothing to lose and nothing to ask a human about. This should not depend on someone remembering.

**But the obvious implementation is wrong**, and wrong in both directions. "Is this branch merged?" cannot be answered with git ancestry in a squash-merge repository, and both failure modes are observable on a real working copy today:

| Branch | PR state | `git branch --merged origin/main` |
| --- | --- | --- |
| `ci/codeql-action-v4` | **MERGED** | says **no** |
| `fix/102-path-taking-request-helper` | **MERGED** | says **no** |
| `ci/87-sha-pin-actions` | **MERGED** | says **no** |
| `chore/orchestrator-scratch` | **no PR** | says **yes** |
| `fork-only` | **no PR** | says **yes** |

Squash-merging is the cause: the branch's commits never enter `main`'s ancestry, so an ancestry check **under-detects 3 of those 8** genuinely merged trees and reclaims nothing for them. That direction is merely useless.

The other direction is destructive. Ancestry reports *merged* for branches that were simply never advanced past `main` — including `chore/orchestrator-scratch`, which has no PR at all and is an actively-used scratch worktree. A naive "git says merged, delete it" rule destroys live work.

## Solution Overview

**Two conditions, both required, before any worktree is removed:**

1. **The PR's state is `MERGED`**, queried via `gh` — never inferred from git ancestry.
2. **The worktree is clean** — `git status --porcelain` empty.

**The clean check is not redundant with the merge check.** A merged branch's worktree can still hold uncommitted or untracked files that were never part of the PR — files that are genuinely *not* in `main`. That is precisely the case the "the code is already merged" reasoning does not cover, and removing them is data loss.

**Encode the gate in a tested command, not in documentation.** The reason those 8 trees accumulated is that the rule lived in prose, and the squash-merge subtlety above is exactly what someone working from prose gets wrong. A command makes the safety logic testable and reduces the instruction to "run this", which cannot be misremembered. The orchestrator then instructs a worker to run it after a merge, with no human confirmation step — because with both gates satisfied there is nothing to confirm.

**Remove the worktree; keep the branch.** The branch costs nothing and keeps committed work recoverable.

## Scope

### In Scope

- A pure, testable gate: `MERGED` PR **and** clean tree → reclaimable; anything else → keep, with the reason reported.
- A CLI verb to list reclaimable worktrees and to remove them, following the conventions of the existing read-only `daemon status [--json]` command — human-readable table plus versioned JSON, meaningful exit codes.
- Reporting *why* a worktree was kept (dirty, no PR, PR still open, PR closed-unmerged), so the output is actionable rather than a bare list.
- Wiring the command into the orchestrator's post-merge protocol, and repointing the existing prose rule at the command instead of restating the policy.
- Tests per CLAUDE.md rule 4, explicitly covering the squash-merged case and the no-PR-but-ancestor case, since those are the two that bite.
- Docs and a changelog fragment.

### Out of Scope

- **Deleting branches.** Deliberately excluded: committed work stays recoverable, and the disk and clutter cost this PRD addresses is the tree, not the ref.
- **Changing the tab-close removal path.** The destructive `git worktree remove --force` in `remove_worktree` (`src/issue_dispatch_run.rs:133`) is #236's subject, not this PRD's. This adds no new removal path to the close flow.
- **Removing trees that are dirty**, under any flag. A force mode would recreate exactly the hazard this exists to avoid; if one is ever wanted it needs its own justification.
- **Daemon-side registry reclamation.** `WorktreeRegistry` state is #236's territory; this is a git-and-`gh` operation.
- **Automatic background sweeps.** Removal stays explicitly invoked, not scheduled.

## Success Criteria

- A clean worktree whose PR is **MERGED** is reclaimed, including when the merge was a **squash** merge and git ancestry says otherwise.
- A worktree with **no PR** is never removed, even when its branch is an ancestor of `main`.
- A **dirty** worktree is never removed, even when its PR is merged; it is kept and the reason reported.
- A worktree whose PR is open or closed-unmerged is never removed.
- The branch survives every removal.
- The orchestrator can invoke reclamation after a merge with no human confirmation, and the safety decision is made by tested code rather than by the agent.
- `cargo test-fast` green per task; `cargo test-e2e` green pre-PR.

## Milestones

### Phase 1: The gate

- [ ] **M1.0** — The reclaim decision as a pure function over (PR state, tree cleanliness), returning reclaim-or-keep with a reason.
- [ ] **M1.1** — Table-driven tests covering: squash-merged clean tree → reclaim; merged dirty tree → keep; no-PR ancestor branch → keep; open PR → keep; closed-unmerged PR → keep.

### Phase 2: The command

- [ ] **M2.0** — Enumerate worktrees and resolve each one's PR state via `gh` and cleanliness via `git status --porcelain`.
- [ ] **M2.1** — The CLI verb: listing (human table + versioned JSON) and removal, with removal explicit rather than implicit.
- [ ] **M2.2** — Tests for the command surface, including the kept-with-reason output.

### Phase 3: Ship

- [ ] **M3.0** — Wire into the orchestrator's post-merge protocol; repoint the existing prose rule at the command.
- [ ] **M3.1** — Docs and changelog fragment; PR, review, merge, close #422.

## Key Files

- `src/issue_dispatch_run.rs` — `remove_worktree` (`:133`) and its `--force`; the existing removal primitive to reuse or deliberately bypass.
- `src/daemon_status.rs` — the `daemon status [--json]` command whose output conventions this mirrors (human table plus `schema_version`-carrying JSON, non-zero exit when unavailable).
- `prds/236-worktree-removal-safety-reclamation.md` — the broader removal-policy PRD this must stay aligned with.

## Risks and Mitigations

- **`gh` is unavailable or unauthenticated.** PR state cannot be resolved. Mitigation: fail closed — an unresolvable PR state means keep, never remove. The gate must be satisfied affirmatively, never by absence of evidence.
- **A PR is matched to the wrong branch.** Removing on a mismatched association would be wrong. Mitigation: match on the PR's head branch exactly, and treat multiple or ambiguous matches as keep.
- **The dirty check races an agent still writing.** A tree can be clean at check time and written to a moment later. Mitigation: reclamation is invoked after a merge, when the work is finished by definition; and the worst case is a removed tree whose content was already merged.
- **Users stop trusting that anything is cleaned up.** Conditional behaviour is harder to predict. Mitigation: always report the outcome and the reason for every tree examined, so the command is legible in both directions.
- **Divergence from #236.** Two documents touching worktree removal could drift. Mitigation: state the relationship explicitly in both; #236's Phase 2 reclamation would subsume this if it lands.

## Open Questions

- **Dry-run by default, or list-and-apply as separate verbs?** The safer default costs an extra step every time; the ergonomic one risks a surprise the first time someone runs it.
- **Which worktrees are in scope for enumeration?** Only those the deck created, or every worktree of the repo including hand-made ones? The measured 8 include hand-made ones, which argues for the wider scope — but the deck removing a tree it never created deserves an explicit decision.
- **What about a merged PR on a *remote* whose branch was already deleted?** The head branch may no longer exist to query. Does that read as merged, or as unresolvable-therefore-keep?
- **Should the orchestrator run this automatically after every merge, or on a cadence?** Per-merge is timely and noisy; a cadence is quieter but leaves trees around longer.
