# PRD fork#166: Enforce worktree isolation between concurrent orchestrations — make the safe path the default one

**GitHub Issue**: [fork #166](https://github.com/prageethw/dot-agent-deck/issues/166)

**Priority**: High

**Status**: Planning

**Fork-only**: yes — confirmed mechanically against `upstream/main` (`worktree_slug`: 0 occurrences; `src/worktree_reclaim.rs`: does not exist). The live-orchestration guard *is* upstream, but this change depends on two fork-only pieces and cannot be offered as-is. See `docs/develop/upstream-contribution-policy.md`.

**Related**: PRD #140 (concurrent orchestration safety — establishes the worktree-per-orchestration model and the advisory guard this PRD reverses) · fork #122 (per-orchestration worktree, the mechanism this makes mandatory) · fork #144 (the ownership marker this extends) · PRD #421 (the "provenance is reporting content, not a decision input" precedent) · PRD #220 (dispatcher mode — its M1.1 worktree-naming question is adjacent and still open) · fork #74, fork #76 (the recurring collisions this exists to stop)

**Filename convention note**: this is the **first PRD named after a fork issue** — every existing PRD cites `vfarcic/dot-agent-deck`. A bare `166-` prefix would be ambiguous against a future upstream #166, so fork-numbered PRDs take a `fork-<n>-` prefix. Recorded here because it is a new convention, not an existing one.

## Problem Statement

Two orchestration tabs opened against the same directory share one working tree and one `.dot-agent-deck/` coordination directory. Their agents then step on each other.

This is not hypothetical. It is the failure CLAUDE.md rule 1 exists for, it recurred as fork #74 **twice**, and on 2026-08-09 it happened again in a different form: three worker reports collided at the same `work-done` output path across two concurrent orchestrations, each silently archiving the previous one.

**Every piece needed to prevent it is already built. What is missing is a default that uses them.**

| Piece | State |
|---|---|
| Per-tab unique identity — `orchestration_id` (PRD #140 M2.0) | Built |
| Same-directory collision **detection** (PRD #140 M4.0) | Built, **advisory only** |
| Per-orchestration worktree (fork #122) | Built, **opt-in, empty by default** |
| Ownership marker at worktree creation (fork #144) | Built, **presence-only, no content** |

`src/ui.rs:989` states the gap outright: *"no worktree by default — preserves today's behavior."* The guard warns and the isolation is opt-in, so the easy path remains the colliding one. A user has to already know about the hazard to avoid it — which inverts what a guard is for.

## Solution Overview

Three changes, one theme: **make the isolating path the default rather than a discovery.**

1. **Hard block.** A second orchestration in a directory that already hosts a live one cannot open without a distinct worktree slug.
2. **Auto-propose the slug** as the next free `<repo>-orchestrator-N`, so the safe path is one keystroke.
3. **Record provenance in the ownership marker** — orchestration name/slug, instance id, host, timestamp — so a leftover worktree stops being anonymous.

### This deliberately reverses PRD #140

PRD #140 chose to warn rather than forbid, and said why: *"The warning informs power users without forbidding them; it makes layers 2 and 3 explicit at the exact moment they become a risk."*

That judgement was reasonable and has now been tested by events. The warning has been in place, and the collision has recurred anyway — three times that we have records for. A hint that is routinely stepped past is not informing anyone; it is documentation in the wrong place.

**This PRD reverses that stance and removes a workflow that works today.** The reversal is deliberate. It is recorded here, and cross-referenced from PRD #140, so a future reader does not "fix" it back as an oversight.

### Provenance is recorded, and nothing ever branches on it

The marker gains content, but `Ours` vs `Foreign` continues to be decided by **presence alone**, exactly as fork #144 shipped it.

Two rejected alternatives, both of which look reasonable and are not:

- **Prefix-matching** (an orchestration owns every worktree matching `<repo>-orchestrator-*`) would let an orchestration claim a worktree it did not create. fork #144 makes `Ours` + merged + clean removable by a **bare `reclaim` with no prompt** — so a hand-made worktree that happens to match the pattern would be adopted and then silently deleted. That is the P1 fork #144 closed; do not reopen it.
- **Comparing `orchestration_id`** fails for a subtler reason: `mint_orchestration_id()` runs fresh on every tab open, so a tab that closes and reopens would no longer recognise its own worktree, blocking legitimate self-resume.

PRD #421 settled this exact question one PRD ago, for issue claims rather than worktrees: *"The claimant becomes reporting content, not a decision input. There is no 'is this my own claim?' comparison to implement, and therefore no subtle failure mode where a deck talks itself into re-dispatching something… The self-resume case that comparison might have served is already covered by the existing worktree-exists signal."*

Applied here, this is strictly simpler than what it replaces: no identity comparison to implement, no stable-identity requirement, and a new session with the same name reuses its worktree with no special handling.

### Fail-closed is not a flag flip — this is the hard part

`live_orchestration_cwds()` (`src/ui.rs:790`) returns `Vec::new()` on daemon failure. **That is indistinguishable from "no orchestrations are running."** The intent is explicit in its own doc: *"The warning is a best-effort hint, never a correctness gate… so a slow or down daemon fails open to 'no live orchestrations' and the form opens instantly."* `live_orchestration_in_same_cwd()` returns a bare `bool`, and best-effort `canonicalize` failures collapse into `false` too.

Blocking on that detector would produce **a gate that silently does not apply** — the same shape as every empty-gate trap catalogued in CLAUDE.md rule 8 (Greptile's credit limit, `semgrep --no-error`, `continue-on-error`, SonarQube's absent token). *A check that cannot fail is not a gate; a check that never ran is not a pass.*

So the detector must first be able to say **"I could not tell"**:

- `live_orchestration_cwds()` must distinguish transport failure from an empty result.
- `live_orchestration_in_same_cwd()` becomes three-state, not `bool`.
- Both call sites — the interactive `Ctrl+n` path and the L1 seam `render_new_pane_orchestration_guard_to_buffer` — must handle the third state.

**The risk this creates, and the decision it forces.** The query is time-boxed at `DAEMON_HINT_TIMEOUT` *specifically because a wedged daemon once froze form-open for the full default deadline*. Under a naive fail-closed, that same wedged daemon would force a worktree on **every** orchestration open, including the first one in an empty directory — turning the most common degradation into the most obstructive.

**Decision: a transport error blocks; a timeout does not.** A transport error means the daemon is unreachable, so no orchestration can be running that this one could collide with — blocking costs nothing and closes the hole honestly. A timeout means the daemon is alive but slow, which is precisely the recoverable degradation the time-box was added for; blocking there would re-create the freeze PRD #140 fixed, in a worse form. A timeout therefore **warns and permits**, and says so on screen, so a non-block is never silent.

## Scope

### In Scope

- Three-state collision detection distinguishing collision / no collision / undetermined, with transport error and timeout resolved differently per the decision above.
- A hard block in `Action::SpawnPane`'s orchestration branch when detection reports a collision (or a transport error) and no distinct worktree slug is given.
- Pre-filling `worktree_slug` with the next free `<repo>-orchestrator-N`.
- Writing provenance into the ownership marker, and reporting it.
- Backward compatibility for existing empty markers.
- Docs: the non-git consequence, and the reversal of PRD #140's stance.

### Out of Scope

- **The orchestration tab's displayed name.** `resolve_orchestration_name` is untouched, so tab naming, hydration (PRD #107) and the daemon's `handle_delegate` name lookup are all unaffected.
- **Retroactively isolating the first orchestration** in a directory. It is already running; only the second is blocked.
- **Automatic worktree removal.** fork #122 deliberately never removes worktrees, because auto-removal risks destroying uncommitted work. Unchanged.
- **`reclaim` consulting provenance** — see Open Questions; recommended rejected.
- **PRD #220's dispatcher-mode naming question.** Adjacent, still open, and settled there rather than here.

## Success Criteria

- Opening a second orchestration in a directory that already hosts a live one **cannot proceed** without a distinct worktree slug.
- The slug field arrives **pre-filled** with the next free `<repo>-orchestrator-N`; accepting it is one keystroke.
- When detection cannot determine the answer, the outcome is **never silent**: a transport error blocks and says why, a timeout warns and permits and says why.
- A worktree created before this change still resolves `Ours`, and `reclaim` still auto-removes it.
- A leftover worktree can name the orchestration, instance, host and time that created it.
- Provenance appears in **no** decision path — grep confirms it is read only for reporting.
- No `PROTOCOL_VERSION` bump (confirm, do not assume).

## Milestones

### M1 — three-state detection

- [ ] **M1.0** — `live_orchestration_cwds()` distinguishes transport failure from an empty result.
- [ ] **M1.1** — `live_orchestration_in_same_cwd()` returns three-state; both call sites handle it.
- [ ] **M1.2** — transport error vs timeout resolved per the decision above, each with its own on-screen message.
- [ ] **M1.3** — the L1 seam's synthetic paths (which fail `canonicalize` by design) do not read as blocked.

### M2 — the block and the proposed slug

- [ ] **M2.0** — `SpawnPane`'s orchestration branch refuses when detection says collide and the slug is empty or taken.
- [ ] **M2.1** — pre-fill the next free `<repo>-orchestrator-N`. **`resolve_orchestration_worktree_path` stays I/O-free** — the existence probe is a separate step.
- [ ] **M2.2** — non-git directories: refuse, with a message that explains no worktree is possible and names the second-checkout workaround.

### M3 — provenance

- [ ] **M3.0** — `mark_worktree_owned` writes name/slug, instance id, host, timestamp.
- [ ] **M3.1** — `ownership_of` still decides on presence alone; an empty or unparseable marker resolves `Ours` with provenance unknown.
- [ ] **M3.2** — provenance surfaced in the `reclaim` report.

### M4 — ship

- [ ] **M4.0** — docs, changelog fragment (this **removes a working flow** and ships unflagged, so the fragment matters more than usual), PRD #140 cross-reference.
- [ ] **M4.1** — rule 12 answered explicitly.

## Key Files

- `src/ui.rs` — `SAME_CWD_ORCHESTRATION_WARNING` (`:731`), `live_orchestration_in_same_cwd` (`:760`), `live_orchestration_cwds` (`:790`), the form's `worktree_slug` and empty default (`:824`, `:903-906`, `:989`), `resolve_orchestration_worktree_request` (`:6542`) / `_path` (`:6627`) / `validate_orchestration_worktree_slug` (`:6572`), `SpawnPane`'s orchestration branch (`:8190`), `render_new_pane_orchestration_guard_to_buffer`
- `src/worktree_reclaim.rs` — `OWNER_MARKER_FILENAME`, `mark_worktree_owned`, `ownership_of`
- `src/issue_dispatch_run.rs` — `create_worktree` / `create_worktree_sync`
- `src/agent_pty.rs` — `mint_orchestration_id` (`:383`)
- `tests/CATALOG.md` — `orchestration/worktree/*`, `worktree/reclaim/*`

## Risks and Mitigations

- **A wedged daemon becomes obstructive.** Mitigated by resolving timeout and transport error differently — the recoverable case warns rather than blocks.
- **Existing markers are empty files.** If `ownership_of` required parseable content, every existing deck worktree would silently flip to `Foreign` and `reclaim` would stop auto-removing anything. Mitigated by keeping the presence check authoritative and treating unparseable content as unknown provenance. **This needs its own test** — it protects every worktree created before this ships.
- **Non-git projects lose concurrent orchestration entirely.** Accepted deliberately: no worktree can isolate anything there, so permitting it would only preserve the collision. Mitigated by documentation and an explicit on-screen reason naming the workaround.
- **Removing a working flow with no flag.** Accepted per decision 6. Mitigated by the changelog fragment and by the pre-filled slug making the new path one keystroke.
- **Provenance drifting into a decision input later.** The exact failure PRD #421 warned about. Mitigated by stating it as a success criterion that grep can check.

## Open Questions

- **What makes `N` taken** — an existing directory, a registered git worktree, or a live orchestration? The interesting case is a directory left behind by a closed tab, since fork #122 never removes worktrees. Leaning toward "an existing path", because that is what `git worktree add` itself will refuse.
- **Should `reclaim` spare a live orchestration's worktree?** **Recommended: no.** That is provenance becoming a decision input, which is exactly what decision 1 rests on rejecting. The honest signals are the ones `decide()` already uses — unmerged PR, dirty tree. Recorded so it is not silently revisited.
- **Does the block belong on the dispatcher path too?** Issue-dispatch already creates a per-issue worktree, so it is likely unaffected — but PRD #220's `dispatch` verb will create user-driven units, and the same rule should probably apply there. Settle when #220 moves.
