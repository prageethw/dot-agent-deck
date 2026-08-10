# PRD fork#166: Unique orchestration names, and worktrees the agent provisions itself

**GitHub Issue**: [fork #166](https://github.com/prageethw/dot-agent-deck/issues/166)

**Priority**: High

**Status** *(updated 2026-08-10, verified against `main` @ `2d89c30` rather than from these checkboxes)*: **Phase 1 complete** — shipped via the carved-out PRD [fork#192](https://github.com/prageethw/dot-agent-deck/issues/192) (PR #193, merged `7426b25`), **released in v0.37.0**. **Phase 2 complete except M2.3's display half** — the marker write, the interactive typed-name value and the read side are all on `main`; `worktree list --json` already carries `owner`, but nothing renders it in the human table. **Phase 3 (M3.0) is blocked** on a newly-identified supply gap, now tracked as **M2.4** below. **Phase 4 pending.**

**Fork-only**, and intended to stay so. Confirmed mechanically against `upstream/main`: `src/worktree_reclaim.rs` **does not exist** there and `worktree_slug` has **0** occurrences. The ownership half is built entirely on fork-only code, so per `docs/develop/upstream-contribution-policy.md` this is not an offer-later case.

**Related**: fork [#192](https://github.com/prageethw/dot-agent-deck/issues/192) (Phase 1 carved out and **shipped** — names as instance identity, released in v0.37.0) · fork [#201](https://github.com/prageethw/dot-agent-deck/issues/201) (the residual #192 left open: uniqueness is advisory against a form-open snapshot) · fork #144 (the ownership marker and its containment check — the foundation) · fork #122 (per-orchestration worktree creation, and the hardened path validation reused here) · PRD #422 (`worktree list` / `reclaim`) · PRD #140 (per-tab routing identity; its `display_title` contract changes here) · PRD #120 (issue-dispatch, which already provisions its own worktrees) · PRD #220 (the upstream cousin; its M1.1 naming question is settled differently here) · fork #74 (the motivating collision) · PRD #421 (the provenance precedent, applied to a case it did not cover)

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

**Update 2026-08-10 — two of those three are now done, via fork#192.** Item 1 (no read side) is closed by `read_marker_owner`/`owner_of`. Item 2 (wrong identity) is closed on the normal path: `src/ui.rs:9103` now passes the typed name, though the `orch_config.name` fallback at `:9107` and the `orchestration:unknown` fallback at `:9105` both still exist. Item 3 (nothing surfaces the owner) is **half** closed — `--json` carries it, the human table does not. What that left unnoticed until now is that none of the three was the actual blocker for Phase 3: **the running orchestration has no way to know its own owner string**, which is M2.4.

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
- **An orchestration can determine its own identity at runtime.** *(Added 2026-08-10.)* Every criterion below assumes this and none of them stated it, which is exactly how the gap survived into implementation planning — see M2.4.
- The suggested name is accepted with one keystroke.
- Every worktree an orchestration creates records it as owner; the owner is listable.
- **After closing and reopening a tab with the same name, its earlier worktrees are still recognised as its own.**
- A worktree created by a different orchestration is never owned. One the deck did not create is never owned, whatever it is named.
- `main` never appears as owned.
- A worktree created before this ships still resolves `Ours`, so `reclaim` keeps working on it.
(Replacing CLAUDE.md rule 1's manual `git worktree add` is fork #175's success criterion, not this PRD's — this one supplies the identity that makes it possible.)

## Milestones

### Phase 1: Name as identity

**All of Phase 1 shipped via PRD [fork#192](https://github.com/prageethw/dot-agent-deck/issues/192)**, not on this PRD's own branch — carved out precisely because it carried the two risky parts. PR #193, merged `7426b25`, released in **v0.37.0**.

- [x] **M1.0** — the interactive Name is required and refused if it matches a live orchestration; suggested as the next free `<foldername>-orchestrator-N`. **Done (fork#192).** The refusal renders on PRD #140's existing guard seam; the liveness query was extended to carry names, so it costs no extra daemon round-trip.
- [x] **M1.1** — `display_title` promoted to identity; PRD #140's field docs updated; every cosmetic-assumption site checked. **Done (fork#192).**
- [x] **M1.2** — rule 12 answered: `.breaking.md` fragment, and an explicit `PROTOCOL_VERSION` decision. **Done (fork#192).** `changelog.d/192.breaking.md` shipped; `PROTOCOL_VERSION` deliberately **not** bumped (the wire shape is unchanged, so no peer can mis-parse a frame) and the cross-version manual test ran against `v0.36.1` with all four items passing.

**Known residual from M1.0, tracked as fork [#201](https://github.com/prageethw/dot-agent-deck/issues/201):** the uniqueness refusal reads a snapshot of live names taken once when the form opens and never refreshed. Two forms open *concurrently* can still be suggested — and both submit — the same name, and nothing at the marker write enforces uniqueness either. That concurrent case is the one fork #74 was actually about, so Phase 1 narrows the collision rather than closing it.

### Phase 2: Ownership

- [x] **M2.0** — ~~`mark_worktree_owned` records the owner~~ **done by fork PR #173** (`a6fee76`), including the format and the sanitizer. The dispatch path already passes the scheduled task's name.
- [x] **M2.1** — the interactive path passes the **typed unique name** instead of `orch_config.name`, so the recorded identity distinguishes live instances. **Done (fork#192).** `src/ui.rs:9103` builds `format!("orchestration:{typed_name}")` and `:9109` passes it to `create_worktree_sync`. **Two fallbacks remain and matter:** `orchestration:unknown` (`:9105`) when no name is available, and `orchestration:<orch_config.name>` (`:9107`) — the config/type name this milestone was written to replace. The typed-name branch is the normal path now, but the config-name branch was not deleted, so "instead of `orch_config.name`" is true of the common case rather than of every case.
- [x] **M2.2** — a read side: parse `created-by:` back out per #173's stated contract (strip the literal prefix, treat the remainder as opaque, **never** `split(':')`). Containment and presence stay authoritative — an empty or prefix-less marker resolves `Ours` with owner unknown. **Done (fork#192).** `read_marker_owner` (`src/worktree_reclaim.rs:502`, `strip_prefix("created-by: ")` at `:510`) and `owner_of` (`:545`).
- [ ] **M2.3** — `worktree list` shows the owner; `--json` carries it; decide whether `SCHEMA_VERSION` moves. **Half done.** The **JSON half already shipped**: `WorktreeReport.owner` exists as `Option<String>` with `skip_serializing_if`, omitted entirely rather than serialized as `null` so an older client round-trips cleanly. What remains is the **human table** — `format_list_human` (`:900`) emits `PATH BRANCH PR CLEAN OWNED VERDICT REASON` and no OWNER column; `:491` says so outright: *"latent today (no consumer renders `owner` yet)"*. **`SCHEMA_VERSION` decision: it stays at `1`** — the `owner` field shipped *at* schema 1 and is additive-optional, so bumping now would announce a change that already happened silently. This milestone asked for a decision, not necessarily a move.

- [ ] **M2.4 — supply the owner identity to the running orchestration** *(added 2026-08-10; not in the original plan)*. **M3.0 cannot be built without it**, and nothing in this PRD previously said so.

    **The gap, as measured.** An orchestration cannot learn its own identity at runtime. `DOT_AGENT_DECK_AGENT_ID` and `DOT_AGENT_DECK_PANE_ID` (`src/agent_pty.rs:41`, `:56`) are the *only* identity in a pane's environment; both are small daemon-scoped integers (observed as `6`) that **recycle across a daemon restart**, and nothing carries the owner string to a pane. So `worktree list --mine` has no way to compute "mine". This is a CLAUDE.md rule 16 shape — a consumed value with no named supplier — and it is the same wall CLAUDE.md rule 23 hit, which is why that rule anchors issue claims to worktree paths rather than to orchestration names.

    **The owner string is namespaced by producer, and exact-matching depends on knowing that.** `owner_of` returns the whole `created-by:` remainder verbatim and never splits on `:`, so ownership is an exact string comparison against a value whose *shape depends on which path created the worktree*:

    | Producer | Owner string | Source |
    |---|---|---|
    | Interactive orchestration | `orchestration:<typed_name>` | `src/ui.rs:9103` |
    | Issue-dispatch | `issue-dispatch:<task_name>#<issue>` | `src/issue_dispatch_run.rs:402` |

    Earlier sections of this PRD describe both producers correctly but read as though there is one uniform identity. There is not, and M3.0's *"correct after a restart"* is meaningless without saying **which string** is being matched.

    **The fix.** Inject `DOT_AGENT_DECK_WORKTREE_OWNER` into every pane, carrying **the exact creator string this orchestration stamps when it creates a worktree** — `orchestration:<typed_name>` interactively, the `issue-dispatch:<task>#<n>` string on the dispatch path. One source for both sides, so marker and filter cannot drift. Named for what it carries, not for one producer: calling it `…_ORCHESTRATION` would misdescribe it the moment a dispatched orchestration used it.

    **Decided consequences.** A **dispatched** orchestration therefore also matches the worktree it is running in, because that is the same string — M3.0's promise must hold for dispatched work, which is the autonomous case this PRD exists to serve. And `orchestration:unknown` (`src/ui.rs:9105`) is a **sentinel, never an identity**: two nameless orchestrations would otherwise match each other's worktrees and each be handed the other's work, so it must be treated exactly like an absent variable — fail loudly.

    **Open, and to be settled before implementation:** whether the daemon already holds the creator string at spawn for both paths, or whether it must be threaded into `SpawnRequest`/`RoleSpawn` (`src/spawn.rs:74`, `:152`). The dispatch path computes its creator at `src/issue_dispatch_run.rs:402`, well before spawn, so this is threading an existing value rather than deriving a new one — but if the wire shape moves, rule 12 applies and the bump is minor rather than patch.

### Phase 3: Query

- [ ] **M3.0** — an orchestration can list the worktrees it owns, **correctly after a restart**. This is the milestone the whole identity choice exists for. **Depends on M2.4.** Shipped as `worktree list --mine`, filtering `owner_of` == `DOT_AGENT_DECK_WORKTREE_OWNER`. With no identity available — variable absent, or set to the `orchestration:unknown` sentinel — it **fails loudly and explains why**, never silently returning everything or nothing; a wrong answer here hands one orchestration another's worktree. Restart-correctness falls out of the design rather than needing machinery: both sides derive from the same stable string and nothing depends on in-memory state.

    **Scope of the query surface: `--mine` only.** No `--owner <name>`, and no `reclaim --mine`. This PRD's *"reports and filters by owner"* is satisfied by the OWNER column (M2.3) plus self-filtering — every other owner becomes visible, so filtering on an arbitrary one is a `grep`. `reclaim` in particular is the destructive path whose gating fork #144 hardened deliberately; widening it belongs with fork #175's provisioning work, not here.

### Phase 4: Ship

- [ ] **M4.0** — docs; the deployment precondition below stated in the changelog. CLAUDE.md rule 1's manual `git worktree add` stays for now — it is replaced by fork #175, not by this PRD.

### Moved to fork #175

Provisioning (`delegate` creating `<orchestration-name>-<change>` in one step), worktree reuse, and refusal when a target is owned by another orchestration. All depend on the identity and ownership this PRD establishes.

## Key Files

- `src/ui.rs` — `FormField::Name` (`:822`), the orchestration `display_title` assignment (`:8264-8266`), `resolve_orchestration_worktree_path` (`:6627`), `validate_orchestration_worktree_slug` (`:6572`), `live_orchestration_cwds` (`:790` — the existing liveness query a uniqueness check can reuse)
- `src/agent_pty.rs` — `TabMembership::Orchestration.display_title` (`:341`) and its contract docs
- `src/worktree_reclaim.rs` — `OWNER_MARKER_FILENAME`, `mark_worktree_owned` (`:602`), `ownership_of`, and the containment protecting `main`; plus the read side and display seam this PRD finishes: `read_marker_owner` (`:502`), `owner_of` (`:545`), `WorktreeReport.owner` (`:168`), `format_list_human` (`:900` — where the OWNER column goes), `SCHEMA_VERSION` (`:37`)
- `src/issue_dispatch_run.rs` — `create_worktree_sync` (`:1058`), `mark_worktree_owned_best_effort` (`:1036`), the `issue-dispatch:<task>#<issue>` creator (`:402`), and the `display_title: None` dispatch sites
- `src/main.rs` — `WorktreeCmd::List` (`:393`, where `--mine` goes), dispatch (`:1235`), `run_worktree_list_cli` (`:1672`)
- `src/agent_pty.rs` — the env-var pattern M2.4 follows: `DOT_AGENT_DECK_PANE_ID` (`:41`), `DOT_AGENT_DECK_AGENT_ID` (`:56`), and the scrub-then-overlay block (`:1073`–`:1079`) that stops a daemon launched inside another deck's pane leaking a stale value
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
- ~~**`SCHEMA_VERSION`** for the added owner field in `worktree list --json` — the field is additive.~~ **Settled 2026-08-10: it stays at `1`.** The `owner` field shipped *at* schema 1 with `skip_serializing_if`, so it is already absent-not-null for older clients; bumping now would announce a change that has already happened silently. See M2.3.
