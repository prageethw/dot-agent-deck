# PRD #370: Treat underlying shell activity as Working status inside a worker pane

**Status**: In progress — M1-M4 complete, M5 (docs/changelog) partially done (changelog fragment added; see M5 note)
**Priority**: Medium
**Created**: 2026-08-04 (issue filed) / 2026-08-05 (PRD written)
**GitHub Issue**: [#370](https://github.com/vfarcic/dot-agent-deck/issues/370)
**Related**: [#234](https://github.com/vfarcic/dot-agent-deck/issues/234) (`prds/234-screen-state-observation-hookless-agents.md`) — adjacent problem (hookless/redrawing-TUI agents), different mechanism (vt100 screen-diff vs. this PRD's PTY foreground-process-group check); do not conflate the two. `src/state.rs` (`SessionStatus`, `AppState::apply_event`), `src/hook.rs`, `src/wrap.rs` (`classify_line`), `src/platform/proc/{mod.rs,unix.rs}` (`AgentProcessGroup`, `pid_to_pgid`)

## Problem Statement

Pane status (`Idle`/`Working`/`Thinking`/`WaitingForInput`/etc.) is driven entirely by agent-emitted signals: Claude Code hook payloads (`src/hook.rs`), OpenCode/Pi's own event surfaces, or — for Codex and anything run via `dot-agent-deck wrap`— stdout-line pattern matching (`src/wrap.rs::classify_line`). There is no process-tree or PTY-foreground-process inspection anywhere in the codebase today; `src/platform/proc/*` exists solely for teardown (`killpg` on shutdown), not activity observation.

Confirmed scope with the user: when a role that already has hooks or a wrapper (e.g. `coder`, `release`) shells out to run something long-running — `cargo build`, `cargo test`, a release script — and no hook/wrapper-line event fires in between, the pane's status falls back to whatever it last was, typically `Idle`, while a command is visibly still executing in the pane. This is misleading: the user sees "Idle" on a pane that is plainly busy.

## Solution Overview

Add a supplementary, agent-agnostic activity signal: periodically poll each pane's PTY for its foreground process group (`tcgetpgrp`/`getpgid`, mirroring the pgid-resolution already used for teardown in `src/platform/proc/unix.rs`). If the foreground pgid differs from the shell's own pgid, a child command is actively running in the foreground of that pane. Feed this into the existing `SessionStatus` pipeline as a supplementary signal — not a replacement for hook/wrapper-derived events, but a fallback that keeps the pane out of a stale `Idle` while a foreground child process is alive and no more specific status is available.

**Decision needed at implementation start**: whether the foreground-process signal can ever downgrade a status set by a genuine agent event (e.g. an agent hook says `WaitingForInput` but a background child is still technically running) — default assumption is it should only ever promote `Idle`/stale states to `Working`, never override a more specific in-flight status like `WaitingForInput` or `Error`.

## Scope

**In Scope**: PTY foreground-process-group polling (Unix; extend `src/platform/proc/unix.rs` rather than duplicating pgid-resolution logic), a poll cadence tied to the existing tick loop, a supplementary status signal wired into `AppState::apply_event`'s consumers (or a parallel path feeding the same `SessionStatus`), unit test coverage for: foreground pgid differs from shell pgid → `Working`; foreground pgid equals shell pgid → no override; a genuine agent-emitted event (e.g. `WaitingForInput`) is not clobbered by a still-alive background child.

**Out of Scope**: Windows/non-Unix PTY foreground-process detection (platform gap noted as a risk, not solved here); the vt100 screen-diff mechanism from PRD #234 (separate, hookless-agent-scoped problem); changing hook/wrapper classification rules themselves.

## Technical Approach

Extend `src/platform/proc/unix.rs`'s existing pgid-resolution helpers (used today only for `killpg` at shutdown) with a `foreground_pgid(pty_fd)` query via `tcgetpgrp`. On each tick (or a coarser dedicated interval, to avoid syscall overhead on every render frame), compare each live pane's foreground pgid against its shell's own pgid (`pid_to_pgid` on the shell's pid). A mismatch is evidence of an active foreground child; surface it as a `Working`-equivalent `SessionStatus` unless a more specific status (`WaitingForInput`, `Error`, `PermissionRequest`) is already active from a genuine agent event. Precedence rules between this signal and agent-emitted events need to be made explicit in `AppState` so the two sources don't fight each other on every tick.

## Success Criteria

- A pane running a role agent that shells out to a long-running command (e.g. `cargo build`) shows `Working` for the duration of that command, even when no hook/wrapper-line event fires during it.
- A pane genuinely idle at its shell prompt (foreground pgid == shell pgid) is not falsely reported `Working`.
- A more specific agent-emitted status (`WaitingForInput`, `Error`, `PermissionRequest`) is never silently overridden by the foreground-process signal.
- No measurable per-tick performance regression from the added polling.

## Milestones

- [x] **M1 — Foreground-pgid query helper.** `foreground_pgid` added to `src/platform/proc/{unix,windows}.rs` (Unix: wraps `portable_pty::MasterPty::process_group_leader`, i.e. `tcgetpgrp`; Windows: unconditional `None`, the trait doesn't expose the method there at all) plus `RunningAgent::shell_foreground_busy` in `src/agent_pty.rs`. Two tests: a raw-`openpty` mechanism test and an end-to-end test through the real `AgentPtyRegistry::spawn_agent` path.
- [x] **M2 — Daemon-side comparison and status signal.** Deviates from the original "wire into the render tick" plan: a dedicated `run_shell_activity_monitor` daemon task (`src/daemon.rs`) polls `AgentPtyRegistry::shell_foreground_busy_snapshot()` every 500ms (edge-triggered — only emits on a busy/idle transition per pane) and synthesizes `EventType::ShellBusy`/`ShellIdle` through the SAME pipeline real hook events use (`event_tx` broadcast + `AppState::apply_event`), rather than a separate parallel status path. This reaches an *attached* TUI (a separate process over the daemon's wire), which the render-tick approach could not have — the wire touch required adding `EventType::ShellBusy`/`ShellIdle` + a `#[serde(other)] Unknown` catch-all (mirroring PRD #201's `AgentType` retrofit) and bumping `PROTOCOL_VERSION` 6 → 7 (`src/daemon_protocol.rs`, `src/event.rs`). Precedence rules live in `AppState::apply_event`'s new arms plus `SessionState::shell_synthetic_working` (`src/state.rs`): `ShellBusy` only promotes a stale `Idle`/`Unknown`; `ShellIdle` only reverts a promotion THIS mechanism made; any real event clears the marker so a stale `ShellIdle` can never revert a real status.
- [x] **M3 — Status integration.** No separate code path per consumer — the synthetic events flow through the exact same `SessionStatus` field every hook/wrapper-derived event does, so tab coloring / footer / dashboard stats need zero changes.
- [x] **M4 — Test coverage.** `src/state.rs`: `shell_busy_idle_promote_and_revert_without_clobbering_real_status` (promote/revert/non-clobber precedence) and `event_type_unknown_string_deserializes_to_the_catch_all`. `src/daemon.rs`: `shell_activity_monitor_reflects_a_real_foreground_command` — a real `/bin/sh` pane, a real foreground `sleep`, zero agent/hook involvement, proving the whole pipeline end to end.
- [ ] **M5 — Docs and changelog.** `changelog.d/370.feature.md` added. No `.breaking.md` fragment — per `docs/develop/versioning.md`, that type is reserved for *semantic* breaks behind a stable wire; this is a *structural* wire-shape change, already caught mechanically by the `PROTOCOL_VERSION` bump. No `docs/develop/agent-adapters.md` addition — that doc enumerates per-agent `IntegrationStrategy` variants, and this mechanism is agent-agnostic/daemon-level, not a new strategy; forcing it in would conflate two different concepts. **Still open: the cross-version manual test** (previous-release daemon + this branch's TUI, confirm delegate/hooks still flow) required by CLAUDE.md rule 12 before merge.

## Risks

- **Platform scope.** `tcgetpgrp`/`getpgid` are POSIX/Unix-specific; this PRD's mechanism does not cover Windows PTYs. If Windows support matters, a follow-up or a documented gap is needed.
- **Signal precedence bugs.** Getting the interplay wrong between this new agnostic signal and existing agent-emitted `SessionStatus` values risks flapping (`Working` ↔ `Idle` on every tick) or masking real `WaitingForInput`/`Error` states — needs careful precedence rules and test coverage, not just "OR them together."
- **Polling overhead.** Per-pane, per-tick syscalls need to stay cheap; may warrant a coarser interval than the main render tick if profiling shows cost.

## Open Questions

Resolved during M2:

- **Poll cadence** — resolved: a dedicated daemon-side 500ms interval (not the render tick — see M2), first-cut, adjustable if profiling ever says otherwise.
- **Precedence rules** — resolved: `SessionState::shell_synthetic_working` (see M2) — `ShellBusy` only promotes `Idle`/`Unknown`; `ShellIdle` only reverts its own promotion; any other event clears the marker.
- **Wire-protocol approach for M2** — resolved with the user: new `EventType` variants + `PROTOCOL_VERSION` bump (not a reconnect-only, no-bump alternative), so an *already-attached* TUI updates live, not only on reconnect.

- **Cross-version check** (CLAUDE.md rule 12) — built `v0.35.5` (the previous release tag) from source and confirmed via `dot-agent-deck daemon hello` that its `server_version` is `6` against this branch's `7`, exactly the intended bump. Risk is asymmetric here (unlike PRD #201's case): an OLD daemon never emits `ShellBusy`/`ShellIdle` (the feature doesn't exist server-side), so a NEW client attaching to an OLD daemon is safe by construction — existing flows are unaffected, the new feature is just silently absent. The protected direction (an OLD client meeting a NEW daemon) is guarded by two PRE-EXISTING, unmodified mechanisms: the ssh-remote `connect` flow's exact-match `PROTOCOL_VERSION` refusal, and the local PRD #103 build-mismatch nudge. A full interactive PTY click-through (per the doc's literal steps) hit tooling friction in this environment (no `tmux`; `script(1)` with redirected stdin did not yield a usable capture) and was not completed live — the verification above is static/handshake-level rather than a full delegate/hooks round-trip against the old daemon. Flagging this explicitly for reviewer awareness rather than silently calling it done.
