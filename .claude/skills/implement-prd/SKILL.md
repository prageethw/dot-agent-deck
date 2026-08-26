---
name: implement-prd
description: Work through the entire open high-priority PRD/enhancement queue — inventory, scope, implement via the full PRD workflow, review, and auto-merge each item without pausing for the test-plan or merge gate, except where CLAUDE.md rule 25 requires it. Use when the user asks to clear the PRD backlog, implement all high-priority PRDs, or run an unattended feature-implementation sweep. Does not pick up bugs, chores, refactors, or unrelated cleanup.
user-invocable: true
---

# Implement all high-priority PRDs and enhancements, unattended through merge

Clears the open `PRD`/`enhancement` + `priority-high` queue on `prageethw/dot-agent-deck`, end to end — without pausing at the gates `orchestrator-context.md`'s ordinary PRD workflow otherwise stops at. Read that deliberate deviation carefully below before invoking this: it is a real change in how much gets built and merged without a human looking, and it is not free.

**Scope discipline**: PRDs and enhancements only (`PRD` or `enhancement` label, `priority-high`). Do not pick up anything labeled `bug` unless it also carries `PRD` or `enhancement` — a `bug`-labeled issue that also carries `PRD` is this skill's, not `fix-bugs`' (see Step 1's post-filter below; `fix-bugs` explicitly routes `bug`+`PRD` issues here, live example right now: #358, #256, #254). Anything labeled `chore` or `documentation` is out of scope here regardless of its other labels — including a `PRD`+`chore` issue (live example: #286), which is out of scope for both this skill and `fix-bugs`. A bug with no `PRD`/`enhancement` label that's merely blocking a PRD gets reported as a blocker, not silently fixed inline (fix it via the `fix-bugs` skill or a separate delegation, so it's tracked as its own change). If a queued item turns out to be genuinely bug-shaped rather than PRD-shaped once you read it — and carries no `PRD`/`enhancement` label — say so and leave it for `fix-bugs` rather than implementing it here.

## The deliberate deviation from the standing workflow, stated plainly

`.dot-agent-deck.toml`'s orchestrator role and `orchestrator-context.md` establish the **test-plan approval** (before any implementation starts) as a standing user gate for ordinary PRD work, plus a **merge confirmation** (before the PR lands) that CLAUDE.md rule 25 makes conditional — a gate only when its checklist fails or its always-ask exclusion applies, not unconditionally. This skill exists specifically to run **unattended through the test-plan gate, and through the merge gate whenever rule 25's checklist would otherwise let it proceed unattended anyway**, because "do not stop after the first PRD" and "merge each completed PRD as soon as it is ready" are the whole point of invoking it by name.

That is an explicit, scoped opt-in — the user invoking `/implement-prd` is the approval that would otherwise be sought interactively per PRD. It is not a standing change to how PRD work happens outside this skill; a PRD started any other way still gets both gates.

**What still stops you, even here**: CLAUDE.md rule 25's always-ask exclusion is not waived by this skill. If a PRD's diff touches a rule-12 protocol change, needs a `.breaking.md` fragment, cuts a release/tag, or edits CLAUDE.md/`.github/workflows/**`/the release flow, **stop and ask the user for that one PRD's merge specifically** — do not fold it into "merge automatically." Keep working the rest of the queue while that one waits. Expect this to trigger often: a "high-priority" PRD is disproportionately likely to be exactly the kind of substantive, protocol-touching change rule 25 singles out (this happened on the very first PRD run through this repo's normal flow — a routine daemon-authoritative-id PRD turned out to bump `PROTOCOL_VERSION` and needed the exclusion).

## Step 1 — Inventory

```bash
gh issue list --repo prageethw/dot-agent-deck --label PRD --label priority-high --state open --json number,title,labels,updatedAt
gh issue list --repo prageethw/dot-agent-deck --label enhancement --label priority-high --state open --json number,title,labels,updatedAt
```

Each query is a **pre-filter only** — `gh --label` is AND-only and has no negation, so it cannot express the scope-discipline exclusion above (`bug`, `chore`, `documentation`). Apply that exclusion as an explicit **post-filter** on the results: for each issue returned, check its full label set. If an issue also carries `bug`, `PRD`/`enhancement` wins here — this is the mirror of `fix-bugs`' own routing (that skill drops `bug`+`PRD` issues and routes them here), so a co-labeled issue stays in scope for this skill rather than getting dropped (real example right now: #358, #256, #254 all carry both `bug` and `PRD`, and all three are returned by the first query above). If an issue also carries `chore` or `documentation`, that label wins instead and the issue is out of scope here — report it under Step 6 (real example: #286 carries both `PRD` and `chore`, though not currently `priority-high`).

Cross-reference against `prds/` (root = not started or in progress; `prds/done/` = already merged and just needs archiving, not implementing — see Step 2) and against open/merged PRs and branches (`gh pr list --repo prageethw/dot-agent-deck --state all --search "<keywords>"`, `git branch -a`) per CLAUDE.md rule 20 — search **both** issues and PRs, on **both** this fork and `vfarcic/dot-agent-deck`, before assuming an item needs fresh work.

For each remaining result, also check whether it's already claimed (`in-progress` label) — this is a cheap **pre-filter**, not the check itself: a claimed issue with a worktree that still exists is likely someone else's, so deprioritize it in this pass. The actual check happens at Step 4 item 1's `worker-agent-deck issue claim`, which refuses and exits non-zero if another identity already holds the issue (CLAUDE.md rules 14/23) — see that step for what to do on refusal.

## Step 2 — Confirm relevance, scope, and "not already implemented"

This is the single most valuable check before writing any code, and skipping it is how duplicate work happens. For each candidate:

- **Already merged, doc not archived?** Check the PRD file's own `**Status**` line and cross-check against `gh pr list --repo prageethw/dot-agent-deck --state all --search "<PRD keywords>"` — rule 20 requires `--state all`, not `--state merged`: a `merged`-only search can't see an *open* PR, which is exactly what the next bullet ("partially implemented, continue don't restart") needs to find. A PRD whose code already landed needs its doc moved to `prds/done/` (an orchestrator-direct edit, rule 17 — not implementation), not a re-implementation. This is common enough that it should be the FIRST thing checked, not an afterthought — a prior sweep of this exact repo found 6+ PRDs in this state in one pass.
- **Partially implemented?** Check for an existing branch/worktree/open PR touching this issue. If a real branch with real commits exists, **continue that work, don't restart it** — read its commit history and any PRD-doc "Status"/decisions section for where it left off, same as resuming any interrupted PRD run.
- **Still correctly scoped?** Re-read the issue against current `main` — referenced files/functions may have moved, or a related PRD may have already covered part of it. If the scope has shifted enough that the issue's own description is stale, note that in your final report rather than guessing at a new scope yourself.

## Step 3 — Dependencies and grouping

Two PRDs are independent if they touch disjoint subsystems and neither's success criteria assumes the other has landed. As with `fix-bugs`: **this orchestration has one coder/tester identity each, not a pool** — "parallel" means overlapping *roles* across different PRDs' worktrees (tester on PRD B's tests while coder implements PRD A), not two coders running two PRDs at once. Dependent or overlapping PRDs are sequenced — merge one before starting the next's implementation, never worked "in parallel" against each other even nominally.

## Step 4 — Per PRD: the full workflow, minus the two interactive stops

For each PRD, in the order Step 3 established, follow `orchestrator-context.md`'s PRD workflow exactly, with these two changes:

1. **Worktree + claim, as always** (CLAUDE.md rule 1, rule 14): dedicated worktree, upstream unset immediately, `worker-agent-deck issue claim <n> --repo prageethw/dot-agent-deck` before the first delegation. State the worktree's absolute path, exact commit SHA, branch name, and the explicit push form (`git push origin HEAD:refs/heads/<branch>`) in every delegation — never let a task name no path, no push form, or the root checkout. **If the claim refuses** (another identity already holds it): remove the worktree you just created — it's orphaned, since the claim never succeeded — report the issue under Step 6 as claimed by another live orchestration, and move to the next candidate; do not self-authorize a takeover (rule 23). Only if the held claim's worktree no longer exists, and the user has explicitly confirmed the prior orchestration actually stopped, re-run with `--takeover --confirm-stopped` — never infer that confirmation yourself.
2. **Test plan — build it, record it, don't stop for it.** Read the PRD file (or, if only an `enhancement` issue exists with no PRD doc, that issue's own description) for scope and acceptance criteria. Produce the same catalog-ID / tier / scenario / action table the ordinary workflow presents to the user — but instead of pausing, proceed directly into implementation with that table as your own plan of record (write it into the PRD file or a `.dot-agent-deck/` task-list note, per rule 27, so it's still visible and auditable, just not gated on).
3. **Open a draft PR immediately** (rule 5) — before any push, or CI produces nothing to read. **At this first push, invoke the `dot-ai-changelog-fragment` skill** to create the `changelog.d/<n>.<type>.md` fragment — this is the standard PRD workflow's own convention for fragment *creation*, and it is a separate, explicit step here, not something `orchestrator-context.md` step 6(c) already covers (see item 8 below).
4. **TDD chain**: tester (RED) → coder (GREEN, production code only) → tester (confirm), or coder-direct for pure-data/non-test items, exactly as the standing workflow describes. Never run tests locally (rule 5); batch each worker's pushes (rule 22).
5. **Follow existing fork architecture and conventions** — this is not optional flavor text: read the relevant `CLAUDE.md` rules for the subsystem being touched (protocol/daemon changes → rule 12; experimental-flag surfaces → rule 9; test tier choice → rule 4) before implementing, the same way every PRD run in this repo has had to.
6. **When implementation surfaces a regression or a scope gap the PRD didn't anticipate** (this is common, not exceptional — a straightforward-looking PRD in this repo has repeatedly turned out to regress pre-existing tests or leave a stated success criterion unmet) — treat it as in-scope repair, not a reason to stop and ask, unless it independently qualifies as a bug outside this PRD's own subsystem (in which case: flag it, don't fix it here — see Scope discipline above).
7. **Review**: `reviewer` + `auditor` in parallel once CI is green. State the findings-file absolute path in each delegation — the root checkout's `.dot-agent-deck/` directory (derivable from inside any worktree via `dirname "$(git rev-parse --path-format=absolute --git-common-dir)"`, CLAUDE.md rule 15), so the merge report can quote each verdict together with its path. Resolve every finding you agree with (rule 8); get a confirmation pass from both on the final head before merging if the fix round touched anything substantive (rule 25 — a review at an earlier SHA doesn't cover a later commit).
8. **Merge gate — proceed without asking, per this skill's whole point, unless rule 25's always-ask exclusion applies (see above).** Verify the full rule 25 checklist (every CI check present and passing, e2e read from the job log not the run conclusion, both reviewer and auditor returned on the final head, every finding resolved **and** every behavioural fix pinned by a test — record the disposition and reason in the merge report for anything a test cannot pin (docs, comments, verified-no-change), no blocker accepted rather than fixed, `mergeable=MERGEABLE`/`mergeStateStatus=CLEAN`, closing-keywords clean on both the PR body and every commit message, and the changelog fragment present — created via the `dot-ai-changelog-fragment` skill at item 3's first push (above); confirm it landed, don't assume it did. `orchestrator-context.md` step 6(c) is a separate, later concern: it only *appends* the demo-reel link to a fragment that already exists, and it skips even that when the branch has no e2e change — it never creates a fragment). Post the merge report as a PR comment before merging. Delegate the merge itself to `release` with `--match-head-commit`. **If any condition fails, stop and ask the user about that PRD's merge specifically, exactly as with rule 25's always-ask exclusion above — the rest of the queue keeps moving.**
9. **Upstream offer, once merged** (rule 19): apply rule 19's decision test to every merged PRD — "would upstream want this?" — regardless of whether the PRD's own M4 milestone or scope mentions it; rule 19's test applies uniformly to every PRD, not only ones that call it out. If the merged work is a genuine improvement upstream would also want and lands on code the fork had already diverged on, file the tracking issue at merge time, in the shape of `#322` (naming what to port, upstream's matching issue if one exists, and what makes the port non-trivial). **Check, don't assume, whether that filing already happened**: an `affects-upstream` label means a maintainer decided the defect *should* be tracked upstream, not that anyone actually filed it — search the upstream tracker (rule 20: both issues and PRs, `--state all`) for an issue naming this fork's issue or PR before concluding it's already there. If nothing turns up, file it now; otherwise note it in the merge report instead of re-filing. Read CLAUDE.md rule 19 directly before applying this — the decision test, the four "file upstream" shapes, and the maintainer-only closing decision are easy to paraphrase wrong.
10. **Clean up**: remove the worktree once merged. Delete your own `.dot-agent-deck/` task files once each handoff succeeds.

## Step 5 — After the batch: full validation from `main`

Confirm `main`'s post-merge CI is green (rule 5's addendum — this is the only thing that catches a squash-merge combining cleanly on GitHub but not in practice). A regression traced to this batch gets fixed through the same per-PRD flow above, as its own tracked step — not patched silently outside the loop.

## Step 6 — Confirm the queue is clear

Re-run Step 1's queries. For anything remaining, report the exact reason: genuinely blocked (name the blocker — a dependency on another open issue, a design question nobody's answered, a rule-25 merge waiting on the user), out of scope (turned out to be bug-shaped, or needs a scope decision only a human can make), or claimed by another live orchestration.

Also run `gh issue list --repo prageethw/dot-agent-deck --label PRD --label needs-triage --state open` and the same for `enhancement`, and report those counts separately — they're invisible to step 1's queries (no `priority-high` label yet) but represent real open items the queries can't see (CLAUDE.md rule 26). "Queue clear" means clear of *triaged* high-priority PRDs/enhancements, not that none remain.

## Final report format

```
PRDs/enhancements completed  — list, with issue numbers
key changes                  — one or two lines each
merges performed              — PR numbers + merge commit SHAs
validation status             — main's post-merge CI result
remaining blocked items       — exact reason each
```

Flag every PRD whose merge paused on rule 25's exclusion separately and explicitly — those need the user's attention in a way nothing else in this report does.

## Related

- `fix-bugs` — the equivalent unattended sweep for the `bug`/`priority-high` queue; route anything bug-shaped there instead of implementing it inline here.
- `reproduce-first` — applies within Step 4's TDD chain wherever a PRD item is really "fix behavior that's wrong" rather than "add behavior that doesn't exist yet."
- CLAUDE.md rules 1 (worktrees), 4 (test tier), 5 (CI-only tests, draft-PR-first), 8 (reviewer+auditor as the gate), 9 (experimental flag), 12 (protocol/breaking changes), 14/23 (issue claim), 16 (named suppliers), 17 (orchestrator never writes src/tests), 19 (upstream offer), 20 (search both trackers), 22 (batch pushes), 25 (merge checklist and always-ask exclusions), 26 (size/priority labels), 27 (task list before the first edit/delegation).
