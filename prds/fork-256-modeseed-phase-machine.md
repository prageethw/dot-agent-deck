# PRD fork#256: Route the mode-seed path through the phase machine, and delete the third spelling of "landed"

**GitHub Issue**: [fork #256](https://github.com/prageethw/dot-agent-deck/issues/256)

**Priority**: High

**Status** *(corrected 2026-08-17, third correction the same day — see below)*: **Close as resolved. `main`'s actual behavior already satisfies M2's closing criteria — but not for the reason the previous version of this section claimed, and that wrong reason was suppressing a real, unresolved question.**

Earlier today this section read "M1 complete and verified in source (PR #270, `436b745f`); M2 unblocked and starting now" — wrong, caught by the tester delegated to write M2's RED test rather than assumed correct. The next correction attributed the gap to "fork#365 superseded fork#256's machinery" — also wrong, caught independently by both reviewer and auditor on this PR. **The corrected chain, verified by pickaxe against the root checkout:**

- `DeliveryPhase`/`delivery_phase()`/`reset_delivery_cycle()` were introduced by PRD fork#197's commit `619d0f60`.
- PR #270's M1 work (`3f306354`, `436b745f`) built on top of that.
- **The 2026-08-15 upstream sync dropped all three commits.** `619d0f60`, `3f306354` and `436b745f` are not ancestors of current `main`'s HEAD. A commit carrying PR #270's *merge message* is an ancestor (`8fbc9470`) — the sync/rebase process recreated the merge commit but its tree lost the source changes; only the doc-only half of that merge survived.
- **This is already documented, and finding it required a repo-wide grep, not a `src/`-scoped one** — `grep -rn "DeliveryPhase\|..." src/` (the check the previous correction ran) returns zero hits and looks conclusive, but it excludes the one file that explains why: `docs/develop/fork-sync-workflow.md:268` records this drop as "DROPPED 2026-08-15 — correctly for one half, wrongly for the other," and `:300` says the sync "kept upstream's new confirmation model but lost the fork's `DeliveryPhase`-aware re-arm eligibility and per-cycle reset." **This is a CLAUDE.md rule 24 event — a fork feature lost in a sync, which rule 24 treats as a defect by default, not as supersession** — and it is not this PRD's first time causing damage: the same commit-drop already silently reintroduced a duplicate-prompt defect once before, restored separately as `e26982ba` (issue #194). Nothing indicates the rest of that 2026-08-15 sync has been audited for further losses. **Tracked as its own follow-up, not solved here:** [#443](https://github.com/prageethw/dot-agent-deck/issues/443), auditing the rest of that sync for other silently-dropped fork commits of the same shape.

**Separately — and this is the reason closing is still correct despite the above** — `main`'s current behavior, arrived at through different, later, unrelated work (not through fork#365 "replacing" anything; no single commit does what the previous correction claimed), already meets M2's actual closing bar. Verified independently by reviewer and auditor, not merely asserted: `process_pending_seed_prompts` (`src/ui.rs:3630`) and `deliver_orchestrator_prompt` (`src/ui.rs:4693`) both call the same `schedule_unconfirmed_retry` (`:3920`, `:4989`) on the same backoff schedule, run the same `ConfirmationCapability` three-way match in the same order (confirmation checked before backoff on both), and both finalize their deadline honestly (`log_prompt_unconfirmable`/`log_prompt_abandoned`, never a silent drop). Both paths now read `ui.prompt_delivery`/`ui.send_retry_backoff` — **one shared representation, not three** — which is #256's actual closing condition, satisfied without a rename (the anti-gaming clause under Success Criteria is honored). This is already tested: `pane_input_025_unconfirmed_retry_deadline_and_late_readiness` (`src/ui.rs:35901`) drives *both* delivery functions in one test, and `pane_input_024_seed_write_is_provisional_until_confirmation` (`src/ui.rs:34631` — its current scenario is confirmation/retry-state, not the readiness buffer this PRD's text elsewhere describes it as; the catalog has moved since this PRD was written).

**The one documented difference between the two paths is deliberate, not a gap** (independently judged, not just quoted): `seed_result_is_terminal` (`:4620`) vs. `is_terminal_send_result` (`:4592`) differ on whether a pre-write `WrongSession`/`Unknown` is terminal. The asymmetry is confined to the pre-write case — nothing is bound yet, so a refusal there is an ordinary non-delivery, with an asymmetric consequence (the orchestrator's abandon path also finalizes tab role state, which the seed path has no equivalent of). `Ambiguous` is terminal unconditionally on both, and the safety-critical post-write half is identical, pinned by `pane_input_028`/`pane_input_006`.

**Accepted residual, not fixed here:** the two paths' parity is behaviorally present but not pinned as an *invariant* — `pane_input_025` asserts each path separately rather than asserting they can't diverge, so a future refactor could re-split them with every existing test still green. This is exactly the recurrence class the #188 → #197 → #256 lineage keeps predicting; recording it rather than fixing it, since fixing it is a new, small, separate change, not part of closing this PRD.

**Recommendation: close #256 as resolved by current `main`'s actual behavior.** #182 (the parent) is already closed — do not re-close it. No further code work belongs on this PRD's branch. M3/M4 (comment updates at now-nonexistent line numbers, changelog, upstream offer) are moot — there is no behavior change on this branch to document or offer.

Round 1 shipped a **rename** rather than a migration and was caught the same day; see the note under Success Criteria for what the gameable criterion was and why it mattered.

**Parent**: [fork #197](https://github.com/prageethw/dot-agent-deck/issues/197), merged as PR [#219](https://github.com/prageethw/dot-agent-deck/pull/219). This PRD carries the half of fork [#182](https://github.com/prageethw/dot-agent-deck/issues/182) that M2 did not ship.

**Related**: fork [#188](https://github.com/prageethw/dot-agent-deck/issues/188) (the "landed vs confirmed" naming thesis this regresses against) · fork [#254](https://github.com/prageethw/dot-agent-deck/issues/254) · fork [#257](https://github.com/prageethw/dot-agent-deck/issues/257) · [upstream #424](https://github.com/vfarcic/dot-agent-deck/issues/424)

**Fork-only?** **No** — `src/ui.rs`'s delivery machinery is upstream code. Offer upstream per rule 19.

## Problem Statement

fork #182 was *"the mode-seed prompt path still assumes 'bytes written' means 'delivered' and bypasses the readiness buffer"* — **two** defects. PRD fork#197's M2 fixed **one**.

**Shipped, and genuinely good:** the 10 s no-`SessionStart` fallback now honours `SPAWN_TIME_READINESS_BUFFER` instead of firing with a zero buffer (`src/ui.rs:3846`), pinned by `prompt/pane-input/024`. A return keystroke could previously be sent while the agent was still starting — exactly the window in which a return does not submit.

**Not shipped, and the reason this PRD exists:** the mode-seed path still has **no confirmation check and attempts no retry**.

| `Applied`/`Queued` case | before (`main`) | after M2 |
|---|---|---|
| write attempts | 1 | 1 |
| retry after a landed write | none | none |
| confirmation check | none | none |
| a genuinely lost write | silently dropped | silently **retained**, then reclaimed at 60 s |
| tracking entries | removed immediately | removed at 60 s |

**The delivery outcome is identical.** A lost mode-seed prompt is still lost — now held for a minute and logged on the way out. The code is honest about this in its own comments (`src/ui.rs:2640`, `:3865`); it was the PRD and changelog text that claimed otherwise, and both have since been corrected.

### The structural problem, which matters more than the missing feature

PRD fork#197 justified folding four issues into one PRD on this basis:

> **One implementation, two callers.** The mode-seed path and the orchestrator path **share the phase machine**. A fix applied once is applied everywhere — the property that failed for #182.

**That is not what shipped.** `DeliveryPhase` (`src/ui.rs:2076`), `delivery_phase()` (`:2114`) and `reset_delivery_cycle()` (`:2134`) are used **only** by the orchestrator path. `process_pending_seed_prompts` (`:3787`) touches none of them — it has a third, parallel representation of "landed", `seed_delivery_landed: HashSet<String>` (`:2679`), with its own lifecycle, keyspace and reclaim rule.

fork #188 counted **two** open-coded spellings of "landed vs confirmed" and predicted a fourth instance of the same bug class. **There are now three.** The recurrence #188 exists to stop is one step further away, not closer — and the entire argument for one PRD instead of four was that this would not happen.

So the work is **not** "add a confirmation check to the mode-seed path". Bolting a fourth check beside the third `HashSet` would satisfy the issue title and deepen the actual defect.

## Solution Overview

**Route `process_pending_seed_prompts` through the existing phase machine, and delete `seed_delivery_landed`.**

The confirmation check is then **inherited**, not re-implemented — which is the property that was claimed and not delivered. Concretely:

1. `process_pending_seed_prompts` reads and writes `DeliveryPhase` via `delivery_phase()` / `reset_delivery_cycle()` rather than `std::mem::take`-ing its own `HashSet` at `:3811` and restoring it at `:3947`.
2. `UiState::seed_delivery_landed` is **removed from the struct**, not merely left unread. A field that still exists is a field the next change re-uses.
3. The mode-seed path gains retry-on-unconfirmed on the **same terms** as the orchestrator path — same grace period, same one-retry-per-cycle budget, same bare-CR mechanism, same deadline and same delivered-unconfirmed terminal report.

**The success test is a deletion, not an addition.** If `seed_delivery_landed` still exists when the work is done, the issue is not closed however many confirmation checks were added.

## Scope

### In Scope

- `process_pending_seed_prompts` migrating onto `DeliveryPhase`.
- Deleting `UiState::seed_delivery_landed` and its initialiser (`:2854`).
- Mode-seed retry, confirmation and terminal reporting reaching parity with the orchestrator path.
- Updating the two comments (`:2640`, `:3865`) that currently, correctly, describe the absence this PRD removes.
- fork #182 closing **fully**.

### Out of Scope

- **The confirmation signal itself** — fork #254. This PRD consumes whatever signal exists; it does not invent one.
- **The retry-floor override** — fork #257.
- **Merging the orchestrator path's own two spellings.** fork #188 counted two before this one appeared; collapsing those is its own change. This PRD must not *increase* the count, and reducing three to two is its measurable contribution.

## Milestones

### M1 — migrate, then delete

**These checkboxes describe work that shipped on PR #270 and was then lost to the 2026-08-15 sync (see Status) — left `[x]` as a historical record of what was verified true on 2026-08-14/15, not a claim about current `main`. `git grep` for any of the cited identifiers on `main` today returns nothing.**

- [x] *(historical — lost in sync)* `process_pending_seed_prompts` uses `DeliveryPhase` / `delivery_phase()` / `reset_delivery_cycle()`. — Verified true as of PR #270's merge: `src/ui.rs:3980` matched on `delivery_phase(true, deliveries.get(&sp.pane_id).is_some_and(|d| d.landed))`.
- [x] *(historical — lost in sync)* `UiState::seed_delivery_landed` was **deleted** — struct field, initialiser, take/restore — and **not replaced by an equivalent under another name**. — Verified true as of PR #270's merge: `git grep 'seed_delivery_landed\|seed_prompt_landed' src/` returned only comments explaining why round 1's rename was rejected, no live field.
- [x] *(historical — lost in sync)* The keyspace difference is resolved explicitly. The `HashSet` is keyed by pane id string while the phase machine is keyed by tab; state which is authoritative and why, rather than mapping between them at each call site — a mapping layer is a fourth representation wearing a disguise. — Resolved (2026-08-13, see the note below): pane id is authoritative.

  **Resolved (2026-08-13): pane id is authoritative, and the tab-less constraint behind it is real.** Verified by the reviewer at `src/ui.rs:10083-10100`: the plain dashboard-card enqueue constructs a `PendingSeedPrompt` from `new_id` after switching to the dashboard, with **no orchestration tab id in play at all** (the mode-tab enqueue at `9995-10016` is a distinct site). So a pane→tab bridge would be invented state, exactly as this milestone forbids.

  **But that constraint rules out a mapping layer; it does not license a second store.** The reviewer's words: *"A pane id being authoritative is real, but it only rules out a tab-id mapping; it does not justify retaining a second landed-state implementation."* The resolution is therefore to make the **shared** phase representation capable of pane-keyed cycles — one store that both paths use — not two stores agreeing on a classifier.

### M2 — parity

- [ ] A landed-but-unconfirmed mode-seed write is retried on the same terms as the orchestrator path.
- [ ] The deadline finalises with the same delivered-unconfirmed honesty, never as a silent drop.
- [ ] `prompt/pane-input/024`'s readiness-buffer guarantee is **unchanged** — it is the half that already shipped and works, and a regression there would trade a fixed defect for a fixed defect.

### M3 — close the parent honestly

- [ ] fork #182 closes fully, with both halves named.
- [ ] The comments at `:2640` and `:3865` are updated to describe what the code now does.
- [ ] Changelog fragment states the behaviour change: a lost mode-seed prompt is now retried, where before it was retained and reclaimed.

### M4 — offer upstream

- [ ] Branch from `upstream/main` and open the PR there.

## Success Criteria

1. **There is ONE store of "landed", shared by both paths** — not two stores behind one classifier. Concretely: no pane-keyed `HashSet` exists alongside the phase machine's own state, under any name.
2. The number of open-coded "landed vs confirmed" spellings goes **down**, from three to two.

> **Criterion 1 was rewritten on 2026-08-13, because the original was gameable and got gamed by accident.** It read *"`grep -rn seed_delivery_landed src/` returns **zero** hits"* — which a **rename** satisfies. That is what happened: the field became `seed_prompt_landed`, still a pane-keyed `HashSet<String>` with its own take/insert/restore/clear lifecycle, and the grep passed.
>
> The coder reported the rename prominently and argued for it rather than quietly clearing the bar, which is why it was caught immediately. Independent reviewer and auditor rounds then reached the same verdict without conferring. The reviewer put it best: `delivery_phase(true, landed.contains(...))` with a fixed `true` is *"semantically the old `if landed.contains(...) { return true; }` with an enum wrapper"* — and the unreachable `Idle`/`Finalized` arms prove the call cannot exercise the phase machine's meaningful distinction at all.
>
> **The lesson is about the criterion, not the work.** A success criterion phrased as the absence of an identifier tests the *name*; the thing that mattered was the *shape*. Criterion 1 now names the property. `grep` is still a fine check — it is just not the definition.
3. A genuinely lost mode-seed write is retried and, if still unconfirmed, reported delivered-unconfirmed rather than silently reclaimed.
4. `prompt/pane-input/024` still passes unmodified.

## Key Files

**Stale — describes the M1 implementation lost to the 2026-08-15 sync (see Status), not current `main`.** `:2076` is now an unrelated comment and `:2114` is `selected_index`; none of the identifiers below exist in `src/` today. Left here only as a historical record of what the original M1 touched, not as a guide for any further work — see the Status section for the actual current-`main` locations (`process_pending_seed_prompts` at `:3630`, `deliver_orchestrator_prompt` at `:4693`, `schedule_unconfirmed_retry` at `:3920`/`:4989`).

- `src/ui.rs:2076` — `DeliveryPhase` *(gone)*
- `src/ui.rs:2114` — `delivery_phase()` *(gone)*
- `src/ui.rs:2134` — `reset_delivery_cycle()` *(gone)*
- `src/ui.rs:2679`, `:2854` — `seed_delivery_landed` *(gone)*
- `src/ui.rs:3787-3947` — `process_pending_seed_prompts` as it existed pre-sync-loss
- `src/ui.rs:2640`, `:3865` — comments referenced by the original M1 scope

## Sequencing

**Moot — this PRD is closing, not proceeding to M2.** This section previously argued M2 was "blocked on fork #254" (an unfalsifiable-confirmation premise that no longer holds — see fork#254's rescoping comment, 2026-08-17, and PRD `prds/254-confirmation-capability-defer-to-hook-outcome.md`), then briefly argued M2 was "unblocked and can proceed independently." Both were reasoning about work that turns out not to be needed: see the Status section above — `main`'s current behavior already satisfies M2's closing bar through unrelated later work, so there is no M2 implementation left to sequence.

For the historical record: fork#254's actual remaining scope (a `ConfirmationCapability` latch resolved from agent type before hook install/trust is known) is a narrow, orthogonal gap that never bore on whether TEXT confirmation is sound for this path — so the original "blocked on #254" reasoning was already wrong on its own terms, independent of the sync-loss/closing conclusion above. fork #257 is closed.

## Rule 12 — cross-version contract

This changes when and how often the TUI writes into a pane, and adds a retry where none existed. It touches no frame shape, so **no `PROTOCOL_VERSION` bump is expected** — but it is a genuine behaviour change on a hook-adjacent path, so the **manual cross-version run is required** and its result recorded here before the PR. Isolate `DOT_AGENT_DECK_LOG` along with the sockets, `HOME` and the state dir.

Answer the question explicitly rather than inheriting fork#197's answer: that PRD's classification covered the mechanism it shipped, and this one ships the part it did not.

## Risks and Mitigations

| Risk | Mitigation |
|---|---|
| The migration is done by adding a check beside the `HashSet` rather than replacing it | M1's success criterion is a `grep` returning nothing. Deletion is the deliverable. |
| Mode-seed retry duplicates a prompt on agents where a bare CR behaves differently | Same mechanism, same accepted risk as the orchestrator path — and fork #257's suppressed-CR test is what turns that from accepted-and-unverified into proven. |
| The keyspace mismatch (pane id vs tab) is bridged instead of resolved | Called out as an explicit M1 item. A mapping layer is a fourth representation. |
| Regressing the readiness-buffer half that already works | `prompt/pane-input/024` must pass **unmodified**; changing that test to accommodate this work is itself the alarm. |
