# PRD fork#254: A confirmation signal for wrapper-strategy agents that can actually return false

**GitHub Issue**: [fork #254](https://github.com/prageethw/dot-agent-deck/issues/254)

**Priority**: High

**Status**: Planning

**Parent**: [fork #197](https://github.com/prageethw/dot-agent-deck/issues/197), merged as PR [#219](https://github.com/prageethw/dot-agent-deck/pull/219). This PRD carries the half of fork [#187](https://github.com/prageethw/dot-agent-deck/issues/187) that closed only partially.

**Related**: fork [#194](https://github.com/prageethw/dot-agent-deck/issues/194) (the duplicate-seed bug, same state machine) · fork [#256](https://github.com/prageethw/dot-agent-deck/issues/256) (**depends on this** — see Sequencing) · fork [#257](https://github.com/prageethw/dot-agent-deck/issues/257) (the deterministic retry harness this will want) · [upstream #424](https://github.com/vfarcic/dot-agent-deck/issues/424)

**Fork-only?** **No.** `src/wrap.rs`, the classifier and the confirmation cycle are all upstream code. Offer upstream per rule 19 — see M4.

## Problem Statement

Seed-prompt delivery confirms a prompt actually reached the agent through two signals:

- **TEXT** — `prompt_text_confirms` (`src/ui.rs`), comparing a hook-reported `user_prompt` against the text we sent.
- **LEVEL** — a `SessionStatus::Thinking` sample observed after the write.

**Wrapper-strategy agents have neither in a sound form.**

TEXT is **structurally unavailable**: `emit_with_metadata` hardcodes `user_prompt: None` (`src/wrap.rs:304`) for every event the wrapper emits, so there is never an observed prompt to compare against. That leaves LEVEL, and LEVEL is not a submit signal.

**Codex is the agent this bites, and it is not a corner case** — this fork runs its `reviewer` and `auditor` roles on Codex.

### The measurement, because the mechanism matters more than the symptom

Reproduced across two runs with an isolated `DOT_AGENT_DECK_LOG`:

```
14:02.329  SessionStart  agent_type=Codex
14:03.008  orchestrator prompt: write applied; awaiting submit confirmation   delivery_id=…-2-0
14:03.008  Received event  event_type=Thinking          <- SAME millisecond as the write
14:03.511  orchestrator prompt: write applied…          <- retry, +503ms
15:01.872  WARN deadline reached with a landed write still unconfirmed …      <- +58.87s
```

The `Thinking` event lands in the **same millisecond** as the write, and 0–160 ms after it across runs. That is far too fast to be a response to our prompt — it is the classifier's boot heuristic firing, not evidence that anything was submitted.

So LEVEL "works" for Codex only in the sense that it **reliably returns true**, including when the write was lost. *A confirmation signal that cannot fail is the same as no signal at all.*

This is why `7cd091e` (PRD fork#197 M3's LEVEL removal) had to be reverted: with LEVEL gone, Codex had no confirmation path whatsoever, every delivery waited out the full 60 s deadline and fired a stray CR. LEVEL therefore remains in the tree **as a known-unsound path, not as a solution**.

## The contract this PRD must satisfy

> **A confirmation must be capable of returning false when the write was genuinely lost.**

LEVEL's failure is not that it is imprecise. It is that it is **unfalsifiable**. Any replacement is judged against that one sentence, and the test that proves it must exhibit a genuinely lost write that the signal *declines* to confirm.

## Solution Overview

**Give the Wrapper classifier a real `user_prompt`, sourced from the agent's OUTPUT stream.**

The wrapper already tees the child's output through a pattern-matching layer — `classify_line` / `classify_line_with` against a per-agent `RuleSet`, with `CODEX` already specialised (`src/wrap.rs:229-243`). The work is to recognise the agent **echoing the submitted prompt into its own transcript**, and to emit an event carrying that text as `user_prompt`, so `prompt_text_confirms` starts working for wrapper agents with no new confirmation mechanism at all.

### Why output, and explicitly not stdin

The wrapper also pumps **stdin** (byte-exact raw stdin on the pipe path; stdin → inner PTY on the PTY path), so it can see our own bytes and our own CR go past. **That is not a sound signal and must not be used.** The deck writes into the pane the wrapper runs in, so those bytes pass through the wrapper *whether or not the agent ever accepts them*. A stdin-derived confirmation would reproduce LEVEL's exact defect — always true, including for a genuinely lost write — while looking more principled.

The agent's own echo of the prompt is downstream of acceptance, which is what makes it falsifiable: if the agent never took the prompt, it never echoes it, and the signal correctly returns false.

### Alternatives considered and rejected

| Direction | Verdict |
|---|---|
| A submit-shaped event distinct from `Thinking` | Same destination, more machinery. `user_prompt` on an existing event reuses `prompt_text_confirms` verbatim; a new event type needs a new comparison path, a new wire field and a rule 12 answer. Revisit only if the echo proves unrecognisable. |
| Viewport differencing (composer emptied / transcript grew) | Agent-agnostic but fiddly, and the same class of problem as the composer-clear primitive fork #194 rejected. Keep as fallback. |
| Narrow LEVEL back to a "first Thinking after our write" window | **Rejected outright** — deliberately, and this is the crux. It would confirm on a heuristic *known* to fire on boot. Being narrower does not make it falsifiable. |

## Scope

### In Scope

- A `user_prompt`-bearing event from the wrapper's output classification path.
- The `CODEX` ruleset extension that recognises Codex's prompt echo.
- Truncation parity with the hook path, so `prompt_text_confirms`'s existing 200-byte prefix comparison behaves identically for wrapper agents.
- Tests proving falsifiability, including at least one real-Codex run.

### Out of Scope

- **Removing LEVEL.** That is this PRD's *closing condition*, but it happens only once the replacement is proven — see M3, and do not let it slide forward into M1.
- **The mode-seed path** (fork #256) and **the retry-floor override** (fork #257).
- **Other wrapper agents** beyond establishing that the seam is per-`RuleSet` and Codex is the first consumer. Pi, opencode and any future wrapper agent inherit the seam, not a hardcoded Codex pattern.

## Milestones

### M1 — the seam

- [ ] `emit_with_metadata`'s hardcoded `user_prompt: None` becomes a value the caller can supply. Every existing caller keeps passing `None`, so this milestone alone changes no behaviour.
- [ ] The classification path can carry a detected prompt string from `classify_line_with` through to the emitted `AgentEvent`.
- [ ] Truncation matches the hook side exactly (200 bytes), so the comparison is symmetric — PR #219's F9 made the *sent* prompt truncate to match the *observed* one, and a third truncation rule here would silently break that.

### M2 — the Codex signal

- [ ] The `CODEX` `RuleSet` recognises Codex echoing a submitted user message and emits an event carrying it.
- [ ] **The falsifiability test is the deliverable, not a nicety:** a scenario where the write is genuinely lost, in which the signal returns **false** and the delivery is correctly reported unconfirmed. A test that only shows the happy path confirming does not close this issue — that is precisely what LEVEL already does.
- [ ] A real-Codex run showing confirmation in milliseconds rather than at the 60 s deadline, with no stray CR.

### M3 — remove LEVEL

- [ ] With M2 proven, delete the LEVEL path, finishing what `7cd091e` started.
- [ ] fork #187 closes **fully**.
- [ ] Re-check the four findings PR #219 recorded as "mooted by the revert" (reviewer F6, audit F1's `seed/004`//`007` items, `expected_session_id`'s load-bearingness): with LEVEL gone again, they stop being mooted and must be re-answered rather than assumed.

### M4 — offer upstream

- [ ] Branch from `upstream/main` and open the PR there.

## Success Criteria

1. A Codex delivery confirms on a genuine submit, not on boot.
2. A genuinely lost Codex write is reported **unconfirmed**, and a test proves it.
3. LEVEL is gone and fork #187 closes fully.
4. The seam is per-`RuleSet`, so the next wrapper agent supplies a pattern rather than a mechanism.

## Key Files

- `src/wrap.rs:304` — the hardcoded `user_prompt: None`.
- `src/wrap.rs:148-243` — `classify_line`, `classify_line_with`, `classify_codex_line`, the `CODEX` ruleset.
- `src/wrap.rs:980` — `classify_and_emit`.
- `src/ui.rs` — `prompt_text_confirms` and the LEVEL sampling this eventually deletes.
- `tests/e2e_orchestration_seed_retry_real.rs` — where `orchestration/seed/016` already drives real Codex.

## Sequencing

**#254 blocks #256.** Extending confirmation to the mode-seed path while the only wrapper signal is LEVEL would spread a known-unfalsifiable check to a second path — #256's own body says so. #256 waits.

**#257 does not block this**, but landing #257 first is useful: its overridable retry floor is what lets M2's falsifiability test drive the retry branch deterministically instead of racing it.

## Rule 12 — cross-version contract

**Answer required before the PR, not after.** `AgentEvent.user_prompt` already exists on the wire and is already `Option<String>`, so populating a field that was previously always `None` on one producer is **not** a wire-shape change — no `PROTOCOL_VERSION` bump.

It *is* a candidate **semantic** change behind a stable wire, which is exactly the case rule 12 exists for: a consumer that treated "wrapper events never carry `user_prompt`" as an invariant would now see one. Before opening the PR, grep for consumers keying on that emptiness. If any exists, this needs a `changelog.d/254.breaking.md` fragment. If none does, record the negative finding **in this PRD with the grep that established it** — a milestone ticked on neither a run nor a waiver is the state rule 12 prevents.

The **manual cross-version run applies regardless**, since it is a hook/daemon-adjacent change and the run is what catches a semantic break behind a stable wire. Isolate `DOT_AGENT_DECK_LOG` along with the sockets, `HOME` and state dir — a sandbox daemon otherwise appends into the real `~/.local/state/dot-agent-deck/deck.log`, and two interleaved daemons are genuinely hard to attribute after the fact.

## Risks and Mitigations

| Risk | Mitigation |
|---|---|
| Codex's echo format changes and the pattern silently stops matching | A silent stop means falling back to *unconfirmed*, which fails safe (a stray CR at the deadline) rather than falsely confirming. Pin the pattern with a fixture test so a format change fails loudly in CI, not quietly in production. |
| The echo is recognised but arrives so late that the retry fires first | Measure it in M2's real run. If the echo lags, the answer is the grace period, not a looser signal. |
| Prompt text echoed by the agent differs from what we sent (wrapping, ANSI, reflow) | Normalise before comparing, and pin the normalisation with fixtures. **Do not** relax the comparison to "close enough" — a fuzzy match reintroduces unfalsifiability by the back door. |
| Two prompts sharing a 200-byte prefix confirm each other | Already true on the hook path since PR #219's F9 and deliberately accepted there. Do not make it worse; note it in the PR. |
