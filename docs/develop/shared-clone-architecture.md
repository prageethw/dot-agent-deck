# Concurrent orchestrations and the shared-clone model — design record

> **Developer / maintainer reference.** This page documents internal rationale and is intentionally excluded from the published documentation site (CLAUDE.md rule 11). It exists so a future contributor doesn't have to rediscover Model A/B/C's divergence, or the ownership-attribution mechanism's two-attempt history, the hard way — the way [PRD fork#325](https://github.com/prageethw/dot-agent-deck/blob/main/prds/fork-325-shared-clone-architecture.md)'s own investigation had to.

## The incident this all exists to prevent

Two orchestrations running concurrently against **one root checkout** — one `.git` object store, one worktree registry — is not automatically safe. On 2026-08-14, on one deck running three-plus concurrent orchestrations, this happened twice in one evening:

1. An orchestration deleted five worktrees it did not own, mid-use — one vanished while a reviewer was actively working in it.
2. An orchestration ran a shallow fetch. `.git/shallow` lives in the **common dir**, shared by every linked worktree — so one orchestration's fetch broke merge/rebase for every orchestration on the box, silently. Symptoms looked like a corrupt or wrong remote, not a truncated one; the tell was a branch measured hours apart going from "78 behind / 7 ahead" to "1 behind / 1122 ahead" — an absurd enough number to investigate.

Neither failure left a trace of who did it. Both are what [issue #325](https://github.com/prageethw/dot-agent-deck/issues/325) reports, and what this whole line of work closes.

## Three provisioning models, and why they diverged

Before this PRD, the repo had three call sites that provision a worktree/clone for a new orchestration or task, and they'd quietly diverged onto different clone strategies without anyone deciding that on purpose:

| Model | Call site | Historically |
|---|---|---|
| **A** — TUI-initiated orchestration | `src/ui.rs`'s `Action::SpawnPane` (PRD fork#166) | Every orchestration provisioned as a `git worktree add` sibling of the deck's own already-open root checkout — intended, not a bug, per fork#166's own text. |
| **B** — the `dispatch` CLI | `src/dispatch.rs`'s `handle_dispatch` (PRD #220) | Identical pattern: worktrees as siblings of the caller's own checkout. |
| **C** — scheduled issue-dispatch | `src/issue_dispatch_run.rs`'s `provision_repo` (PRD #120) | Already a **one-clone-per-task-name** model: a fresh clone under `workspace.join(sanitize_clone_segment(task_name))` if absent, otherwise fetch/fast-forward — with every issue *that same scheduled task* dispatches sharing that one clone via `.worktrees/issue-<n>` worktrees off it. A **different** task name gets a **different, fully separate clone**, automatically, with zero additional code.

So "should concurrent orchestrations share one clone" was never really a single yes/no question — Model C had already answered it (no) for its own case, years before Models A and B's answer (yes, unconditionally) caused the 2026-08-14 incident. The real question fork#325 (M3) had to settle was whether A and B should adopt something closer to C's already-working per-scope isolation. They did — but only for the **Nth-concurrent** case, not unconditionally, which is the next section.

Correcting the framing further: the disk/build-cache tradeoff the original issue named for "many clones" doesn't actually hold for build cache. `grep -i CARGO_TARGET_DIR` returns zero hits anywhere in this codebase — nothing sets it, so every worktree already gets its own independent `target/` today, purely from cargo's own per-directory default (CLAUDE.md rule 1 already treats this as a *feature*: switching branches doesn't force a rebuild). The real, remaining cost of N clones instead of one is git object-store duplication only — deferred, not measured, and out of scope for this PRD.

## The gate: Nth-concurrent, not "always isolate"

**The decision (M1, approved 2026-08-18, corrected same day):** the trigger is not "human vs. automated" (the originally-approved framing, which matched no actual code signal) — it's **whether a live orchestration already shares the target root checkout's git object store**, compared by `--git-common-dir`, never by raw path equality (two orchestrations can each have their own worktree sibling and still share one underlying object store).

- **1st orchestration against any root checkout**: unaffected. Shares the checkout exactly as before creation — `create_worktree_sync` (Model A) / `create_worktree` (Model B), a plain `git worktree add` sibling.
- **Nth (2nd, 3rd, …) orchestration against a root checkout a live one already shares**: isolated into its own fresh clone instead — `provision_isolated_clone_sync` (`src/issue_dispatch_run.rs`), generalized from Model C's clone-if-absent pattern.
- **The daemon query fails, or answers in a shape that can't be trusted to mean "no live sibling"** (an error response, or the older-daemon shape omitting per-agent tab membership): **fail closed** — refuse to provision at all. This was a deliberate M3 design decision (Design step 4), not inherited silently from the existing `live_orchestration_in_same_cwd` hint path it was repurposed from, which fails *open* on a down daemon (fine for a cosmetic warning, wrong for a correctness gate — failing open there would let the exact "a down/wedged daemon hides a live sibling" race recur). Model A's fail-closed decision is pinned by `orchestration/worktree/015`; Model B's own equivalent gate re-applies the *same* decision rather than re-litigating it.

One consequence worth stating plainly, because the changelog's first wording of it was misleading: for `dispatch`'s **dominant caller** — a role pane inside a live orchestration issuing `dispatch` — the calling pane's cwd already equals that orchestration's own `orchestration_cwd`, so it **always** self-matches the gate's fast path and is **always** isolated, including what would otherwise be "the first" dispatch against that specific target. Isolation is `dispatch`'s normal case, not its edge case.

**Shipped**: Model A in [PR #481](https://github.com/prageethw/dot-agent-deck/pull/481); Model B in [PR #504](https://github.com/prageethw/dot-agent-deck/pull/504) (issue #490) — which also needed isolated-clone-specific spawn-rollback, tab-close cleanup, and `origin_warning` surfacing that Model A didn't, because Model A never calls `record_worktree` for what it provisions.

**Proven against the actual incident shape, not a reduced one**: `orchestration/worktree/016` (PRD fork#325 M5) reproduces **three-plus** concurrent orchestrations — not the simpler 2-orchestration case `014`/`015` already covered — and both of the incident's concrete original failures, asserting each is now structurally impossible: cross-repo `git worktree remove` fails outright (separate object stores, nothing to look up), and a shallow fetch in one orchestration's directory leaves the others' object stores untouched. Verified empirically during review that both assertions would have **failed** against the pre-M3 architecture — the test doesn't merely pass, it discriminates.

## Isolated clones are permanent until someone removes them

An isolated clone is a fully independent repository — its own `.git`, not a linked worktree of anything. That has two consequences neither Model matched at the outset:

- **Its `origin`** is repointed at the **source checkout's own origin URL** (or removed, if the source has none) — never left as a local filesystem path, which would otherwise make `git push origin HEAD:refs/heads/<branch>` (the exact command CLAUDE.md rule 1 tells every agent to run) land silently in the source checkout instead of reaching GitHub.
- **It is never automatically removed.** `RemovalPolicy::IsolatedClone` / `KeptReason::IsolatedClone` (`src/worktree_reclaim.rs`, `src/event.rs`) always report `Kept`, regardless of cleanliness or PR state, with no removal probe attempted at all. This is deliberate, not an oversight: a clean working tree proves nothing for an isolated clone the way it does for a linked worktree — a linked worktree's commits remain in the shared object store even after the worktree directory is removed, while an isolated clone's local-only branch commits have no copy anywhere else. Whether (and under what stricter condition — see "What's still open" below) an isolated clone can ever become safely auto-reclaimable is **M4c**, not yet decided.

## Why `owned_git_dir` didn't need to change (and the PRD said it would)

The original design (M3 Design item 2) expected `owned_git_dir` and the attach-race lock to need to become "clone-aware… iterating a small registry of known clone roots." That concern **never materialized** in the shipped architecture, and correcting it cost nothing: every real call site of `owned_git_dir` (`src/worktree_reclaim.rs`, `src/issue_claim.rs`) and of the attach-lock path functions (`src/issue_dispatch_run.rs`) already receives the single, correct repo/clone root for the one specific thing it's checking — never a case needing to check one target against multiple candidate roots. `owned_git_dir`'s whole reasoning is containment: is this worktree's `.git` under *this* repo's common-dir `worktrees/`? An isolated clone is a separate top-level repository, so that question doesn't even apply to it — which is exactly why isolated-clone discovery (next section) needed a **parallel**, not a modified, mechanism.

## Discovering isolated clones: a second, unrelated enumeration

`worktree list`/`worktree reclaim` (`examine_worktrees`/`run_reclaim`, `src/worktree_reclaim.rs`) walk `git worktree list --porcelain` from one `repo_dir` — which structurally **cannot** see an isolated clone, since it's a separate repository, not a linked worktree of anything. Until **M4a** ([PR #510](https://github.com/prageethw/dot-agent-deck/pull/510)), an isolated clone that outlived its orchestration was invisible to every discovery surface, accumulating on disk with no way to find it short of knowing where to look.

`discover_isolated_clones` fixes this with a **structurally separate** scan: sibling directories of the (correctly root-checkout-resolved, not assumed-equal-to-`repo_dir`) root checkout, filtered to a `.git` present as a **directory**, not a **file** — the same test that already tells an isolated clone apart from a linked worktree's `.git` redirect. Reported via a new `kind` field (`"linked"` vs `"isolated_clone"`) and, deliberately, **never** an automatic-removal verdict — hardcoded to a value `run_reclaim`'s existing match falls through to its default `kept` arm for, with no new arm added, so a future contributor editing that match can't accidentally make an isolated clone reachable by `--yes` (this mirrors the daemon-side `RemovalPolicy::IsolatedClone` precedent more directly than routing through the existing `Ask` verdict would have, which would have let `--yes` reach `git worktree remove` against something that isn't a linked worktree at all — failing loudly rather than cleanly).

## Ownership attribution: a two-attempt history worth reading before touching this code

**This is the part of M4 most likely to bite a future change**, because the wrong-looking answer passed code review twice before the right one shipped, and both times looked plausible.

The question `owned`/`owner`/`owner_kind` answer for a discovered isolated clone: *did this deck genuinely create this directory, and who owns it?* Three shapes were tried, in this order:

1. **`candidate_shares_history_with`** (M4a's original, [PR #510](https://github.com/prageethw/dot-agent-deck/pull/510)) — bound ownership to the candidate's current `HEAD` naming a commit the root checkout already has. **Broken two ways**, found by two different review rounds: (a) *functional* — a genuine clone's `HEAD` moves the moment the dispatched agent makes its first real commit, silently dropping the clone out of discovery the moment it holds the local-only work this whole milestone exists to protect; (b) *security* — a same-uid actor could forge acceptance with 4 files, no `git` invocation, no real objects at all (a `.git/HEAD` containing a real, public SHA as plain text is enough).

2. **A dedicated provenance artifact inside each candidate's own `.git`** (M4b's first attempt, [PR #515](https://github.com/prageethw/dot-agent-deck/pull/515) round 1) — closed (1) cleanly. **Reopened the ORIGINAL forgery this whole line of work started from** (auditor A1/B1, M4a's very first review round), at the cost of one `touch` — because a same-uid attacker who can plant a sibling directory at all already controls everything inside that directory's `.git`, including any filename the deck might check for there. Caught by both reviewer and auditor independently, each with a working exploit, before merge.

3. **`state_dir()`, keyed by a path hash, outside every candidate** (M4b's shipped fix) — the artifact lives in `crate::platform::paths::state_dir()` (the deck's own per-user state directory, owner-only permissions), keyed by `fnv1a64` of the candidate's canonical clone path — the exact same hashing scheme the attach-lock path already used, reused rather than reinvented. This closes the loop back to the actual reason `owned_git_dir` is trustworthy for linked worktrees: **the evidence has to live somewhere the *enumerating* party controls, never somewhere the *candidate* controls** — containment is one instance of that property, not the property itself. A same-uid attacker who can only plant a sibling directory cannot write into `state_dir()`; one who *can* write there already has the same access `owned_git_dir`'s own linked-worktree case assumes as its ceiling.

**The lesson, stated so it doesn't have to be relearned**: any future change to how ownership is decided for a discovered isolated clone must keep the evidence outside the candidate. An implementation that "just" writes something recognizable inside the clone's own `.git` — however specific the filename, however unlikely to collide by accident — is the shape that failed twice already, once in each direction (too fragile, then too forgeable).

Two honest, deliberately-not-fixed residuals from the shipped mechanism (tracked, not silent): the attach-lock namespace is not entirely walled off from ordinary linked-worktree creation (a removed linked worktree's stale lock *could* theoretically be inherited — closed for the provenance artifact specifically, since `create_worktree_sync` never writes that filename); and `provision_isolated_clone_sync` acquires its lock before checking `clone_dir.exists()`, by design (moving the check earlier would reopen a TOCTOU auditor A3/fork #282 already closed) — bounded because a pre-planted directory at that point still fails visibly as `AlreadyClaimed`. Both are documented in `candidate_has_attach_lock`'s own doc comment, not just here.

## What's still open

- **M4c — auto-reclaim eligibility.** Whether an isolated clone can ever become automatically reclaimable, and under what stricter safety condition than the linked-worktree gate's "PR merged + clean" (a clean tree doesn't prove an isolated clone's local-only commits are safe to lose). This is a genuine design decision, the same shape M1 needed a recorded maintainer decision for — not something to bundle into an implementation PR and settle as a side effect. See the PRD's M4c bullet for the current candidate condition under discussion (PR-merged AND HEAD-equals-merge-SHA exactly) and the residuals it inherits from M4b's history.
- **Issue #516** — two Low-severity residuals found in M4b's final review round: `state_dir()` has no guard against an empty (not merely unset) `DOT_AGENT_DECK_STATE_DIR`, newly security-relevant now that path backs an ownership decision; and the fast-tier unit tests exercising the real provisioner don't sandbox that variable, writing real artifacts into the developer's/CI runner's actual state directory.
- **Upstream offer** — the underlying provisioning code (`dispatch.rs`, `issue_dispatch_run.rs`, `ui.rs`'s spawn path) is upstream code, not fork-only. [Issue #509](https://github.com/prageethw/dot-agent-deck/issues/509) tracks offering the M3 gate mechanism upstream once M4-M6 settle, per CLAUDE.md rule 19.

## Related

- [Issue #325](https://github.com/prageethw/dot-agent-deck/issues/325) — the original incident report.
- [PRD fork#325](https://github.com/prageethw/dot-agent-deck/blob/main/prds/fork-325-shared-clone-architecture.md) — full milestone history, decisions table, and the residuals each milestone deferred to the next.
- PRD fork#166 (agent-provisioned worktrees) — the PRD that made Model A's single-shared-clone default **explicit and intended**, the premise this PRD's M3 had to add an exception to rather than reverse.
- PRD #120 (scheduled issue-dispatch) / PRD #220 (`dispatch` CLI) — Models C and B's own origins.
