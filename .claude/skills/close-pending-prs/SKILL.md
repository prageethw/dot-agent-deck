---
name: close-pending-prs
description: Review every open PR on prageethw/dot-agent-deck, take over the ones nobody is actively driving, finish what's left, and merge or close each as soon as it is genuinely done — without pausing for approval except where CLAUDE.md rule 25 or the fork's branch-protection ruleset requires it. Use when the user asks to close out pending PRs, clear the PR queue, or finish PRs nobody is actively working on. Does not pick up unrelated issues, PRDs, or refactors.
user-invocable: true
---

# Close out every pending PR, unattended through merge or close

Clears every open PR on `prageethw/dot-agent-deck` — draft, blocked, or otherwise incomplete — that nobody is actively driving, end to end, without pausing for a merge go-ahead on each one. This is the orchestrator's workflow; every step below assumes `.dot-agent-deck/orchestrator-context.md`'s delegation model (worker-agent-deck delegate, the coder/tester/reviewer/auditor/release roles) is already in context.

**Scope discipline**: this fork's own open PRs only. Do not touch `vfarcic/dot-agent-deck` PRs (that's `review-pr-upstream`), do not start a fresh issue/PRD/refactor you happen to notice while reading a diff, and do not expand a PR's scope beyond what it already set out to do. If finishing a PR would require PRD-scale design work nobody has signed off on, say so and leave it as a blocked item (same escape hatch `fix-bugs`/`implement-prd` use) rather than improvising a design.

**Hard stop — this skill never submits an approving GitHub review, on anyone's behalf, ever.** This fork's `main-protected` ruleset requires one approval from *another* maintainer (CLAUDE.md rule 8) precisely so a second human genuinely looks at the diff — that is the whole point of the "no automated reviewer" discipline stated there. Delegated `reviewer`/`auditor` passes are a real substitute for the automated-bot review this fork never had; they are not a substitute for the required human approval, and this skill must not blur the two by clicking approve as if it were the human. If a PR is otherwise finished and only lacks that approval, request it and report the PR as waiting on a person — never approve it yourself, however confident the diff looks.

## Step 0 — Resolve identity

```bash
ME="$(gh api user --jq .login)"
```

The fork has exactly two maintainers today, `prageethw` and `vfarcic` (CLAUDE.md rule 8). Whichever one `$ME` is not is the "other maintainer" referenced throughout — the one whose approval a `$ME`-authored PR needs, and the one who owns a PR you must not silently take over.

## Step 1 — Inventory

```bash
gh pr list --repo prageethw/dot-agent-deck --state open --json number,title,author,assignees,isDraft,mergeable,mergeStateStatus,reviewDecision,statusCheckRollup,updatedAt,headRefName,headRepositoryOwner,labels,body
```

This is every PR in scope for this pass — open, draft or ready, whatever its CI/review state. There is no separate "incomplete" filter to apply; an open PR that is fully green and merge-ready is just a PR whose remaining step is Step 6's merge, not a different category.

## Step 2 — Determine ownership, conservatively

The product has no PR-level equivalent of `worker-agent-deck issue claim` — there is no refusing command to lean on here, so this step is judgment, and the instruction is explicit: **do not take over a PR that is actively owned and progressing.** Use these signals together, not any single one in isolation:

- **An assignee, or an author who is a human maintainer (`prageethw` or `vfarcic`), with a push or comment in the last 7 days** → actively owned. Skip it entirely — report it under "already owned by others," do not touch it, do not comment on it.
- **No assignee, and no push/comment from a human in the last 7 days** (bot comments — `github-actions[bot]`, CI status updates — don't count as activity) → stalled, a candidate for Step 3.
- **Author is `renovate[bot]` or another bot with no human assignee** → unowned by construction, a candidate for Step 3 regardless of age (a fresh Renovate PR is still fine to take through this flow if its own CI is green and it needs nothing but a merge).
- **Genuinely ambiguous** (a maintainer commented once weeks ago and went quiet, an assignee who never followed up) — leave it alone and report it as ambiguous rather than guessing; the cost of wrongly sitting on someone's PR is much lower than the cost of colliding with it (same asymmetry CLAUDE.md rule 14/23's issue-claim incidents were about, applied here without that command's safety net).

A PR authored by `$ME` with no other assignee is already yours by authorship — it needs no assignment in Step 3, only Step 4 onward.

## Step 3 — Assign unowned/stalled PRs to me

For each Step 2 candidate:

```bash
gh pr edit <n> --repo prageethw/dot-agent-deck --add-assignee "$ME"
```

This is a visible marker for anyone else looking at the queue, the same reasoning CLAUDE.md rule 23 gives for issue claims — it doesn't gate anything here (there is no refusing command), but leaving the assignment unrecorded would make this pass indistinguishable from silently working someone else's PR.

## Step 4 — Per assigned PR: gather full context

For each PR now assigned to you, read it fully before deciding what remains — this mirrors `review-pr-upstream`'s Step 2, applied to a PR you intend to finish rather than merely review:

- `gh pr view <n> --repo prageethw/dot-agent-deck --json mergeable,mergeStateStatus,reviewDecision,statusCheckRollup,comments,reviews` — current CI, current review state, whatever's already been said.
- `gh pr diff <n> --repo prageethw/dot-agent-deck` — the diff as it stands.
- Unresolved review threads, via the GraphQL query in `review-pr-upstream`'s Step 2 — page past `hasNextPage` before trusting a count.
- The linked issue, if any (`gh pr view <n> --json closingIssuesReferences`), for the original scope and acceptance criteria.

From this, write down exactly what remains before the PR can be complete: requested changes to address, unresolved threads, failing CI, merge conflicts with `main`, missing tests for a PRD/enhancement-labeled change (CLAUDE.md rule 4), or nothing at all (already done, just needs review/merge — go straight to Step 6).

## Step 5 — Per PR: worktree and finish the implementation

**A worktree per PR, same as any other change (CLAUDE.md rule 1) — but check out the PR's own branch, don't cut a new one.**

- Same-repo PR (`headRepositoryOwner` is `prageethw`):
  ```bash
  git fetch origin <headRefName>
  git worktree add ../dot-agent-deck-pr<n> origin/<headRefName>
  ```
  This tracks the PR's own remote branch, not `origin/main` — unlike rule 1's fresh-branch case, do **not** run `git branch --unset-upstream` here; a bare `git push` correctly lands back on the PR's own branch, which is exactly what you want.
- Fork-authored PR (`headRepositoryOwner` is someone else, and not `renovate[bot]` — Renovate's branches live on this repo): you cannot push fixes to someone else's fork. Leave it as blocked with that reason, or use `gh pr review --comment`/`--request-changes` to say what's needed instead of implementing it yourself.

State the worktree's absolute path, exact commit SHA, and branch name in every delegation (CLAUDE.md rule 16) — the same supply obligation as any other worktree.

Delegate what Step 4 found is missing:
- **Requested-changes / review findings to address** → `coder` (implementation) or `tester` (test-side), matching the finding.
- **Failing tests, or a functional change with no test coverage** → `tester` writes/extends the test, `coder` implements, per the standing TDD chain (CLAUDE.md rule 4).
- **Merge conflicts with `main`** → delegate to `coder` to rebase and resolve. A straightforward conflict (adjacent lines, an obviously-compatible combination) gets resolved and pushed. A conflict that changes what either side's code actually does — not just where it sits — is not "straightforward": stop, report it, and let the user (or the PR's actual owner) decide rather than guessing at intent.
- **Nothing missing** → skip straight to Step 6.

Never run tests locally (CLAUDE.md rule 5) — every RED/GREEN confirmation is a CI round trip on the PR's own branch, which already re-runs CI via `synchronize` since the PR exists. Batch each worker's changes so one push answers everything it can (rule 22).

## Step 6 — Per PR: review, then merge or close

Once Step 5 leaves nothing outstanding and CI is green:

1. **Reviewer + auditor**, in parallel, on the final head — state the findings-file absolute path (root checkout's `.dot-agent-deck/`, derivable via `dirname "$(git rev-parse --path-format=absolute --git-common-dir)"`, CLAUDE.md rule 15). Resolve every finding you agree with (rule 8); a finding you disagree with needs a stated reason, not silence.
2. **Make sure review has actually been requested.** If the PR is still draft, `gh pr ready <n> --repo prageethw/dot-agent-deck` first — CODEOWNERS does not route a draft (rule 8). If it's ready but carries no pending review request and no current approval — routing can fail silently, or a push can have invalidated an earlier approval (`dismiss_stale_reviews_on_push`) — explicitly request the other maintainer: `gh pr edit <n> --repo prageethw/dot-agent-deck --add-reviewer <other-maintainer-login>`. Request review **last**, after CI is settled and threads are resolved (rule 8) — asking earlier just buys a second round trip.
3. **Check the full CLAUDE.md rule 25 checklist** before merging: every expected CI check present and passing (e2e read from the job log, never the run conclusion — rule 8), reviewer and auditor both returned on the final head, every agreed finding resolved and every behavioural fix pinned by a test, no blocker accepted rather than fixed, closing-keywords audited clean on both the PR body and every commit message.
4. **`mergeStateStatus`** is where the required-approval reality actually surfaces: `CLEAN` means everything the ruleset needs is satisfied, including the human approval from Step 6.2. `BLOCKED` most commonly means that approval genuinely has not arrived yet — per the hard stop above, that is not yours to force. Report the PR as blocked on `@<other-maintainer>`'s approval and move to the next PR; do not merge, do not re-request repeatedly.
5. **On `CLEAN`**: post the merge report as a PR comment (rule 25), then delegate the merge to `release` with `--match-head-commit`.
6. **Rule 25's always-ask exclusion still applies** even inside this unattended sweep: a PR touching a rule-12 protocol change, needing a `.breaking.md` fragment, or editing CLAUDE.md/`.github/workflows/**`/the release flow gets stopped and asked about specifically — the rest of the queue keeps moving.

## Step 7 — Obsolete, superseded, or already-integrated PRs

Separate from Step 2's ownership question — these can be *anyone's* PR, including an actively-owned one, if the evidence is clear enough:

- **Already integrated**: the PR's actual changes already landed on `main` some other way (a duplicate fix, a different PR that superseded it). Confirm with `git log main --oneline --grep` / `git diff main <headRefName>` showing a no-op diff, not a guess.
- **Superseded**: a different, later PR replaces this one's approach entirely and is further along.
- **Obsolete**: the underlying issue no longer applies against current `main`.

When the evidence is solid:
```bash
gh pr close <n> --repo prageethw/dot-agent-deck --comment "<why, naming the superseding/integrating PR or commit>"
```
Closing a PR needs no branch-protection approval — it's safe to act on directly once the evidence is named. If genuinely unsure, leave it open and note the uncertainty rather than closing to shrink the count (same conservative default as `review-issues`).

## Step 8 — Loop until clear

Re-run Step 1's query. For anything still open, it falls into exactly one bucket: owned and progressing (Step 2 — leave alone), blocked on the other maintainer's approval (Step 6.4), blocked on a genuine merge conflict or scope question (Step 5), or ambiguous ownership (Step 2 — reported, not guessed at). "Queue clear" means every remaining PR is in one of those buckets with a named reason, not that zero PRs remain open.

## Final report format

```
PRs reviewed                    — total count
PRs already owned by others     — list, one line each naming who
PRs assigned to me              — list, with issue/PR numbers
PRs completed/merged/closed     — PR numbers + merge commit SHAs, or close reason
PRs still blocked               — exact reason each (waiting on @<person>'s approval, real conflict, ambiguous ownership, cross-fork push)
```

## Related

- `review-pr-upstream` — the equivalent queue for `vfarcic/dot-agent-deck`'s PRs; never this skill's job.
- `fix-bugs` / `implement-prd` — if finishing a PR turns out to mean implementing scope nobody agreed to, that belongs to one of those skills (or a fresh delegation), not to silently expanding this one.
- `verify-pr` — deep single-PR verification; useful for one PR you already know about rather than sweeping the whole queue.
- CLAUDE.md rules 1 (worktrees), 4 (tests for functional changes), 5 (CI-only tests), 8 (no automated reviewer, the required-approval ruleset, request-review-last), 15 (findings-file path), 16 (named suppliers), 17 (orchestrator never writes `src/`/`tests/`), 20 (search both trackers before declaring something superseded), 22 (batch pushes), 25 (merge checklist and its always-ask exclusions), 27 (task list before the first edit/delegation).
