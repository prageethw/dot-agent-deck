# PRD fork#197: The seed/prompt delivery state machine — name the phases, then fix the three defects riding on them

**GitHub Issue**: [fork #197](https://github.com/prageethw/dot-agent-deck/issues/197)

**Priority**: High

**Status**: **Complete.** *(Status corrected 2026-08-14: this line read "RESUMED and awaiting re-review … PR #219 (draft)" for two days after the PR had merged.)* All four milestones landed. Merged 2026-08-12T23:04:44Z via [PR #219](https://github.com/prageethw/dot-agent-deck/pull/219) (original merge commit `7f925e8a`, superseded by a later fork-sync rebase; the equivalent commit on `main` today is `619d0f60`, confirmed by `git merge-base --is-ancestor`); on `main`, not yet in a release tag. `changelog.d/197.bugfix.md` is present and unconsumed. The park below is retained as the historical record.

**2026-08-15 sync correction**: the "0 occurrences upstream" claim below is stale. `upstream/main` at `58a752b3` implements the SAME contract (`prompt_submission_evidence`/`SubmissionEvidence` in `src/ui.rs`, tested as `prompt/pane-input/023`-`028`) under entirely different symbol names — a grep for this PRD's own identifiers therefore missed it. Upstream's implementation reads the pane's `recent_events` journal directly (never `last_user_prompt`'s single overwritable slot) and never uses this PRD's LEVEL/`Thinking`-status heuristic at all, avoiding the false-positive class this PRD's rounds 2-4 fought. At Stage B of the 2026-08-15 sync this PRD's own code (`619d0f60`, `4252bd30`, `504032e8`, `c4c679e4`, and `9d55559c`'s CATALOG-only portion) was therefore dropped as superseded rather than replayed — see the sync's own `work-done` report for the file-by-file verification. This PRD stays fork-only in this document's status line above only in the sense that it never shipped upstream under its own name; the underlying defect it fixed did.

**Read the 14 before reading anything into them.** `main` itself is red at `66077b2a` and this branch inherits its failing set **byte-identically** — ten `keybindings::tests::*`, `mode_scroll_002`, `remap_003`, `state::tests::a_tagged_frame_…`, `ui::tests::orchestration_011_…`, plus a red `semgrep` (4 findings under `--error`). Tracked as [#255](https://github.com/prageethw/dot-agent-deck/issues/255) and deliberately **not** fixed here. This PR therefore cannot show a green board; the achievable bar is "the same 14 as `main`, and no more", which is what it meets.

**What changed on resume:**

- **The parked blocker is resolved.** `7cd091e` (the LEVEL removal) is reverted. #187 closes **partially** — TEXT is fixed, LEVEL stays — and the LEVEL half is now [#254](https://github.com/prageethw/dot-agent-deck/issues/254), which carries the measurement (the `Thinking` event lands 0–160 ms after the write, i.e. the classifier's boot heuristic rather than a submit) and the contract a replacement must satisfy: **a confirmation must be capable of returning false when the write was genuinely lost.**
- **Four findings were mooted by that revert** and were deliberately NOT actioned, because doing so would have been wrong work rather than merely wasted: reviewer F1 (blocker) and audit F3 are resolved outright; reviewer F6 and audit F1's `seed/004`/`seed/007` items assumed a LEVEL-less tree — with LEVEL restored, session B's `Thinking` is live bait again, `expected_session_id` is load-bearing again, and `seed/004`'s frames test LEVEL's falling edge again.
- **Reviewer F3 is closed with falsifiable evidence**, not a green run: `prompt_text_confirms` now has a direct unit test, and the `>` → `>=` mutation was pushed to prove it bites — CI [31613154740](https://github.com/prageethw/dot-agent-deck/actions/runs/31613154740) RED on exactly that test, [31613905410](https://github.com/prageethw/dot-agent-deck/actions/runs/31613905410) GREEN once reverted.
- **`orchestration/seed/016` passes for the first time.** Its root cause was never the readiness needle: Codex was stuck at its own first-run trust gate, because `import_codex_credentials` seeded `trust_level` against the raw `/var/folders/…` tempdir path while Codex's `getcwd()` reports the resolved `/private/var/folders/…` form. `with_claude_trust_workdir` already guarded this exact bug for Claude and was never applied to Codex. Confirmed by the authorised carve-out (a) run: **`seed/016` PASS (20.71s), `seed/015` PASS (14.69s), `seed/011` PASS (15.07s)**, none skipped.
- **Rebased onto `origin/main`** after the 2026-08-12 upstream sync left the branch `CONFLICTING` (171 ahead / 329 behind). PRD-197 **builds on** upstream #424's machine rather than colliding with it: `main` carries `orchestration_seed_001`–`010`; this branch adds only `seed_012`/`013`/`014`, `pane_input_023`/`024` and a `confirmation_grace_period()` test helper.

**#182 closes PARTIALLY, like #187** *(recorded 2026-08-13 after the re-review; this corrects an earlier claim in this document)*. M2 shipped one of #182's two halves. The **readiness-buffer** half is genuinely fixed — the 10 s no-`SessionStart` fallback now honours `SPAWN_TIME_READINESS_BUFFER` instead of firing with a zero buffer (`src/ui.rs:3846`), pinned by `prompt/pane-input/024`. The **silent-loss** half did not ship: on the `Applied`/`Queued` path the delivery outcome is *identical to `main`* — one write, no retry, no confirmation check — so a genuinely lost mode-seed prompt is still lost, now retained for 60 s and logged rather than dropped immediately. The code says so in its own comments (`src/ui.rs:2650`, `:3865`); it was this document and the changelog that claimed otherwise, and both are corrected.

That also means this PRD's Solution Overview point 2 — *"One implementation, two callers … the mode-seed path and the orchestrator path share the phase machine"* — **did not hold**. `DeliveryPhase` / `delivery_phase()` / `reset_delivery_cycle()` are used only by the orchestrator path; `process_pending_seed_prompts` got a third parallel representation, `seed_delivery_landed: HashSet<String>`. #188 counted two open-coded spellings of "landed vs confirmed" and predicted a fourth instance of the bug class; there are now **three** spellings, so that recurrence is one step further from being stopped, not closer. Carried to [#256](https://github.com/prageethw/dot-agent-deck/issues/256), whose closing condition is that the mode-seed path routes through the phase machine and `seed_delivery_landed` is deleted — not merely that a confirmation check is bolted on beside it. Sequencing note there: it likely wants [#254](https://github.com/prageethw/dot-agent-deck/issues/254) first, since extending confirmation to a second path using today's signals would extend a known-unfalsifiable check.

**Two things a re-reviewer should look at specifically**, because they are new since the last review rather than carried over:

1. **F9's fix is a behavioural change, not a comment.** The *sent* prompt is now truncated to 200 bytes before comparison so it matches the hook-side truncation of the *observed* prompt, making the prefix match reachable for prompts over 200 bytes. The cost: two prompts sharing their first 200 bytes would now confirm each other.
2. **`DOT_AGENT_DECK_LOG` does not reach a harness-spawned deck.** `TuiDeckBuilder` calls `cmd.env_clear()` and forwards only `PATH` plus a pinned list, and no seed test adds it via `.with_env(...)`. Log-based verification of the delivery cycle in real-agent tests is therefore structurally unreachable without a harness change — which is why the carve-out run could not answer whether a confirmation-retry fired.

## Decisions taken

The first two were left open by this document for the implementer to choose. The user has decided them, so they are no longer open questions and the implementer should not re-litigate either. The third and fourth were decided during implementation.

- **M3 → a submission event, not a per-cycle nonce.** Confirmation compares against a *submission event* rather than diffing `last_user_prompt`'s value. The nonce alternative is rejected on exactly the ground this PRD's own Risks section raises: the remit pointer is user-visible text in the pane, and a varying token in it is a UX regression.
- **M4 → submit-only, not clear-then-rewrite.** A confirmation-retry sends a bare CR rather than text plus CR, since `Applied` already means the bytes reached the PTY. This avoids inventing an agent-agnostic composer-clear primitive that claude/opencode/codex/pi do not share.

  **This makes the lost-write risk live, not hypothetical**, which the Risks section anticipated: if the bytes were genuinely lost downstream, a bare CR does nothing and the deadline finalizes as *delivered-unconfirmed* — the exact silent-loss symptom upstream #424 exists to catch. So extending `orchestration/seed/005` for the lost-write half is **mandatory** under this choice, not optional, and it is the single most important thing for review and audit to probe.

  It also keeps the rule 12 answer unchanged: submit-only needs no new pane RPC, so no `PROTOCOL_VERSION` bump and no cross-version manual run. Had clear-then-rewrite been chosen, the composer-clear primitive would have made this a contract change. **Re-answer if the implementation ends up needing a new RPC anyway.**

- **M4's feasibility question is answered: a bare CR rides the existing pane API — no new RPC.** The probe traced `write_and_submit_to_pane_with_identity("")` end to end: `encode_pane_payload("")` returns an empty vec (already pinned by `encode_pane_payload_empty`, `src/pane_input.rs:156-161`), the zero-length write loop never issues a syscall, and execution falls through to `SUBMIT_DELAY` then `\r` — mechanically identical to a real Enter keypress on a composer that already holds text. The delivery ledger needs nothing new: the confirmation-retry branch already mints a fresh `delivery_id` per attempt, so it never reaches `admit_delivery`'s replay path. **M4 is therefore a pure call-site change** in `deliver_orchestrator_prompt`: pass `""` instead of the prompt text when `awaiting.is_some()`. The rule 12 answer above stands — no bump, no fragment, no cross-version run.

  The probe also corrected this document: the *"`prompt_text_confirms` rejects empty strings"* obstacle **conflated two different strings**. The text sent on one write attempt (which M4 makes empty) and the text remembered for confirmation matching (`orchestrator_prompt`/`sent_prompt`, which stays full-text for the cycle's duration) are already separate variables. M4 does not need to unify them, and the empty-string guard never fires on this flow.

- **M4's real-agent e2e targets Claude Code and Codex only** (user decision, superseding the OpenCode decision recorded below). OpenCode and Pi are explicitly **out of scope** for this PRD's verification. The rationale for picking these two is that they are exactly the harnesses whose CR-as-submit behaviour the codebase already documents (`src/agent_pty.rs:3566`, `:3668`), so the tests confirm documented behaviour rather than probing an undocumented one.

  **What this leaves open, stated plainly rather than implied:** M4's submit-only retry sends a standalone CR to *whatever* harness a role runs, since the write path is agent-agnostic by construction (`b"\r"`, no `AgentType` branch). If a standalone CR does not submit on OpenCode or Pi, the confirmation-retry is a silent no-op there. That is an accepted, documented risk — not a verified-safe path.

  *(Superseded — retained for the reasoning, which still explains why this was investigated at all.)* **M4's real-agent e2e covers OpenCode as well as Claude** (earlier user decision, reversed above after the carve-out run found the OpenCode CLI absent on the dev machine). The probe surfaced a risk this document had not recorded: the write path is agent-agnostic by construction — `b"\r"` is written with no branch on `AgentType` — but the codebase documents CR-as-submit only for **claude and codex** (`src/agent_pty.rs:3566`, `:3668`), and `SUBMIT_DELAY`'s 150 ms is documented as empirically tuned against *claude* (`src/pane_input.rs:77-82`). Nothing documents how **opencode** or **pi** treat a *standalone* CR arriving ~2 s after a separate write — a different case from the CR fused to its own payload, which is what demonstrably works today. If a standalone CR does not submit on those harnesses, M4 fixes duplicate delivery on Claude while making the retry a **no-op** there, which is arguably worse than the duplication it replaces.

  **Consequence to plan around, not a formality:** OpenCode is one of the credential-gated e2e files that **self-skip in CI**, so this test will report green there having executed nothing — the empty-gate-versus-passed-gate trap of CLAUDE.md rule 8. It proves something only under an orchestrator-authorised **carve-out (a)** local run, filtered to the real-agent files, before `/prd-done`. Report that run's actual result; a green board is not evidence here. Pi remains an accepted, documented unknown.

**Fork-only**, and currently intended to stay so. Verified mechanically against `upstream/main`: `orchestration_awaiting_confirmation`, `CONFIRMATION_GRACE_PERIOD`, `prompt_text_confirms` and `AwaitingConfirmation` all have **0** occurrences there. Upstream issue #424 is still OPEN — upstream carries the original problem and never received the fix, so there is nothing there to fix. This becomes an upstream offer only if `46b64f0` is eventually contributed, which is better done with this PRD already folded into it.

**Closes**: fork [#188](https://github.com/prageethw/dot-agent-deck/issues/188) · [#182](https://github.com/prageethw/dot-agent-deck/issues/182) · [#187](https://github.com/prageethw/dot-agent-deck/issues/187) · [#194](https://github.com/prageethw/dot-agent-deck/issues/194)

**Related**: upstream #424 (the confirmation mechanism, fixed here as `46b64f0`, still open upstream) · upstream #423 (the compaction re-assertion that rides this path) · PRD #20 (R20-003/004/005, the delivery identity and retry backoff) · PRD #127 (mode `seed_prompt`s) · PRD #128 (the readiness buffer)

## Problem Statement

**One concept has two implementations, and the phases of neither are expressed in code.**

`worker-agent-deck` delivers an automatic prompt into a freshly-spawned agent's pane in two places:

- `deliver_orchestrator_prompt` (`src/ui.rs:3636`) — the orchestrator role's remit pointer.
- `process_pending_seed_prompts` (`src/ui.rs:3374`) — a mode's `seed_prompt`, PRD #127.

They solve the same problem: wait for readiness, write text plus a submit, decide whether it worked, retry or give up. Issue #424 rebuilt the first one to stop treating *"bytes reached the PTY"* as *"the agent received it"*. **The second was never touched**, so today the two have genuinely different semantics for the same operation:

| | `deliver_orchestrator_prompt` | `process_pending_seed_prompts` |
|---|---|---|
| `Applied`/`Queued` means | write landed; await confirmation | **delivered — seed dropped** (`:3463-3467`) |
| Submit confirmation | LEVEL + TEXT paths, session-scoped | **none** |
| Readiness buffer on the 10 s no-`SessionStart` path | honoured (#424 finding #3, `:3774-3780`) | **bypassed** — `if timeout_ready { true }` (`:3420`) |
| Retry after a landed write | one budgeted, fresh `delivery_id` | n/a — nothing to retry, seed already dropped |

That divergence *is* fork #182. It is not a separate bug that happens to resemble #424; it is #424's own defect, surviving in the copy nobody edited.

### The phases exist, but only in prose

Fork #188 is the structural finding, and it is the reason the other three keep happening. It records that **the same mistake occurred three times inside issue #424 alone** — the delivery ledger (round 2), the confirmation grace period (rounds 3/4), and the re-arm gate (rounds 3/4). Each was an expression that was correct when written and became wrong when the state machine gained a phase. Its author, after the third fix shipped as a *comment*:

> A comment documents that conclusion at one site but does not stop the next person writing the same expression and meaning the other sense — it still compiles and still reads correctly. **I would expect a fourth instance.**

Fork #194 was filed the following day. The retry path re-writes the **full prompt text** into a composer that already holds it, because nothing in the code distinguishes *"the write did not land, try again"* from *"the write landed, do not re-send it"*. That is precisely the distinction #188 says lives only in comments — so #194 is best read as the predicted fourth instance, not as an unrelated defect.

### The confirmation mechanism cannot tell the two apart

Fork #187 closes the loop. Confirmation has two OR'd paths: **LEVEL** (the session became `Thinking`) and **TEXT** (`last_user_prompt` matches what was sent *and* differs from a baseline captured at write time). The baseline guard is correct and was verified sound — but `prepare_orchestrator_prompt` always returns the **same constant pointer text**, so on any second-or-later cycle for a tab the observed prompt equals the baseline and a **genuine** submit is rejected. The TEXT path is structurally dead exactly where #423's compaction re-assertion needs it, leaving re-assertions on LEVEL alone — which the same review established is unsound, because `PostCompact`, `permission.replied` and OpenCode's status catch-all all produce `Thinking` with no submit behind it.

So: **#182 is the silent-loss bug #424 set out to fix, #194 is the silent-duplication bug #424's fix introduced, and #187 is the mechanism meant to tell them apart being unable to.** Three faces of one under-specified state machine.

## Why one PRD and not four PRs

Three of the four touch the same ~300 lines of `deliver_orchestrator_prompt` and would conflict with each other. More importantly, each would have to re-derive the landed-versus-confirmed distinction in prose, which is the mechanism that has already produced four instances. Fixing the naming once and writing the three behavioural fixes in terms of it is less total work and removes the recurrence, rather than adding a fifth opportunity for it.

## Solution Overview

**Name the phases in the type system, unify the two paths behind them, then fix the three behaviours.**

1. **A `DeliveryPhase` the compiler understands.** The four real states are `Idle`, `Armed` (waiting for readiness), `Landed` (bytes reached the PTY, submit unconfirmed) and `Finalized`. The re-arm gate and the retry gate then visibly ask *different questions* instead of both spelling `orchestrator_prompt.is_none()` and meaning different things.
2. **One implementation, two callers.** The mode-seed path and the orchestrator path share the phase machine. A fix applied once is applied everywhere — the property that failed for #182.
3. **A retry that expresses intent.** `Landed` plus "no submit observed" must be able to mean *re-submit what is already there*, not *send it again*. Whether the mechanism is submit-only or clear-then-rewrite is an implementation choice (see M4); what matters is that the phase makes the choice expressible.
4. **Confirmation that works for repeated text.** Identify a *submission event* rather than diffing a value, so byte-identical re-delivery stops being unconfirmable. Once TEXT confirms unconditionally, **delete the LEVEL path** — the round-4 review was explicit that LEVEL is currently load-bearing and must not be removed before then.

### What is deliberately not proposed

No change to `PROTOCOL_VERSION` is anticipated: this is TUI-side delivery logic, and upstream #424's own analysis notes `DelegateSignal` is not involved. **Re-evaluate if M4 needs a new pane RPC** (a composer-clear primitive would be one) — that would make it a rule 12 contract change and require the cross-version manual run.

## Milestones

**M1 — Express the phases (closes #188).** Introduce `DeliveryPhase` and a `reset_delivery_cycle()` helper replacing the three open-coded sites that clear overlapping subsets of per-cycle state by hand. **No behaviour change**; the existing `orchestration/seed/*` catalog must stay green untouched. This is the foundation and lands first.

**M2 — Unify the mode-seed path (closes #182).** Route `process_pending_seed_prompts` through the M1 machine: honour the readiness buffer on the 10 s fallback, and stop treating `Applied`/`Queued` as delivered. Highest user-visible value — this is where the *original* silent-loss bug still lives.

**M3 — Confirmation for repeated text (closes #187).** Make TEXT confirmation fire on a genuine resubmission of identical text, then remove LEVEL. Unblocks #423's re-assertion, which today either confirms on a `PostCompact` that is not a submit (right outcome, wrong reason) or waits out the 60 s deadline.

**M4 — Non-duplicating retry (closes #194).** A retry after a `Landed` write must not deliver the prompt text a second time, **and** a genuinely lost write must still get a real retry. Both halves are required — satisfying one alone regresses the other. Two candidate mechanisms, for the implementer to choose on evidence: *submit-only* (send a bare CR; trivially satisfies the first half, risks the second if the bytes never landed) or *clear-then-rewrite* (correct either way, but needs an agent-agnostic composer-clear primitive that claude/opencode/codex/pi do not share).

Order is load-bearing: M1 first so M2–M4 are written in phase terms. M2–M4 are then independent of each other.

## Testing

Extend the existing `orchestration/seed/*` L1 family (`004`–`010`), which already drives `deliver_orchestrator_prompt` directly against a `SendResultPaneController` with an injected clock — the right harness, already built.

- **M1**: no new tests; the existing family passing unchanged *is* the assertion.
- **M2**: a mode-seed test asserting an `Applied` write is not treated as delivered, and that the 10 s fallback waits out the readiness buffer.
- **M3**: a second delivery cycle on one tab with byte-identical prompt text, asserting a genuine submit confirms. **This test goes RED today** — known-failing behaviour, not a coverage gap.
- **M4**: frame 1 lands an `Applied` write; frame 2, past the grace period with no submit observed, retries **without** the prompt text reaching the pane twice. Extend `orchestration/seed/005` for the lost-write half.

`orchestration/seed/002`'s catalog entry currently calls `SessionStatus::Thinking` "the most direct stand-in" and disclaims being the signal the fix must reconcile against; it should gain a `last_user_prompt` flip alongside the status flip when LEVEL is removed in M3 — matching the stand-in to what ships, not weakening a green test.

All runs go to CI (CLAUDE.md rule 5). A draft PR opens before the first RED push or there is nothing to read.

## Risks

- **M3 changes what the agent sees** if a per-cycle nonce is the chosen mechanism for making repeated text distinguishable. The remit pointer is user-visible text in the pane; a visible nonce is a UX regression. Prefer an event-based signal.
- **M4's clear-then-rewrite needs a primitive that does not exist** and differs per agent harness. If M4 lands as submit-only, record the lost-write risk explicitly rather than leaving it implied.
- **LEVEL removal (M3) is irreversible in one direction** — it is load-bearing until TEXT works unconditionally. Do not remove it in the same commit that changes TEXT; land TEXT, confirm green, then remove.
- **The evidence for #194 is a code-path inference**, not a captured log: `DOT_AGENT_DECK_LOG` was unset when it was observed. M4 should begin by capturing the two-write sequence with logging enabled, which also gives the fix a before/after signal. Isolate the sandbox's log path along with its sockets, `HOME` and state dir — the log path resolves separately and will otherwise append into the real `~/.local/state/dot-agent-deck/deck.log`.

## Out of Scope

- Raising `SPAWN_TIME_READINESS_BUFFER` from 500 ms. It is palliative — it moves the boundary rather than fixing the mechanism. Decide it separately, on evidence.
- The Pi re-assertion path, explicitly out of scope in #423's own implementation notes.
- Offering any of this upstream. See the header — there is nothing upstream to fix today.
