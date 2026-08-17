---
name: review-issues
description: Triage the entire open issue list on prageethw/dot-agent-deck into a clean, actionable state — classify, size, prioritize, dedupe, close what's obsolete, split what's overloaded, link what's dependent. Read-only against code (verification only, never implementation). Use when the user asks to clean up the issue tracker, triage the backlog, or bring the issue list into shape.
user-invocable: true
---

# Triage, size, prioritize, and clean the issue list

Reviews every open issue on `prageethw/dot-agent-deck` and brings the list into a classified, sized, prioritized, duplicate-free, actionable state. **This skill never writes to `src/` or `tests/`, and never opens a branch, worktree, or PR** — every action here is a GitHub issue operation (`gh issue edit`/`close`/`comment`), not a code change. The one exception: reading code to check whether an issue is already fixed (Step 2) is verification, not implementation — if that check turns into "this needs a real fix," stop and hand it off (to `fix-bugs`/`implement-prd`/a normal delegation), don't implement it here.

**Be conservative.** Do not close an issue you're not sure about just to shrink the count — an issue left slightly stale is a much smaller cost than one closed wrongly. When genuinely uncertain, leave it as-is and note the uncertainty in the final report rather than guessing.

## Step 1 — Inventory

```bash
gh issue list --repo prageethw/dot-agent-deck --state open --json number,title,body,labels,updatedAt,createdAt --limit 300
```

Read the whole list before acting on any single issue — several of the steps below (duplicate detection, splitting) require seeing the full set, not one issue in isolation.

## Step 2 — Validate: still relevant, correctly described, not already done

For each issue:
- **Already fixed?** Search merged PRs referencing it (`gh pr list --state merged --search "<keywords>"`, and check `gh issue view <n> --json closedByPullRequestsReferences` even on an open issue — sometimes a PR fixed the underlying code without formally closing the issue). If the described behavior no longer reproduces against current `main`, this is a close candidate (Step 6), not something to leave open unlabeled.
- **Correctly described?** Referenced files/functions may have moved since filing. If the issue is still real but the description is stale enough to mislead, that's Step 8 (update), not Step 6 (close).
- **Genuinely uncertain?** Leave it — this step feeds Steps 6/8, it isn't itself a place to make a final call under doubt.

## Step 3 — Classify

This repo's actual label set (not a generic four-way split — read `docs/develop/work-types.md` for the authoritative definitions before assuming a mapping): `bug`, `PRD`, `enhancement`, `chore`, `documentation`, plus cross-cutting labels (`ci-cd`, `config`, `tests`, `source`, `affects-upstream`). Map the requested BUG/PRD/DOC/CHORE buckets onto these:
- **BUG** → `bug`
- **PRD** → `PRD` (runs the full PRD workflow, owns a doc under `prds/` — not every feature-shaped issue qualifies; a small, scoped ask is `enhancement` instead)
- **DOC** → `documentation`
- **CHORE** → `chore`
- **enhancement** doesn't collapse cleanly into any of the four — it's real and distinct in this repo's vocabulary (a feature-shaped ask too small/simple for the full PRD workflow). Keep it as its own bucket in the report rather than forcing it into PRD or CHORE.

Apply the label if missing; don't relabel an issue that's already correctly classified just to have touched it.

## Step 4 — Size and prioritize

Use the existing convention (CLAUDE.md rule 26) — `size-high`/`size-medium`/`size-low` and `priority-high`/`priority-medium`/`priority-low` — not an invented S/M/L/XL scale. Every issue gets exactly one of each. **If you're not confident of a priority call, apply `needs-triage` and leave priority unset rather than guess** — rule 26 is explicit that a wrong priority is worse than an absent one, since an absent one is visibly unclassified and a wrong one looks considered until someone checks.

Priority factors: user-visible impact, urgency, whether it blocks other open work, dependency position (a blocker for several other open issues outranks an equally-severe standalone one).

Note: `.github/workflows/issue-triage.yml` already auto-stamps `needs-triage` on any issue missing either label, on every `opened`/`reopened`/`labeled`/`unlabeled` event — this skill's pass is the deep, considered version of what that workflow enforces shallowly. Applying both labels correctly here is what clears the auto-stamp; you don't need to manually remove `needs-triage`.

## Step 5 — Duplicates

Group by subject, not by title wording — two issues describing the same underlying defect from different angles (a symptom report and a root-cause report) are duplicates even with no shared keywords. `gh issue list --search "<keywords>"` per candidate cluster, but the actual read is manual: skim bodies for the same file/function/mechanism.

When found: keep the more complete/accurate one open, close the other with a comment pointing at the survivor (`gh issue close <n> --comment "Duplicate of #<m>" --reason "not planned"`), and fold in anything the closed one had that the survivor lacks (a repro step, an edge case) as a comment on the survivor rather than losing it.

## Step 6 — Close what's obsolete

Already fixed (Step 2), superseded by a different approach, or no longer reproducible against current `main`. Always comment with the reason before closing (`gh issue close <n> --comment "<why>" --reason completed|"not planned"`) — a silent close is indistinguishable from one nobody thought about.

## Step 7 — Split overloaded issues

An issue bundling multiple unrelated concerns (e.g. "fix X, also Y is broken, and while we're at it Z") gets split: file a new issue per concern with `gh issue create`, cross-link both directions (new issue references the original; comment on the original linking each split-out issue), and narrow the original's title/body to just the concern it's actually tracking. Only split when the concerns are genuinely unrelated — a bug and its two symptoms in different files is still one issue, not three.

## Step 8 — Update unclear titles/descriptions

Only where it's needed to make the issue actionable — not a drive-by rewrite of every issue you touch. **Preserve existing discussion and history**: edit the issue body if the description itself is misleading, but never delete prior comments, and note significant edits in a comment rather than silently rewriting history readers may have already referenced.

## Step 9 — Link dependencies and blockers

Where one open issue genuinely can't be started until another lands, record it: a comment or body line on the blocked issue naming the blocker (`Blocked by #<n>`), and GitHub's native issue-linking relationship where available. Don't invent a dependency that's really just "these are related" — reserve this for a genuine can't-start-without-it relationship.

## Step 10 — Confirm the result

Re-list open issues (Step 1's query) and confirm every one now carries exactly one classification label, one size, one priority (or `needs-triage` where priority is genuinely uncertain), and no unresolved duplicate.

## Final report format

```
total issues reviewed        — count
counts by BUG/PRD/DOC/CHORE   — plus enhancement, called out separately
size breakdown                 — size-high / size-medium / size-low counts
priority breakdown             — priority-high / -medium / -low / needs-triage counts
duplicates/obsolete closed     — issue numbers, one-line reason each
issues split or reclassified   — what changed and why
remaining high-priority items  — issue numbers, for whoever picks up fix-bugs/implement-prd next
```

## Related

- `fix-bugs` / `implement-prd` — where a `bug`/`PRD` this pass surfaces as genuinely actionable gets picked up next; this skill classifies and prioritizes, it doesn't implement.
- CLAUDE.md rule 20 (search both issues and PRs, both trackers, before assuming something is undone), rule 26 (the size/priority label vocabulary and the `needs-triage` backstop), `docs/develop/work-types.md` (the authoritative BUG/PRD/CHORE/DOC definitions this skill's classification step maps onto).
