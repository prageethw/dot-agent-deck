# PRD fork#256: Route the mode-seed path through the phase machine, and delete the third spelling of "landed"

**GitHub Issue**: [fork #256](https://github.com/prageethw/dot-agent-deck/issues/256)

**Priority**: High

**Status**: Planning — **blocked on [fork #254](https://github.com/prageethw/dot-agent-deck/issues/254)**, see Sequencing.

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

- [ ] `process_pending_seed_prompts` uses `DeliveryPhase` / `delivery_phase()` / `reset_delivery_cycle()`.
- [ ] `UiState::seed_delivery_landed` is **deleted** — struct field, initialiser, take/restore. `grep seed_delivery_landed` returns nothing.
- [ ] The keyspace difference is resolved explicitly. The `HashSet` is keyed by pane id string while the phase machine is keyed by tab; state which is authoritative and why, rather than mapping between them at each call site — a mapping layer is a fourth representation wearing a disguise.

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

1. `grep -rn seed_delivery_landed src/` returns **zero** hits.
2. The number of open-coded "landed vs confirmed" spellings goes **down**, from three to two.
3. A genuinely lost mode-seed write is retried and, if still unconfirmed, reported delivered-unconfirmed rather than silently reclaimed.
4. `prompt/pane-input/024` still passes unmodified.

## Key Files

- `src/ui.rs:2076` — `DeliveryPhase`
- `src/ui.rs:2114` — `delivery_phase()`
- `src/ui.rs:2134` — `reset_delivery_cycle()`
- `src/ui.rs:2679`, `:2854` — `seed_delivery_landed`, the field to delete
- `src/ui.rs:3787-3947` — `process_pending_seed_prompts`, including the take at `:3811` and restore at `:3947`
- `src/ui.rs:2640`, `:3865` — the two honest comments to update

## Sequencing

**Blocked on fork #254.** A confirmation check is only as sound as the signal behind it, and for wrapper-strategy agents that signal is currently LEVEL, which cannot return false on a genuinely lost write. Extending confirmation to the mode-seed path today would spread a **known-unfalsifiable** check to a second path — mechanising the defect rather than fixing it.

**fork #257 is not a blocker but should land first** — its overridable retry floor is what lets M2's parity tests drive the mode-seed retry branch deterministically.

Order: **#257 → #254 → #256.**

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
