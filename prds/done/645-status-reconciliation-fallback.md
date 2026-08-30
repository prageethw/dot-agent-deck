# PRD #645 — Reconcile agent status when Codex's hook stream goes silent

**Issue:** [prageethw/dot-agent-deck#645](https://github.com/prageethw/dot-agent-deck/issues/645)
**Priority:** Medium *(downgraded from High 2026-08-30 — see reframe below; the deterministic, higher-frequency bug now has its own direct fix)*
**Status:** Closed — Not Pursued (2026-08-30). #644's actual root cause turned out to be an unrelated, already-fixed-directly bug (see Reframe below), and this PRD's own motivating observation (Codex's hook subsystem going totally silent) was a single, unreproduced sighting — parked rather than building reconciliation infrastructure for an unconfirmed problem. Reopen if the total-silence symptom recurs with a cleaner repro.
**Related:** #644 (originating bug report — **now has a confirmed, deterministic, unrelated root cause**, see reframe below), #640 (hook/status marker CI test durability — adjacent, about test pinning not runtime reconciliation), `prds/91-hook-freshness-check.md` (adjacent — static hook *registration* check at startup, not runtime hook-*firing* reconciliation), issue #638 (the classify_and_emit / `suppress_text_status` machinery this PRD builds on)
**Fork-only?** No in principle (a general dot-agent-deck reliability gap), but not yet offered upstream — see M8.

## Reframe (2026-08-30): #644's actual root cause is a different, more urgent, already-fixable bug

A delegated diagnosis pass reproduced #644's stuck-`Working` symptom live against a genuine interactive Codex pane and found the root cause is **not** Codex's hook subsystem going silent — the native `Stop` hook fires correctly and does set `session.status = Idle`. The real defect: 0.49 seconds later, the daemon's **shell-activity monitor** (PRD #386) fires an unrelated `ShellBusy` event that permanently re-promotes the status to `Working`, because it structurally misclassifies Codex's own primary child process (which gets its own PTY/POSIX session — an ordinary consequence of how `wrap` spawns any interactive child) as a still-busy detached command. The argv-shape veto that protects Claude Code panes from exactly this false positive (`MEASURED_SHELL_TOOL_SHAPES`, `src/platform/proc/scan.rs`) was only ever measured for Claude — Codex (and likely OpenCode/Pi/Devin) get no veto at all. Full evidence: `.dot-agent-deck/644-diagnosis.md` in worktree `dot-agent-deck-644`.

That bug is now being fixed **directly under #644** as an ordinary bugfix (TDD chain: tester RED → coder fix → tester GREEN → review), not through this PRD's reconciliation machinery — it doesn't need a new signal-source/staleness model, just a correction to the existing shell-activity classifier.

**This PRD is not obsolete, but it is demoted.** The live evidence that motivated it in the first place (`.dot-agent-deck/644-live-finding-hook-subsystem-silent.md`: Codex's own trace log showing *zero* `hook/started`/`hook/completed` events of any kind, for any session, across ~10 hours and 15 completed turns) is a separate, real observation that the #644 diagnosis does not explain — that diagnosis's reproduction shows the hook firing and being received; the live sighting shows Codex's own log recording that it never even attempted to fire *any* hook for hours. Both can be true at once (a deterministic per-turn race, and a separate, rarer, total-subsystem-silence failure mode with an unconfirmed trigger). This PRD now exists to cover the second case as defense-in-depth, once the #644 fix ships — not to compete with it for priority.

## Problem Statement

#644 documented an interactive Codex pane stuck at **Working**/**Thinking** after a normal, cleanly-completed turn, with hook trust fully confirmed (`hooks.json` + `config.toml`'s `[hooks.state]` both correct — `CodexSpawnPrep::hook_trust_confirmed = true`). Live investigation on this checkout (`.dot-agent-deck/644-live-finding-hook-subsystem-silent.md`) found the root cause is not in this repo's event handling: Codex's own trace log (`~/.codex/logs_2.sqlite`) showed **zero** `hook/started`/`hook/completed` events of *any* type, for *any* session, for roughly 10 hours, despite 15 completed model turns in that window and correct on-disk trust state. Codex's own hook-execution subsystem silently stopped firing, for reasons outside this repo's control (trigger unconfirmed — possibly a stale in-memory app-server component after some event around the time it stopped).

We cannot fix Codex's runtime. This PRD is about not depending on it exclusively once trust is confirmed.

### Why this is fixable here, cheaply

`src/wrap.rs`'s `classify_and_emit` (issue #638 round 6) already computes a text-derived status classification on **every** line of Codex's interactive output, unconditionally. `suppress_text_status` (set once per session, from `CodexSpawnPrep::hook_trust_confirmed`) only affects the *last* step — the `emitter.emit()` call at the end — where every classified event except `Error` is silently discarded once hook trust is confirmed. The signal this PRD needs already exists and is computed continuously; it is just thrown away. This PRD is about **reconciling** the two signals instead of discarding one of them outright.

### Why not just remove `suppress_text_status`

#638 spent five rounds establishing that the raw text heuristic is not reliable enough to be the *primary* signal for a full-screen interactive TUI repaint stream — it produces both false-`Idle` mid-turn and false-`Working` wedges at turn end (`.dot-agent-deck/638-audit-findings-r3.md` F1/F2). Reverting to always-trust-text would reintroduce that flakiness for the common case where hooks work fine. The fix here is narrower: use the text signal only when there is direct evidence the hook channel has gone quiet for longer than a real turn should take.

## Decisions

| Question | Decision |
|---|---|
| Reuse #638's text classifier, or build a new detector? | **Reuse.** `classify_and_emit` already computes it every line; nothing new to build on the detection side. |
| Where does the staleness decision live — `wrap.rs` (source) or the daemon (sink)? | **The daemon.** `wrap.rs` (the long-lived tee process) and `src/hook.rs`'s `handle_hook` (invoked as a separate short-lived process per hook event) do not share in-process state — the daemon is the only party that observes both a session's hook-sourced events and its would-be text-sourced events, and is the sole owner of `session.status`. |
| What does `wrap.rs` do differently? | Stop discarding at the `emitter.emit()` gate. Every classified event (not just `Error`) is sent, tagged with its source, so the daemon has the data to reconcile. |
| What counts as "stale"? | Time since the daemon last applied a **hook-sourced** status-changing event for that session — not time since session spawn, so a long-lived session that has been legitimately hook-driven for hours isn't penalized for its age. |
| Staleness threshold? | Not fixed in this PRD — must be grounded empirically (real captured turn durations, e.g. from `.dot-agent-deck/638-audit-findings-*` or a fresh capture) rather than guessed, during implementation. Err conservative: long enough that a real, slow "thinking" turn never false-positives into a spurious correction. |
| Does a text-sourced correction ever override a text-sourced `Error`? | No change from today — `Error` stays authoritative regardless of source or staleness. |
| Cross-version / wire contract impact? | **Must be resolved during implementation, not guessed here.** Tagging AgentEvents with a source and changing what `wrap.rs` sends over the hook-ingestion socket may be a wire-shape change (`PROTOCOL_VERSION` bump, CLAUDE.md rule 12) or same-wire-different-meaning (`.breaking.md` fragment) depending on how it's implemented. The rule-12 cross-version manual test is required either way before this merges. |
| Visibility when a correction fires | Surface a `session_warnings`-style notice (same mechanism `prds/91-hook-freshness-check.md`/PRD #126 already use) when a text-sourced correction actually overrides a stale hook status, so a user can tell "the daemon just caught a real Codex hook outage" from silence, and so this becomes a concrete signal worth pointing at in an eventual Codex-side bug report. |

## Design

1. **Tag AgentEvent sources.** Add an `EventSource` (naming TBD during implementation — e.g. `Hook` vs `TextHeuristic`) to the status-affecting event path. `src/hook.rs`'s `handle_hook` tags `Hook`. `src/wrap.rs`'s `classify_and_emit` tags `TextHeuristic`.
2. **`classify_and_emit` stops discarding.** Under `suppress_text_status`, every classified event is still emitted (not just `Error`), carrying the `TextHeuristic` tag — the suppression *decision* moves downstream to the daemon.
3. **Daemon tracks hook recency per session.** Wherever `session.status` lives (`src/state.rs`), add a `last_hook_status_at` (or equivalent) updated only by `Hook`-sourced events.
4. **Reconciliation policy at the daemon's event-apply path.** A `Hook`-sourced event always applies immediately — unchanged, still the authoritative fast path. A `TextHeuristic`-sourced event applies **only** if the time since `last_hook_status_at` exceeds the staleness threshold; otherwise it's recorded for diagnostics (useful for tests and for tightening the threshold later) but not applied to `session.status` — preserving #638's "don't race the hook channel" property whenever hooks are actually working.
5. **User-visible notice on correction** (see Decisions row above).

## Scope

### In scope
- The reconciliation mechanism above (wrap.rs tagging → daemon-side staleness policy → status correction).
- Tests pinning the reconciliation policy itself (see Milestones M7).
- The rule-12 cross-version contract check.
- A session-warning notice when a correction fires.

### Out of scope
- Fixing Codex's own hook subsystem (not our code — see M8 for the upstream angle).
- Building a new/better text classifier — this reuses #638's existing one as-is.
- Any change to non-Codex agent status handling (Claude, opencode, etc.) — this is scoped to the Codex `suppress_text_status` path specifically, since that's the only path currently discarding a computed signal.
- Making the staleness threshold user-configurable in `.dot-agent-deck.toml` — may become a follow-up if a single default can't fit both short and long turns; not required for the first cut.

## Success Criteria

- A session whose hook channel goes silent (Codex-side outage, matching #644's live-observed symptom) self-corrects to the true status within one staleness-threshold window, without needing a restart.
- A session with a normally-functioning hook channel sees **zero** behavior change — no new false transitions, no regression of #638's fixed flakiness.
- The correction is visible to the user (session-warning notice), not silent.
- `Error` classification is never delayed or suppressed by this change.
- CLAUDE.md rule 12's cross-version manual test passes.
- All local gates pass: `cargo fmt --check`, `cargo clippy --workspace --all-targets --features e2e -- -D warnings`, `cargo xtask linkage-check`.

## Milestones

- [ ] M1 — Add the `EventSource` tag to the status-affecting `AgentEvent` path; thread it through `src/wrap.rs` (`classify_and_emit`, `Emitter::emit`) and `src/hook.rs` (`handle_hook`).
- [ ] M2 — `classify_and_emit` stops discarding non-`Error` events under `suppress_text_status`; always emits them tagged `TextHeuristic`.
- [ ] M3 — Daemon-side `last_hook_status_at` (or equivalent) per session, updated only by `Hook`-sourced events.
- [ ] M4 — Reconciliation policy: `Hook` events always apply; `TextHeuristic` events apply only once `last_hook_status_at` is stale past the threshold.
- [ ] M5 — Staleness threshold chosen empirically (see Decisions) and documented at its definition site.
- [ ] M6 — Session-warning notice surfaced when a `TextHeuristic` correction actually overrides a stale `Hook` status.
- [ ] M7 — Tests: state/protocol-level tests for the reconciliation policy (hook-recent + text-disagrees → text discarded and recorded, not applied; hook-stale + text-disagrees → text applied + notice fires; `Error` always applies regardless of staleness or source). An end-to-end real-agent L2 test reproducing #644's live condition may not be feasible on demand (we cannot force Codex's hook subsystem silent deterministically) — if so, record that as a known, accepted test-coverage gap covered instead by the state-level policy tests plus the live evidence already gathered in `.dot-agent-deck/644-live-finding-hook-subsystem-silent.md`.
- [ ] M8 — File the Codex-side hook-subsystem-silence defect with OpenAI/Codex separately, once we have enough evidence to describe a trigger (CLAUDE.md rule 19 doesn't apply directly since Codex isn't `vfarcic/dot-agent-deck`, but the same "don't block on it" principle does — this PRD's fallback ships regardless of that report's outcome).
- [x] M9 — Fold in the #644 diagnosis: **done** (2026-08-30, see Reframe above). #644's own root cause (the `ShellBusy`/argv-veto-gap race) is a separate, deterministic bug fixed directly under #644, not through this PRD. #644 stays open, tracked independently, and is not closed by this PRD landing — this PRD covers a different failure mode (total hook-subsystem silence) that #644's fix does not address.

## Key Files

- `src/wrap.rs` — `classify_and_emit`, `Emitter`/`Emitter::emit`, `suppress_text_status` wiring (`codex_spawn_prep`, the interactive-session path around line ~2351).
- `src/hook.rs` — `handle_hook`, `map_event_type`.
- `src/state.rs` — per-session status state; new `last_hook_status_at` field and the reconciliation policy in the event-apply path.
- `.dot-agent-deck/644-live-finding-hook-subsystem-silent.md` — the live evidence this PRD is built on.

## Test plan

To be produced and presented to the user as the standing test-plan gate (orchestrator workflow step 1) before any implementation delegation begins.
