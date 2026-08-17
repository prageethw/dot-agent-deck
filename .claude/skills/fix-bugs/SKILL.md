---
name: fix-bugs
description: Work through the entire open high-priority bug queue — inventory, reproduce, fix, review, and auto-merge each defect without pausing for approval, except where CLAUDE.md rule 25 requires it. Use when the user asks to clear the bug queue, fix all high-priority defects, or run an unattended bug-fixing sweep. Does not pick up PRDs, chores, refactors, or unrelated cleanup.
user-invocable: true
---

# Fix all high-priority defects and merge automatically

Clears the open `bug` + `priority-high` queue on `prageethw/dot-agent-deck`, end to end, one defect at a time or in parallel where they're independent — without pausing for a merge go-ahead on each one. This is the orchestrator's workflow; every step below assumes `.dot-agent-deck/orchestrator-context.md`'s delegation model (worker-agent-deck delegate, the coder/tester/reviewer/auditor/release roles) is already in context.

**Scope discipline**: bugs only (`bug` label, `priority-high`). Do not pick up anything labeled `PRD`, `chore`, `enhancement`, or `documentation` — even if you notice something nearby that looks worth fixing. If a bug turns out to require PRD-scale design work, say so and leave it as a blocked item rather than expanding scope.

## Step 1 — Inventory

```bash
gh issue list --repo prageethw/dot-agent-deck --label bug --label priority-high --state open --json number,title,labels,updatedAt,body
```

For each result, also check whether it's already claimed (`in-progress` label) by a live orchestration — a claimed issue with a worktree that still exists is someone else's; do not touch it (CLAUDE.md rule 14). An issue whose claim's worktree no longer exists is stale and can be taken over via `worker-agent-deck issue claim <n> --repo prageethw/dot-agent-deck --takeover --confirm-stopped`, but only after confirming with the user that the prior orchestration has actually stopped — do not infer this yourself.

## Step 2 — Confirm each is genuine and still relevant

For each candidate, skim the issue body and check the referenced code paths still look as described (files/functions may have moved since it was filed). Don't assume "filed" means "still true" — a fix elsewhere may have already resolved it. If you can't tell from reading, that's what step 5 (reproduce) settles definitively; don't gate step 1's inventory on doing a full reproduction for every candidate up front.

## Step 3 — Group by independence, and by what "parallel" actually means here

Two bugs are independent if they touch disjoint files/subsystems and neither issue's fix plausibly changes behavior the other depends on. Note the group, but also note the real constraint: **this orchestration has one coder identity and one tester identity, not a pool** — "delegate to different workers in parallel" (rule in `orchestrator-context.md`) means different *roles* at once (e.g. tester writing bug B's RED test while coder implements bug A's fix), not two coders running two bugs simultaneously. So:
- Independent bugs run **sequentially through the coder/tester chain**, each in its own worktree/branch, but you can overlap *roles* across bugs (e.g. reviewer+auditor confirming bug A's PR while tester starts bug B's RED test) to shorten wall-clock time.
- Dependent or overlapping bugs (same file, same function, one's fix changes what the other's test asserts) are handled **in dependency order**, never in parallel with each other even nominally — merge one before starting the other's implementation, so the second doesn't build against a moving target.

## Step 4 — Per defect: worktree, claim, reproduce, fix, review, merge

For each defect, in the order step 3 established:

1. **Worktree** (CLAUDE.md rule 1): `git worktree add -b fix/<n>-<slug> ../dot-agent-deck-<slug> origin/main`, immediately `git branch --unset-upstream`. State the worktree's absolute path, exact commit SHA, and branch name in every delegation — never let a task name no path or the root checkout.
2. **Claim**: `worker-agent-deck issue claim <n> --repo prageethw/dot-agent-deck` from inside that worktree, before the first delegation.
3. **Open a draft PR immediately** (rule 5) — a push with no PR open produces no CI run to read.
4. **Reproduce, then fix** — this is the `reproduce-first` skill's discipline, applied per defect: delegate to `tester` to turn the reported symptom into a failing test that fails for the reporter's actual reason (not a setup error), confirmed RED from CI. Then delegate to `coder` to implement the smallest correct fix — production code only, never editing the tester's test. Then back to `tester` to confirm GREEN. Never run `cargo test-fast`/`cargo test-e2e` locally (rule 5) — every RED/GREEN confirmation is a CI round trip; batch each worker's changes so one push answers everything it can (rule 22).
5. **Review**: delegate to `reviewer` + `auditor` in parallel once CI is green. Resolve every finding you agree with (rule 8) — re-delegate fixes to coder/tester, then get a confirmation pass from both on the fixed head before merging, the same way a multi-round fix needs re-verification against the final SHA (rule 25).
6. **Merge — the default here is yes, without asking, per this skill's whole point.** Before merging, verify EVERY condition in CLAUDE.md rule 25's checklist: every expected CI check present and passing (read e2e from the job log, never the run conclusion — rule 8), both reviewer and auditor returned on the final head, every agreed-with finding resolved or dispositioned, no blocker accepted rather than fixed, `mergeable=MERGEABLE` / `mergeStateStatus=CLEAN`, closing-keywords audited clean on both the PR body and every commit message. Post the merge report as a PR comment (rule 25) before merging, then delegate the merge itself to `release` with `--match-head-commit`.

   **The one hard stop, even in this skill**: rule 25's always-ask exclusion is not yours to waive. If the fix touches a rule-12 protocol change, adds/needs a `.breaking.md` fragment, or edits CLAUDE.md/`.github/workflows/**`/the release flow, **stop and ask the user for that one merge specifically** — do not silently fold it into the "merge automatically" instruction. Everything else in the queue keeps moving; only that one defect's merge pauses.
7. **Clean up**: remove the worktree once merged (`git worktree remove`). Delete your own `.dot-agent-deck/<task-slug>.md` task files once each handoff succeeds.

## Step 5 — After the batch: full validation from `main`

Once every mergeable defect has landed, confirm `main`'s own CI is green (the post-merge `push` trigger on `ci.yml`/`e2e.yml` — rule 5's addendum: this is what catches "main broke because a squash-merge combined cleanly on GitHub but not in practice"). If a regression shows up that traces to one of this batch's merges, treat it exactly like any other reported bug: reproduce it as a failing test, fix it, merge that fix through the same per-defect flow above — don't patch it silently outside the loop.

## Step 6 — Confirm the queue is clear

Re-run step 1's `gh issue list` query. Report each remaining item's exact reason: genuinely blocked (name the blocker, same as CLAUDE.md rule 1/PRD-workflow's "record the exact blocker and move to the next" discipline), out of scope (PRD-scale, or not actually a bug), or claimed by another live orchestration.

## Final report format

Brief, per the shape this skill was asked for:

```
defects fixed        — list, with issue numbers
root causes           — one line each
fixes merged           — PR numbers + merge commit SHAs
validation status      — main's post-merge CI result
defects still blocked  — exact reason each, not "couldn't get to it"
```

## Related

- `reproduce-first` — the per-defect reproduce → fix → confirm discipline this skill applies in a loop.
- `verify-pr` — if a defect's "fix" turns out to already exist on an open PR from someone else, verify that PR instead of duplicating the work (CLAUDE.md rule 20 — search both trackers, issues *and* PRs, before starting).
- CLAUDE.md rules 1 (worktrees), 5 (CI-only tests), 8 (no automated reviewer — reviewer+auditor are the gate), 14/23 (issue claim), 16 (named suppliers), 20 (search both trackers), 22 (batch pushes), 25 (the merge checklist and its always-ask exclusions), 27 (task list before the first edit/delegation).
