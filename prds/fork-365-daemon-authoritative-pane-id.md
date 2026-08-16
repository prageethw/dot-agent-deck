# PRD fork#365: Make `pane_id` daemon-authoritative

**GitHub Issue**: [fork #365](https://github.com/prageethw/dot-agent-deck/issues/365)

**Priority**: High

**Status**: M1/M2/M3 implemented and landed on [PR #424](https://github.com/prageethw/dot-agent-deck/pull/424); M4 (offer upstream) not yet done. Filed out of [fork #358](https://github.com/prageethw/dot-agent-deck/issues/358), whose first fix attempt was withdrawn after review; see "What has already been tried" for why that attempt is the argument for this one.

**This PRD does not close #358.** #358's two halves are this work (identifier reuse) and [upstream #524](https://github.com/vfarcic/dot-agent-deck/issues/524) (stale entries surviving pane death). Both must land before #358 is answered.

## Problem Statement

`pane_id` is the daemon's routing key, and the daemon does not allocate it.

Each attached TUI mints it locally from a per-process counter:

```rust
// src/embedded_pane.rs:812
fn allocate_id(&self) -> String {
    let mut id = self.next_id.lock().unwrap();
    let current = *id;
    *id += 1;
    current.to_string()
}
```

`next_id` starts at `1` (`embedded_pane.rs:424`) on a field of `EmbeddedPaneController`, which is constructed **once per attached TUI client process** (`main.rs:1923`, the only non-test construct site). It is reconciled against the daemon exactly once — at that TUI's startup, in `hydrate_from_daemon` (`embedded_pane.rs:1161`), which calls `ListAgents` and bumps `next_id` past the largest numeric id it sees (`embedded_pane.rs:1290`). After that single bump there is **no further coordination**: not with the daemon, and not with any other attached TUI.

Two properties make this the worst available shape for a primary key.

**It is chosen by parties that cannot see each other.** Two TUI client processes never learn what the other allocated. Nothing in the protocol tells one that `"3"` is taken.

**The one reconciliation point is blind to exactly the dangerous case.** `ListAgents` returns `registry.agent_records()`, and `agent_records` **filters out exited agents** (`agent_pty.rs:5175`, pinned by `agent_records_filters_exited_entries`). A stale registration — a pane that died without going through `StopAgent`, so its routing entries survive — is invisible to hydration by construction. A fresh TUI therefore re-mints precisely the low-numbered ids most likely to be stale.

### What depends on the key

Every mechanism that decides who receives a delegate and whose report is whose:

| Consumer | Where |
|---|---|
| `pane_orchestration_map` — routing identity for `handle_delegate` / `handle_work_done` | `src/state.rs:541` |
| `pane_role_map`, `pane_cwd_map`, `orchestrator_pane_ids` | `src/state.rs:516-521` |
| `AgentPtyRegistry`, keyed on `pane_id_env` | `src/agent_pty.rs` |
| the on-disk report path, via `pane_digest_hex(pane_id)` | `src/state.rs:734` |
| `DOT_AGENT_DECK_PANE_ID`, read by hooks and by `issue claim`'s identity resolver | `src/issue_claim.rs` |

### The structural problem, which matters more than any single symptom

The daemon is the only party that sees every pane — live and exited, across every attached client — and it is the one party not allowed to name them. Every consumer above is therefore forced to reason about liveness on its own, and each one that gets it wrong produces a different symptom. Three have already been found in three different subsystems:

- **fork #56** (closed) — `build_pane_status` silently picks an arbitrary status when two sessions share a pane_id.
- **upstream #398** (closed) — `build_pane_status`: duplicate pane_id sessions collide non-deterministically.
- **fork #358** — routing and report-path collisions.

Fixing consumers one at a time is what this PRD exists to stop.

## Evidence

Observed on this machine while working #358. The report path `.dot-agent-deck/work-done-coder-07f88b07b4b9fcbf.md` is `pane_digest_hex("19")`. Its archive chain holds four unrelated orchestrations' reports across six days — a `sync/upstream-2026-08-12` cherry-pick, fork #351, and fork #226 twice — with 18 archived generations back to Aug 9.

The user-visible consequence: an orchestrator working #226 was handed #351's completion report, and its own delegation was silently lost. `delegate` exited 0.

## What has already been tried, and why it is not enough

**`spawn_agent`'s duplicate rejection** (`agent_pty.rs:3964`) refuses a spawn while an agent holding that `pane_id_env` is `!exited`. This is real and load-bearing — it is why two *concurrent* orchestrations cannot share an id. It says nothing about a dead incumbent, which is the common case.

**PRD #140's `OrchestrationIdentity::Instance`** (`cb307ca7`) replaced the `(name, cwd)` routing identity with a per-tab token, so routing is scoped by orchestration rather than by a tuple two tabs could share. That fixed identity *scoping*; the pane key underneath it is still client-minted.

**fork #358's first attempt** refused a registration whose `pane_id` was already bound to a different `OrchestrationIdentity`. **Withdrawn before merge.** Reviewer and auditor independently established that it could only ever produce false refusals: registration runs only after a successful spawn, so a live incumbent means the newcomer never registers at all, and a dead incumbent means the guard blocks a legitimate registration. Against the real pane-`"19"` history it would have blocked a coder from receiving any delegate.

That withdrawal is the argument for this PRD. Every fix that leaves the identifier client-chosen has to re-derive liveness at each consumer, and the last one to try got it backwards while passing a full green CI board.

## Solution Overview

Make the daemon assign `pane_id` and return it, rather than the client choosing it. The daemon is the only party with the complete view.

## Scope

### In Scope

- Daemon-side allocation of `pane_id`, returned to the client on the spawn path.
- Retiring `EmbeddedPaneController::allocate_id` and its `next_id` counter, plus the hydration bump that exists only to service it.
- `PROTOCOL_VERSION` bump and a stated, tested cross-version behaviour (rule 12).
- A test proving two independently-attached clients cannot obtain the same id.

### Out of Scope

- **Stale routing entries surviving pane death** — [upstream #524](https://github.com/vfarcic/dot-agent-deck/issues/524). Unique ids make a stale entry *harmless* rather than collidable; they do not evict it. Both are needed for #358.
- Unbounded growth of pane-keyed maps — [upstream #542](https://github.com/vfarcic/dot-agent-deck/issues/542).
- Changing what `pane_digest_hex` does. With unique ids its input stops colliding, so it needs no change.
- Re-litigating `spawn_agent`'s duplicate rejection, which stays as a backstop.

## Open Design Questions

These are genuinely open and should be settled in M1, not assumed.

**Assign vs. validate.** Does the daemon mint and return the id, or does the client propose one the daemon may reject or rewrite? Assignment is simpler to reason about and removes the class outright; validation is a smaller protocol change and keeps a client-visible id stable across the call.

**Shape.** A daemon-side monotonic counter is the obvious answer and is **not sufficient alone**: `next_pane_id` (`src/spawn.rs`) already uses a process-global `AtomicU64` that resets to zero on every daemon restart, and CLAUDE.md rule 23 records that recycling as the reason `issue claim` refuses to anchor identity on a pane id. Prior art in-tree: `mint_orchestration_id` (`agent_pty.rs:426`) combines a per-process nonce (PID + nanosecond timestamp) with a monotonic sequence, and carries a 1000-mint collision test.

**Legibility.** Bare integers are short and appear in logs, filenames and `DOT_AGENT_DECK_PANE_ID`. A UUID is unique but hostile to read. A prefixed scheme like `spawn.rs`'s existing `sched-foo-3-r0` is a middle path worth weighing.

**Cross-version behaviour.** Old TUI against a new daemon: degrade to client-chosen ids, or refuse the handshake? Note fork #17 / upstream #405 already cover a local daemon attach ignoring `PROTOCOL_VERSION`, which bounds how much can be assumed here.

**Migration reach.** `pane_id` appears in `DOT_AGENT_DECK_PANE_ID` (hooks, `issue claim`), in report filenames, and in on-disk artefacts from previous runs. Changing its *shape* reaches beyond the daemon; changing only its *source* may not.

## M1 Decisions

Settled 2026-08-16. Each answer cites the evidence it rests on; M2/M3 should not need to re-derive any of this.

### 1. Assign vs. validate — **assign**

The daemon mints `pane_id` and returns it; the client no longer proposes one at all.

`pane_id` is not even a first-class `StartAgent` field today — it rides inside `env: Vec<(String, String)>` as `DOT_AGENT_DECK_PANE_ID` (`embedded_pane.rs:863` sets it before the call; `daemon_protocol.rs:1339-1343` extracts it server-side into `pane_id_env`, gated by `is_valid_pane_id_env`). That extraction is already a *validate* shape in miniature — and the PRD's own "What has already been tried" section shows exactly why validate-only doesn't work: fork #358's withdrawn attempt added a validate-style guard (refuse a registration whose id was already bound to a different identity) and it could only ever produce false refusals, because a live incumbent means the newcomer never reaches registration and a dead incumbent means the guard blocks a legitimate one. Validation can reject a bad id; it cannot supply a good one, and supplying a good one is the actual problem.

Assignment is also not a new pattern being introduced — it already exists for every *headless* spawn path. `spawn.rs`'s `next_pane_id` (`spawn.rs:1961-1977`, called from the scheduler/dispatch/issue-dispatch fire paths at `spawn.rs:447` and `spawn.rs:531`) is 100% daemon-side minting today; no client proposes an id for those panes at all. This PRD extends that existing daemon-side pattern to the one path that doesn't yet have it — the TUI-attach `StartAgent` path — rather than inventing a second mechanism.

Concretely, for M2: `AttachRequest::StartAgent` needs no new *input* field (the client stops sending `DOT_AGENT_DECK_PANE_ID` in `env`, or the daemon simply ignores/overrides it if sent, per the cross-version note below). The daemon mints the id at the same point it currently reads `pane_id_env` out of `env` (`daemon_protocol.rs:1339`), writes `DOT_AGENT_DECK_PANE_ID` into the env vec it hands to `spawn_agent` (`SpawnOptions.env`, `agent_pty.rs:795`, which is what actually reaches the child process's environment — confirmed this is daemon-controlled, not client-forwarded-verbatim, so redirecting its source costs nothing structurally), and returns the minted value on `AttachResponse` alongside the existing `id` field (`daemon_protocol.rs:598`, currently populated via `AttachResponse::with_id` at the `StartAgent` response site). `spawn_agent`'s duplicate-`pane_id_env` rejection (`agent_pty.rs:3964`) stays exactly as-is as a backstop — it now defends against a daemon-minting bug instead of a client-minting race, which is a strictly smaller threat surface.

### 2. Shape — **daemon-lifetime nonce + monotonic sequence, prefixed by spawn origin**

Format: `{origin-prefix}{process-nonce}-{seq}` — the same recipe `mint_orchestration_id` already uses (`agent_pty.rs:426-440`: a per-process nonce hashed from PID + nanosecond epoch time, combined with a monotonic `AtomicU64` sequence, `format!("orch-{nonce:016x}-{seq}")`), reusing its 1000-mint collision test as the pattern to pin the new minter against.

A bare monotonic counter is ruled out on direct in-tree precedent, not just by inference: `spawn.rs`'s existing daemon-side `next_pane_id` already uses exactly that shape — a process-global `AtomicU64` (`PANE_COUNTER`, `spawn.rs:82`) that resets to 0 on every daemon restart — and it is *already* the mechanism CLAUDE.md rule 23 names as "dangerous for identity." There is a second, independent confirmation of the same failure mode in this codebase: `issue_claim.rs`'s identity resolver tried anchoring on `DOT_AGENT_DECK_PANE_ID` in an earlier round and explicitly rejected it for the identical reason — "round 2 — a small daemon-scoped integer that recycles across a daemon restart" (`issue_dispatch.rs:457-458`). Two independent subsystems hit the same wall; the nonce-based shape is what both eventually needed.

Why the nonce (not just a wider counter, e.g. a UUID or a random 64-bit int): the PRD's own evidence shows the collision that actually happened (six days, 18 archived report generations) crossed daemon restarts, not just concurrent same-lifetime clients — so uniqueness has to survive a restart, and a counter alone cannot do that without persisting state to disk (which nothing in this codebase does for the registry — see the Migration reach note below). A nonce hashed from PID + nanosecond timestamp at first use, exactly as `mint_orchestration_id` does, makes two different daemon processes (including two runs of the same daemon across a restart) astronomically unlikely to mint the same nonce, while the per-lifetime `AtomicU64` sequence guarantees no two mints *within* one daemon process ever collide. A pure random UUID would achieve similar uniqueness but fails the legibility need below and has no in-tree precedent or existing test harness the way `mint_orchestration_id` does.

Legibility and the `sched-` prefix: `pane_id` appears in logs, filenames, and `DOT_AGENT_DECK_PANE_ID`, so bare-nonce-only ids (`orch-3f8a...-9`) are a legibility regression from today's readable `sched-<task-name>-<n>` scheme (`spawn.rs:74-78`, `SCHEDULE_PANE_ID_PREFIX`). That prefix is not decorative — `ui.rs:902` uses `starts_with(SCHEDULE_PANE_ID_PREFIX)` to detect schedule-owned panes for the manager dialog's live-status check, so it is a **load-bearing consumer**, not a display nicety, and M2 must preserve it (or replace the string-prefix sniff with a daemon-tracked field — a legitimate M2 design choice, but out of scope for this decision). The recommended shape therefore keeps a human-legible origin prefix (`sched-`, or a new one for plain TUI-spawned panes, e.g. `pane-`) ahead of the nonce+sequence suffix, so a reader can still tell a pane's origin at a glance while the suffix carries the collision-resistance. All of this fits comfortably inside `is_valid_pane_id_env`'s existing charset (ASCII alphanumeric, `_`, `-`) and 64-byte cap (`agent_pty.rs:198-214`) — `mint_orchestration_id`'s own output is ~30 bytes, well under the limit — so no validator change is needed.

### 3. Cross-version behaviour — **refuse the handshake, matching the codebase's only existing policy**

There is no precedent anywhere in this codebase for "degrade to client-chosen ids," and adding one here would be a new failure mode, not a conservative fallback. Every `PROTOCOL_VERSION` consumer refuses on any mismatch, both directions, with no partial-compatibility path:

- **Local attach** (`build_version_handshake.rs`, fork #17): `ensure_compatible_daemon_or_die` refuses on `probe.response.server_version != Some(PROTOCOL_VERSION)` — exact equality, not a floor (`build_version_handshake.rs:237-239`) — then recovers by restarting the daemon to match the attaching binary's own version: silently if no agents are running, via a TTY consent prompt if agents are live (naming them), or by exiting non-zero on a non-TTY with no agents to lose (`build_version_handshake.rs:26-44`). This is the scenario the open question names ("old TUI against a new daemon") — on a single machine it self-resolves by forcing the *daemon* down to whichever binary's TUI happens to launch it, never by the client silently accepting a shape it doesn't understand.
- **Remote/SSH attach** (`connect.rs`, `probe_remote_protocol`): refuses with `RemoteConnectError::ProtocolMismatch` on any `server_version != PROTOCOL_VERSION`, including "remote too old to answer `daemon hello` at all" (`connect.rs:545-573`), with no daemon-restart escape hatch since the remote can't be recycled from the laptop — the user is pointed at `remote upgrade` instead.

So this PRD's decision is the same as every prior protocol bump: **bump `PROTOCOL_VERSION`, refuse the mismatched pairing, let the existing recovery paths (local restart-to-match / remote upgrade-hint) do their job.** No new degrade path needs to be designed or tested. Per rule 12's bump policy, this is a **minor** bump (0.x, breaking).

Whether the new `pane_id` field on `AttachResponse` is technically wire-additive (it would be, per the `#[serde(default, skip_serializing_if = "Option::is_none")]` pattern used for every other optional field on that struct) is beside the point: this is the "same-wire, different-*meaning*" semantic break CLAUDE.md rule 12 calls out by name — an old client that doesn't know to read a returned `pane_id` keeps calling its own now-deleted `allocate_id()` (M2 retires it) or, if left in place, keeps minting client-side ids the new daemon never asked for and doesn't recognize as authoritative. The bump is what makes the refuse-and-recover machinery above actually trigger for this change instead of silently letting a stale client limp along in the exact broken mode this PRD exists to close.

### 4. Migration reach — **changing the source alone does not require touching any downstream consumer**

Enumerated every production consumer of `pane_id`/`DOT_AGENT_DECK_PANE_ID` in the tree (`grep -rn "DOT_AGENT_DECK_PANE_ID" src/*.rs`, cross-checked against the PRD's own "What depends on the key" table). Every one of them treats the value as an **opaque string** — none parses its shape, none assumes it's numeric, none hardcodes a length — so a daemon-minted value in the shape decided above passes through every consumer unchanged, confirmed consumer-by-consumer:

- **`src/hook.rs:429,530,1908,1937`** — reads `DOT_AGENT_DECK_PANE_ID` via `std::env::var(...).ok()` and stores it verbatim on the outgoing `AgentEvent`. No parsing (`pane_id_propagated_from_env_claude_code`/`_opencode` tests pin exactly this: set the env var to an arbitrary string, assert it round-trips). **No change needed.**
- **`src/issue_claim.rs:154-186`** (`resolve_caller_identity`) — checks only *presence* of the env var (even a blank value counts, per `issue/claim/006`), never its content; the actual identity anchor is the worktree's own path + branch, not the pane id's value (`issue_claim.rs:134-153`). This is the same conclusion the PRD's Evidence section implies but stronger: `issue_claim` deliberately stopped trusting `pane_id`'s *value* for identity in an earlier round specifically because of its recycling problem (see Q2 above) — it now only cares whether the variable is set. **No change needed**, and this holds regardless of source *or* shape.
- **`src/main.rs`** (`delegate`, `work-done`, `get-seed`, and sibling CLI subcommands, e.g. `main.rs:1116-1123`) — reads the env var as a plain `String` and forwards it verbatim into daemon RPC requests (`WriteAndSubmit { pane_id, .. }` etc.). **No change needed.**
- **`src/wrap.rs:1049`** — same opaque read-and-forward pattern for the codex/claude process-wrapper path. **No change needed.**
- **`src/state.rs:734-766`** (`pane_digest_hex`, `work_done_file_name`) — FNV-1a hashes the raw bytes of whatever string it's given; the function signature is `fn pane_digest_hex(pane_id: &str) -> String`. **No change needed** — and this is in fact the fix: today's collisions happen because two *different* clients independently produce the *same value* ("19"), not because the hash collides. Once ids are unique by construction, the digest is unique too, with zero code change here.
- **`src/state.rs:516-541`** (`pane_orchestration_map`, `pane_role_map`, `pane_cwd_map`, `orchestrator_pane_ids`) — plain `HashMap<String, _>` keyed on whatever string is populated; the maps themselves are shape-agnostic. **No change needed** to the maps — but see below for the population site.
- **`AgentPtyRegistry`** (`src/agent_pty.rs`) keyed on `pane_id_env` — same, a `HashMap`/`Vec` keyed by opaque string, plus `is_valid_pane_id_env`'s charset/length check, already confirmed wide enough (`agent_pty.rs:198-214`). **No change needed.**

What **does** need code changes in M2 — not because any consumer's shape assumption breaks, but because the *source* of the value moves from "already known before the spawn call" to "returned by the spawn call":

- **`src/embedded_pane.rs:812-817`** (`allocate_id`) and its `next_id` field (`embedded_pane.rs:379,424`) — deleted per Scope.
- **`src/embedded_pane.rs:3036-3070`** (`create_pane_with_options`) — currently allocates the id *before* calling `create_stream_pane` because, in its own words, "it has to be injected into the child's environment" (`embedded_pane.rs:3042-3046`); that comment becomes false once the daemon does the injecting, so the call sequence inverts: call `StartAgent` first, receive the minted `pane_id` back, *then* populate the local pane map with it.
- **`src/embedded_pane.rs:1229-1313`** (the `hydrate_from_daemon` dedup/bump logic, including the `id.parse::<u64>()` counter-bump at `embedded_pane.rs:1297-1301`) — this whole block exists only to keep the client's own `next_id` counter from re-minting a colliding value; once the client mints nothing, the block simplifies to "trust `record.pane_id_env` when present and valid, else the daemon never assigned one (a daemon predating this PRD) and hydration falls back to whatever M2 decides for that edge case."
- **`src/daemon_protocol.rs:1339-1343`** (`pane_id_env` extraction from client `env`) and the `StartAgent` response construction (currently `AttachResponse::with_id`, near `daemon_protocol.rs:1497` relative to the handler) — extraction becomes minting; the response gains the minted `pane_id`.
- **`src/spawn.rs:1961-1977`** (`next_pane_id`) — its `PANE_COUNTER: AtomicU64` (`spawn.rs:82`) has the exact restart-reset defect named in Q2; M2 should unify this with whichever new minting function the `StartAgent` path uses rather than leaving two divergent daemon-side schemes, so there is one collision-resistant minter for every spawn origin (headless and TUI-attached alike).
- **`ui.rs`**'s call sites into `embedded_pane`'s pane-creation API — need to consume the daemon-returned id the same way `create_pane_with_options`'s caller will, wherever it currently assumes the id is already known.

No on-disk artefact needs migrating. Archived report filenames (`work-done-<role>-<digest>.md`) from before this change stay exactly as they are — they're historical and nothing reads them by reconstructing their name from a live pane_id after the fact (the PRD's own Evidence section describes them as accreting *because* of collisions, not as something a fix must clean up; stale-entry cleanup is explicitly upstream #524's job, not this PRD's — see Scope). There is also no persisted-to-disk registry/session file that stores `pane_id` across a daemon restart (`AgentPtyRegistry` is in-memory only; a reconnecting client rebuilds its view via `ListAgents` against the live daemon), so there is no serialization schema to migrate.

## Milestones

### M1 — settle the design

- [x] Decide assign vs. validate, and the id shape, with the reasoning recorded here.
- [x] Decide the cross-version behaviour for an old TUI against a new daemon.
- [x] Confirm the blast radius of a shape change against `DOT_AGENT_DECK_PANE_ID` consumers (`issue claim`, hooks) and on-disk artefacts.

### M2 — daemon allocates

- [x] Daemon assigns `pane_id` on the spawn path and returns it.
- [x] `EmbeddedPaneController::allocate_id`, `next_id` and the hydration bump removed.
- [x] `PROTOCOL_VERSION` bumped; cross-version behaviour implemented as decided in M1.

### M3 — prove it

- [x] A test that two independently-attached clients cannot obtain the same id — pinned, not argued.
- [x] A test that an id is not reissued after its pane exits.
- [x] Rule 12 cross-version manual test run, with an isolated `DOT_AGENT_DECK_LOG`, sockets, `HOME` and state dir.

### M4 — offer upstream

- [ ] Offer to `vfarcic/dot-agent-deck` per CLAUDE.md rule 19 — this is a bugfix that happens to be written here, not a fork preference. Cross-link upstream #524, #542, #398.

## Success Criteria

- A `pane_id` is unique across every pane the daemon has seen for its lifetime, regardless of how many clients attach or how many panes have exited.
- Two independently-attached TUI clients provably cannot mint the same id, demonstrated by a test.
- `work_done_file_name` can no longer point two unrelated panes at one report path, for `pane-`-origin (TUI-attach) panes. This is **not** true for `sched-`-origin (scheduler) panes: `spawn.rs`'s `next_pane_id`/`PANE_COUNTER` was deliberately left unchanged by this PR (out of M2 Scope) and still resets on daemon restart, so two scheduled fires of the same task name across a restart can still mint the identical `sched-<task>-0` id and collide on the same report path. Tracked as follow-up [#430](https://github.com/prageethw/dot-agent-deck/issues/430).
- The protocol change is versioned and its cross-version behaviour is tested, not assumed.

**A criterion deliberately not used:** "no collisions observed in practice." The #358 collisions were invisible for six days and 18 report generations while every gate stayed green. Absence of an observed collision is not evidence, and a criterion that could be satisfied by not looking is worse than none. Each criterion above names an artefact someone can check.

## Key Files

| File | Why |
|---|---|
| `src/embedded_pane.rs:812` | `allocate_id` — the client-side mint being retired |
| `src/embedded_pane.rs:424`, `:1161-1302` | `next_id` init and the one-shot hydration bump |
| `src/daemon_protocol.rs` | `StartAgent` / `ListAgents`; where an assigned id would be returned |
| `src/agent_pty.rs:3964` | `spawn_agent`'s duplicate rejection — the backstop that stays |
| `src/agent_pty.rs:426` | `mint_orchestration_id` — prior art for a collision-resistant id |
| `src/spawn.rs` | `next_pane_id`, the prefixed daemon-side scheme and its restart-reset counter |
| `src/state.rs:734` | `work_done_file_name` / `pane_digest_hex` — the report-path consumer |
| `src/state.rs:516-541` | the four pane-keyed routing maps |

## Rule 12 — cross-version contract

This moves `pane_id` from client-chosen to daemon-returned, so the **wire shape changes** and `PROTOCOL_VERSION` must be bumped. The cross-version manual test is required before the PR: build the branch, start a daemon from the previous release with an agent under it, run the branch TUI against that older daemon, and confirm a delegate still routes and hooks still arrive.

Isolate `DOT_AGENT_DECK_LOG` along with the sockets, `HOME` and state dir. The log path resolves separately, so a sandbox daemon otherwise correctly isolated still appends into the real `~/.local/state/dot-agent-deck/deck.log` — and two interleaved daemons in one log file is genuinely hard to read afterwards.

Bump policy while `0.x`: breaking → minor.

**Manual run recorded (2026-08-17).** Built this branch (PROTOCOL_VERSION 9) against a clean pre-#365 build at `v0.38.3-3-g4e84eb8b` (PROTOCOL_VERSION 8, confirmed an ancestor of this branch's base), fully isolated (`DOT_AGENT_DECK_LOG`, sockets, `HOME`, state dir all pointed at a scratch dir). Started the old daemon with a live stand-in agent under it, then attached the new TUI: the handshake refused unconditionally with `error: local daemon speaks attach protocol v8 but this binary speaks v9`, exit non-zero — confirmed at the source (`build_version_handshake.rs:242`) that the `PROTOCOL_VERSION` check runs before any build-id/TTY branching, so this refusal is unconditional regardless of TTY state, unlike the softer same-protocol build-id skew that does get an interactive consent-restart. Ran the printed recovery (`dot-agent-deck daemon stop --force`, then relaunch), which lazily spawned a fresh matched-version (9) daemon; the new TUI then attached cleanly. On that reconciled daemon, set up a two-role orchestration (`orchestrator` + `coder`, stand-in `cat` commands) and confirmed end-to-end: `delegate --to coder` wrote `worker-task-coder.md` and injected the prompt into the coder pane (daemon log: `Received delegate signal ... targets=["coder"]`), and `work-done` from the coder pane delivered its summary back into the orchestrator's pane (daemon log: `Received work-done signal ...` / `work-done: wrote worker summary ...`; visible in the orchestrator's scrollback: "Worker coder has completed their task"). No deviation from decision 3's expectation — refuse then recover, not silently degraded.

## Risks and Mitigations

**The id shape has reach beyond the daemon.** `DOT_AGENT_DECK_PANE_ID` is read by hooks and by `issue claim`'s identity resolver, and appears in report filenames. *Mitigation:* M1 confirms the blast radius before M2 commits to a shape; changing the id's **source** without changing its **shape** is a viable smaller step.

**A daemon-side counter looks sufficient and is not.** `next_pane_id` already demonstrates the trap — a process-global `AtomicU64` that resets on restart. *Mitigation:* named explicitly in M1's open questions, with `mint_orchestration_id` as the in-tree pattern that solves it.

**Unique ids will not fix #358 on their own.** Without upstream #524, stale entries still accumulate — they simply stop being collidable. *Mitigation:* stated in Scope, and #358 stays open until both land.

**A green board is not evidence here.** The withdrawn #358 fix passed 3210 fast-tier and 9367 e2e tests while being logically inverted. *Mitigation:* M3's criteria name specific provable properties, and reviewer plus auditor passes are mandatory given the withdrawn attempt.

## Adjacent, deliberately separate

Filed alongside this PRD and intentionally **not** folded into it, because each is fixable and shippable on its own:

- **[fork #366](https://github.com/prageethw/dot-agent-deck/issues/366)** — a delegate to a worker that is *still working* is accepted silently. `handle_delegate` never consults worker state, and `pane_dispatch_lock` serializes competing writes rather than refusing them. Worker **state**, not worker identity.
- **[upstream #545](https://github.com/vfarcic/dot-agent-deck/issues/545)** (open) — `delegate --to <role>` cannot express *which* worker, so with several orchestrations live the orchestrator cannot say "mine". Worker **addressing**.
- **[upstream #524](https://github.com/vfarcic/dot-agent-deck/issues/524)** (open) — routing entries survive pane death, because `unregister_pane` has one caller.

Together with this PRD these are four different failures that all present as "my task went somewhere else". Unique ids fix the one where the *name* was ambiguous; they do nothing for the other three.
