# PRD fork#325 — Should concurrent orchestrations share one git clone at all?

**Issue:** [prageethw/dot-agent-deck#325](https://github.com/prageethw/dot-agent-deck/issues/325)
**Priority:** High
**Status:** Planning — decision needed, not yet an implementation plan
**Predecessors:** PR #331 (attach-race lock, shallow-clone detection), PR #420 (repository-state preflight), PR #458 / PR #471 (worktree-removal attribution + liveness gate) — all three already-merged directions of #325; this PRD covers only the fourth, deliberately deferred by all three: *"Consider whether concurrent orchestrations should share one clone at all."*
**Related:** PRD fork#166 (agent-provisioned worktrees — the PRD that made today's single-shared-clone model **explicit and intended**), PRD fork#175 (delegate provisions the worktree — unimplemented, assumes single-clone), PRD fork#298 (worktree owner human/agent), PRD #236 (worktree removal safety — already notes #120 and #220 use two different, unreconciled clone strategies)
**Fork-only?** No — the underlying provisioning code (`dispatch.rs`, `issue_dispatch_run.rs`, `ui.rs`'s spawn path) is upstream code. Offer upstream per rule 19 once a direction is decided and shipped.

## Problem Statement

Issue #325's original incident was two failures from concurrent orchestrations sharing one `.git` object store and one worktree registry: an orchestration deleted worktrees it didn't own mid-use, and a shallow fetch broke merges across every worktree on the shared repo. The first three of the issue's four "directions" are fixed — a shallow-repo preflight (PR #420), an attach-race lock (PR #331), and removal attribution plus a liveness gate for the one production removal path that lacked one (PR #458/#471). All three explicitly declined to touch the fourth: *"Consider whether concurrent orchestrations should share one clone at all... the strongest version of this is that they shouldn't — but that trades against build-cache sharing and disk, so it's a real design question."*

That framing needs correcting before a decision can be made on it, on two counts this PRD's investigation surfaced.

### Correction 1 — "share one clone" is not this repo's actual behavior; three different models already coexist

- **Model A — TUI-initiated orchestration** (`src/ui.rs:10214`, PRD fork#166): every orchestration is provisioned as a `git worktree add` sibling off **the deck's own already-open root checkout**. PRD fork#166 states this outright: *"Every orchestration starts from the deck's own checkout (`main`). **Intended** — not something to prevent"* (`prds/done/fork-166-agent-provisioned-worktrees.md:40`), and fork#175's live measurement found **0 of 8** orchestrator-shaped panes carrying any distinct owner identity — all 8 rooted at the same root checkout.
- **Model B — the `dispatch` command** (`src/dispatch.rs:263`, PRD #220): identical pattern — `clone_dir = ctx.working_dir.clone()`, worktrees are siblings of the user's own checkout.
- **Model C — scheduled issue-dispatch** (`src/issue_dispatch_run.rs:352`, PRD #120): **already a one-clone-per-task-name model.** `provision_repo` (`:1038`) clones fresh under `workspace.join(sanitize_clone_segment(task_name))` if that directory doesn't already exist, otherwise fetches and fast-forwards. Every issue *that same scheduled task* dispatches shares that one task-scoped clone via `.worktrees/issue-<n>` worktrees off it — but a **different** task name (even against the same upstream repo) gets a **different, fully separate clone**, automatically, today, with zero additional code.

So the real question is not "single clone vs. many" in the abstract — it is **whether Models A and B should adopt something closer to Model C's already-shipped, already-working per-task-scope isolation**, and PRD #236 already notes (in its own Out of Scope section) that #120 and #220 run two unreconciled clone strategies side by side.

### Correction 2 — the disk/build-cache tradeoff the issue names does not actually hold for build cache

`grep`ing this codebase for `CARGO_TARGET_DIR` returns zero hits — nothing sets it, anywhere. CLAUDE.md rule 1 states the reason plainly: *"A worktree also keeps each branch's `target/` build state isolated, so switching work does not force a full rebuild"* — meaning **every worktree already gets its own independent `target/` today**, under the current single-shared-clone model, purely from cargo's ordinary per-directory default. Sharing one `.git` object store does not currently buy any orchestration a shared build cache — that per-worktree isolation is already treated as a *feature* (rule 1), not a cost.

The real, remaining cost of N clones instead of one is **git object-store duplication only** (pack files, one copy per clone) — not the build-cache loss the issue's own framing implied. That materially changes the tradeoff's weight, and no existing doc or PRD quantifies it (no `git clone --reference`, alternates, or any shared-object-store tooling exists anywhere in this codebase — a multi-clone direction would start from zero on that mitigation, not extend something already there).

### What reversing Model A/B's shared-clone default would actually cost in code

Every coordination primitive #325's predecessor PRDs built assumes single-clone containment:

- `owned_git_dir` (`src/worktree_reclaim.rs`, ~`:892-901` on `origin/main`) checks that a worktree's `.git` dir resolves under **one** `repo_dir`'s common dir — a multi-clone world needs this to enumerate and check against *every* clone the deck knows about, not just the caller's cwd.
- The attach-race lock (PR #331, `worktree_attach_lock_path`/`git_common_dir`, `src/issue_dispatch_run.rs:1491-1621`) is anchored under the shared clone's common `.git` dir — genuinely per-clone today; a multi-clone model would need either N locks or a different anchor.
- `WorktreeRegistry` (`src/issue_dispatch_run.rs:99-115`) is daemon-wide but its `WorktreeEntry` already carries a per-entry `clone_dir` — this part costs nothing extra for a multi-clone world.

## Decisions

This section is deliberately a proposal, not a settled call — reversing PRD fork#166's explicit "intended" design decision for Models A/B needs the maintainer's sign-off, not just an implementation plan.

| Question | Proposed direction | Why |
|---|---|---|
| Should Models A/B (TUI orchestration, `dispatch`) move toward Model C's per-task/per-orchestration clone isolation? | **Yes, but scoped to concurrent-orchestration collision risk specifically** — not a wholesale rewrite of how the deck's own root checkout is used for the user's own interactive work. | Model C already proves the pattern works and ships today; the disk-cost objection that blocked considering this is weaker than assumed (build cache is unaffected either way). |
| What triggers a separate clone vs. reusing the shared one? | **Concurrent-orchestration-shaped work** (scheduled dispatch, delegate-provisioned orchestrations) gets its own clone, keyed the same way Model C already keys on `task_name`; a human's own interactive TUI session against their own working checkout is unaffected. | Matches where the actual incidents happened — automated/concurrent orchestration activity, not a human's single foreground session. |
| Does this touch PRD fork#166's "intended" framing? | **Yes — needs an explicit amendment**, not a silent reversal. fork#166's PRD should be updated to record why the intended-shared-checkout model is being narrowed, and for which callers. | fork#166 stated its choice deliberately; undoing part of it deserves the same deliberateness, and a future reader should find the reasoning rather than a contradiction. |
| Object-store duplication cost — mitigate now, or accept it? | **Accept it initially**, revisit `git clone --reference`/alternates only if the disk cost is measured and found to matter. | No existing tooling to build on; premature optimization here risks a second unproven architecture change stacked on the first. |

## Design (sketch, pending the decision above)

1. Extend Model C's `provision_repo` pattern (`src/issue_dispatch_run.rs:1038`, clone-if-absent + fetch/ff-pull otherwise, keyed on a scope identifier) to be reusable by Models A/B, keyed on something equivalent to `task_name` — likely the orchestration's own identity/name rather than an issue-dispatch task name.
2. `owned_git_dir` and the attach-race lock need to become clone-aware rather than single-repo-aware — likely by iterating a small registry of known clone roots rather than assuming exactly one.
3. `WorktreeRegistry`'s existing per-entry `clone_dir` already supports this without a schema change (Correction 1 above) — the registry itself is not blocking.
4. Decide and document the exact boundary between "reuses the shared root checkout" (a human's own interactive session) and "gets its own clone" (a concurrent/automated orchestration) — this is the crux of the maintainer decision this PRD exists to surface, not something to default silently.

## Milestones

- [ ] M1 — Maintainer decision recorded on the Decisions table above (or a revised version of it) — this milestone gates all others.
- [ ] M2 — PRD fork#166 amended to record the narrowed scope of its "intended" shared-checkout model, once M1 is decided.
- [ ] M3 — `provision_repo`'s clone-if-absent pattern generalized for reuse by Models A/B, scoped per the M1 decision.
- [ ] M4 — `owned_git_dir` and the attach-race lock updated to be clone-aware rather than assuming exactly one shared repo.
- [ ] M5 — Tests: two concurrent orchestrations under the new model provably cannot collide on worktree removal or object-store state (the original #325 incident, reproduced and shown fixed under the new architecture — not just argued).
- [ ] M6 — `docs/develop/` updated to describe the resulting model (which callers share a clone, which don't, and why), so a future contributor doesn't rediscover Model A/B/C's divergence the hard way, the way this PRD's own investigation had to.

## Test plan

L2 required — this is inherently about real concurrent git/filesystem behavior, not renderable in an L1 widget test. The reproduction target is #325's own original incident shape: two orchestrations, one attempting a worktree removal the other still has live, under whichever clone-isolation boundary M1 settles on — proving the boundary actually prevents the collision, not merely that it changes where clones live.

## Out of scope

- Object-store sharing mitigation (`git clone --reference`/alternates) — deferred pending a measured cost, per the Decisions table.
- Any change to a human's own interactive TUI session's use of their own working checkout — this PRD is about automated/concurrent orchestration provisioning only.
- Re-litigating directions 1-3 of the original #325 issue — those are shipped (PR #331, #420, #458, #471).
