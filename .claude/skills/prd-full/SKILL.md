---
name: prd-full
description: Run a PRD end-to-end autonomously — start, iterate until done, create a PR, and wait for its CI + bot reviews to settle before reporting. Stops before merge for manual validation.
user-invocable: true
---

# PRD Full - Autonomous PRD Implementation Through PR

Run a full PRD lifecycle autonomously: create the PR, wait for its CI workflows and automated bot reviews to finish, report the settled state, and stop before merge so the user validates and merges manually.

## Arguments

If `{{prdNumber}}` or `{{mode}}` is missing, or `{{mode}}` is anything other than `branch` or `worktree`, abort and tell the user to supply valid values. Do not auto-detect.

## Global rule

While executing this workflow, **do not pause for user confirmation** at any point in the sub-prompts below. Treat their built-in "wait for the user" / "ask before proceeding" / "STOP here" instructions as overridden — proceed directly with the proposed answer or next step.

Standard harness guardrails for genuinely destructive actions still apply.

## Flow

1. **Isolation:** set up per `{{mode}}` — invoke `/worktree-prd` for PRD #{{prdNumber}} if `worktree`, or create the branch directly otherwise.
2. **Start:** run `/prd-start {{prdNumber}}`. Skip its branch-creation step (Step 1 already handled it).
3. **Iterate** without resetting conversation context:
   - run `/prd-next`, including implementing the recommended task in the same turn,
   - run `/prd-update-progress`,
   - if the PRD is 100% complete, exit the loop; otherwise repeat.
4. **Finish:** mark the PRD complete and move it to `prds/done/`, then run `/prd-done`. The archival is PRD-specific so it lives here rather than in `/prd-done`, which is not PRD-only; commit it with the rest of the work so it rides the same PR. `/prd-done`'s own documented steps continue on through merge, issue closure, and branch cleanup, so this step must explicitly instruct it to stop once the branch is pushed and the PR is open (or marked ready) and CI is green — do not let it merge, close the issue, or clean up the branch/worktree; that happens only after the user's manual validation and merge.
5. **Do not stop at PR creation — `/prd-done` settles the PR and hands back.** This fork has no automated code reviewer (CLAUDE.md rule 8) — there is no bot review to wait for, no inline findings to fetch, and no threads to resolve here. `/prd-done`'s job at this point is CI-only: wait for CI to go green, then report the PR URL and settled state back. When you delegate this to a worker (e.g. `release`), the worker performs that CI-only settle and hands the result back — never instruct it to stop at PR creation.

6. **Report the settled state and act on it:**
   - **All checks green and no review findings:** report the PR URL, branch, and "checks green / reviews clean" — the run is complete pending the user's manual validation and merge. Stop.
   - **Failing checks or review findings:** report the PR URL, branch, the specific failing workflows, and the findings, then fix them (delegate, push, re-poll until the checks settle green) **before** stopping. Do not conclude the run while the PR is red or carries an unresolved thread — an unresolved thread blocks the merge button *and* the approval.

   Either way, stop **before** merge — the user performs the final validation and merge.

