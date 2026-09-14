# PRD fork#777 — Restore PRD #544's plain `<repo>-<name>` workspace naming; disambiguate only on a genuine provisioning-time collision

**Issue:** [prageethw/dot-agent-deck#777](https://github.com/prageethw/dot-agent-deck/issues/777)
**Priority:** High
**Status:** Drafted, not yet implemented.
**Predecessors:** PRD [fork#544](https://github.com/prageethw/dot-agent-deck/issues/544) ("named, persistent orchestrator workspaces") introduced the plain naming this PRD restores. Issue [#607](https://github.com/prageethw/dot-agent-deck/issues/607) ("two different directories with the same suggested name still share one physical clone") is the regression this PRD fixes properly rather than reverting outright. PRD [fork#760](https://github.com/prageethw/dot-agent-deck/issues/760) churned on naming ergonomics (a typed "Worktree slug" field, added then reverted via PR #771) without ever touching this actual defect.
**Related:** `docs/develop/shared-clone-architecture.md` (the design record both #544 and this PRD update).
**Fork-only?** No — `src/ui.rs`'s New Pane spawn path and `src/issue_dispatch_run.rs`'s provisioning path are upstream code, same lineage as #544. Offer upstream per rule 19 once shipped.

## Problem Statement

PRD #544's Design §1 specified workspace directory naming as `<root-checkout-basename>-<sanitize_clone_segment(name)>` — picking repo `im` with Name `features` gave the folder `im-features`. No subpath, no prefix, for either a toplevel or a nested (subdirectory) pick.

Issue #607 found a real bug in that scheme: two different picks could sanitize to the same segment and silently share one physical clone, mixing branch/checkout state across sessions. Concretely:

1. **Name uniqueness is live-only, not durable.** `orchestration_name_claims` (`src/agent_pty.rs`) is an in-memory map; `release_orchestration_name` fires on `StopAgent`. Close an orchestration named `features`, later open a new one also named `features` against a *different* subdirectory of the same repo — no uniqueness violation, but the same segment, a different actual target.
2. **Name uniqueness checks the raw typed string, not the sanitized segment.** `claim_orchestration_name`'s own doc comment gives the example: `"fix/544"` and `"fix-544"` are different raw Names, both independently pass live uniqueness, and both sanitize to the identical segment `fix-544` — while simultaneously live.

Issue #607's fix, `disambiguate_workspace_segment` (`src/ui.rs:10341`, commit `8fef97a0`, PR #751), closes this correctly but unconditionally: for **any** nested pick, it always prepends `{segment.len()}-` and appends the sanitized subpath, whether or not a collision would ever actually occur. A repo `im`, Name `features`, subdirectory `baseline/intent` now always produces `im-8-features-baseline-intent` instead of #544's plain `im-features` — even though, in the overwhelming majority of picks, no other orchestration is or ever will be contending for that same segment.

PRD #760 attempted to address the resulting naming-ergonomics complaints (a restored, optional "Worktree slug" field, PR #761) but was reverted 2026-09-13/14 (PR #771) per the maintainer's own direct call, and never actually touched this regression — the length-prefix/subpath join applies unconditionally regardless of which field feeds the segment, so #760's whole back-and-forth left the actual defect untouched. Issue #763 even found the prefix still applied to a *typed* slug, defeating the point of that field, which was part of what led to the revert.

No existing open issue or PRD proposes the actual fix: making disambiguation conditional on a genuine collision, detected at the point machinery already exists to detect it, instead of unconditional for every nested pick.

## Decisions

| Question | Direction | Why |
|---|---|---|
| Does the default naming scheme change? | **Yes — reverts to PRD #544's plain `<root-checkout-basename>-<sanitize_clone_segment(name)>`**, for both toplevel and nested picks. `disambiguate_workspace_segment`'s unconditional length-prefix/subpath join is no longer applied up front. | Matches #544's original, reviewed design. The subpath/prefix machinery was never a deliberate ergonomics tradeoff — commit `8fef97a0`'s own message and issue #607's discussion never consider or reject a collision-only alternative; it just wasn't discussed. |
| Where does collision detection move? | **Into the provisioning transaction** (`provision_isolated_clone_sync_resolved`, `src/issue_dispatch_run.rs`), inside the same attach-race lock already used for the resume path — not as a separate pre-provisioning pure function. | Naming and provisioning happening as two independent steps (pure-name-then-provision) is exactly what created the TOCTOU concern that made #607's fix go unconditional in the first place. Doing the check inside the lock that already exists for resume-safety closes that concern without inventing new locking. |
| How is "genuine collision" distinguished from "safe to resume"? | Reuse `isolated_clone_ancestry_matches_source` and the provenance marker's `creator=`/`name=` fields — exactly what `resume_existing_isolated_clone` already uses to decide `Resumed` vs. `NameCollision`/`AncestryMismatch`. If the plain-named folder that already exists is *this* pick's own prior clone (same repo, same subpath) → resume as today, unaffected. If it belongs to a genuinely different pick → **that's** the collision, and only then does the disambiguated form get computed and (re-)attempted under the same lock. | This is the exact machinery #544 M3/M4 built for resume-safety and #607 partially reused for `resume_existing_isolated_clone` — extending it to also gate the *initial* naming choice, rather than adding a second, different collision-detection mechanism. |
| Does the length-prefix/subpath join itself change? | No — `disambiguate_workspace_segment`'s actual join format is untouched and still used, just moved from "always" to "only on confirmed collision, as a retry." | The join format itself was never the problem; when it fires is. No reason to redesign a working format. |
| Any retry-loop / infinite-fallback risk? | No new one. On a confirmed collision, exactly one retry with the disambiguated name is attempted under the same held lock; a further real collision on the disambiguated name itself (already effectively impossible today, since #751 made the join length-prefixed/injective) surfaces as an ordinary `Rejected`, same as any other unresolvable provisioning failure today. | The disambiguated name was already designed to be collision-resistant (#751); this PRD doesn't need to make it more so, only decide when it's computed. |
| Does this touch Name uniqueness (`ClaimOrchestrationName`) itself? | No. Both gaps in Name uniqueness identified in the Problem Statement (live-only scope, raw-string-not-sanitized-segment check) are accepted as-is — they're what *causes* a possible collision, not what this PRD is fixing. This PRD only changes how a collision, once it occurs, is handled. | Tightening Name uniqueness (e.g. scoping it to the sanitized segment, or making it durable across closes) is a separate, larger design question with its own tradeoffs (does a closed orchestration's Name become permanently unavailable forever?) and isn't needed to fix the naming regression this PRD targets. |

## Design

1. **Naming.** `resolve_workspace_path` (or its current equivalent call site in `src/ui.rs`) stops calling `disambiguate_workspace_segment` unconditionally. The segment passed into provisioning becomes plain `sanitize_workspace_segment(name)` for both toplevel and nested picks.
2. **Provisioning-time collision check.** `provision_isolated_clone_sync_resolved` (`src/issue_dispatch_run.rs`), after acquiring its existing attach-race lock and finding the plain-named target directory already exists, runs the same ancestry + provenance-marker identity check `resume_existing_isolated_clone` already performs. Same identity → proceed to resume, unchanged. Different identity → compute `disambiguate_workspace_segment`'s form for this pick's actual `(segment, subpath)`, and retry provisioning under the disambiguated name, still inside the same lock acquisition (no lock release/reacquire race window).
3. **Existing collision-safety machinery.** `disambiguate_workspace_segment` (#607/#751), the symlink-toplevel guard (#595), and the resolved-workspace-path-qualified `creator` identity all stay exactly as they are — this PRD changes only *when* the first of these is invoked, not its own logic or the other two at all.
4. **Migration for existing on-disk workspaces.** A workspace already sitting at a disambiguated path (e.g. `im-8-features-baseline-intent`, created before this change) is not renamed automatically — renaming a live git worktree out from under any in-flight session is its own hazard, out of scope here. It remains resumable at its existing disambiguated path (the resume-matching logic keys off ancestry/provenance, not path shape) until explicitly retired via the existing "forget this workspace" action, after which a fresh pick for the same target produces the plain name going forward.
5. **Rule 12 check.** No wire/protocol field changes — this is entirely local naming/provisioning logic. No `PROTOCOL_VERSION` bump, no `.breaking.md` fragment expected; confirm during implementation.

## Milestones

- [ ] **M1 — RED.** Tests pinning: (a) a fresh nested pick produces the plain `<repo>-<name>` form, no prefix/subpath; (b) a repeat pick of the identical `(repo, name, subpath)` resumes the same plain-named folder; (c) two picks with different Names that sanitize to the same segment, against different subpaths, correctly detect the collision at provisioning time and the second one lands on the disambiguated form; (d) the disambiguated form itself still resumes correctly on a repeat of *that* pick; (e) an existing on-disk workspace at a legacy disambiguated path remains resumable unchanged.
- [ ] **M2 — GREEN.** Implement per Design above.
- [ ] **M3 — Review/audit.** Delegated `reviewer` + `auditor` passes (CLAUDE.md rule 8), particular attention to the lock-acquisition ordering around the new in-transaction retry (no window where two racers could both compute the plain name, both find it absent, and both create it — verify this is already excluded by the existing lock's placement, per PRD #544 §3).
- [ ] **M4 — Docs.** Update `docs/develop/shared-clone-architecture.md` to describe the corrected model; note in this PRD's Status.

## Out of Scope

- Tightening `ClaimOrchestrationName` uniqueness itself (durable-across-close scope, or checking the sanitized segment instead of the raw string) — see Decisions table; a separate, larger design question.
- Renaming/migrating existing on-disk disambiguated-path workspaces.
- Any change to Name's role as instance identity (PRD fork#192) or to the resume eligibility rules themselves (PRD fork#544 §"Decisions").

## Key Files

- `src/ui.rs` — `disambiguate_workspace_segment` (~`:10341`), `resolve_workspace_path`, the New Pane spawn path.
- `src/issue_dispatch_run.rs` — `provision_isolated_clone_sync_resolved`, `resume_existing_isolated_clone`, `isolated_clone_ancestry_matches_source`, the attach-race lock acquisition.
- `docs/develop/shared-clone-architecture.md` — the design record to update.
- `tests/CATALOG.md` — likely new entries alongside the existing `orchestration/workspace/*` family (PRD #544 M9).

## Risks and Mitigations

- **Reintroducing #607's original bug via a subtle lock-ordering mistake.** **Mitigation:** the collision check must run strictly inside the same held lock the create-vs-resume decision already uses — M3's review explicitly checks this ordering, not just that the feature works on the happy path.
- **A legacy disambiguated-path workspace becoming unresumable.** **Mitigation:** M1(e)/design §4 explicitly keep the old path resumable; nothing renames or migrates it.

Prageeth Warnak
