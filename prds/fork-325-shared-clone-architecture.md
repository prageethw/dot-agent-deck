# PRD fork#325 — Should concurrent orchestrations share one git clone at all?

**Issue:** [prageethw/dot-agent-deck#325](https://github.com/prageethw/dot-agent-deck/issues/325)
**Priority:** High
**Status:** Planning — M1 decided 2026-08-18, **corrected same day** after implementation-prep investigation found the originally-approved trigger ("human vs. automated") doesn't match the actual incident shape or any existing code signal; see "M1 correction" below. M2 (fork#166 amendment) landed against the original framing and needs a follow-up amendment once M3 lands. M3 — Model A shipped in PR #481; Model B (`dispatch.rs`) landing via PR #504 (issue #490), including the isolated-clone rollback/cleanup/origin-warning follow-ups its review found. M4-M6 not yet started.
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

| Question | Direction | Why |
|---|---|---|
| Should Models A/B (TUI orchestration, `dispatch`) move toward Model C's per-task/per-orchestration clone isolation? | **Yes, but scoped to concurrent-orchestration collision risk specifically** — not a wholesale rewrite of how the deck's own root checkout is used for the user's own interactive work. | Model C already proves the pattern works and ships today; the disk-cost objection that blocked considering this is weaker than assumed (build cache is unaffected either way). |
| Does this touch PRD fork#166's "intended" framing? | **Yes — needs an explicit amendment**, not a silent reversal. | fork#166 stated its choice deliberately; undoing part of it deserves the same deliberateness. |
| Object-store duplication cost — mitigate now, or accept it? | **Accept it initially**, revisit `git clone --reference`/alternates only if the disk cost is measured and found to matter. | No existing tooling to build on; premature optimization here risks a second unproven architecture change stacked on the first. |

### M1 correction — the trigger, corrected same day

M1 was first approved as "concurrent/automated orchestration provisioning (scheduled dispatch, delegate-provisioned orchestrations) gets its own clone; a human's own interactive TUI session is unaffected." Implementation-prep investigation (before any code was written) found this doesn't hold:

- **The actual #325 incident was N concurrent orchestrations, all spawned the human/TUI-form way** — one human, one deck, 3+ orchestration tabs open at once via `Action::SpawnPane` (`ui.rs:8767`/`:11155`, reachable only from a keypress or click on the New Pane form). "A human's own interactive session" is not a category the incident falls outside of — it *is* the incident, repeated.
- **"Delegate-provisioned orchestrations" is not a real code path.** PRD fork#175 (the PRD that would create it) is 0% implemented — a superseded design doc with no code trace anywhere (`Delegate`'s wire signal carries no path/branch/SHA field at all).
- **No code anywhere counts "how many orchestrations are already active."** There was no existing signal to gate on; the approved trigger required inventing one.

**Corrected trigger, replacing the row above:** the **Nth-concurrent-orchestration gate**. `live_orchestration_cwds_and_titles()` (`ui.rs:1011`) already computes, on every `Ctrl+n` form-open, whether the target cwd hosts a live orchestration — today purely a cosmetic warning banner via `live_orchestration_in_same_cwd()` (`ui.rs:957`), never a correctness gate. Repurposed: the **1st** orchestration against a root checkout shares it, exactly as today (no behavior change for the common case); the **2nd and later concurrent** orchestration against the *same* root checkout gets its own isolated clone. This matches the incident's actual shape — a 3rd orchestration colliding with two already-live ones — rather than a human/automated split the incident never had. Model B (`dispatch` CLI) has no equivalent live-orchestration query today and needs one added (Design step 4 below).

## Design

1. Extend Model C's `provision_repo` pattern (`src/issue_dispatch_run.rs:1094`, clone-if-absent + fetch/ff-pull otherwise, keyed on a scope identifier) to be reusable by Models A/B, keyed on the orchestration's own name/identity rather than an issue-dispatch task name.
2. `owned_git_dir` and the attach-race lock need to become clone-aware rather than single-repo-aware — likely by iterating a small registry of known clone roots rather than assuming exactly one.
3. `WorktreeRegistry`'s existing per-entry `clone_dir` already supports this without a schema change (Correction 1 above) — the registry itself is not blocking.
4. **The gate itself**: at `Action::SpawnPane`'s worktree-provisioning point (`ui.rs:10214`), consult `live_orchestration_in_same_cwd`'s existing daemon query — no longer only for the cosmetic warning, but to decide provisioning: if the target cwd already hosts a live orchestration, provision via the (new, Model-A-reusable) `provision_repo`-style path instead of `create_worktree_sync` against the shared checkout. **The existing query fails open on a down daemon** (no warning, proceeds normally) — for a *gate*, not a hint, failing open means the exact race #325 originally reported (a down/wedged daemon hides a live sibling) reappears. This needs its own decision at M3 time: fail open (accept the residual race, same risk profile as everything else on a down daemon) or fail closed (refuse to spawn until the daemon answers) — flagged here so M3 doesn't inherit the hint path's fail-open behavior silently.
5. Model B (`dispatch.rs:263`) needs the equivalent live-orchestration query added — it has none today, unlike Model A which already computes one for the warning banner.

## Milestones

- [x] M1 — Maintainer decision recorded — **approved 2026-08-18, corrected same day**: the Nth-concurrent-orchestration gate (see "M1 correction" above), not the originally-approved human/automated split.
- [x] M2 (partial) — PRD fork#166 amended for the original framing (PR #478); **needs a follow-up amendment** once M3's actual gate mechanism lands, since fork#166's note currently describes the superseded trigger.
- [x] M3 — The Nth-concurrent-orchestration gate: `provision_repo`'s clone-if-absent pattern generalized and reused by Model A at `Action::SpawnPane`'s provisioning point, gated on `live_orchestration_in_same_cwd`'s existing query (repurposed from cosmetic warning to correctness gate); the fail-open-vs-fail-closed question (Design step 4) resolved explicitly, not inherited silently. Model A shipped in PR #481. Model B (`dispatch.rs`) got the equivalent query added in PR #504 (issue #490), plus the isolated-clone-specific rollback, tab-close cleanup, and `origin_warning` surfacing that path needed and Model A didn't (Model A never calls `record_worktree`).
- [ ] M4 — `owned_git_dir` and the attach-race lock updated to be clone-aware rather than assuming exactly one shared repo.
- [ ] M5 — Tests: reproduce #325's actual incident shape (a 3rd orchestration racing two already-live ones on the same root checkout) under the new gate and show it can no longer collide — not a synthetic two-orchestration case that doesn't match what actually happened.
- [ ] M6 — `docs/develop/` updated to describe the resulting model (which callers share a clone, which don't, and why, and what fail-open/closed choice M3 made), so a future contributor doesn't rediscover Model A/B/C's divergence the hard way, the way this PRD's own investigation had to.

## Test plan

L2 required — this is inherently about real concurrent git/filesystem behavior, not renderable in an L1 widget test. The reproduction target is #325's own original incident shape: two orchestrations, one attempting a worktree removal the other still has live, under whichever clone-isolation boundary M1 settles on — proving the boundary actually prevents the collision, not merely that it changes where clones live.

## Out of scope

- Object-store sharing mitigation (`git clone --reference`/alternates) — deferred pending a measured cost, per the Decisions table.
- **The 1st orchestration against any given root checkout** — unaffected either way; it always shares the checkout as today, whether or not a 2nd one later arrives.
- Finishing PRD fork#175 (delegate provisions the worktree) — unrelated; this PRD's gate does not depend on it.
- Re-litigating directions 1-3 of the original #325 issue — those are shipped (PR #331, #420, #458, #471).
