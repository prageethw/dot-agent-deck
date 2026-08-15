# PRD fork#365: Make `pane_id` daemon-authoritative

**GitHub Issue**: [fork #365](https://github.com/prageethw/dot-agent-deck/issues/365)

**Priority**: High

**Status**: Not started — PRD written 2026-08-15, no implementation. Filed out of [fork #358](https://github.com/prageethw/dot-agent-deck/issues/358), whose first fix attempt was withdrawn after review; see "What has already been tried" for why that attempt is the argument for this one.

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

## Milestones

### M1 — settle the design

- [ ] Decide assign vs. validate, and the id shape, with the reasoning recorded here.
- [ ] Decide the cross-version behaviour for an old TUI against a new daemon.
- [ ] Confirm the blast radius of a shape change against `DOT_AGENT_DECK_PANE_ID` consumers (`issue claim`, hooks) and on-disk artefacts.

### M2 — daemon allocates

- [ ] Daemon assigns `pane_id` on the spawn path and returns it.
- [ ] `EmbeddedPaneController::allocate_id`, `next_id` and the hydration bump removed.
- [ ] `PROTOCOL_VERSION` bumped; cross-version behaviour implemented as decided in M1.

### M3 — prove it

- [ ] A test that two independently-attached clients cannot obtain the same id — pinned, not argued.
- [ ] A test that an id is not reissued after its pane exits.
- [ ] Rule 12 cross-version manual test run, with an isolated `DOT_AGENT_DECK_LOG`, sockets, `HOME` and state dir.

### M4 — offer upstream

- [ ] Offer to `vfarcic/dot-agent-deck` per CLAUDE.md rule 19 — this is a bugfix that happens to be written here, not a fork preference. Cross-link upstream #524, #542, #398.

## Success Criteria

- A `pane_id` is unique across every pane the daemon has seen for its lifetime, regardless of how many clients attach or how many panes have exited.
- Two independently-attached TUI clients provably cannot mint the same id, demonstrated by a test.
- `work_done_file_name` can no longer point two unrelated panes at one report path.
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
