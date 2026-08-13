# PRD fork#254: A confirmation signal for wrapper-strategy agents that can actually return false

**GitHub Issue**: [fork #254](https://github.com/prageethw/dot-agent-deck/issues/254)

**Priority**: High

**Status**: Planning

**Parent**: [fork #197](https://github.com/prageethw/dot-agent-deck/issues/197), merged as PR [#219](https://github.com/prageethw/dot-agent-deck/pull/219). This PRD carries the half of fork [#187](https://github.com/prageethw/dot-agent-deck/issues/187) that closed only partially.

**Related**: fork [#194](https://github.com/prageethw/dot-agent-deck/issues/194) (the duplicate-seed bug, same state machine) · fork [#256](https://github.com/prageethw/dot-agent-deck/issues/256) (**depends on this** — see Sequencing) · fork [#257](https://github.com/prageethw/dot-agent-deck/issues/257) (the deterministic retry harness this will want) · [upstream #424](https://github.com/vfarcic/dot-agent-deck/issues/424)

**Fork-only?** **No.** `src/wrap.rs`, the classifier and the confirmation cycle are all upstream code. Offer upstream per rule 19 — see M4.

## RESCOPED (2026-08-13) — read this before anything below it

**Two observation rounds refuted this PRD's founding premise. Everything below is retained as the record; this section supersedes it.**

The premise was: *"TEXT is **structurally unavailable** for wrapper-strategy agents"*, because `emit_with_metadata` hardcodes `user_prompt: None` (`src/wrap.rs:304`). That is true of the **wrapper's** event path and irrelevant, because Codex's **native `UserPromptSubmit` hook** already supplies the text by a different road. Established empirically in M2.0b, five questions, all yes:

| Question | Answer |
|---|---|
| Are Codex's native hooks installed before the child spawns? | **Yes, by construction.** `codex_spawn_prep` (`src/wrap.rs:679-710`) calls `auto_install()` + `trust_deck_hooks_in()` synchronously at `:1096-1097`, strictly before the spawn at `:1113`/`:1115`. No boot race. |
| Does the event reach the daemon? | **Yes.** `hook::handle_hook("codex")` reuses the Claude-compatible parser; `map_event_type("UserPromptSubmit")` → `EventType::Thinking`, posted through the same `send_to_socket` as every other producer. |
| Does it carry the seed text? | **Yes, verbatim.** Raw capture shows `"prompt":"… sentinel: SENTINEL-254-M2-0B-PROBE-…"` — byte for byte what was submitted. |
| Does it carry usable identity? | **Yes, via an existing reconciliation.** Codex's own `session_id` is a fresh internal UUID and is *not* the deck's, but `AppState::apply_event`'s same-agent reuse guard (`state.rs:3707-3757`) remaps a `pane_id`+`agent_id`-matching event onto the wrapper's fork-time `SessionStart` session — the exact session `AwaitingConfirmation::expected_session_id` tracks. `DOT_AGENT_DECK_PANE_ID`/`_AGENT_ID` were confirmed to reach the hook subprocess unchanged. |
| Is `prompt_text_confirms` therefore already reachable for Codex? | **Yes.** It and its caller read `session.last_user_prompt`, populated by `apply_event` whenever `event.user_prompt.is_some()`, with **no branch on `agent_type` anywhere in that path.** |

**So LEVEL's dependence for Codex is a missed wiring, not a structural gap.** This PRD is no longer "invent a confirmation signal". It is "find out why the existing one is not being used, and make its falsifiability provable".

### N1 ANSWERED (2026-08-13): candidate 4, confirmed and root-caused

**LEVEL was never confirming Codex deliveries via a genuine signal at all.** It was confirming via the **wrapper's own generic stdout-classification fallback** — `DetectedEvent::Working` → `EventType::Thinking` (`src/wrap.rs:80`) — which fires on **any non-blank output line** from the interactive Codex TUI. Nothing to do with Codex's native hooks.

This is the direct consequence of upstream **#540**: because the `CODEX` `RuleSet`'s JSON markers can never match the interactive process (which emits no JSON), *every* line falls through to the generic `Working` classification. #540 is therefore not cosmetic dead code — **it is the mechanism that made LEVEL falsely confirm.**

Three real-agent runs of `orchestration/seed/016`, with file-based diagnostic instrumentation:

| Run | Configuration | Result |
|---|---|---|
| 1 | LEVEL removed, no instrumentation | Passed in 27.87s — **and proves nothing.** See below. |
| 2 | Both signals live, instrumented | **LEVEL confirmed at t+549 ms while `last_user_prompt` was still `None`** — direct proof the confirming event carried no prompt text, i.e. LEVEL, not TEXT. |
| 3 | LEVEL forced off, instrumented | **TEXT confirmed at t+8.45 s** via the genuine native-hook event, exact byte match, submission counter 0→1. LEVEL would have falsely fired at t+778 ms had it been active. |

**The race was never close: ~550–780 ms versus ~8.45 s.** The generic fallback beats the real hook by roughly 10×, every time, so LEVEL always won and TEXT never got to decide anything. That is why removing LEVEL looked like "there is no confirmation path" — the path was there and had simply never been reached.

**Run 1 is the part worth keeping.** The coder flagged that its pass does **not** distinguish "TEXT confirmed" from "confirmation never fired", because the original write lands before confirmation is checked and the 60 s-deadline path has no observable side effect the test asserts on. Had it been reported as the answer, it would have been a green run standing in for a cause — the exact failure this PRD keeps encountering. The decisive evidence is Runs 2 and 3, not the passing test.

**Method note worth reusing:** the harness's `env_clear()` blocks `DOT_AGENT_DECK_LOG`, but **not** a filesystem path — instrumentation writing to an absolute path sidesteps the isolation entirely. That reopens log-based verification inside the harness, which fork#197 recorded as structurally unreachable. Also flagged: `grep -n "EventType::Thinking" src/wrap.rs src/agent_pty.rs` might have found this **before** spending any real-agent run.

### Consequences for the remaining milestones

- **N2 plausibly collapses to re-landing `7cd091e`.** Run 3 shows TEXT confirming cleanly with no change beyond the LEVEL removal itself. One run is not a distribution, so N3 must confirm it rather than assume it — but there is no wiring defect to fix.
- **N3 is unchanged and is now the whole job:** a test that exhibits a genuinely lost write and shows the signal **declining** to confirm. It must be written to **fail while LEVEL is present** — because LEVEL will falsely confirm — which makes the TDD order load-bearing rather than ceremonial.
- **LEVEL is not merely unfalsifiable in theory.** It confirms on "the TUI printed a line". Restoring any narrowed variant of it is off the table.

### The original question, retained



**If TEXT was available all along, why did removing LEVEL break Codex?**

That removal (`7cd091e`, fork#197 M3) was reverted on a hard measurement: with LEVEL gone, *every* Codex delivery waited out the full 60 s deadline and fired a stray CR. That measurement is not in doubt. What is now in doubt is its **diagnosis** — "there is no confirmation path at all" — because M2.0b shows there is one.

Something prevented TEXT from confirming. Candidates, none yet eliminated:

1. **The comparison fails.** `prompt_text_confirms` compares a 200-byte prefix; PR #219's F9 changed the *sent* side's truncation to match the observed side. If that fix postdates the LEVEL-removal measurement, TEXT may simply have been mismatching then and would work now.
2. **Hook install or trust was not yet in place** at the time of that measurement, and has since been fixed — M2.0b confirms it is in place *today*.
3. **The event arrives but is not attributed** to the awaiting cycle — the reuse-guard remap exists, but whether it fired in that build is unverified.
4. **`map_event_type` maps `UserPromptSubmit` → `Thinking`**, which is *also* LEVEL's signal. So with LEVEL present the two are indistinguishable, and removing LEVEL may have removed the only consumer that was actually firing — meaning TEXT never confirmed even once, and its reachability is theoretical.

**Candidate 4 is the most dangerous and the most likely**, because it would mean today's Codex confirmations are still LEVEL's, that TEXT has never actually fired, and that this PRD's problem is entirely intact under a new description. It must be eliminated first.

### What M2.0b deliberately did NOT establish

The tester was explicit, and this matters more than the positive result:

> `orchestration/seed/016` proves the qualifying event reaches the daemon broadcast; it does not by itself prove `prompt_text_confirms` (rather than LEVEL, which still reliably-but-falsely confirms for Codex) was what finalized that particular delivery cycle.

**The two are indistinguishable from a passing test.** That is the same trap the whole PRD is about — a signal that cannot fail looks exactly like one that works. Do not treat `seed/016` passing as proof the native-hook path confirms anything.

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

### Finding (2026-08-13): Codex emits a STRUCTURED event stream, which changes the leading candidate

The design above assumed the wrapper would have to pattern-match a **rendered transcript line**. Inspecting `src/wrap.rs`'s `CODEX` ruleset shows that assumption is wrong:

```rust
pub static CODEX: RuleSet = RuleSet {
    error_markers: &["\"type\":\"error\""],
    idle_markers: &["\"type\":\"turn.completed\""],
};
```

Codex emits **JSON events**, not free-form text, and the classifier already matches on them. Event types referenced across `src/` and `tests/`: `turn.started`, `turn.completed`, `item.started`, `command_execution`, `reasoning`, `api`, `error`.

**`turn.started` is a submit-shaped event.** A turn begins when a prompt is submitted — which is exactly the signal LEVEL only approximates, and unlike `Thinking` it has no reason to fire on boot. That makes it falsifiable in the required sense: no submission, no turn, no confirmation.

This is **direction 2** from the issue (*"a submit-shaped event distinct from `Thinking`"*), and it is now the leading candidate over the echo-matching in direction 1 — structured parsing beats pattern-matching rendered text, which is fragile to reflow, ANSI and wrapping, and whose normalisation this PRD's Risks table was already worried about.

**This is grep evidence, not observation, and must not be implemented on that basis.** Two questions are unanswered and only a real Codex session can answer them:

1. Does `turn.started` fire on a genuine user submit, and **only** then? (If it also fires on boot or on a resumed session, it is LEVEL again in better clothing.)
2. Does it — or a neighbouring `item.started` — **carry the prompt text**? If yes, TEXT becomes available and `prompt_text_confirms` works unchanged, which is strictly better than a bare submit signal because it also distinguishes *our* prompt from any other.

**M2 therefore begins with an empirical step, not an implementation step.** See M2.0.

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

## Milestones (RESCOPED 2026-08-13) — these supersede M1–M4 below

### N1 — eliminate candidate 4, or confirm it

- [ ] Establish whether `prompt_text_confirms` has **ever** fired for Codex, or whether every Codex confirmation to date has been LEVEL's. `map_event_type("UserPromptSubmit")` → `EventType::Thinking` means both consumers see the same event, so distinguish them **at the consumer**, not by observing the event.
- [ ] The decisive experiment is cheap: **re-apply `7cd091e`'s LEVEL removal on a scratch branch and run `orchestration/seed/016`.** If it confirms via TEXT, LEVEL can go and the original removal was right on a wrong diagnosis. If it still waits out 60 s, TEXT is not firing and N2 is where the real work is.
- [ ] Whichever way it lands, record **which of the four candidates** it was. "It works now" without a named cause is how this PRD got its wrong premise in the first place.

### N2 — make TEXT actually fire for Codex, if N1 says it does not

- [ ] Fix whatever N1 identifies — a truncation mismatch, an attribution miss, a consumer that LEVEL was shadowing.
- [ ] Scope note: this is expected to be **small** — a wiring fix, not a mechanism. If it starts looking like a mechanism, stop and re-report; that would mean N1's diagnosis was wrong too.

### N3 — the falsifiability proof, which is still the deliverable

- [ ] A test exhibiting a **genuinely lost write** that the signal **declines** to confirm. This was the deliverable before the rescope and it is unchanged by it — everything above only changes *which* signal is being proved.
- [ ] It must discriminate TEXT from LEVEL explicitly. A test that passes with either is worthless here, for exactly the reason M2.0b flagged.

### N4 — remove LEVEL

- [ ] Once N3 proves TEXT falsifiable for Codex, delete LEVEL, finishing what `7cd091e` started. fork #187 closes **fully**.
- [ ] Re-answer the four findings PR #219 recorded as "mooted by the revert" (reviewer F6, audit F1's `seed/004`/`/007` items, `expected_session_id`'s load-bearingness) — with LEVEL gone again they stop being mooted.

### N5 — decide the wrapper seam's fate

- [ ] The original M1 (`emit_with_metadata`'s `user_prompt` seam) is **no longer required for Codex**. Decide whether to keep it for a future wrapper-strategy agent with no native hooks (Gemini, Aider), or drop it as speculative generality. A rule-19 judgement, and explicitly **not** a reason to keep this PRD open.

### N6 — offer upstream

- [ ] `src/wrap.rs`, `hook.rs` and the confirmation cycle are all upstream code. Branch from `upstream/main` and offer it there. Note that upstream **#540** (the dead `CODEX` ruleset) came out of this work and is related but separate.

---

## Milestones (ORIGINAL — superseded by N1–N6 above, retained as the record)

### M1 — the seam

- [ ] `emit_with_metadata`'s hardcoded `user_prompt: None` becomes a value the caller can supply. Every existing caller keeps passing `None`, so this milestone alone changes no behaviour.
- [ ] The classification path can carry a detected prompt string from `classify_line_with` through to the emitted `AgentEvent`.
- [ ] Truncation matches the hook side exactly (200 bytes), so the comparison is symmetric — PR #219's F9 made the *sent* prompt truncate to match the *observed* one, and a third truncation rule here would silently break that.

### M2 — the Codex signal

- [ ] **M2.0 — observe the real event stream first.** Capture a real Codex session's JSON events around a genuine submit, and answer the two questions in the Finding above: does `turn.started` fire on submit and *only* on submit, and does it or a neighbouring `item.started` carry the prompt text? **Nothing is implemented before this is answered** — implementing on grep evidence is how LEVEL got adopted in the first place, and the whole point of this PRD is not to repeat that.

  This needs a **real-agent run** (CI has no Codex credentials), so it is CLAUDE.md rule 5 carve-out (a) and requires an explicit orchestrator authorisation naming the tests. Record the captured events **in this PRD** — the observation is the deliverable, and a finding that lives only in a worker's context is one interruption from being lost.

  #### M2.0 finding (2026-08-13): `turn.started` is real, but it lives on a channel the wrapper never taps

  **Bottom line: this is a "no", and it disqualifies the whole M2 plan as currently scoped — not because `turn.started` behaves badly, but because it is invisible to the process the deck actually spawns.**

  **What the deck spawns.** `wrap_launch_command("codex", &AgentType::Codex)` rewrites the launch to `dot-agent-deck wrap -- codex` — the **bare interactive TUI**, no `exec`, no `--json`. Grepped `src/agent_pty.rs`, `src/issue_dispatch*.rs` and all of `src/` for any construction of `exec`/`--json` alongside a Codex invocation: **no hits**. The wrapper tees that interactive process's raw stdout — the same stdout the user watches render live in the pane.

  **Question 1 answer — inside the channel where `turn.started` exists (`codex exec --json`), it is well-behaved:**
  - Fires exactly once per genuine submit, immediately (before the model responds), never spuriously.
  - `codex exec` cannot be invoked without a prompt at all, so there is no "boot with nothing submitted" case to test inside this channel — invocation *is* submission.
  - Resuming a prior thread and submitting a new prompt (`codex exec resume <id> --json "…"`) re-emits exactly one `turn.started` per submitted prompt, keyed to the **same** `thread_id` — no separate "resume" event type, no spurious extra `turn.started`.
  - A tool call **within** an existing turn (a shell command) does **not** re-fire `turn.started` — it fires `item.started`/`item.completed` for the `command_execution` item instead, once each. Confirmed with a real run (below).

  **But — the interactive TUI the wrapper actually tees emits ZERO JSON, ever, under any condition tested:**
  - Fresh boot, 12s idle, nothing submitted: raw PTY capture, 15.9KB of output, `grep -oE '"type":"[a-zA-Z._]+"' ` → **0 matches**. Pure ANSI (`cursor`, `SGR`, alt-screen) redraw bytes, confirmed by `strings`.
  - A tmux-driven session, booted to the composer, given a genuine sentinel-bearing prompt (`SENTINEL-254-EVENTSTREAM-PROBE-G`), and watched through a full real response cycle (native `SessionStart`/`UserPromptSubmit` hook banners rendered, then `PONG`): raw log grew from 25,616 → 50,028 bytes; `grep -c '"type":'` → **0** across the entire capture.
  - So `turn.started` cannot be "LEVEL in better clothing" (falsely firing on boot) — it never fires **at all**, in either direction, on this channel. It is not unfalsifiable; it is **absent**.
  - Conclusion: the `CODEX` `RuleSet`'s `error_markers`/`idle_markers` (`"type":"error"`, `"type":"turn.completed"`) and `classify_codex_line`'s `serde_json` parse path (`src/wrap.rs:229-243`) were authored against `codex exec --json`'s output shape, but that is **not the shape of what the wrapper ever tees**. Against the actual spawned process they are dead code — the substring/JSON checks simply never match, and every line falls through to the generic non-blank-line `Working` classification (which is why the *existing* Working/Idle-via-process-exit behaviour still works today: it doesn't depend on these markers ever firing).

  **Question 2 answer — no, and it's moot given Q1, but recorded for completeness:** even in the channel where `turn.started` exists, it carries **no fields at all** beyond the type discriminator — `{"type":"turn.started"}`, nothing else. `item.completed` for an `agent_message` item carries `item.text`, but that is the **agent's own reply**, never the submitted prompt. The literal sentinel string (`SENTINEL-254-EVENTSTREAM-PROBE-*`) was grepped for across all four `codex exec --json` captures below and found in **zero** of them.

  **Timing** (process start → event, `codex exec --json`, wall-clock via `date +%s%N` around each stdout line):
  ```
  +495ms  {"type":"thread.started","thread_id":"019ff92a-733b-7951-bc00-b0c75ce35a6c"}
  +594ms  {"type":"turn.started"}
  +5488ms {"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"PONG"}}
  +5614ms {"type":"turn.completed","usage":{...}}
  ```
  `turn.started` lands ~99ms after `thread.started` and well before the model's own response (~5s later) — genuinely tied to submission, not a boot artifact, *in the channel where it exists*. Irrelevant to the wrapper, which never sees this channel.

  **Raw JSON shapes captured** (`codex exec --json`, real runs, `codex-cli 0.147.0`):

  Genuine submit, no tool call:
  ```
  {"type":"thread.started","thread_id":"019ff924-819d-7162-9528-05c4e41b5123"}
  {"type":"turn.started"}
  {"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"PONG"}}
  {"type":"turn.completed","usage":{"input_tokens":12731,"cached_input_tokens":6912,"cache_write_input_tokens":0,"output_tokens":6,"reasoning_output_tokens":0}}
  ```
  Genuine submit that includes a tool call (`ls -la`) — note `item.started`/`item.completed` bracket the tool item, `turn.started` fires only once:
  ```
  {"type":"thread.started","thread_id":"019ff924-e158-7bd0-8c7a-87870dc1a19a"}
  {"type":"turn.started"}
  {"type":"item.started","item":{"id":"item_0","type":"command_execution","command":"/bin/bash -lc 'ls -la'","aggregated_output":"","exit_code":null,"status":"in_progress"}}
  {"type":"item.completed","item":{"id":"item_0","type":"command_execution","command":"/bin/bash -lc 'ls -la'","aggregated_output":"total 40\n…","exit_code":0,"status":"completed"}}
  {"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"DONE"}}
  {"type":"turn.completed","usage":{"input_tokens":26895,"cached_input_tokens":19968,"cache_write_input_tokens":0,"output_tokens":164,"reasoning_output_tokens":55}}
  ```
  Resumed thread (`codex exec resume <thread_id> --json "…"`) — same `thread_id`, one `turn.started`, no distinct resume marker:
  ```
  {"type":"thread.started","thread_id":"019ff924-819d-7162-9528-05c4e41b5123"}
  {"type":"turn.started"}
  {"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"RESUMED"}}
  {"type":"turn.completed","usage":{"input_tokens":26790,"cached_input_tokens":18944,"cache_write_input_tokens":0,"output_tokens":13,"reasoning_output_tokens":0}}
  ```

  **Cases probed for Q1:** fresh boot / no submit (interactive TUI, PTY capture, 12s idle — 0 JSON) · genuine submit (interactive TUI via tmux with a real sentinel prompt and a full response cycle — 0 JSON; also `codex exec --json`, 4 separate runs — JSON present, well-behaved) · resumed session with a new prompt (`codex exec resume <id> --json` — JSON present, same shape, same `thread_id`) · tool call inside an existing turn (`codex exec --json` with a shell command — confirmed `item.started`/`item.completed` only, no extra `turn.started`). **Not probed:** resuming the *interactive* TUI with no new submit (moot — the interactive channel already shows zero JSON under every other condition, so there is nothing left to disprove there).

  **Aside, flagged as an observation and not a recommendation:** while capturing the tmux genuine-submit run, Codex's **native** `UserPromptSubmit` hook fired and rendered in the pane (`codex_hooks_manage.rs`'s `CODEX_HOOK_EVENTS` already installs it, and `hook.rs:357`/`hook.rs:428` already extract real `user_prompt` text from that hook's stdin payload — confirmed by the existing test `build_event_user_prompt_submit_extracts_prompt`). That is a **second, independent, already-wired path** that appears to carry real submitted-prompt text for Codex specifically, separate from the wrapper's stdout-classification path this PRD scopes M2 around. Whether that native-hook path is reliable enough to serve as the seed-confirmation TEXT signal (e.g. whether hook trust/install lands in time for the very first seed prompt, which is the case this PRD cares about) is unverified — flagging it for the orchestrator to weigh, not deciding it here.

  **Recommendation (orchestrator's call per the PRD, not mine to decide):** M2 as scoped — recognise `turn.started` off the wrapper's teed stdout — is not implementable; the event does not reach that channel under any condition. Direction 1 (echo-matching rendered TUI text) or Direction 3 (viewport differencing), the PRD's two listed fallbacks, are the remaining candidates, unless the native-hook aside above turns out to be usable, which would need its own verification pass before being treated as a candidate.

  #### M2.0b finding (2026-08-13): the native-hook aside is a "yes" — `prompt_text_confirms` is already reachable for Codex today, with no code change

  **Bottom line, answering the five questions in order: install — yes, timely; delivery to the daemon — yes; text — yes, verbatim; identity — yes, via an existing reconciliation path; and therefore `prompt_text_confirms` is reachable for Codex TODAY. LEVEL's dependence for Codex is a bug (a missed wiring), not a structural gap. This changes the PRD's founding premise: TEXT is not "structurally unavailable" for Codex — it is structurally unavailable only on the WRAPPER's stdout-classification channel this PRD's M1/M2 scoped around; a second, independent, already-wired channel (Codex's own native hooks) already carries it.**

  **Q1 — install timing.** `codex_spawn_prep` (`src/wrap.rs:679-710`) calls `codex_hooks_manage::auto_install()` then `trust_deck_hooks_in()` SYNCHRONOUSLY, and the call site (`src/wrap.rs:1096-1097`) runs strictly before `run_wrap_pty`/`run_wrap_pipe` (`:1113`/`:1115`) — the functions that do `StdCommand::new(program).spawn()` for the real interactive `codex` child. So hooks.json is written and trust is recorded in `config.toml` before the child that would read them even exists; there is no boot race by construction. This is belt-and-suspenders with a SECOND, earlier install: `codex_hooks_manage::auto_install_and_trust_at_startup()` runs once at TUI/daemon startup (`src/main.rs:1352`, wired as `AgentType::Codex`'s `startup_auto_install`, `src/agent_registry.rs:243`), well before any pane — let alone a seed write — exists. Empirical confirmation below (the passing real-agent test) shows exactly ONE qualifying native-hook event for the very first genuine submit in a fresh pane — trust landed in time.

  **Q2 — does the event reach the daemon?** Yes, unchanged from how Claude's hooks already work: `codex_hooks_manage.rs`'s installed command for every event (including `UserPromptSubmit`) is `<binary> hook --agent codex` (`HOOK_COMMAND_SUFFIX`). `hook::handle_hook("codex")` (`src/hook.rs:59-65`) parses the SAME `ClaudeCodeHookInput` shape Claude posts (module doc: "Codex ships a Claude-Code-compatible hooks engine"), `map_event_type("UserPromptSubmit")` → `EventType::Thinking` (`hook.rs:111`), and the built `AgentEvent` is posted via `send_to_socket` exactly like every other hook producer.

  **Q3 — does it carry the seed text?** Yes, verbatim, confirmed with a direct raw capture (below) AND with a real deck-spawned pane. `hook::build_event_typed` sets `user_prompt = prompt.map(|p| truncate(...))` (`hook.rs:357`) straight from the hook payload's `prompt` field — no gating on event type, no special-casing by agent.

  **Q4 — identity (pane/agent/session).** Yes, established two ways:
  - `pane_id`/`agent_id` on the event come from `std::env::var(DOT_AGENT_DECK_PANE_ID)` / `_AGENT_ID` READ BY THE HOOK SUBPROCESS ITSELF (`hook.rs:358`,`:364`) — i.e. this depends on Codex's OWN hook engine propagating its process env down to the command hooks it spawns, which is a claim about Codex's code, not the deck's, and had to be verified directly (see raw capture below).
  - The daemon does NOT need the native hook's `session_id` to match the deck's own `session_id_for(pane_id)` (`"<pane_id>-session"`, `wrap.rs:540-551`) — and empirically it does NOT match (raw capture below: Codex's own hook `session_id` is a fresh internal thread UUID). What reconciles them is `AppState::apply_event`'s same-agent reuse guard (`src/state.rs:3707-3757`): when an incoming event's `pane_id` matches an existing session on that pane AND `agent_id` matches, the event's `session_id` is REMAPPED onto that existing session's id before it is recorded. The existing session it lands on is the wrapper's own fork-time `SessionStart` (`Emitter::emit_fork_session_start`, `wrap.rs:279-293`, emitted the instant `cmd.spawn()` returns — well before Codex even boots, let alone loads hooks), which carries exactly the same `pane_id`/`agent_id` the wrapper set on the child's env (and which the child, per the raw capture below, faithfully forwards to its own hook subprocesses). So a native-hook event with a foreign `session_id` still lands on the SAME session card `AwaitingConfirmation::expected_session_id` was captured against.

  **Q5 — is `prompt_text_confirms` reachable for Codex today?** Yes. `prompt_text_confirms` (`src/ui.rs:2174`) and its caller (`ui.rs:4197-4251`) read `session.last_user_prompt`/`last_user_prompt_seq` — fields `AppState::apply_event` populates (`state.rs:4177-4198`) whenever `event.user_prompt.is_some()`, with NO branch on `agent_type` anywhere in that gate or in `prompt_text_confirms` itself. Given Q1-Q4 above, a genuine Codex submit produces exactly such an event, attributed to the right session. Nothing gates this off for Codex; it was simply never observed before because M1/M2 scoped exclusively around the wrapper's OWN stdout-classification path (`emit_with_metadata` hardcodes `user_prompt: None`, `wrap.rs:304`), which is a real dead end (M2.0's finding stands), but is not the only path into `prompt_text_confirms` for this specific agent.

  **Direct raw-payload proof (isolated from the deck entirely — Codex's own hook engine, driven manually to settle Q3/Q4 with byte-exact evidence).** Built a scratch `CODEX_HOME` with `hooks.json` routing `SessionStart`/`UserPromptSubmit` to a debug script that dumps `env` and stdin verbatim (not the deck's `dot-agent-deck hook` binary — this isolates "does Codex forward env and post real text" from any deck code), launched real interactive `codex` (v0.147.0, `--dangerously-bypass-hook-trust` to skip the deck's own trust bookkeeping for this isolated probe) via tmux with `DOT_AGENT_DECK_PANE_ID=pane-probe-42` and `DOT_AGENT_DECK_AGENT_ID=agent-probe-77` set on it (mirroring exactly what the wrapper sets on a real spawn), and submitted a sentinel prompt:

  ```
  UserPromptSubmit stdin (verbatim):
  {"session_id":"019ff93d-ab2d-7852-9060-300be7f422c0","turn_id":"019ff93d-dd1b-7782-b78c-18db10eef2b0","transcript_path":"…/rollout-2026-08-13T13-49-56-019ff93d-ab2d-7852-9060-300be7f422c0.jsonl","cwd":"…/codex_hook_probe/work","hook_event_name":"UserPromptSubmit","model":"gpt-5.6-terra","permission_mode":"bypassPermissions","prompt":"reply with the single word PONG only, nothing else. sentinel: SENTINEL-254-M2-0B-PROBE-1786593007"}

  SessionStart stdin (verbatim, same session, fired at boot):
  {"session_id":"019ff93d-ab2d-7852-9060-300be7f422c0","transcript_path":"…","cwd":"…","hook_event_name":"SessionStart","model":"gpt-5.6-terra","permission_mode":"bypassPermissions","source":"startup"}

  Hook subprocess env (both invocations, grepped):
  DOT_AGENT_DECK_AGENT_ID=agent-probe-77
  DOT_AGENT_DECK_PANE_ID=pane-probe-42
  ```

  `prompt` is the exact sentinel-bearing text, byte for byte — not empty, not a different field name, not a paraphrase. `session_id` (`019ff93d-…`) is Codex's own internal id and identical across both hooks for the one session, but is NOT `pane-probe-42-session` (what `session_id_for` would derive) — confirming the reconciliation in Q4 is load-bearing, not redundant. Both env vars set on the parent `codex` process reached the hook subprocess's own environment unchanged, for both event types.

  **Empirical confirmation against the REAL deck+daemon+production code path (not the isolated probe above), authorized carve-out (a):** ran the existing, unmodified real-agent test `orchestration/seed/016` (`tests/e2e_orchestration_seed_retry_real.rs::orchestration_seed_016_real_codex_confirmation_retry_never_duplicates_the_prompt`) against a genuine interactive Codex session spawned through the deck's OWN `wrap`/`codex_spawn_prep`/`hook.rs` path (`DOT_AGENT_DECK_CODEX_TEST_MODEL=gpt-5.6-terra`, since this host's ChatGPT-subscription auth cannot reach the suite's default `gpt-5.1-codex-mini`). **Passed** (`64.71s`, `test result: ok`). Its assertion is exactly the shape this finding needs: `events.snapshot()` (the daemon's raw broadcast, tapped BEFORE `apply_event` — `src/daemon.rs:1141` sends to the broadcast channel one line before `state.apply_event(event)` at `:1143`) contains **exactly one** event with `event_type == Thinking`, `agent_type == Codex`, and `user_prompt` containing the delivered seed pointer (`"Read .dot-agent-deck/orchestrator-context.md"`) **exactly once**. Per a repo-wide grep (`grep -rn "user_prompt: Some\|user_prompt = " src/*.rs`), the ONLY non-test production code that ever populates `AgentEvent.user_prompt` with `Some(...)` is `hook.rs`'s `build_event_typed`/`build_opencode_event` — `wrap.rs`'s own emitters always pass `None` (M2.0's finding). So this event could only have originated from the native hook path, in a genuine deck-managed pane, confirming Q1-Q3 hold in production, not only in the isolated probe.

  **What this does NOT establish, stated so it isn't overclaimed:** `orchestration/seed/016` proves the qualifying event reached the daemon's broadcast stream; it does not, by itself, prove `prompt_text_confirms` was the mechanism that FINALIZED that delivery cycle inside `ui.rs` (LEVEL is still in the tree and — per the Problem Statement's own measurement — reliably (falsely) confirms for Codex almost instantly regardless, so a passing test cannot distinguish "TEXT confirmed it" from "LEVEL confirmed it as it already does"). Establishing that `text_confirmed=true` specifically fires inside a live `ui.rs` confirmation cycle for Codex would need either log access this harness's `DOT_AGENT_DECK_LOG` isolation gap (see task's Known obstacle) currently blocks, or a new test — both out of scope for an observation-only round. What IS established without that: every precondition `prompt_text_confirms` needs (a matching, non-stale `last_user_prompt` on the exact session the write targeted) is mechanically satisfied for Codex today, by code already in the tree, doing nothing this PRD would need to add.

  **Recommendation (orchestrator's call, not mine to decide):** M2 as scoped (recognise a submit-shaped event off the wrapper's teed stdout) is confirmed not implementable by M2.0, and now looks unnecessary rather than merely blocked — the native-hook path already does the job M2 was designed to build, for Codex specifically. Worth weighing a rescope: M2 becomes "confirm `prompt_text_confirms` fires via the native-hook path for Codex" (verification + the falsifiability test the PRD already requires) rather than "build a wrapper-side signal", which would also let M1's `emit_with_metadata` seam work stand mostly unused for Codex (it may still matter for a FUTURE wrapper-strategy agent without native hooks, e.g. Gemini — worth a rule-19 style check before shrinking M1's scope, not decided here).

- [ ] The `CODEX` `RuleSet` recognises whichever event M2.0 establishes, and emits it carrying the prompt where available.
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
