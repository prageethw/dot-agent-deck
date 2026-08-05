# PRD #370: Treat underlying shell activity as Working status inside a worker pane

**Status**: In progress — M1 complete
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
- [ ] **M2 — Tick-driven comparison and status signal.** Per-pane foreground-vs-shell pgid comparison wired into the tick loop; precedence rules against agent-emitted `SessionStatus` values made explicit.
- [ ] **M3 — Status integration.** Signal reaches the same `SessionStatus` consumers as hook/wrapper-derived events (tab coloring, footer, etc.) without a separate code path per consumer.
- [ ] **M4 — Test coverage.** Unit tests for pgid-mismatch → `Working`, pgid-match → no override, and non-clobbering of a genuine in-flight agent status.
- [ ] **M5 — Docs and changelog.** Note the new signal source in relevant developer docs (`docs/develop/`) and a changelog fragment.

## Risks

- **Platform scope.** `tcgetpgrp`/`getpgid` are POSIX/Unix-specific; this PRD's mechanism does not cover Windows PTYs. If Windows support matters, a follow-up or a documented gap is needed.
- **Signal precedence bugs.** Getting the interplay wrong between this new agnostic signal and existing agent-emitted `SessionStatus` values risks flapping (`Working` ↔ `Idle` on every tick) or masking real `WaitingForInput`/`Error` states — needs careful precedence rules and test coverage, not just "OR them together."
- **Polling overhead.** Per-pane, per-tick syscalls need to stay cheap; may warrant a coarser interval than the main render tick if profiling shows cost.

## Open Questions

1. **Poll cadence** — every render tick, or a coarser dedicated interval? Needs a decision once M2 profiling data exists.
2. **Precedence rules** — exact rule set for when the foreground-process signal may vs. may not override an existing `SessionStatus` (see Solution Overview's "Decision needed at implementation start").
