# PRD #655: `/clear` re-asserts the orchestrator remit, not just compaction

**Status**: Not started
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
- **CLAUDE.md rule 12 check (to confirm during implementation, not asserted here):** `AgentEvent.metadata` is already a wire-carried field used for other narrowly-forwarded keys (e.g. `SESSION_START_ORIGIN_METADATA_KEY`), so adding one more specific key is expected to be additive, not a same-wire/different-meaning break — likely no `PROTOCOL_VERSION` bump. The coder must verify this against `src/daemon_protocol.rs` before closing the PRD and record the finding either way (bump + `.breaking.md`, or confirmed-no-break) rather than assuming it.
- **Experimental flag → no.** This re-arms an existing, always-on delivery mechanism (issue #423 shipped unflagged) to a second trigger; it is not a new surface or new semantics, matching the reasoning PRD #611 used for a comparable "make existing behavior work correctly" change. No `show_<feature>()` wrapper, no `experimental_enabled()` call on this path.

## Success Criteria

- Running `/clear` in an orchestrator's start-role pane causes the orchestrator-context pointer prompt to be retyped, without any user action, within the same delivery-confirmation model issue #423 already uses (bounded retries, confirmed-or-abandoned).
- Compaction re-arm (issue #423) and reconnect (design decision 3) behavior are both unchanged — pin both with existing or extended tests, not just the new `/clear` case.
- Codex, OpenCode, and Pi start roles are unaffected — no new behavior fires for them.
- `cargo test-fast` green per task; `cargo test-e2e` green pre-PR (per this fork's CI-only test policy).

## Milestones

- [ ] **M1 — Capture and forward the `SessionStart` `source` field.** `ClaudeCodeHookInput` gains `source: Option<String>`; `build_event_typed` forwards it into `AgentEvent.metadata` under a new, narrowly-scoped key, only for `SessionStart` events, mirroring the existing `SESSION_START_ORIGIN_METADATA_KEY` pattern.
- [ ] **M2 — Render-loop edge trigger and re-arm.** A new edge-detection state observes "the start-role pane just reported a clear-originated SessionStart" (once, not per-frame); on that edge, re-arm `deliver_orchestrator_prompt` exactly as issue #423's compaction path does (reset per-cycle delivery state, re-anchor the deadline, re-run `prepare_orchestrator_prompt`).
- [ ] **M3 — Test coverage.** Unit tests for the new hook field; an e2e test alongside `tests/e2e_orchestration_remit.rs` for the `/clear` case; `tests/CATALOG.md` updated.

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

## Open Questions

- None at this time — the coder should re-open this section if implementation surfaces one.
