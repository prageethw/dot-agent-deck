# PRD fork#760 — Restore a human-typed workspace slug, decoupled from the orchestration Name; periodic sync for persistent workspaces

**Issue:** [prageethw/dot-agent-deck#760](https://github.com/prageethw/dot-agent-deck/issues/760)
**Priority:** Medium
**Status:** Part A (restore the "Worktree slug" field) implemented on PR [#761](https://github.com/prageethw/dot-agent-deck/pull/761), branch `feat/760-workspace-slug-sync`. Part B (periodic sync for persistent workspaces) not yet started.
**Predecessors:** PRD [fork#544](https://github.com/prageethw/dot-agent-deck/issues/544) ("named, persistent orchestrator workspaces") retired the pre-existing separate "Worktree slug" field and made the workspace directory/branch derive solely from the orchestration Name. PRD [fork#192](https://github.com/prageethw/dot-agent-deck/issues/192) ("orchestration names as instance identity") is unaffected — Name keeps its job as the tab title and the `ClaimOrchestrationName` uniqueness key.
**Related:** Fork issue [#546](https://github.com/prageethw/dot-agent-deck/issues/546) (auto-reclaim vs. named workspaces) — resolved already (pin/unpin), explicitly out of scope here.
**Fork-only?** No — the underlying code (`src/ui.rs`'s New Pane form and spawn path) is upstream code, same as fork#544/fork#325. Offer upstream per rule 19 once shipped, since fork#544 (the PRD this partially reverts) was itself upstream-eligible code.

## Problem Statement

PRD fork#544 kept a genuine improvement — persistent, resumable isolated-clone workspaces instead of `orchestrator-N` auto-numbered sprawl — but, in doing so, collapsed two previously-independent identities (the descriptive orchestration Name, and a short slug naming the on-disk workspace) into one. Since #544, a long, descriptive Name (e.g. "Implement 8 features: baseline intent") becomes the workspace's folder and branch name almost verbatim, because Name is the *sole* input to `sanitize_workspace_segment`/`resolve_workspace_path` (`src/ui.rs`).

Discussed with the maintainer (2026-09-12): the fix is not to revert fork#544's persistence/reuse model, but to decouple naming ergonomics from it again — restore a short, human-typed, stable identifier independent of the descriptive Name, matching the pre-#544 design, while keeping every collision-safety/disambiguation fix layered on top since (#607, #595, #751).

A second, related gap surfaced in the same conversation: a persistent, long-lived, reused workspace has no periodic freshness mechanism. `worker-agent-deck worktree sync` (fork #744) only runs on-demand, as one step the orchestrator triggers right after a merge. Nothing keeps a workspace that stays open and reused over days/weeks current with `origin/<default branch>` between merges.

## Decisions

| Question | Direction | Why |
|---|---|---|
| Does Name go back to driving the workspace path? | **No.** Name stays exactly what fork#544 and fork#192 made it: the tab title and the `ClaimOrchestrationName` uniqueness key. It never feeds `sanitize_workspace_segment`. | Restoring Name-driven paths would just reproduce the original PRD fork#544 problem (auto-numbered sprawl) that #544 fixed. The regression being fixed here is narrower: the *replacement* input (a typed slug) that #544 removed. |
| What drives the path instead? | A restored, optional `FormField::WorktreeSlug` field. Non-blank slug (validated: non-empty after trim, first char alphanumeric/`_`, remaining chars alphanumeric/`-`/`_`) is used verbatim as the segment. Blank slug auto-generates `orchestrator-N` (`auto_generate_worktree_slug`, exactly the pre-#544 fallback) — **not** derived from Name. | Matches the actual pre-#544 behavior byte-for-byte (verified against `git show 275904eb^:src/ui.rs`), rather than inventing a new fallback (an earlier draft of this fix incorrectly proposed falling back to Name-derived text; corrected before merge). |
| Does the existing collision-safety machinery need to change? | No. `disambiguate_workspace_segment` (fork #607's length-prefixed join), the symlink-toplevel guard (fork #595), and resume-vs-create matching (`provision_isolated_clone_sync_resolved`'s directory-exists check) all operate on the opaque `segment` string produced above — they are unchanged, just fed a different (slug-or-auto-generated) input. | These fixes were hard-won across multiple review rounds; the goal is decoupling the *input*, not re-deriving the safety logic. |
| Should persistent workspaces sync periodically, or only post-merge? | **Undecided — Part B, not yet implemented.** Candidate directions: the daemon runs `worktree sync`'s existing logic on a timer across all live isolated-clone workspaces, or the TUI triggers a read-only fetch on tab focus. Whichever is chosen must preserve `worktree sync`'s existing safety property (only fast-forward/switch when the workspace carries nothing extra — never discard uncommitted or unmerged local work). | Not yet designed; left for the implementer of Part B, informed by what fits the daemon's existing scheduling (if any). |

## Out of Scope

- Auto-deletion/reclaim policy (`worktree reclaim`, pin/unpin) — unaffected, per the maintainer's explicit direction. Issue #546 is already resolved and needs no further change here.
- Any change to Name's role as instance identity (PRD fork#192).

## Milestones

- **Part A (done, PR #761):** Restore `FormField::WorktreeSlug`, wire it into the segment-derivation call chain, restore `auto_generate_worktree_slug` for the blank case, update stale doc comments asserting "Name is the sole input," add/update tests (`orchestration/worktree/017`-`020` plus updated fixtures in pre-existing `orchestration/workspace`/`orchestration/worktree` tests).
- **Part B (not started):** Design and implement periodic sync for persistent workspaces, preserving `worktree sync`'s uncommitted/unmerged-work safety guarantee.

Prageeth Warnak
