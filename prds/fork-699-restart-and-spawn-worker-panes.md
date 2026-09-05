# PRD fork#699 — Restart a crashed worker pane, or spin up an additional configured-but-unspawned role, without restarting the orchestration tab or the deck

**Issue:** [prageethw/dot-agent-deck#699](https://github.com/prageethw/dot-agent-deck/issues/699)
**Priority:** High
**Status:** Planning
**Related:** upstream [#868](https://github.com/vfarcic/dot-agent-deck/issues/868) (narrower half — restart only; this PRD is broader), PRD #140 (per-tab orchestration identity token), fork issue #465 (worker-exit signal, the existing notice this PRD's self-healing loop builds on)
**Fork-only?** No — this is a general capability gap upstream also wants (per open #868). Building here first per rule 19 (this fork is never blocked on an upstream decision), offering upstream once shipped — this design deliberately diverges from #868's own proposal (see Decisions).

## Problem Statement

Today, if a worker/role pane in a running orchestration crashes, or a running orchestration needs one more role that wasn't part of its initial role set, the only recovery path is a human noticing from the TUI and manually restarting the pane or the whole tab. There is no CLI-level way for an orchestrating agent to detect this itself or fix it — a `delegate` call against a dead pane has no documented failure mode, and there is no `pane restart`/`pane spawn` subcommand at all.

This stalls a multi-agent fix-and-recheck loop until a human intervenes, and is easy to miss if nobody is watching the TUI at that moment (upstream #868's own motivating incident: a Codex-backed `reviewer` role died mid-task with no CLI-level recovery path).

### What already exists, and what's actually missing

This is not "build restart from scratch" — most of the machinery already exists, just not wired to be callable on demand:

- **Crash vs. deliberate-close is already distinguished.** `pump_reader`'s EOF branch (`src/agent_pty.rs`, gated on `is_agent_still_registered` — `close_agent`/`respawn_agent_for_pane` remove the registry entry *before* killing the child, so EOF correctly reads `false` for a deliberate action, `true` for a natural crash) already knows the difference. It's just never surfaced or acted on beyond existing delegation bookkeeping (`sweep_delegations_on_exit`) and the existing "exited without work-done" notice (fork issue #465, `deliver_worker_exited_notice`) delivered straight into the orchestrator's own pane.
- **The respawn primitive already exists.** `respawn_agent_for_pane`/`respawn_agent_for_pane_declared` (`src/agent_pty.rs`) already atomically removes the registry entry, kills the old child, and spawns a fresh one into the same pane — deliberately not filtered on `exited`, so it already handles a dead-but-not-yet-reaped agent correctly. Its only caller today is `handle_delegate` (via a `clear = true` delegate), so there's no way to invoke it standalone without also sending a work payload.
- **Role→pane spawning is batch-only.** `TabManager::open_orchestration_tab_with_isolated_clone_origin` (`src/tab.rs`) spawns every configured role at once when a tab opens; the per-role bookkeeping (role_index/`TabMembership` tagging, spawn-order, the `ui.rs` post-spawn registration loop) is inlined in that batch loop, not factored into a callable "spawn one more role" primitive.

## Decisions

| Question | Decision |
|---|---|
| Wire protocol | Ride the existing **unversioned hook socket** (`DaemonMessage` enum, same channel `Delegate`/`Dispatch`/`WorkDone`/`GetSeed` already use) — **not** a new `AttachRequest` variant, which would require a `PROTOCOL_VERSION` bump per that protocol's own doc comment. `GetSeed` is the existing precedent for "new hook-socket message, no version bump." |
| Auto-detection scope | **Process exit only** (crash/unexpected exit) — not wedged-but-alive detection. No existing plumbing for the latter; would need new liveness heuristics, out of scope. |
| Auto-detection action | **Make crash state visible/queryable, don't auto-restart.** Unlike upstream #868's proposed `auto_restart` config flag, this fork already delivers an "exited without work-done" notice straight into the orchestrator's own pane (fork #465) — the orchestrator can read that notice and decide to call `pane restart` itself. No daemon-side policy flag needed; this PRD only adds the "do something about it" half of a pattern whose "notice" half already ships. |
| Trigger surface | **CLI subcommand only** for v1 (`dot-agent-deck pane restart <role>` / `pane spawn <role>`) — no TUI keybind. Both subcommands are `DOT_AGENT_DECK_PANE_ID`-scoped exactly like `Delegate`/`Dispatch`, callable by a human or by the orchestrating agent itself. |
| Spin-up duplicate-role semantics | **Not-yet-live role config entries only.** `pane spawn <role>` succeeds only when `<role>` exists in `.dot-agent-deck.toml` and has no live pane yet in this orchestration instance. A true second pane under an already-running role name is out of scope — the codebase's `role_pane_ids: Vec<String>` is index-aligned one-to-one with `config.roles`; true duplication would need that to become one-to-many, plus a `delegate --to <role>` disambiguation rule and reworked drift detection. A distinct config entry (e.g. `reviewer2`) already covers "I want a second reviewer" today with zero new capability. |
| Restarting a healthy (non-crashed) pane | **Refuse unless `--force`.** `respawn_agent_for_pane` doesn't filter on `exited`, so without a guard `pane restart` against a live, healthy pane becomes an accidental "force-kill a working agent" command. |
| Orchestration-instance scoping | Resolve the target role's pane **within the calling pane's own orchestration instance** (PRD #140's `orchestration_id` token), never by a bare `(role name, cwd)` pair — two parallel same-named orchestration tabs must not cross-wire. |

## Design

### Half 1 — restart an existing crashed pane

1. **Crash visibility** (`src/agent_pty.rs`): extend `pump_reader`'s natural-exit branch to mark the dying `AgentRecord` with an additive, optional crash-signal field (`#[serde(default, skip_serializing_if = "Option::is_none")]`, matching every other optional field already on that struct — no protocol bump). Surfaced over `list_agents`. Guarded behind the same `is_agent_still_registered` check that already distinguishes crash from deliberate close, avoiding a TOCTOU race with a concurrent manual restart.
2. **`Commands::Pane::Restart` CLI** (`src/main.rs`, modeled on the existing `Delegate` arm): reads `DOT_AGENT_DECK_PANE_ID`, sends a new `DaemonMessage::RestartRole { role, .. }` via the existing request/reply hook-socket helper (a restart needs a real answer — not-found / not-actually-crashed / restarted — not fire-and-forget).
3. **Daemon-side handling** (new `handle_restart_role_with_state` in `src/state.rs`, mirroring `handle_delegate_with_state`): validate the calling pane, resolve the target role's pane within the same orchestration instance, refuse if the target isn't actually crash-flagged (unless `--force`), call the existing `respawn_agent_for_pane`, re-register the fresh pane's orchestration role (reusing the same registration call the delegate-triggered `clear=true` respawn path already uses), reply with a structured outcome.
4. Wire the new `DaemonMessage::RestartRole` arm into `src/daemon.rs`'s hook-socket message loop alongside the existing `Delegate`/`Dispatch` arms.

### Half 2 — spin up an additional worker

5. **`Commands::Pane::Spawn` CLI + daemon handling** (new `handle_spawn_role_with_state` in `src/state.rs`): resolve the calling pane's orchestration cwd + `orchestration_id`, re-read `.dot-agent-deck.toml` at that cwd, find the requested role by name, refuse clearly if it's not in the on-disk config or already has a live pane in this instance. Call `AgentPtyRegistry::spawn_agent` directly with `TabMembership::Orchestration { orchestration_id: <same as caller>, role_index: <position in the freshly-read config>, .. }`. Register into daemon-side `pane_role_map`/`pane_orchestration_map` — this alone makes `delegate --to <newrole>` work immediately, independent of any attached TUI. Broadcast a small `BroadcastMsg::OrchestrationSurface` carrying just this one new role.
6. **TUI live-append into the already-open tab** (`src/ui.rs`): `surface_one_orchestration`'s `already_built` guard only checks whether the surfaced pane ids already belong to a tab — since the new role's pane id is novel, it would currently build a duplicate second tab for the same orchestration. Fix: look up an existing `Tab::Orchestration` for `(cwd, name)` first; if found, call a new `TabManager::add_role_to_existing_orchestration` that grows `role_pane_ids`/`role_statuses`/`config.roles` (`src/tab.rs`) and mirrors the existing per-pane registration bookkeeping, rather than falling through to tab creation.
7. **Config-drift correctness**: verify a save/restore cycle including an added role doesn't false-positive as drift in `resolve_orchestration_for_restore` (`src/ui.rs`) — since the live tab's `config.roles` is correctly extended in-memory by step 6, the next session-save snapshot naturally includes it. Also check the reconnect-hydration path's role-index bounds check for a role added while no TUI was attached at all.

## Milestones

- [ ] M1 — Crash visibility: `pump_reader`'s natural-exit path marks the dying `AgentRecord` with a crash-signal field, surfaced over `list_agents`. Test: kill a worker's child process, assert the daemon's next `list_agents` reports the crash state; assert a deliberately-closed pane does not.
- [ ] M2 — `pane restart <role>` CLI + daemon handling (`Commands::Pane::Restart`, `DaemonMessage::RestartRole`, `handle_restart_role_with_state`), calling the existing `respawn_agent_for_pane` and re-registering the role. Test: crash a worker, run the CLI from the orchestrator's env, assert the pane comes back alive under the same `pane_id_env` and a subsequent `delegate --to <role>` reaches it.
- [ ] M3 — `pane spawn <role>` CLI + daemon handling (`Commands::Pane::Spawn`, `DaemonMessage::SpawnRole`, `handle_spawn_role_with_state`): config re-read, role lookup, refuse-if-already-live, `spawn_agent` with correct `TabMembership`/`orchestration_id`, role-map registration. Test: add a role to `.dot-agent-deck.toml` after opening the orchestration, invoke the CLI, assert `delegate --to <newrole>` reaches the new pane even with no TUI attached.
- [ ] M4 — TUI live-append: fix `surface_one_orchestration`'s duplicate-tab gap; new `TabManager::add_role_to_existing_orchestration`. **Requires PTY-attached L2 coverage** (CLAUDE.md rule 4 — major user-facing TUI-visible behavior): open a tab, spin up a role via CLI, assert the new role's card appears inside the SAME tab, not a duplicate.
- [ ] M5 — Config-drift correctness under a live-grown tab: regression coverage for a save/restore cycle including an M3/M4-added role, and for the reconnect-hydration path when a role was added while no TUI was attached at all.
- [ ] M6 — Edge-case hardening: `pane restart` refuses a non-crashed pane without `--force`; `pane spawn` refuses an already-live role; verify `AGENT_TERMINATE_GRACE`/kill-before-reap invariants are inherited for free (both paths call existing primitives, don't reimplement termination); size the CLI's round-trip timeout to not time out mid-grace-period; confirm the M1 crash-flag write and a concurrent manual restart share the same `is_agent_still_registered` gate (no TOCTOU).
- [ ] M7 — Tests: L1 unit coverage for both new `handle_*_with_state` functions (unknown pane, non-orchestration pane, unknown role, already-crashed/already-live races); L2 PTY-attached e2e for both subcommands' full round trip (rule 4 — new user-facing behavior even though CLI-triggered).
- [ ] M8 — Docs: both subcommands' help text + `docs/`, explicitly documenting the self-healing pattern (orchestrator reads the existing fork-#465 crash notice, then calls `pane restart` itself) as the primary intended usage, not just a human-operator escape hatch.
- [ ] M9 — Offer upstream per rule 19 once merged: comment on upstream #868 with the shipped approach, naming the deliberate divergence (no `auto_restart` flag — this fork's existing crash-notice-to-orchestrator pattern already covers that need) and linking the fork's implementation.

## Test plan

L1 (widget/unit) covers the daemon-side handler logic (pane/role resolution, refusal paths) — no PTY needed for those branches. L2 (PTY-attached, real spawned binary) is required for M4 specifically per CLAUDE.md rule 4, since a live TUI correctly absorbing a dynamically-added role into an existing tab (not a duplicate) is fundamentally a rendered-UI behavior no unit test can exercise. M2/M3's end-to-end round trip (CLI → daemon → pane state change, confirmed via a follow-up `delegate`) also wants L2 coverage per the same rule, even though triggered via CLI rather than a TUI keystroke, since it's genuinely new user-facing behavior.

## Out of scope

- Wedged-but-alive pane detection (no existing plumbing; would need new liveness heuristics — separate PRD if wanted).
- A TUI keybind for restart/spin-up (CLI-only for v1; a keybind could reuse the same daemon-side handlers later with no re-architecture).
- True duplication of an already-running role (second pane, same role name) — would need `role_pane_ids` to become one-to-many plus a `delegate` disambiguation rule; a distinct config entry already covers the practical need today.
- Upstream #868's `auto_restart` config flag — deliberately not adopted; this fork's existing crash-notice-to-orchestrator pattern (fork #465) already gives the orchestrator what it needs to decide to restart itself.
