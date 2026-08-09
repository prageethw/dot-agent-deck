# PRD fork#166: Agent-provisioned worktrees with orchestration ownership — spin up autonomous work without a human creating worktrees

**GitHub Issue**: [fork #166](https://github.com/prageethw/dot-agent-deck/issues/166)

**Priority**: High

**Status**: Planning

**Fork-only**: yes, and intended to stay so. Confirmed mechanically against `upstream/main`: `src/worktree_reclaim.rs` **does not exist** there and `worktree_slug` has **0** occurrences. The ownership half is built entirely on fork-only code. Per `docs/develop/upstream-contribution-policy.md`, that settles it — this is not an offer-later case.

**Related**: fork #144 (the ownership marker and its containment check — this PRD's foundation) · fork #122 (per-orchestration worktree creation, the mechanism this makes agent-callable) · PRD #422 (`worktree list` / `reclaim`, the command surface this extends) · PRD #140 (concurrent orchestration safety — **not** reversed by this PRD; see Out of Scope) · PRD #220 (dispatcher mode — the upstream cousin; its M1.1 naming question is adjacent and settled differently here) · fork #74 (the collision that motivates it) · PRD #421 (the provenance precedent)

**Filename convention note**: the first PRD named after a fork issue — every existing one cites `vfarcic`. A bare `166-` would be ambiguous against a future upstream #166, so fork-numbered PRDs take a `fork-<n>-` prefix.

## Problem Statement

Creating a worktree is a **human step**, and CLAUDE.md rule 1 makes it a mandatory one: the orchestrator must run `git worktree add` by hand, unset the upstream, and state the absolute path, exact SHA and branch name in every task it delegates. Rule 16 exists because that supply obligation kept being forgotten.

The cost is not ceremony — it is the documented failure. Fork #74: *"a second orchestration's task named no worktree path, so its worker found the first orchestration's existing branch and reasonably joined it — writing production code into a worktree it did not own, then pushing to it and cancelling the first orchestration's in-flight CI run."* That has happened more than once.

**The goal is autonomous operation.** An orchestration should be able to start a new line of work without a human provisioning a directory for it first, and should be able to answer "which worktrees are mine?" rather than relying on a path someone typed into a task file.

### The workflow this serves

1. Every orchestration starts from the deck's own checkout (`main`). That is intended, not a problem to be blocked.
2. Each change — a PRD, a fix — gets **its own** worktree, created by the agent at the moment work starts.
3. That worktree is tagged with the orchestration that created it.
4. The orchestration can list what it owns, and work only on those.
5. A worktree from a previous session whose ownership matches is **available again** — a restart resumes rather than orphans.

Steps 2–5 have no product support today. Step 2 is manual; steps 3–5 do not exist.

## Solution Overview

Three additions, all on the existing `worktree` command surface.

1. **`worktree create` — agent-callable provisioning.** One call creates the worktree, attaches or creates its branch, and records ownership. The orchestrator calls it exactly as it already calls `delegate` and `work-done`, removing `git worktree add` from rule 1's manual burden.
2. **Ownership recorded in the marker.** fork #144's `dot-agent-deck-owner` marker is written at creation but is empty. It gains the owning orchestration's identity.
3. **`worktree list` reports and filters by owner.** It already reports ownership as `Ours`/`Foreign`; it gains *which* orchestration, and a way to ask for only mine.

### Authority is the marker; the name is only a convenience

Worktrees are named for legibility — a `<repo>-<orchestration>-<change>` shape makes it obvious at a glance in `ls` and in the tab strip which work belongs to which orchestration.

**But the name never decides ownership.** Only the marker does. This distinction is the whole safety argument:

- **Name-based (prefix) ownership was considered and rejected.** It would adopt a worktree the deck did not create — a hand-made folder that happens to match the pattern becomes `Ours`, and fork #144 makes `Ours` + merged + clean removable by a bare `reclaim` with **no prompt and no path shown**. That is the P1 fork #144 closed. Reopening it to save a file read is a bad trade.
- With the marker authoritative, a matching name on a folder the deck did not create is simply `Foreign`. Nothing is adopted, and the naming convention stays useful for humans.

### Identity must survive a restart

The obvious identity — `orchestration_id` — is **wrong here**, and the reason is worth recording because it is not obvious: `mint_orchestration_id()` runs fresh on every tab open. A tab that closes and reopens would no longer recognise its own worktrees, so step 5 (resume) would break precisely when it matters.

The identity written into the marker must therefore be the orchestration's **stable name/slug**, which does not change across restarts.

Note this differs from PRD #421's conclusion for *issue claims*, and deliberately. There, provenance is reporting content and never a decision input, because a claim is authoritative regardless of who made it. Here, ownership genuinely **is** a decision input — "may this orchestration work here?" — so it needs an identity that means the same thing tomorrow. The two PRDs answer different questions; #421's reasoning is not being contradicted, it is being applied to a case it did not cover.

### `main` is already protected, structurally

An orchestration starting in `main` must never treat `main` as an owned worktree. **This needs no new check.** fork #144's containment already guarantees it:

```rust
if !git_dir.starts_with(common_dir.join("worktrees")) {
    return Ownership::Foreign;
}
```

The main checkout's git dir is `<repo>/.git`, which sits *above* `<repo>/.git/worktrees`, so it can never match — `main` always resolves `Foreign`. A second, independent protection: `mark_worktree_owned` is only ever called immediately after a successful `git worktree add`, "never for a pre-existing or foreign worktree", so no marker is written there in the first place.

That check was built to defeat a forged marker. It protects this case for free, because "is this a real linked worktree of this repo?" is the same question either way.

## Scope

### In Scope

- `worktree create` — agent-callable creation, branch attach-or-create, marker written with owner.
- Owner identity in the marker; backward compatibility with existing empty markers.
- `worktree list` reporting the owner and filtering to the caller's own.
- The reuse rule: an ownership match means resume; no match means create a new one.
- The `<repo>-<orchestration>-<change>` naming convention, as convention.
- Docs, and the rule 1 amendment that follows from `git worktree add` no longer being a manual step.

### Out of Scope

- **Any restriction on where orchestration tabs start.** All orchestrations starting from `main` is the intended workflow. An earlier draft of this PRD proposed blocking a second orchestration in one directory; that was a misreading of the workflow and is withdrawn. **PRD #140's advisory stance stands unchanged.**
- **Automatic worktree removal.** fork #122 deliberately never removes worktrees, because auto-removal risks destroying uncommitted work. `worktree reclaim` (PRD #422) remains the deliberate, gated path.
- **Adopting worktrees the deck did not create.** Never, by any mechanism.
- **Stopping an agent from `cd`-ing into another orchestration's worktree.** No deck-side check can see that; it is agent discipline, enforced by CLAUDE.md rule 1.
- **Namespacing `worker-task-<role>.md`** — see Risks.

## Success Criteria

- An orchestration can provision a worktree for a new change **without a human running `git worktree add`**.
- Every worktree it creates records it as owner; `worktree list` names that owner.
- An orchestration can ask which worktrees are its own and get a correct answer **after a restart**.
- A worktree created by a *different* orchestration is never reported as owned.
- A worktree the deck did **not** create is never owned, whatever it is named.
- `main` never appears as an owned worktree.
- A worktree created before this ships still resolves `Ours`, and `reclaim` still auto-removes it.
- CLAUDE.md rule 1's manual `git worktree add` step is replaced by the new call.

## Milestones

### Phase 1: Ownership identity

- [ ] **M1.0** — `mark_worktree_owned` writes the owning orchestration's **stable** identity (plus instance id, host, timestamp for diagnosis).
- [ ] **M1.1** — `ownership_of` reports the owner. Containment and presence remain authoritative — an empty or unparseable marker still resolves `Ours` with owner unknown, so every pre-existing worktree keeps working.
- [ ] **M1.2** — `worktree list` shows the owner; `--json` carries it. Decide whether this needs a `SCHEMA_VERSION` bump.

### Phase 2: Agent-callable provisioning

- [ ] **M2.0** — `worktree create <change-slug>`: resolve the path, `git worktree add`, attach-or-create the branch, write the marker with owner, return the absolute path.
- [ ] **M2.1** — reuse rule: an existing worktree whose marker names this orchestration is returned rather than re-created; one owned by another is refused with a clear reason.
- [ ] **M2.2** — the `<repo>-<orchestration>-<change>` naming convention, reusing fork #122's hardened `resolve_orchestration_worktree_path` validation rather than a second path builder.

### Phase 3: Query and adoption

- [ ] **M3.0** — a way to ask "which worktrees do I own", answering correctly across restarts.
- [ ] **M3.1** — docs; amend CLAUDE.md rule 1 so the manual `git worktree add` becomes the new call, keeping the supply obligations rule 16 requires.

## Key Files

- `src/worktree_reclaim.rs` — `OWNER_MARKER_FILENAME`, `mark_worktree_owned`, `ownership_of`, `resolve_git_dir` / `resolve_common_dir` (the containment that protects `main`)
- `src/issue_dispatch_run.rs` — `create_worktree` / `create_worktree_sync`, the existing creation path to reuse
- `src/main.rs` — `WorktreeCmd` (`:393`), which already has `List` and `Reclaim`
- `src/ui.rs` — `resolve_orchestration_worktree_path` (`:6627`), `validate_orchestration_worktree_slug` (`:6572`)
- `src/project_config.rs` — `resolve_orchestration_name` (`:243`), the stable identity source
- `tests/CATALOG.md` — `worktree/reclaim/*`, `orchestration/worktree/*`

## Risks and Mitigations

- **Existing markers are empty files.** If `ownership_of` required parseable content, every pre-existing deck worktree would silently flip to `Foreign` and `reclaim` would stop auto-removing anything. Mitigated by keeping presence authoritative and treating unparseable content as unknown owner. **Needs its own test** — it protects every worktree created before this ships.
- **An identity that changes across restarts breaks resume.** The exact trap `orchestration_id` sets. Mitigated by using the stable name/slug, and by a test that resumes *after* a simulated restart rather than within one session.
- **Ownership becoming a licence to delete.** Ownership here answers "may I work here?", not "may I remove this?". `reclaim`'s gates are unchanged, and this PRD adds no removal path.
- **`worker-task-<role>.md` still collides per-directory.** `src/state.rs:2059` keys it by role alone — PRD #140's layer 2, still deferred. It only bites on the inline `--task` path; this fork's documented `--task-file` default supplies a unique slug per delegation. Since every orchestration in this workflow starts in `main`, two inline delegations to the same role could clobber each other. Out of scope, recorded so it is not mistaken for solved. (The `work-done` twin **is** fixed — `work_done_file_name(role, pane_id)` — though the collisions seen on 2026-08-09 came from a running daemon that predates that fix, not from a design gap.)
- **Two orchestrations picking the same change slug.** Both resolve the same path; `git worktree add` refuses the second. Fail-safe, but the error should name the owner rather than surfacing raw git output.

## Open Questions

- **What exactly is the stable identity** — `resolve_orchestration_name`'s output (config name, or directory basename)? Two orchestrations in one directory with the same config name would share it. Since ownership *is* a decision input here, this needs a definite answer, not a convention.
- **Does `worktree create` belong on the CLI, on `delegate`, or both?** A separate verb is explicit and testable; folding it into `delegate` makes the common path one call instead of two.
- **Should an orchestration be able to adopt an unowned-but-deck-created worktree** (marker present, owner unknown — i.e. one created before this ships)? Permissive is friendlier and matches "previously pending worktrees become available"; strict is more predictable. Leaning permissive, since containment already proves the deck created it.
- **`SCHEMA_VERSION`** — does adding an owner field to `worktree list --json` warrant a bump? The field is additive.
