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

This query is a **pre-filter only** — `gh --label` is AND-only and has no negation, so it cannot express the scope-discipline exclusion above (`PRD`, `chore`, `enhancement`, `documentation`). Apply that exclusion as an explicit **post-filter** on the results: for each issue returned, check its full label set. If it also carries `PRD`, `PRD` wins even though the issue is also labeled `bug` — drop it from this sweep, report it under Step 7 as out of scope (PRD-scale), and route it to the `implement-prd` skill instead (real example right now: #358, #256, #254 all carry both `bug` and `PRD`). The same applies if it also carries `chore`, `enhancement`, or `documentation`: that label wins and the issue is out of scope here. Do not fix a co-labeled issue under this skill just because the query returned it.

For each remaining result, also check whether it's already claimed (`in-progress` label) — this is a cheap **pre-filter**, not the check itself: a claimed issue with a worktree that still exists is likely someone else's, so deprioritize it in this pass. The actual check is `worker-agent-deck issue claim <n> --repo prageethw/dot-agent-deck` at step 5.2, which refuses and exits non-zero if another identity already holds the issue (CLAUDE.md rules 14/23) — that refusal, not a label read, is what determines whether the issue is actually available. An issue whose claim's worktree no longer exists is stale and can be taken over via `worker-agent-deck issue claim <n> --repo prageethw/dot-agent-deck --takeover --confirm-stopped`, but only after confirming with the user that the prior orchestration has actually stopped — do not infer this yourself.

## Step 2 — Confirm each is genuine and still relevant (mandatory per-candidate check, not a skim)

**A checkout that's behind `origin/main` gives false confidence here before any other check runs.** Confirm the checkout you're reading code from (root checkout, if you're reading before creating worktrees) is actually current — `git fetch origin --quiet && git log origin/main -1` compared against `git log -1` on whatever you're about to read — and fast-forward it if it's stale. Validating an issue's code references against a checkout that's several merges behind is worse than not checking at all: it produces the appearance of diligence with none of the substance, and every conclusion drawn from it (still valid, already fixed, file/line still accurate) is unreliable until this is confirmed.

For each candidate, this is a required checkpoint, not an optional pass — do not report a queue as "clear" or move a candidate to Step 3 without having done this:

1. **Read the exact code the issue names**, not just its prose description. If it cites a file:line or function, open that file and confirm the named code still exists in the shape described — line numbers drift constantly (every PR before it in the same file shifts them), so a drifted line number is not itself a reason to doubt the issue, but the *function or mechanism* being gone, renamed beyond recognition, or already doing what the issue asks is. `grep -n` for the function/const/symbol name named in the issue body is normally faster than trusting a stale line number.
2. **Check whether a *different, newer* issue already covers the same ground more precisely.** An issue can be superseded without being wrong — a later issue filed during a subsequent review round often restates the same defect with current line numbers and a narrower, more accurate scope (this happened this session: issue #670 was closed as superseded by #683, which said the same thing with the file having moved under it). Search by the underlying mechanism/function name, not the older issue's title verbatim.
3. **Check whether a fix elsewhere already resolved it** — `gh pr list --repo prageethw/dot-agent-deck --state all --search "<keywords>"` (and the same against `vfarcic/dot-agent-deck`) before assuming the defect needs fresh work — CLAUDE.md rule 20's PR half: an issue records that something *should* be done, a PR records that it *has been*, and `gh issue list` never returns PRs. If an open PR already fixes this issue, verify that PR instead of duplicating the work (see `verify-pr` under Related; live example right now: #282 already has PR #431 open against it).

**Don't assume "filed" means "still true."** If you can't tell from reading, that's what step 5.4 (reproduce) settles definitively — this step doesn't require a full reproduction for every candidate, but it does require an actual look at the current code, not a memory of what the issue said or an assumption that nothing has changed since filing.

**When you conclude a candidate is stale, already fixed, or superseded, say so with the evidence** (the PR number, the current code you read and what it shows, the superseding issue number) — the same evidence discipline the `review-issues` skill's Step 3/7 require for a close. An unevidenced "still looks fine" is indistinguishable from not having checked. If the queue turns out to need real closes/updates rather than fixes, that's `review-issues`' job (see Related) — do the check here, hand off the classification work there rather than making ad-hoc closes mid-sweep.

## Step 3 — Determine upstream-code-defect status, at discovery time (rule 19)

Before grouping (Step 4) or delegating any per-defect work, apply CLAUDE.md rule 19's decision test to each confirmed candidate: "would upstream want this?" If yes, ask the follow-up rule 19 actually asks — does upstream still carry this defect, in code the fork has not already fixed? Read rule 19 directly rather than paraphrasing from memory: it has the two-grep evidence test (`-F`, both `upstream/main` fetched fresh and `HEAD`, on the exact defective literal — not the whole line), the four shapes that decide it, and the trust/privilege-boundary carve-out.

- **Genuine upstream-code defect, not yet diverged on**: file it upstream *now*, at the point it's found — this is rule 19's discovery-time path, not something deferred to after a fork fix merges. Search both trackers first (rule 20) so the filing doesn't duplicate an existing upstream issue. Record the filing and move to the next candidate; do not also fix it in the fork.
- **Already diverged, or doesn't clear the upstream-decision test**: proceed through the normal fix flow (Step 5). The *offer* step at Step 5.7 still applies post-merge for a genuine improvement that merely happens to be written here — this discovery-time step only settles the file-instead branch, never the offer-after-fixing branch.
- **Crosses a privilege or trust boundary** (rule 19's carve-out): escalate to the user before filing anywhere, rather than filing immediately.

## Step 4 — Group by independence, and by what "parallel" actually means here

Two bugs are independent if they touch disjoint files/subsystems and neither issue's fix plausibly changes behavior the other depends on. Note the group, but also note the real constraint: **this orchestration has one coder identity and one tester identity, not a pool** — "delegate to different workers in parallel" (rule in `orchestrator-context.md`) means different *roles* at once (e.g. tester writing bug B's RED test while coder implements bug A's fix), not two coders running two bugs simultaneously. So:
- Independent bugs run **sequentially through the coder/tester chain**, each in its own worktree/branch, but you can overlap *roles* across bugs (e.g. reviewer+auditor confirming bug A's PR while tester starts bug B's RED test) to shorten wall-clock time.
- Dependent or overlapping bugs (same file, same function, one's fix changes what the other's test asserts) are handled **in dependency order**, never in parallel with each other even nominally — merge one before starting the other's implementation, so the second doesn't build against a moving target.

## Step 5 — Per defect: worktree, claim, reproduce, fix, review, merge

For each defect, in the order step 4 established:

1. **Worktree** (CLAUDE.md rule 1): `git worktree add -b fix/<n>-<slug> ../dot-agent-deck-<slug> origin/main`, immediately `git branch --unset-upstream`. State the worktree's absolute path, exact commit SHA, branch name, and the explicit push form (`git push origin HEAD:refs/heads/fix/<n>-<slug>`) in every delegation — never let a task name no path, no push form, or the root checkout.
2. **Claim**: `worker-agent-deck issue claim <n> --repo prageethw/dot-agent-deck` from inside that worktree, before the first delegation.
3. **Open a draft PR immediately** (rule 5) — a push with no PR open produces no CI run to read.
4. **Reproduce, then fix** — this is the `reproduce-first` skill's discipline, applied per defect: delegate to `tester` to turn the reported symptom into a failing test that fails for the reporter's actual reason (not a setup error), confirmed RED from CI. Then delegate to `coder` to implement the smallest correct fix — production code only, never editing the tester's test — and, in the same delegation, create the `changelog.d/<n>.bugfix.md` fragment (per the `dot-ai-changelog-fragment` convention this repo already uses) so it rides the same push. `xtask/linkage-check`'s work-type gate falls back to the branch name's work-type prefix when a fragment is missing, so nothing fails without it — this has to be an explicit step, not assumed-covered. Then back to `tester` to confirm GREEN. Never run `cargo test-fast`/`cargo test-e2e` locally (rule 5) — every RED/GREEN confirmation is a CI round trip; batch each worker's changes so one push answers everything it can (rule 22).
5. **Review**: delegate to `reviewer` + `auditor` in parallel once CI is green. State the findings-file absolute path in each delegation — the root checkout's `.dot-agent-deck/` directory (derivable from inside any worktree via `dirname "$(git rev-parse --path-format=absolute --git-common-dir)"`, CLAUDE.md rule 15), so the merge report can quote each verdict together with its path. Resolve every finding you agree with (rule 8) — re-delegate fixes to coder/tester, then get a confirmation pass from both on the fixed head before merging, the same way a multi-round fix needs re-verification against the final SHA (rule 25).
6. **Merge — the default here is yes, without asking, per this skill's whole point.** Before merging, verify EVERY condition in CLAUDE.md rule 25's checklist: every expected CI check present and passing (read e2e from the job log, never the run conclusion — rule 8), both reviewer and auditor returned on the final head, every agreed-with finding resolved **and** every behavioural fix pinned by a test — record the disposition and reason in the merge report for anything a test cannot pin (docs, comments, verified-no-change); this is the condition that matters most on a bug sweep, since every item in the queue is by definition a behavioural fix — no blocker accepted rather than fixed, `mergeable=MERGEABLE` / `mergeStateStatus=CLEAN`, closing-keywords audited clean on both the PR body and every commit message, and the changelog fragment present. Post the merge report as a PR comment (rule 25) before merging, then delegate the merge itself to `release` with `--match-head-commit`. **If any condition fails, stop and ask the user about that merge specifically, exactly as in the hard stop below — the default here is auto-merge, not merge-regardless; the rest of the queue keeps moving.**

   **The one hard stop, even in this skill**: rule 25's always-ask exclusion is not yours to waive. If the fix touches a rule-12 protocol change, adds/needs a `.breaking.md` fragment, or edits CLAUDE.md/`.github/workflows/**`/the release flow, **stop and ask the user for that one merge specifically** — do not silently fold it into the "merge automatically" instruction. Everything else in the queue keeps moving; only that one defect's merge pauses.
7. **Upstream offer** (rule 19), once merged: this defect already cleared Step 3's discovery-time determination as "already diverged, or doesn't clear the upstream-decision test" — so re-apply rule 19's decision test now that the fix is merged: "would upstream want this?" If the merged fix is a genuine correction upstream would also want, file the upstream-offer tracking issue now, in the shape of `#322` — naming what to port, where upstream's matching issue is if one exists, and what makes the port non-trivial. **Check, don't assume, whether that filing already happened**: an `affects-upstream` label on the issue means a maintainer decided the defect *should* be tracked upstream, not that anyone actually filed it — search the upstream tracker (rule 20: both issues and PRs, `--state all`) for an issue naming this fork's issue or PR before concluding it's already there. If nothing turns up, file it now. Read CLAUDE.md rule 19 directly before applying this — the decision test, the four "file upstream" shapes, and the maintainer-only closing decision are easy to paraphrase wrong.
8. **Clean up**: remove the worktree once merged (`git worktree remove`). Delete your own `.dot-agent-deck/<task-slug>.md` task files once each handoff succeeds.

## Step 6 — After the batch: full validation from `main`

Once every mergeable defect has landed, confirm `main`'s own CI is green (the post-merge `push` trigger on `ci.yml`/`e2e.yml` — rule 5's addendum: this is what catches "main broke because a squash-merge combined cleanly on GitHub but not in practice"). If a regression shows up that traces to one of this batch's merges, treat it exactly like any other reported bug: reproduce it as a failing test, fix it, merge that fix through the same per-defect flow above — don't patch it silently outside the loop.

## Step 7 — Confirm the queue is clear

Re-run step 1's `gh issue list` query. Report each remaining item's exact reason: genuinely blocked (name the blocker, same as CLAUDE.md rule 1/PRD-workflow's "record the exact blocker and move to the next" discipline), out of scope (PRD-scale, or not actually a bug), or claimed by another live orchestration.

Also run `gh issue list --repo prageethw/dot-agent-deck --label bug --label needs-triage --state open` and report that count separately — it's invisible to step 1's query (no `priority-high` label yet) but represents real open bugs the query cannot see (CLAUDE.md rule 26). "Queue clear" means clear of *triaged* high-priority bugs, not that no bugs remain.

## Final report format

Brief, per the shape this skill was asked for:

```
defects fixed          — list, with issue numbers
root causes             — one line each
fixes merged             — PR numbers + merge commit SHAs
validation status        — main's post-merge CI result
defects still blocked    — exact reason each, not "couldn't get to it"
stale/superseded/fixed   — issue numbers found obsolete in Step 2, with evidence, and whether you closed them or left that to review-issues
```

## Related

- `reproduce-first` — the per-defect reproduce → fix → confirm discipline this skill applies in a loop.
- `verify-pr` — if a defect's "fix" turns out to already exist on an open PR from someone else, verify that PR instead of duplicating the work (CLAUDE.md rule 20 — search both trackers, issues *and* PRs, before starting).
- `review-issues` — the deeper, dedicated version of Step 2's validation check, run across the whole backlog rather than just this sweep's candidates. Reach for it when Step 2 turns up more staleness than this sweep should absorb ad hoc, or when the user asks for a full triage pass rather than a fix sweep.
- CLAUDE.md rules 1 (worktrees), 5 (CI-only tests), 8 (no automated reviewer — reviewer+auditor are the gate), 14/23 (issue claim), 16 (named suppliers), 20 (search both trackers), 22 (batch pushes), 25 (the merge checklist and its always-ask exclusions), 27 (task list before the first edit/delegation).
