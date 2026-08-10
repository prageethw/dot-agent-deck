# PRD fork#192: Orchestration names as instance identity

**GitHub Issue**: [fork #192](https://github.com/prageethw/dot-agent-deck/issues/192)

**Priority**: High

**Status**: Planning

**Parent**: [fork #166](https://github.com/prageethw/dot-agent-deck/issues/166) — this PRD carves out its parked **Phase 1** (M1.0/M1.1/M1.2). Sibling of [fork #175](https://github.com/prageethw/dot-agent-deck/issues/175), which carved out provisioning.

**Related**: fork #184 (the misleading comment on the exact derivation edited here — its fix rides this PR) · fork #144 (the ownership marker and its containment check) · fork #74 (the motivating collision) · PRD #140 (per-tab routing identity; its `display_title` contract changes here) · PRD #107 (the title-vs-identity split this must preserve) · PRD #120 (issue-dispatch, whose orchestrations carry no typed name) · [upstream #220](https://github.com/vfarcic/dot-agent-deck/issues/220) (the naming question this settles)

**Filename convention**: `fork-<n>-` prefix, per fork#166 — a bare `192-` would be ambiguous against a future upstream #192.

## Problem Statement

fork#166 shipped its ownership half and parked its naming half. The result is an ownership system that can write and read an owner but **cannot tell two live orchestrations apart**.

The new-pane form's Name field **pre-fills with the picked directory's basename** (`src/ui.rs:6997`, `DirPickerIntent::NewPane`) and nothing checks uniqueness. A user who never touches the field still submits a non-empty name — the same one every other orchestration in that directory submits. So every orchestration opened in one repo records the identical marker:

```
created-by: orchestration:worker-agent-deck
```

That is provenance ("a deck orchestration made this"), not identity ("*which* one"). It is exactly the fork #74 collision fork#166 exists to prevent, and it leaves fork#166's own M3.0 — *"an orchestration can list the worktrees it owns, correctly after a restart"* — unanswerable on the value currently recorded.

### What is already delivered, so this PRD does not redo it

Verified against `main` @ `4a68720`, by driving the real binary rather than reading checkboxes:

| fork#166 milestone | State |
|---|---|
| **M2.0** marker write, format, `sanitize_marker_creator` | ✅ fork PR #173 (`a6fee76`) |
| **M2.1** interactive path records the typed name | ✅ `src/ui.rs:8848` |
| **M2.2** read side — `owner_of`, `read_marker_owner`, strip-prefix contract | ✅ `src/worktree_reclaim.rs:471–545` |
| **M2.3** owner surfaced | ⚠️ half — `--json` carries it; the human table has no owner column, `SCHEMA_VERSION` still `1` |
| `worktree/reclaim/017`–`022` | ✅ implemented as `#[spec]` unit tests |
| **M1.0 / M1.1 / M1.2** | ❌ nothing — **this PRD** |

**`worktree/reclaim/022` does not already cover this.** It proves two *distinct typed names* record distinct owners, and it passes. What it cannot prove is that the names are ever distinct, because the pre-fill hands both instances the same one. The gap is upstream of the assertion.

## Solution Overview

**Make the typed name a real identity: required, unique among live orchestrations, and suggested so accepting it costs one keystroke.**

1. The Name pre-fills as the next free `<foldername>-orchestrator-N` instead of the bare basename.
2. Submitting a name a live orchestration already holds is **refused**, not warned about.
3. `display_title` stops being decoration and becomes the recorded identity — a contract change, documented as one.

The marker still decides ownership. The name is how humans and agents *refer* to an orchestration; it carries no authority. fork#166 rejected name-based (prefix) ownership and that rejection stands unchanged here — a hand-made folder matching the pattern is still `Foreign`.

### Why `<foldername>-orchestrator-N` and not "suffix only on collision"

fork#166 already decided this and the decision is adopted rather than reopened. The uniform form means the recorded `created-by:` value and the `<orchestration-name>-<change>` worktree prefix read the same for the first instance as for the fifth. A scheme that suffixes only on collision produces `worker-agent-deck` and `worker-agent-deck-2`, whose markers and worktree prefixes are inconsistent in kind — the first does not announce itself as an orchestration at all.

The cost is honest and belongs in the changelog: **this changes the visible default** on the normal path for every user.

### Promoting `display_title` is a contract change, not a rename

PRD #140 documents the field as presentation-only, and so do its own docs (`src/agent_pty.rs:341/352/608`: *"title-only and never feeds delegate/role lookups"*, *"purely cosmetic"*). Using it as identity contradicts that, so PRD #140's wording is **updated**, not silently outgrown.

Two consequences:

- **Every cosmetic-assumption site needs checking.** `validate_orchestration_surface` currently *nulls* an invalid `display_title` and keeps the surface, on the explicit grounds that it is cosmetic with a defined `None` fallback. Once it is identity, that fallback is a decision to re-make, not one to inherit.
- **This is a semantic break behind a stable wire.** The field's shape is unchanged (`Option<String>`, `skip_serializing_if`, so older peers still round-trip); its meaning is not. CLAUDE.md rule 12 names exactly this case — see M1.2.

### Daemon-initiated orchestrations are unaffected, by design

Scheduled and issue-dispatch orchestrations set `display_title: None` (`src/issue_dispatch_run.rs:1552`, `:1646`; `src/spawn.rs:339`) — nobody types a name. They do not need one: issue-dispatch already provisions its own worktree and already writes a marker keyed on the `ScheduledTask` name it also uses as the reuse key, which is stable across restarts for the same reason a typed name is.

**The uniqueness requirement constrains the interactive path only.** A change that made `display_title` mandatory wire-wide would break every dispatched orchestration; it must not.

### `main` is already protected, structurally

Unchanged from fork#166 and restated so it is not re-litigated: fork #144's containment (`git_dir.starts_with(common_dir.join("worktrees"))`) means the main checkout's `.git` can never match, so `main` resolves `Foreign` whatever it is named. No new check is needed, and `worktree/reclaim/019` already pins it.

## Scope

### In Scope

- **M1.0** — Name required, unique among live orchestrations, suggested as the next free `<foldername>-orchestrator-N`.
- **M1.1** — `display_title` promoted to identity; PRD #140's contract wording updated; every cosmetic-assumption site checked.
- **M1.2** — rule 12 answered: a `changelog.d/192.breaking.md` fragment and an explicit `PROTOCOL_VERSION` decision.
- fork **#184**'s comment fix, since it sits on the lines this changes.

### Out of Scope

- **fork#166's M2.3 remnant** — the owner column in the human `worktree list` table and the `SCHEMA_VERSION` question. Stays in #166.
- **fork#166's M3.0** — listing what an orchestration owns. It *consumes* this identity; it is not this change.
- **Everything in fork #175** — `delegate` provisioning, worktree reuse, refusal when a target is owned elsewhere.
- **Any restriction on where orchestration tabs start.** All orchestrations starting from `main` is the intended workflow; PRD #140's advisory stance is unchanged.
- **Renaming a running orchestration.** Names are fixed at tab open — fork#166's open question, settled the simple way.
- **Uniqueness across *all* names ever used.** Scoped to **live** orchestrations, so a name is reusable once its tab closes — that reuse is how resume is meant to work.

## Success Criteria

- Two live orchestrations cannot share a name.
- The suggested name is accepted with one keystroke, and it is unique on first open.
- Two orchestrations of the same config type in the same directory record **different** owners in their worktree markers.
- A dispatched (daemon-initiated) orchestration, which types no name, still opens and still records its `ScheduledTask` name.
- PRD #107's split survives: the tab TITLE is the typed name, and the daemon IDENTITY used by `lookup_orchestration_role` is still the canonical config name.
- A pre-existing worktree, and an older peer on the wire, both still work.

## Milestones

### M1.0 — Name as a unique, suggested identity

- [ ] The form suggests the next free `<foldername>-orchestrator-N` in place of the bare basename.
- [ ] A name held by a live orchestration is refused at submit, with the refusal rendered on the existing guard seam.
- [ ] The liveness query carries names, not only cwds.

### M1.1 — `display_title` promoted to identity

- [ ] PRD #140's field docs and `src/agent_pty.rs`'s contract comments updated.
- [ ] Every cosmetic-assumption site audited, `validate_orchestration_surface`'s null-and-keep fallback re-decided explicitly.
- [ ] fork #184's comment corrected in the same pass.

### M1.2 — Rule 12 answered

- [ ] `changelog.d/192.breaking.md` fragment.
- [ ] An explicit `PROTOCOL_VERSION` decision, recorded with its reasoning whichever way it goes.
- [ ] The cross-version manual test run: previous-release daemon + this branch's TUI, confirming a delegate still routes and hooks still arrive.

### M2.0 — Offer upstream (trigger, not intention)

- [ ] Once M1.0–M1.2 are merged here, open the form-level commit upstream against [#220](https://github.com/vfarcic/dot-agent-deck/issues/220).

**Why this is a milestone with a trigger rather than a note.** fork#166 declares itself fork-only and justifies it mechanically — `src/worktree_reclaim.rs` does not exist upstream and `worktree_slug` has 0 occurrences there. **That justification covers the ownership half, not this one**: the new-pane form and `display_title` are upstream code, and upstream #220's M1.1 carries an open naming question this answers. Building here first is right, because the only consumers of the identity are fork-only. Leaving it here permanently is not. CLAUDE.md rule 19 exists because "yes, but not yet" is how fourteen commits ended up stranded in `fork-only`, re-rebased and re-verified on every sync forever.

Keep the form-level change a **clean separable commit** so the offer is a cherry-pick, not a rewrite.

## Key Files

- `src/ui.rs:6997` — the basename pre-fill to replace (`DirPickerIntent::NewPane`).
- `src/ui.rs:790` — `live_orchestration_cwds()`, **already called at form-open** (`:7013`) whenever the project offers an orchestration. It discards names today. Extending it costs **no extra daemon round-trip**; do not add a second query.
- `render_new_pane_orchestration_guard_to_buffer` — the existing L1 render seam behind `orchestration/guard/001`, which renders PRD #140's non-blocking shared-cwd warning. The refusal belongs here. Note the two differ in kind: #140's warns and still lets you submit.
- `src/ui.rs:8848` — the `creator` derivation; also fork #184's subject.
- `src/agent_pty.rs:341/352/608` — `display_title`'s contract docs and `validate_orchestration_surface`.
- `prds/140-orchestration-session-partitioning.md` — the contract wording to update.
- `tests/CATALOG.md` — `orchestration/identity/*`, `orchestration/guard/*`, `worktree/reclaim/*`.

## Risks and Mitigations

- **Test churn is the unknown.** 13 test files submit an orchestration form and may assume the basename pre-fill; changing the default moves snapshots. fork#166 called this an audit sweep *"whose blast radius is not known up front"*. **Mitigation:** the RED round reports the real count before implementation starts, so the scope is known rather than discovered.
- **A semantic break behind a stable wire is invisible to CI.** Nothing in the fast tier can see that a field's *meaning* moved. **Mitigation:** M1.2's cross-version manual test is the only thing that can, and it is a milestone rather than a checklist line.
- **Breaking dispatched orchestrations.** They carry `display_title: None` by design. **Mitigation:** a success criterion of its own, and the uniqueness constraint is scoped to the interactive path in as many words.
- **Regressing PRD #107.** The title-vs-identity split is easy to collapse while promoting the title. **Mitigation:** `orchestration/identity/001` already pins it; it must keep passing unmodified, and any change to it is a signal, not a chore.
- **The suggestion racing itself.** Two forms open at once could both be suggested `-orchestrator-2`. The refusal at submit is the backstop; the suggestion is a convenience, not a lock.

## Open Questions

- **Does `PROTOCOL_VERSION` move?** M1.2 must answer rather than assume. The wire shape does not change, which argues no; the meaning does, which is what rule 12 says to weigh.
- **What is `N` counted over — live orchestrations in this cwd, or all live orchestrations?** Per-cwd reads more naturally (`worker-agent-deck-orchestrator-2` is the second one *here*), but two directories would then both offer `-orchestrator-1`. Since uniqueness is what the name is for, global-over-live is the safer default.
- **What happens to a name whose tab closes while a second form is open?** Live-scoped uniqueness means it becomes free mid-form. Harmless — the refusal is evaluated at submit.
