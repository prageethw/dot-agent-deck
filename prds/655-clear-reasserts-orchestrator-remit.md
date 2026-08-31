# PRD #655: `/clear` re-asserts the orchestrator remit, not just compaction

**Status**: M1-M3 implemented, review/audit fix round in progress
**Issue**: [#655](https://github.com/prageethw/dot-agent-deck/issues/655)
**Also filed upstream**: [vfarcic/dot-agent-deck#787](https://github.com/vfarcic/dot-agent-deck/issues/787) — per CLAUDE.md rule 19 (fix-here-then-offer), the fix lands on this fork first; a PR is offered upstream once merged.
**Builds on**: issue #423 (`fix(orchestration): re-assert the orchestrator remit after compaction`) — this PRD extends the same re-arm mechanism to a second trigger, reusing its delivery machinery unchanged.
**Priority**: Medium

## Problem Statement

The orchestrator's role prompt — the pointer instructing an orchestration tab to read `.dot-agent-deck/orchestrator-context.md` and adopt the role/available-agents/delegation-protocol content in it — is currently re-delivered by exactly two triggers:

1. **Spawn time** (`prepare_orchestrator_prompt` + `deliver_orchestrator_prompt`, `src/ui.rs`).
2. **Auto-compaction** — issue #423 edge-triggers a re-delivery when the start-role pane's status is first observed `SessionStatus::Compacting`, derived from Claude Code's native `PreCompact` hook (`"PreCompact" => Some(EventType::Compacting)`, `src/hook.rs:221`).

There is also an explicit design decision (recorded inline near `src/ui.rs:15116`) that **reconnect never replays the prompt** — a deliberate choice, not a gap.

**`/clear` is a third case nothing currently covers.** Claude Code fires its native `SessionStart` hook when the user runs `/clear`, with a `source` field set to `"clear"` (the same hook also fires with `source: "startup"`, `"resume"`, and `"compact"` — Claude Code's own hook schema). Today:

- `ClaudeCodeHookInput` (`src/hook.rs`) has **no field for `source` at all** — an incoming `source` key is silently absorbed into the catch-all `_extra: HashMap<String, Value>` via `#[serde(flatten)]` and never read.
- `map_event_type("SessionStart")` (`src/hook.rs:207`) always maps to the same `EventType::SessionStart` regardless of `source` — there is no way today to distinguish a `clear`-originated `SessionStart` from an ordinary startup one downstream.
- The render loop's re-arm gate (`src/ui.rs`, the loop around line 15060–15145) only ever watches `SessionStatus::Compacting`; it never inspects `SessionStart` at all.

Net effect: after `/clear`, the orchestrator pane has a genuinely fresh model context with no memory of its role, its available agents, or the delegation protocol — worst exactly when a clean slate makes re-seeding that context most valuable — and nothing re-injects it. The user has to manually retype the "read your context file" instruction.

## Solution Overview

Extend the same re-arm mechanism issue #423 built for compaction to a second edge trigger: a `SessionStart` hook event whose `source` is `"clear"`.

1. **Capture and forward the `source` field** (`src/hook.rs`). Add `source: Option<String>` to `ClaudeCodeHookInput`. Forward it into `AgentEvent.metadata` **narrowly** — only on `SessionStart` events, only the literal value needed (mirroring the existing narrow-forwarding pattern already used for `SESSION_START_ORIGIN_METADATA_KEY` / wrapper-fork provenance, documented just above `build_event_typed` in `src/hook.rs`). This is deliberately as narrow as that precedent: it does not open `metadata` as a general passthrough channel.
2. **Give the render loop an edge to observe.** A `SessionStart` is a point-in-time event, not a persisted status like `Compacting` — so this needs its own edge-detection state (a new set/map alongside `ui.orchestration_remit_compacting`), not a reuse of the status-based check. The edge fires once per observed clear-originated `SessionStart` for the start-role pane, then clears, so a pane sitting in a post-clear state does not re-arm every frame.
3. **Reuse `deliver_orchestrator_prompt`'s existing readiness/retry/confirmation machinery unchanged.** This is a second re-arm *trigger*, not a new delivery path — the same reset-and-restart sequence issue #423 already performs (clear `send_retry_backoff`, `prompt_delivery`, `orchestration_ready_since`; set `orchestrator_prompt`; remove from `orchestration_prompted`; re-anchor the delivery deadline to now) applies here too.
4. **Scope to Claude Code only, matching issue #423's own scope.** Codex's native `SessionStart` fires when the first turn starts, not at session initialization (`prompt_delivery.rs`'s `agent_start_precedes_first_prompt` test documents this per-agent difference); OpenCode's `session.created` fires ~16ms after prompt acceptance. Neither shares Claude Code's hook shape, so this PRD does not attempt to cover them.
5. **Exclude the Pi start role**, exactly as the compaction re-arm already does — Pi's role prompt is delivered natively, daemon-side, not via this TUI-owned PTY-injection path.

## Scope

### In Scope

- Capturing and narrowly forwarding the `SessionStart` hook payload's `source` field for Claude Code (`src/hook.rs`).
- A new edge-detection state for "clear-originated SessionStart observed on the start-role pane" in the render loop (`src/ui.rs`), parallel to but distinct from the existing `Compacting`-status edge.
- Re-arming `deliver_orchestrator_prompt` on that edge, reusing its existing machinery unchanged.
- An L2 test in the same family as `tests/e2e_orchestration_remit.rs` (the suite issue #423 added), simulating a `/clear`-shaped `SessionStart` hook event and asserting the pointer prompt is retyped.
- Unit tests for the new `source` field's parsing/forwarding in `src/hook.rs`.

### Out of Scope

- **Codex, OpenCode, Devin, Pi.** None share the Claude-Code-native `SessionStart`-with-`source` hook shape this PRD keys off; a native mechanism for any of them is separate work.
- **Reconnect.** Design decision 3 (never replay on reconnect) is untouched by this PRD.
- **Worker (non-start-role) panes.** Matches issue #423's own scope — only the start role's own pane is ever consulted.
- **Compaction behavior itself.** Issue #423's mechanism is reused, not modified.

## Decisions

- **Reuse `deliver_orchestrator_prompt`'s machinery unchanged; this is a second trigger, not a second delivery path.** Two independent orchestrator-prompt delivery implementations would double the surface area for exactly the kind of confirmation/retry bugs `src/prompt_delivery.rs`'s extensive documentation already catalogs.
- **The `source` forwarding follows the existing narrow-passthrough precedent, not a general one.** `build_event_typed`'s existing comment on `SESSION_START_ORIGIN_METADATA_KEY` explains why a general `metadata` passthrough is a live injection surface (issue #243's audit reproduced a forged `wrapper_interface_ready` `SessionStart` from a bare `python3`); this PRD adds one more specifically-named, specifically-valued key, not a wildcard.
- **CLAUDE.md rule 12 finding (verified, not assumed): no `PROTOCOL_VERSION` bump needed.** `AgentEvent.metadata` (`HashMap<String, String>`, `#[serde(default)]`, no `skip_serializing_if`) is already always on the wire; `daemon_protocol.rs`'s server→client message IS the JSON-encoded `AgentEvent` broadcast verbatim (`BroadcastMsg::Event`), and `AppState::apply_event` stores every applied event's full `metadata` map unfiltered into `SessionState.recent_events`. The new `session_start_source: "clear"` key reaches the TUI exactly the same way `SESSION_START_ORIGIN_METADATA_KEY` already does — additive on an existing free-form map, in both directions (old TUI + new daemon ignores the key; new TUI + old daemon never receives it). No same-wire/different-meaning break, no `.breaking.md` fragment. Independently confirmed by both the reviewer and auditor against the actual code, not taken on the coder's report alone.
- **Experimental flag → no.** This re-arms an existing, always-on delivery mechanism (issue #423 shipped unflagged) to a second trigger; it is not a new surface or new semantics, matching the reasoning PRD #611 used for a comparable "make existing behavior work correctly" change. No `show_<feature>()` wrapper, no `experimental_enabled()` call on this path.

## Success Criteria

- Running `/clear` in an orchestrator's start-role pane causes the orchestrator-context pointer prompt to be retyped, without any user action, within the same delivery-confirmation model issue #423 already uses (bounded retries, confirmed-or-abandoned).
- Compaction re-arm (issue #423) and reconnect (design decision 3) behavior are both unchanged — pin both with existing or extended tests, not just the new `/clear` case.
- Codex, OpenCode, and Pi start roles are unaffected — no new behavior fires for them.
- `cargo test-fast` green per task; `cargo test-e2e` green pre-PR (per this fork's CI-only test policy).

## Milestones

- [x] **M1 — Capture and forward the `SessionStart` `source` field.** `ClaudeCodeHookInput` gains `source: Option<String>`; `build_event_typed` forwards it into `AgentEvent.metadata` under a new, narrowly-scoped key (`CLEAR_SESSION_START_METADATA_KEY`/`_VALUE` in `src/event.rs`), only for `SessionStart` events, mirroring the existing `SESSION_START_ORIGIN_METADATA_KEY` pattern.
- [x] **M2 — Render-loop edge trigger and re-arm.** A new edge-detection state (`orchestration_remit_clear_reasserted_at`) observes "the start-role pane just reported a clear-originated SessionStart" (once, not per-frame, marked only on a permanent outcome so a transiently-blocked frame retries); on that edge, re-arm `deliver_orchestrator_prompt` exactly as issue #423's compaction path does.
- [x] **M3 — Test coverage.** `orchestration_remit_004`/`005` in `tests/e2e_orchestration_remit.rs`; a `src/hook.rs` unit test for the new field's narrow forwarding; `tests/CATALOG.md` updated.
- [ ] **M4 — Review/audit fix round.** Reviewer + auditor independently converged on the same core finding (a strict `source: Option<String>` re-opens the exact decode-blackout the sibling `model` field already carries `lenient_model` to prevent). Fixing: lenient `source` decoding + test pin; a unit test for the new `orchestrator_remit_pane_latest_clear_session_start` helper plus a non-repetition assertion; a time-bounded floor on the transient-retry path (was an unbounded ~62Hz filesystem retry on persistent failure); enforcing the "Claude Code only" scope this PRD already claimed (previously unenforced — a Codex/Devin-stamped event could trigger it); a guard against `latest_clear` predating the tab's own anchor (forecloses a latent design-decision-3 violation if `SessionSnapshot` ever grows `recent_events`); a stale doc-comment reference. See Risks for the one finding accepted rather than fixed (compaction+clear double-delivery).

## Key Files

- `ClaudeCodeHookInput`, `map_event_type`, `build_event_typed` (`src/hook.rs`) — where the `source` field needs to be added and forwarded; the existing `SESSION_START_ORIGIN_METADATA_KEY` narrow-forwarding comment just above `build_event_typed` is the pattern to follow.
- The render-loop re-arm block for the start role (`src/ui.rs`, ~line 15060–15145) — today keyed on `orchestrator_remit_compacting` / `orchestrator_remit_pane_is_compacting`; this PRD adds a parallel edge for the `/clear` case, reusing the same reset-and-redeliver sequence.
- `deliver_orchestrator_prompt`, `prepare_orchestrator_prompt` (`src/ui.rs`) — delivery machinery reused unchanged.
- `src/prompt_delivery.rs` — the shared confirmation-capability policy (`agent_start_precedes_first_prompt`, `agent_reports_submitted_prompt`) documenting why this PRD is scoped to Claude Code only.
- `tests/e2e_orchestration_remit.rs` — the sibling test family issue #423 added; this PRD extends it rather than starting a new file.
- `src/daemon_protocol.rs` — where the rule 12 wire-contract check happens.

## Risks and Mitigations

- **A `/clear`-shaped `SessionStart` could arrive during an in-flight, unconfirmed delivery from an earlier trigger (e.g. a compaction re-arm still probing readiness).** *Mitigation*: follow issue #423's own precedent — a write that has landed (`PromptDelivery::attempts > 0`) is treated as eligible for re-arm; only a delivery still probing readiness/backoff before its first write should keep blocking re-arm, same rule for both triggers.
- **Forwarding an unvalidated `source` string risks widening the injection surface issue #243 already audited.** *Mitigation*: forward only through the same narrow, single-key, single-expected-value pattern already used for `SESSION_START_ORIGIN_METADATA_KEY`, not a general passthrough.
- **A future contributor could conflate this edge with the `Compacting` status check and try to merge the two into one field.** *Mitigation*: this PRD's Decisions section states explicitly why they are separate (point-in-time event vs. persisted status) — leave the note in code, not only here.
- **A test proving F4's Claude-Code-only scope enforcement cannot sequence a negative and positive check on the same pane.** Discovered during the M4 fix round: injecting a Codex-tagged `SessionStart` (correctly filtered, no re-arm) immediately followed by a ClaudeCode-tagged one on the *same* pane advances the daemon's generation-tracking (`pane_hook_session`) on the first event even though it was filtered from re-arming — a real `SessionStart` genuinely does mark a new session boundary, so this is existing, correct machinery (`delivery_target_changed`, documented against issues #424/#532/#608), not a bug this PRD introduced. The resulting two-generation-hop pane then reads as a stale delivery target and `deliver_orchestrator_prompt` abandons it. *This exact sequence cannot occur in real usage* — a single pane's `agent_type` is fixed for its whole life (set once from its role's configured command), so a real pane never receives a Codex-tagged event followed by a ClaudeCode-tagged one. *Resolution*: the positive control was dropped from `orchestration_remit_006` as redundant — `orchestration_remit_004` already independently proves a ClaudeCode-tagged clear event triggers re-arm via a single-hop pane, matching `orchestration_remit_002`'s own established pattern of proving negative and positive cases on two different panes/events rather than sequencing both on one. No `src/state.rs` change was made or needed.
- **Accepted, not fixed: a compaction re-arm and a genuine `/clear` landing in the same narrow window can deliver the pointer twice.** Found by the reviewer (F6). Reachability is low — `"compact"` is correctly excluded from the `/clear` trigger by the narrow-forwarding check, so this needs an actual `/clear` to land while a compaction re-arm's delivery is still in flight, not just any compaction. The visible effect is benign (the deck-authored pointer line is typed into the pane a second time, not incorrect content). *Disposition*: not fixed — closing it would require coupling the two edge-detection mechanisms this PRD's own Decisions section explicitly keeps independent, for a rare, non-harmful double-delivery. Recorded here rather than silently dropped, per CLAUDE.md rule 25's requirement to record a disposition and reason for anything not test-pinned.

## Open Questions

- None at this time.
