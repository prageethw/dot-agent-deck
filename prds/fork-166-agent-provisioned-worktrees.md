# PRD fork#166: Unique orchestration names, and worktrees the agent provisions itself

**GitHub Issue**: [fork #166](https://github.com/prageethw/dot-agent-deck/issues/166)

**Priority**: High

**Status**: Planning

**Fork-only**, and intended to stay so. Confirmed mechanically against `upstream/main`: `src/worktree_reclaim.rs` **does not exist** there and `worktree_slug` has **0** occurrences. The ownership half is built entirely on fork-only code, so per `docs/develop/upstream-contribution-policy.md` this is not an offer-later case.

**Related**: fork #144 (the ownership marker and its containment check — the foundation) · fork #122 (per-orchestration worktree creation, and the hardened path validation reused here) · PRD #422 (`worktree list` / `reclaim`) · PRD #140 (per-tab routing identity; its `display_title` contract changes here) · PRD #120 (issue-dispatch, which already provisions its own worktrees) · PRD #220 (the upstream cousin; its M1.1 naming question is settled differently here) · fork #74 (the motivating collision) · PRD #421 (the provenance precedent, applied to a case it did not cover)

**Filename convention**: the first PRD named after a fork issue — every existing one cites `vfarcic`. A bare `166-` would be ambiguous against a future upstream #166, so fork-numbered PRDs take a `fork-<n>-` prefix.

## Problem Statement

Creating a worktree is a **human step**, and CLAUDE.md rule 1 mandates it: the orchestrator runs `git worktree add` by hand, unsets the upstream, and states the absolute path, exact SHA and branch name in every task it delegates. Rule 16 exists because that supply obligation kept being forgotten.

The cost is the documented failure, fork **#74**: *"a second orchestration's task named no worktree path, so its worker found the first orchestration's existing branch and reasonably joined it — writing production code into a worktree it did not own, then pushing to it and cancelling the first orchestration's in-flight CI run."* More than once.

**The goal is autonomous operation** — spinning up agents without a human provisioning directories first. That needs two things the deck does not have: an orchestration that can create its own worktree, and an identity that says which worktrees are *its*.

### The workflow this serves

1. Every orchestration starts from the deck's own checkout (`main`). **Intended** — not something to prevent.
2. Each change gets **its own** worktree, created by the agent when work starts.
3. That worktree belongs to the orchestration that created it.
4. The orchestration can list what it owns, and works only on those.
5. A worktree from an earlier session whose owner matches is **available again** — a restart resumes rather than orphans.

Step 2 is manual. Steps 3–5 do not exist.

### The missing key: names are not identities

The new-pane form already has a **Name** field, and for an orchestration it becomes `display_title` — which is explicitly decoration. `src/agent_pty.rs:337`: *"is title-only and never feeds delegate/role lookups."* `src/ui.rs:8264`: *"name to the tab TITLE only."* It is optional, and nothing prevents two tabs sharing one.

So there is no answer to "which orchestration is this?" that survives a restart. `orchestration_id` is unique but **minted fresh on every tab open** (`mint_orchestration_id`), so a reopened tab would not recognise its own worktrees — breaking step 5 exactly when it matters. `name` + `orchestration_cwd` are identical for two tabs of one orchestration in one directory, which is the normal case here.

## Solution Overview

**Make the orchestration's name a real identity, and everything else follows.**

1. **A name is required, and must be unique** among live orchestrations.
2. **The name is suggested** as the next free `<foldername>-orchestrator-N`, so accepting it is one keystroke.
3. **The name is the ownership identity**, written into the marker of every worktree the orchestration creates.
4. **Worktrees are named `<orchestration-name>-<change>`**, so the prefix is the owner and `ls` shows it at a glance.
5. **The marker decides ownership, never the name.**
6. **Provisioning is one step** — `delegate` creates the worktree. **Split out to fork [#175](https://github.com/prageethw/dot-agent-deck/issues/175)**; see below.

### Scope split: this PRD is the identity and ownership half

Automatic provisioning is now **fork #175**, which depends on this one. The split is deliberate: this PRD carries the two risky parts — a **semantic break behind a stable wire** (promoting `display_title`, which triggers rule 12's cross-version manual test and a `.breaking.md` fragment) and an **audit sweep** of every existing test that submits an orchestration form, whose blast radius is not known up front.

Landing those separately means #175 builds on settled ground, and that if the wire change causes trouble there is a smaller thing to unpick. What ships here is still independently useful: unique orchestration names, and an answerable *"which worktrees are mine?"* — including after a restart.

### What fork PR #173 already landed, and what it leaves

**Update 2026-08-09.** While this PRD was being written, fork PR **#173** (`a6fee76`, tracking *upstream* issue #425) merged and independently built the **write half** of ownership. That is a help, not a collision — it settles the marker format question this PRD would otherwise have had to answer.

Already on `main`:

- **The marker format.** `mark_worktree_owned(worktree_path, creator)` writes `"deck\ncreated-by: <sanitized>\n"` — one field per line, with the bare `deck` first line kept so an older reader still sees a valid marker. Its doc is explicit about the read contract: *"a future reader must strip the literal `created-by: ` prefix and treat the remainder as opaque, never `split(':')`"*.
- **`sanitize_marker_creator`** — drops C0/DEL controls, collapses newlines to spaces so the two-line shape survives, caps at 200 chars, and maps empty input to `"unknown"`.
- **`creator` threaded through both creation paths** — `create_worktree` and `create_worktree_sync` in `src/issue_dispatch_run.rs` both take it.
- **Issue-dispatch passes `issue-dispatch:<task>#<issue>`**, which matches this PRD's "the scheduled task's name on the dispatch path" exactly.

**This PRD adopts that format rather than proposing another.** It is well-designed, documented, and already shipped.

What it leaves for this PRD:

1. **There is no read side.** Nothing parses `created-by:` back out, so ownership cannot yet be *queried* — only written.
2. **The interactive path records the wrong identity for our purpose.** `src/ui.rs` passes `format!("orchestration:{}", orch_config.name)` — the canonical **config/type** name (`review`, `tdd-cycle`), shared by every orchestration of that type. Two live `review` orchestrations therefore record the *same* creator, which does not distinguish instances. That is not a defect in #173 — its stated scope is *"record which task created a worktree"*, provenance rather than instance identity, and it is honest about that. Making the value instance-unique is precisely what this PRD adds, and #173's own sanitizer doc anticipates it by naming *"a TUI-typed orchestration name"* as an input.
3. **Nothing surfaces the owner** in `worktree list`.

So Phase 2 shrinks: the format is settled and the plumbing exists. What remains is the read side, the right value, and the display.

### Why the marker decides and the name does not

The naming convention exists for humans. It carries no authority, and that distinction is the entire safety argument.

**Name-based (prefix) ownership was considered and rejected.** It would adopt a worktree the deck did not create — a hand-made folder matching the pattern becomes `Ours`, and fork #144 makes `Ours` + merged + clean removable by a bare `reclaim` with **no prompt and no path shown**. That is the P1 fork #144 closed. Reopening it to save a file read is a bad trade.

With the marker authoritative, a matching name on a folder the deck did not create is simply `Foreign`. Nothing is adopted, and the convention stays useful.

### Promoting `display_title` is a contract change, not a rename

Using the typed Name as the identity means `display_title` stops being presentation-only. That contract is stated in PRD #140's own field docs and must be updated there, not silently contradicted.

Two consequences:

- Every site that treats it as cosmetic needs checking. It is `Option<String>` with `skip_serializing_if`, deliberately, so older peers round-trip cleanly.
- **This is a semantic break behind a stable wire** — the field's *shape* is unchanged, its *meaning* is not. CLAUDE.md rule 12 names exactly this case, so a `changelog.d/*.breaking.md` fragment applies, and whether `PROTOCOL_VERSION` moves must be answered rather than assumed.

### Daemon-initiated orchestrations have a different identity, and that is fine

Scheduled and issue-dispatch orchestrations set `display_title: None` (`src/issue_dispatch_run.rs:1552`, `:1646`; `src/spawn.rs:339`) — nobody types a name.

They do not need one. **Issue-dispatch already provisions its own worktree** (`<clone>/.worktrees/issue-<n>`, branch `agent/issue-<n>`) and already writes the marker, keyed on the `ScheduledTask` name it also uses as the reuse key. That name is stable across restarts for the same reason a typed name is.

So the owner recorded is the typed name for interactive orchestrations and the scheduled task's name for dispatched ones. Both are stable identifiers; only the source differs. The requirement in this PRD — *a name must exist and be unique* — is a constraint on the **interactive** path only.

### `main` is already protected, structurally

An orchestration starting in `main` must never treat `main` as an owned worktree. **This needs no new check.** fork #144's containment already guarantees it:

```rust
if !git_dir.starts_with(common_dir.join("worktrees")) {
    return Ownership::Foreign;
}
```

The main checkout's git dir is `<repo>/.git`, which sits *above* `<repo>/.git/worktrees`, so it can never match — `main` always resolves `Foreign`, whatever it is named. Independently, `mark_worktree_owned` runs only immediately after a successful `git worktree add`, "never for a pre-existing or foreign worktree", so no marker is written there.

That check was built to defeat a forged marker. It protects this case for free, because *"is this a real linked worktree of this repo?"* is the same question either way.

## Scope

### In Scope

- A required, unique orchestration name on the interactive path, suggested as `<foldername>-orchestrator-N`.
- Promoting `display_title` from presentation to identity, and updating PRD #140's contract wording.
- Owner recorded in fork #144's marker; `ownership_of` reports it.
- `delegate` provisioning `<orchestration-name>-<change>` in one step.
- Listing the worktrees an orchestration owns, correct after a restart.
- Amending CLAUDE.md rule 1 so manual `git worktree add` is replaced, keeping rule 16's supply obligations.

### Out of Scope

- **Any restriction on where orchestration tabs start.** All orchestrations starting from `main` is the intended workflow. An earlier draft proposed blocking a second orchestration per directory; that was a misreading and is withdrawn. **PRD #140's advisory stance stands unchanged.**
- **Automatic worktree removal.** fork #122 deliberately never removes worktrees; `reclaim` stays the gated path.
- **Adopting worktrees the deck did not create.** Never, by any mechanism.
- **Stopping an agent `cd`-ing into another orchestration's worktree.** No deck-side check can see that; it is agent discipline under rule 1.
- **Namespacing `worker-task-<role>.md`** — see Risks.

## Success Criteria

- Two live orchestrations cannot share a name.
- The suggested name is accepted with one keystroke.
- Every worktree an orchestration creates records it as owner; the owner is listable.
- **After closing and reopening a tab with the same name, its earlier worktrees are still recognised as its own.**
- A worktree created by a different orchestration is never owned. One the deck did not create is never owned, whatever it is named.
- `main` never appears as owned.
- A worktree created before this ships still resolves `Ours`, so `reclaim` keeps working on it.
(Replacing CLAUDE.md rule 1's manual `git worktree add` is fork #175's success criterion, not this PRD's — this one supplies the identity that makes it possible.)

## Milestones

### Phase 1: Name as identity

- [ ] **M1.0** — the interactive Name is required and refused if it matches a live orchestration; suggested as the next free `<foldername>-orchestrator-N`.
- [ ] **M1.1** — `display_title` promoted to identity; PRD #140's field docs updated; every cosmetic-assumption site checked.
- [ ] **M1.2** — rule 12 answered: `.breaking.md` fragment, and an explicit `PROTOCOL_VERSION` decision.

### Phase 2: Ownership

- [x] **M2.0** — ~~`mark_worktree_owned` records the owner~~ **done by fork PR #173** (`a6fee76`), including the format and the sanitizer. The dispatch path already passes the scheduled task's name.
- [ ] **M2.1** — the interactive path passes the **typed unique name** instead of `orch_config.name`, so the recorded identity distinguishes live instances.
- [ ] **M2.2** — a read side: parse `created-by:` back out per #173's stated contract (strip the literal prefix, treat the remainder as opaque, **never** `split(':')`). Containment and presence stay authoritative — an empty or prefix-less marker resolves `Ours` with owner unknown.
- [ ] **M2.3** — `worktree list` shows the owner; `--json` carries it; decide whether `SCHEMA_VERSION` moves.

### Phase 3: Query

- [ ] **M3.0** — an orchestration can list the worktrees it owns, **correctly after a restart**. This is the milestone the whole identity choice exists for.

### Phase 4: Ship

- [ ] **M4.0** — docs; the deployment precondition below stated in the changelog. CLAUDE.md rule 1's manual `git worktree add` stays for now — it is replaced by fork #175, not by this PRD.

### Moved to fork #175

Provisioning (`delegate` creating `<orchestration-name>-<change>` in one step), worktree reuse, and refusal when a target is owned by another orchestration. All depend on the identity and ownership this PRD establishes.

## Key Files

- `src/ui.rs` — `FormField::Name` (`:822`), the orchestration `display_title` assignment (`:8264-8266`), `resolve_orchestration_worktree_path` (`:6627`), `validate_orchestration_worktree_slug` (`:6572`), `live_orchestration_cwds` (`:790` — the existing liveness query a uniqueness check can reuse)
- `src/agent_pty.rs` — `TabMembership::Orchestration.display_title` (`:341`) and its contract docs
- `src/worktree_reclaim.rs` — `OWNER_MARKER_FILENAME`, `mark_worktree_owned`, `ownership_of`, and the containment protecting `main`
- `src/issue_dispatch_run.rs` — `create_worktree_sync`, and the `display_title: None` dispatch sites
- `src/main.rs` — `WorktreeCmd` (`:393`)
- `prds/140-orchestration-session-partitioning.md` — the `display_title` contract to update
- `tests/CATALOG.md` — `worktree/reclaim/*`, `orchestration/worktree/*`

## Risks and Mitigations

- **Deployment precondition: existing worktrees become obsolete.** They carry a marker but no owner, so no orchestration can claim them. **Drain them before shipping.** They must still resolve `Ours` so `reclaim` keeps working — obsolete for *ownership*, not for cleanup. This needs its own test; it protects every worktree created before this ships.
- **An identity that changes across restarts breaks resume.** The trap `orchestration_id` sets. Mitigated by using the name, and by a test that resumes **after a simulated restart** — a same-session test would pass even with the broken design.
- **Promoting `display_title` silently.** It is documented as cosmetic in PRD #140 and in the field's own docs. Mitigated by updating both, and by rule 12's `.breaking.md`.
- **Ownership becoming a licence to delete.** Ownership answers "may I work here?", not "may I remove this?". `reclaim`'s gates are unchanged and this PRD adds no removal path.
- **`worker-task-<role>.md` still collides per-directory.** `src/state.rs:2059` keys it by role alone — PRD #140's layer 2, still deferred. Since every orchestration here starts in `main`, two *inline* `--task` delegations to the same role could clobber each other; the fork's documented `--task-file` default supplies a unique slug, so the common path is unaffected. Recorded so it is not mistaken for solved. (Its `work-done` twin **is** fixed — `work_done_file_name(role, pane_id)`; the collisions seen 2026-08-09 came from a running daemon predating that fix.)

## Open Questions

- **Is uniqueness scoped to live orchestrations, or to all names ever used?** Live-only is simpler and matches the daemon's existing query, but it lets a name be reused after a tab closes — and that reuse is *exactly* how resume is meant to work. Leaning live-only, with resume as the intended consequence rather than a loophole.
- **What happens to a running orchestration whose name is edited?** Simplest is that names are fixed at tab open.
- **Should `<change>` be supplied or derived?** Supplied is explicit; derived from the task risks unstable or colliding slugs.
- **`SCHEMA_VERSION`** for the added owner field in `worktree list --json` — the field is additive.
