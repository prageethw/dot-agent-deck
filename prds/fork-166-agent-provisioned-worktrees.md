# PRD fork#166: Unique orchestration names, and worktrees the agent provisions itself

**GitHub Issue**: [fork #166](https://github.com/prageethw/dot-agent-deck/issues/166)

**Priority**: High

**Status** *(updated 2026-08-11, PR #215)*: **Phase 1 complete** — shipped via the carved-out PRD [fork#192](https://github.com/prageethw/dot-agent-deck/issues/192) (PR #193, merged `7426b25`), **released in v0.37.0**. **Phase 2 complete** — M2.3's display half (the OWNER column in `format_list_human`) and M2.4 (the `DOT_AGENT_DECK_WORKTREE_OWNER` identity supply, threaded through both the interactive and issue-dispatch spawn paths, PERSISTED across session-restore via a new `OrchestrationSnapshot.owner` field, plus the `PROTOCOL_VERSION`-unchanged decision) both landed on this branch. **Phase 3 (M3.0) complete** — `worktree list --mine`, filtering on the supplied identity, fails loudly on an absent or `orchestration:unknown` identity, and now matches correctly across a tab close/reopen too. **Phase 4 (M4.0) complete** — docs and changelog fragment below.

**Fork-only**, and intended to stay so. Confirmed mechanically against `upstream/main`: `src/worktree_reclaim.rs` **does not exist** there and `worktree_slug` has **0** occurrences. The ownership half is built entirely on fork-only code, so per `docs/develop/upstream-contribution-policy.md` this is not an offer-later case.

**Related**: fork [#192](https://github.com/prageethw/dot-agent-deck/issues/192) (Phase 1 carved out and **shipped** — names as instance identity, released in v0.37.0) · fork [#201](https://github.com/prageethw/dot-agent-deck/issues/201) (the residual #192 left open: uniqueness is advisory against a form-open snapshot) · fork #144 (the ownership marker and its containment check — the foundation) · fork #122 (per-orchestration worktree creation, and the hardened path validation reused here) · PRD #422 (`worktree list` / `reclaim`) · PRD #140 (per-tab routing identity; its `display_title` contract changes here) · PRD #120 (issue-dispatch, which already provisions its own worktrees) · PRD #220 (the upstream cousin; its M1.1 naming question is settled differently here) · fork #74 (the motivating collision) · PRD #421 (the provenance precedent, applied to a case it did not cover)

**Filename convention**: the first PRD named after a fork issue — every existing one cites `vfarcic`. A bare `166-` would be ambiguous against a future upstream #166, so fork-numbered PRDs take a `fork-<n>-` prefix.

## Problem Statement

Creating a worktree is a **human step**, and CLAUDE.md rule 1 mandates it: the orchestrator runs `git worktree add` by hand, unsets the upstream, and states the absolute path, exact SHA and branch name in every task it delegates. Rule 16 exists because that supply obligation kept being forgotten.

The cost is the documented failure, fork **#74**: *"a second orchestration's task named no worktree path, so its worker found the first orchestration's existing branch and reasonably joined it — writing production code into a worktree it did not own, then pushing to it and cancelling the first orchestration's in-flight CI run."* More than once.

**The goal is autonomous operation** — spinning up agents without a human provisioning directories first. That needs two things the deck does not have: an orchestration that can create its own worktree, and an identity that says which worktrees are *its*.

### The workflow this serves

1. Every orchestration starts from the deck's own checkout (`main`). **Intended** — not something to prevent.
2. Each change gets **its own** worktree, created by the agent when work starts.
3. That worktree belongs to the orchestration that created it.
4. The orchestration can list what it owns, and works only on those.
5. A worktree from an earlier session whose owner matches is **available again** — a restart resumes rather than orphans.

Step 2 is manual. Steps 3–5 do not exist.

### The missing key: names are not identities

The new-pane form already has a **Name** field, and for an orchestration it becomes `display_title` — which is explicitly decoration. `src/agent_pty.rs:337`: *"is title-only and never feeds delegate/role lookups."* `src/ui.rs:8264`: *"name to the tab TITLE only."* It is optional, and nothing prevents two tabs sharing one.

So there is no answer to "which orchestration is this?" that survives a restart. `orchestration_id` is unique but **minted fresh on every tab open** (`mint_orchestration_id`), so a reopened tab would not recognise its own worktrees — breaking step 5 exactly when it matters. `name` + `orchestration_cwd` are identical for two tabs of one orchestration in one directory, which is the normal case here.

## Solution Overview

**Make the orchestration's name a real identity, and everything else follows.**

1. **A name is required, and must be unique** among live orchestrations.
2. **The name is suggested** as the next free `<foldername>-orchestrator-N`, so accepting it is one keystroke.
3. **The name is the ownership identity**, written into the marker of every worktree the orchestration creates.
4. **Worktrees are named `<orchestration-name>-<change>`**, so the prefix is the owner and `ls` shows it at a glance.
5. **The marker decides ownership, never the name.**
6. **Provisioning is one step** — `delegate` creates the worktree. **Split out to fork [#175](https://github.com/prageethw/dot-agent-deck/issues/175)**; see below.

### Scope split: this PRD is the identity and ownership half

Automatic provisioning is now **fork #175**, which depends on this one. The split is deliberate: this PRD carries the two risky parts — a **semantic break behind a stable wire** (promoting `display_title`, which triggers rule 12's cross-version manual test and a `.breaking.md` fragment) and an **audit sweep** of every existing test that submits an orchestration form, whose blast radius is not known up front.

Landing those separately means #175 builds on settled ground, and that if the wire change causes trouble there is a smaller thing to unpick. What ships here is still independently useful: unique orchestration names, and an answerable *"which worktrees are mine?"* — including after a restart.

### What fork PR #173 already landed, and what it leaves

**Update 2026-08-09.** While this PRD was being written, fork PR **#173** (`a6fee76`, tracking *upstream* issue #425) merged and independently built the **write half** of ownership. That is a help, not a collision — it settles the marker format question this PRD would otherwise have had to answer.

Already on `main`:

- **The marker format.** `mark_worktree_owned(worktree_path, creator)` writes `"deck\ncreated-by: <sanitized>\n"` — one field per line, with the bare `deck` first line kept so an older reader still sees a valid marker. Its doc is explicit about the read contract: *"a future reader must strip the literal `created-by: ` prefix and treat the remainder as opaque, never `split(':')`"*.
- **`sanitize_marker_creator`** — drops C0/DEL controls, collapses newlines to spaces so the two-line shape survives, caps at 200 chars, and maps empty input to `"unknown"`.
- **`creator` threaded through both creation paths** — `create_worktree` and `create_worktree_sync` in `src/issue_dispatch_run.rs` both take it.
- **Issue-dispatch passes `issue-dispatch:<task>#<issue>`**, which matches this PRD's "the scheduled task's name on the dispatch path" exactly.

**This PRD adopts that format rather than proposing another.** It is well-designed, documented, and already shipped.

What it leaves for this PRD:

1. **There is no read side.** Nothing parses `created-by:` back out, so ownership cannot yet be *queried* — only written.
2. **The interactive path records the wrong identity for our purpose.** `src/ui.rs` passes `format!("orchestration:{}", orch_config.name)` — the canonical **config/type** name (`review`, `tdd-cycle`), shared by every orchestration of that type. Two live `review` orchestrations therefore record the *same* creator, which does not distinguish instances. That is not a defect in #173 — its stated scope is *"record which task created a worktree"*, provenance rather than instance identity, and it is honest about that. Making the value instance-unique is precisely what this PRD adds, and #173's own sanitizer doc anticipates it by naming *"a TUI-typed orchestration name"* as an input.
3. **Nothing surfaces the owner** in `worktree list`.

So Phase 2 shrinks: the format is settled and the plumbing exists. What remains is the read side, the right value, and the display.

**Update 2026-08-10 — two of those three are now done, via fork#192.** Item 1 (no read side) is closed by `read_marker_owner`/`owner_of`. Item 2 (wrong identity) is closed on the normal path: `orchestration_creator_string` (`src/ui.rs`) now passes the typed name, though the `orch_config.name` fallback and the `orchestration:unknown` fallback both still exist. Item 3 (nothing surfaces the owner) is **half** closed — `--json` carries it, the human table does not. What that left unnoticed until now is that none of the three was the actual blocker for Phase 3: **the running orchestration has no way to know its own owner string**, which is M2.4.

**Update 2026-08-11, PR #215 round 3 — both residuals in the paragraph above are now closed too, and are left in place rather than rewritten since this paragraph is itself a dated snapshot.** The `orch_config.name` fallback was deleted by the same PR #215 fixup M2.1 below records (reviewer F5 M2 / auditor M2): an empty typed name now always produces the `orchestration:unknown` sentinel, never the config-name string. And M2.3, completed since this paragraph was written, closed "the human table does not [carry the owner]" — `format_list_human` renders the OWNER column now.

### Why the marker decides and the name does not

The naming convention exists for humans. It carries no authority, and that distinction is the entire safety argument.

**Name-based (prefix) ownership was considered and rejected.** It would adopt a worktree the deck did not create — a hand-made folder matching the pattern becomes `Ours`, and fork #144 makes `Ours` + merged + clean removable by a bare `reclaim` with **no prompt and no path shown**. That is the P1 fork #144 closed. Reopening it to save a file read is a bad trade.

With the marker authoritative, a matching name on a folder the deck did not create is simply `Foreign`. Nothing is adopted, and the convention stays useful.

### Promoting `display_title` is a contract change, not a rename

Using the typed Name as the identity means `display_title` stops being presentation-only. That contract is stated in PRD #140's own field docs and must be updated there, not silently contradicted.

Two consequences:

- Every site that treats it as cosmetic needs checking. It is `Option<String>` with `skip_serializing_if`, deliberately, so older peers round-trip cleanly.
- **This is a semantic break behind a stable wire** — the field's *shape* is unchanged, its *meaning* is not. CLAUDE.md rule 12 names exactly this case, so a `changelog.d/*.breaking.md` fragment applies, and whether `PROTOCOL_VERSION` moves must be answered rather than assumed.

### Daemon-initiated orchestrations have a different identity, and that is fine

Scheduled and issue-dispatch orchestrations set `display_title: None` (`src/issue_dispatch_run.rs:1552`, `:1646`; `src/spawn.rs:339`) — nobody types a name.

They do not need one. **Issue-dispatch already provisions its own worktree** (`<clone>/.worktrees/issue-<n>`, branch `agent/issue-<n>`) and already writes the marker, keyed on the `ScheduledTask` name it also uses as the reuse key. That name is stable across restarts for the same reason a typed name is.

So the owner recorded is the typed name for interactive orchestrations and the scheduled task's name for dispatched ones. Both are stable identifiers; only the source differs. The requirement in this PRD — *a name must exist and be unique* — is a constraint on the **interactive** path only.

### `main` is already protected, structurally

An orchestration starting in `main` must never treat `main` as an owned worktree. **This needs no new check.** fork #144's containment already guarantees it:

```rust
if !git_dir.starts_with(common_dir.join("worktrees")) {
    return Ownership::Foreign;
}
```

The main checkout's git dir is `<repo>/.git`, which sits *above* `<repo>/.git/worktrees`, so it can never match — `main` always resolves `Foreign`, whatever it is named. Independently, `mark_worktree_owned` runs only immediately after a successful `git worktree add`, "never for a pre-existing or foreign worktree", so no marker is written there.

That check was built to defeat a forged marker. It protects this case for free, because *"is this a real linked worktree of this repo?"* is the same question either way.

## Scope

### In Scope

- A required, unique orchestration name on the interactive path, suggested as `<foldername>-orchestrator-N`.
- Promoting `display_title` from presentation to identity, and updating PRD #140's contract wording.
- Owner recorded in fork #144's marker; `ownership_of` reports it.
- `delegate` provisioning `<orchestration-name>-<change>` in one step.
- Listing the worktrees an orchestration owns, correct after a restart.
- Amending CLAUDE.md rule 1 so manual `git worktree add` is replaced, keeping rule 16's supply obligations.

### Out of Scope

- **Any restriction on where orchestration tabs start.** All orchestrations starting from `main` is the intended workflow. An earlier draft proposed blocking a second orchestration per directory; that was a misreading and is withdrawn. **PRD #140's advisory stance stands unchanged.**
- **Automatic worktree removal.** fork #122 deliberately never removes worktrees; `reclaim` stays the gated path.
- **Adopting worktrees the deck did not create.** Never, by any mechanism.
- **Stopping an agent `cd`-ing into another orchestration's worktree.** No deck-side check can see that; it is agent discipline under rule 1.
- **Namespacing `worker-task-<role>.md`** — see Risks.

## Success Criteria

- Two live orchestrations cannot share a name.
- **An orchestration can determine its own identity at runtime.** *(Added 2026-08-10.)* Every criterion below assumes this and none of them stated it, which is exactly how the gap survived into implementation planning — see M2.4.
- The suggested name is accepted with one keystroke.
- Every worktree an orchestration creates records it as owner; the owner is listable.
- **After closing and reopening a tab with the same name, its earlier worktrees are still recognised as its own.**
- A worktree created by a different orchestration is never owned. One the deck did not create is never owned, whatever it is named.
- `main` never appears as owned.
- A worktree created before this ships still resolves `Ours`, so `reclaim` keeps working on it.
(Replacing CLAUDE.md rule 1's manual `git worktree add` is fork #175's success criterion, not this PRD's — this one supplies the identity that makes it possible.)

## Milestones

### Phase 1: Name as identity

**All of Phase 1 shipped via PRD [fork#192](https://github.com/prageethw/dot-agent-deck/issues/192)**, not on this PRD's own branch — carved out precisely because it carried the two risky parts. PR #193, merged `7426b25`, released in **v0.37.0**.

- [x] **M1.0** — the interactive Name is required and refused if it matches a live orchestration; suggested as the next free `<foldername>-orchestrator-N`. **Done (fork#192).** The refusal renders on PRD #140's existing guard seam; the liveness query was extended to carry names, so it costs no extra daemon round-trip.
- [x] **M1.1** — `display_title` promoted to identity; PRD #140's field docs updated; every cosmetic-assumption site checked. **Done (fork#192).**
- [x] **M1.2** — rule 12 answered: `.breaking.md` fragment, and an explicit `PROTOCOL_VERSION` decision. **Done (fork#192).** `changelog.d/192.breaking.md` shipped; `PROTOCOL_VERSION` deliberately **not** bumped (the wire shape is unchanged, so no peer can mis-parse a frame) and the cross-version manual test ran against `v0.36.1` with all four items passing.

**Known residual from M1.0, tracked as fork [#201](https://github.com/prageethw/dot-agent-deck/issues/201):** the uniqueness refusal reads a snapshot of live names taken once when the form opens and never refreshed. Two forms open *concurrently* can still be suggested — and both submit — the same name, and nothing at the marker write enforces uniqueness either. That concurrent case is the one fork #74 was actually about, so Phase 1 narrows the collision rather than closing it.

### Phase 2: Ownership

- [x] **M2.0** — ~~`mark_worktree_owned` records the owner~~ **done by fork PR #173** (`a6fee76`), including the format and the sanitizer. The dispatch path already passes the scheduled task's name.
- [x] **M2.1** — the interactive path passes the **typed unique name** instead of `orch_config.name`, so the recorded identity distinguishes live instances. **Done (fork#192).** `src/ui.rs`'s `orchestration_creator_string` builds `format!("orchestration:{typed_name}")` and the caller passes it to `create_worktree_sync`. **One fallback remains: `orchestration:unknown`** when no typed name is available. PR #215 fixup (reviewer F5 M2 / auditor M2) **deleted the second fallback** — `orchestration:<orch_config.name>` — which used to fire whenever the typed name was empty and gave every unnamed orchestration on the same config the IDENTICAL identity, with no refusal, because that string was not the `orchestration:unknown` sentinel `--mine` refuses. An empty typed name now always produces the sentinel, so "instead of `orch_config.name`" is true without qualification.
- [x] **M2.2** — a read side: parse `created-by:` back out per #173's stated contract (strip the literal prefix, treat the remainder as opaque, **never** `split(':')`). Containment and presence stay authoritative — an empty or prefix-less marker resolves `Ours` with owner unknown. **Done (fork#192).** `read_marker_owner` (`src/worktree_reclaim.rs:502`, `strip_prefix("created-by: ")` at `:510`) and `owner_of` (`:545`).
- [x] **M2.3** — `worktree list` shows the owner; `--json` carries it; decide whether `SCHEMA_VERSION` moves. **Done.** The **JSON half already shipped**: `WorktreeReport.owner` exists as `Option<String>` with `skip_serializing_if`, omitted entirely rather than serialized as `null` so an older client round-trips cleanly. The **human table** now carries it too — `format_list_human` (`src/worktree_reclaim.rs:900`) emits `PATH BRANCH PR CLEAN OWNED OWNER VERDICT REASON`, using the existing `DASH` placeholder for a report whose `owner` is `None` (`worktree/reclaim/024`). **`SCHEMA_VERSION` stays at `1`** — the `owner` field shipped *at* schema 1 and is additive-optional, so bumping now would announce a change that already happened silently.

- [x] **M2.4 — supply the owner identity to the running orchestration** *(added 2026-08-10; not in the original plan)*. **Done, corrected in PR #215's review/audit round.** `DOT_AGENT_DECK_WORKTREE_OWNER` (`src/agent_pty.rs`, beside `DOT_AGENT_DECK_PANE_ID`/`DOT_AGENT_DECK_AGENT_ID`, with a third `env_remove` scrub entry) carries the exact creator string an orchestration stamps into its own worktree markers. Threaded through both spawn paths from ONE computed value each — never a daemon-side reconstruction from `TabMembership`/`display_title`, the prohibited shortcut this milestone called out: the interactive path hoists `creator` out of the `Some(worktree_path)` match arm in `Action::SpawnPane` (`src/ui.rs`) and passes it through `open_orchestration_tab`'s `creator` parameter into every role pane's `AgentSpawnOptions::owner`; the issue-dispatch path threads the same `creator` local through `SpawnRequest::owner` into `pane_env` (`src/spawn.rs`). Both branches funnel through one shared function, `orchestration_creator_string` (`src/ui.rs`), which also applies `sanitize_marker_creator` to its result so the marker write and the env var are guaranteed the identical sanitized string rather than one raw and one sanitized (PR #215 fixup).

    **The original landing stopped one hop short of the environment.** `AgentSpawnOptions::owner` reached `create_pane_with_options` and was then dropped — `EmbeddedPaneController::create_stream_pane`, the only production `PaneController`, forwarded every other spawn option but not `owner`, and built its env as `DOT_AGENT_DECK_PANE_ID` alone. So the interactive and session-restore paths never actually carried the variable; only the issue-dispatch path (which writes the env var via a different function, `spawn.rs::pane_env`) worked. Both PR #215's reviewer (F1) and its auditor (H1) caught this independently before merge. **Fixed in the same PR**: `create_stream_pane` now takes an `owner: Option<String>` parameter and pushes it onto the env vec; `create_pane_with_options` forwards `opts.owner` to it. `orchestration/identity/008` (a `CapturingPaneController` mock) proved the value reached `AgentSpawnOptions` and nothing about whether anything read it — the join between producer and reader was the one thing uncovered, and the bug was in the join. `orchestration/identity/009` closes that: a real in-process-daemon-binary spawn (`EmbeddedPaneController` over a real attach socket, `agent_pty::spawn`'s real `portable_pty` child) whose command echoes `$DOT_AGENT_DECK_WORKTREE_OWNER` back out of its own environment.

    **Session-restore persists the identity rather than reproducing or dropping it.** An intermediate design had the restore path recompute `orchestration_creator_string` unconditionally and pass it to `open_orchestration_tab`, which fabricated an identity for every restored tab — including one that never created a worktree, contradicting `AgentSpawnOptions::owner`'s own doc ("`None` for a pane that is not part of a worktree-owning orchestration") and `open_orchestration_tab`'s `creator` param doc ("`None` when this orchestration tab owns no worktree"). Because `OrchestrationSnapshot` recorded nothing that would let restore tell those two cases apart, a first fix made restore always pass `None` — honest, but at the cost of the "closing and reopening a tab still matches its earlier worktrees" success criterion the PRD exists to deliver. **The actual fix: `OrchestrationSnapshot` gained an `owner: Option<String>` field** (`src/config.rs`), written at tab-creation time from the exact same `creator` string that goes into the marker and the env var — a third consumer of the one-literal-string invariant, not a fourth derivation. Restore now **passes this value through unchanged** (`Some(v)` when present, `None` when absent) rather than recomputing or discarding it: an orchestration that owned no worktree still restores with `None`, and one that did restores with the identical string it stamped, so `--mine` matches. A snapshot written before this field existed has no `owner` key, deserializes to `None` under the existing `#[serde(default)]` discipline, and restores with no identity — the same honest "fail loudly, name what's missing" outcome as before, scoped now to exactly the tabs saved before the upgrade. `config/saved-session/001` and `session/restore/017` cover the write→read TOML round trip and the full snapshot→restore→spawn round trip respectively. **Rule 12: confirmed no wire change** — `AttachRequest::StartAgent.env` / `StartAgentOptions.env` already existed as `Vec<(String, String)>`; `PROTOCOL_VERSION` stays at 7.

    **The gap, as measured.** An orchestration cannot learn its own identity at runtime. `DOT_AGENT_DECK_AGENT_ID` and `DOT_AGENT_DECK_PANE_ID` (`src/agent_pty.rs:41`, `:56`) are the *only* identity in a pane's environment; both are small daemon-scoped integers (observed as `6`) that **recycle across a daemon restart**, and nothing carries the owner string to a pane. So `worktree list --mine` has no way to compute "mine". This is a CLAUDE.md rule 16 shape — a consumed value with no named supplier — and it is the same wall CLAUDE.md rule 23 hit, which is why that rule anchors issue claims to worktree paths rather than to orchestration names.

    **The owner string is namespaced by producer, and exact-matching depends on knowing that.** `owner_of` returns the whole `created-by:` remainder verbatim and never splits on `:`, so ownership is an exact string comparison against a value whose *shape depends on which path created the worktree*:

    | Producer | Owner string | Source |
    |---|---|---|
    | Interactive orchestration | `orchestration:<typed_name>` | `orchestration_creator_string`, `src/ui.rs` |
    | Issue-dispatch | `issue-dispatch:<task_name>#<issue>` | `src/issue_dispatch_run.rs:402` |

    Both producers now run `sanitize_marker_creator` at the point of computation, not only inside `mark_worktree_owned`'s own marker write — the interactive path via `orchestration_creator_string`, the issue-dispatch path directly at `src/issue_dispatch_run.rs:402` (PR #215 fixup). Leaving one producer unsanitized before it reaches `AgentSpawnOptions::owner`/`SpawnRequest::owner` was the same divergence class this milestone's "one literal string reaches both consumers" invariant exists to prevent, just with lower-entropy input (a task name and issue number, not free TUI text). `sanitize_marker_creator` is a fixed point (`f(f(x)) == f(x)`), so the marker write's own call stays harmless on top.

    Earlier sections of this PRD describe both producers correctly but read as though there is one uniform identity. There is not, and M3.0's *"correct after a restart"* is meaningless without saying **which string** is being matched.

    **The fix.** Inject `DOT_AGENT_DECK_WORKTREE_OWNER` into every pane, carrying **the exact creator string this orchestration stamps when it creates a worktree** — `orchestration:<typed_name>` interactively, the `issue-dispatch:<task>#<n>` string on the dispatch path. One source for both sides, so marker and filter cannot drift. Named for what it carries, not for one producer: calling it `…_ORCHESTRATION` would misdescribe it the moment a dispatched orchestration used it.

    **Decided consequences.** A **dispatched** orchestration therefore also matches the worktree it is running in, because that is the same string — M3.0's promise must hold for dispatched work, which is the autonomous case this PRD exists to serve. And `orchestration:unknown` (produced by `orchestration_creator_string`, `src/ui.rs`) is a **sentinel, never an identity**: two nameless orchestrations would otherwise match each other's worktrees and each be handed the other's work, so it must be treated exactly like an absent variable — fail loudly.

    **Settled by a read-only spike, 2026-08-10** (`.dot-agent-deck/findings-166-spike-owner-supply.md`):

    - **Rule 12: the wire does NOT move → patch bump, no `.breaking.md`.** `AttachRequest::StartAgent.env` / `StartAgentOptions.env` (`src/daemon_protocol.rs:376`, `src/daemon_client.rs:81`) already exists as `Vec<(String, String)>` and already carries `DOT_AGENT_DECK_PANE_ID`. Adding an entry is additive use of an existing field, not a new one. `PROTOCOL_VERSION` stays at 7.
    - **The creator string is in scope at spawn on both paths.** Interactive: `creator` (`src/ui.rs:9101`) is computed in the same `dispatch_action` that calls `open_orchestration_tab` (`:9206`) — it is scoped to a `match` arm and needs hoisting, nothing more. Dispatch: `creator` (`src/issue_dispatch_run.rs:402`) and the `SpawnRequest` build (`:441`) are in the same function, and that whole path runs in-process in the daemon. Two structs need an owner/env field added — `AgentSpawnOptions` (`src/pane.rs`) and `SpawnRequest`/`RoleSpawn` (`src/spawn.rs`) — both pure in-process Rust, nothing serialized.
    - **DO NOT derive the owner string daemon-side from `TabMembership` / `display_title`.** It round-trips, so it looks like a free shortcut. It is not: `creator`'s explicit `orchestration:unknown` sentinel and `resolve_orchestration_name`'s cwd-basename fallback are **different functions of the same input** and diverge in the untyped-name case — the shortcut would produce a silently wrong, *non-sentinel* owner string, which is exactly the false positive that hands one orchestration another's worktrees. The real value must be threaded through, not reconstructed.
    - **The scrub block needs a third entry.** `cmd.env_remove(DOT_AGENT_DECK_WORKTREE_OWNER)` alongside the existing `PANE_ID`/`AGENT_ID` scrubs (`src/agent_pty.rs:1073`–`:1079`). Without it, a daemon launched from inside one orchestration's pane leaks that orchestration's owner string into every agent a *different* nested orchestration spawns from it — the same class of bug those two scrubs already exist to prevent.
    - **The invariant, stated precisely:** the marker write and the env var must share the **literal same computed string**, not two derivations of one input. Every failure mode above is a case of two derivations drifting.

    **Why an env var rather than a runtime daemon query** *(the alternative was evaluated, not assumed)*: `worktree list` today needs **no daemon at all** — `run_worktree_list_cli` → `examine_worktrees` is a pure filesystem/git read (`src/main.rs:1678`). A `get-seed`-style query would make `--mine` newly depend on the daemon being reachable **exactly when it has just restarted**, which is the scenario M3.0 names as its correctness bar. A query also has to disambiguate several distinct "no answer" cases (dead socket, old daemon, daemon up but never recorded a creator, RPC timeout) that an env var simply does not have. **Rehydration confirms the choice:** it is echo-only — `OrchestrationHydrationBucket.display_title` comes straight from the daemon's stored value and is never recomputed by the TUI (`src/ui.rs:3080`–`:3084`) — and it never touches a running child's OS environment, so a value baked in at spawn cannot drift across any later attach/detach cycle.

### Phase 3: Query

- [x] **M3.0** — an orchestration can list the worktrees it owns, **correctly after a restart**. This is the milestone the whole identity choice exists for. **Done.** Shipped as `worktree list --mine` (`src/main.rs`'s `WorktreeCmd::List`/`run_worktree_list_cli`), filtering on `r.owned && owner_of(...) == DOT_AGENT_DECK_WORKTREE_OWNER` — the `owned` conjunct was added in PR #215 (reviewer F4 / auditor L1) after `owner_of`'s own doc recorded that `owned=false` can land alongside a non-`None` `owner`, and its "accepted as cosmetic, no consumer treats `owner` alone as an ownership signal" reasoning stopped being true the moment `--mine` became such a consumer. With no identity available — variable absent (`worktree/reclaim/027`), set to the `orchestration:unknown` sentinel (`worktree/reclaim/028`), or exported but empty/whitespace-only (`worktree/reclaim/029`, PR #215 reviewer F4: `std::env::var` returns `Ok("")` for an exported-empty variable, so without that guard it became the filter and produced a definitive-looking `no worktrees owned by ` with a blank subject at exit 0) — it **fails loudly and explains why** (non-zero exit, names the missing/sentinel/empty variable), never silently returning everything or nothing. The env value is normalized through `sanitize_marker_creator` exactly once and that single string is used for both the sentinel comparison and the filter (PR #215 round-4 R4-1), because `read_marker_owner` always sanitizes the on-disk marker: comparing a raw env value against a sanitized marker meant a legitimate identity carrying stray whitespace matched nothing while the message printed it sanitized, reporting a confident wrong answer with the cause edited out; a wrong answer here hands one orchestration another's worktree. A filtered-to-empty result also gets its own message (`no worktrees owned by <identity>`, PR #215 reviewer F7 / auditor L1) rather than the generic `no worktrees found`, which used to conflate "no worktrees exist" with "none are yours" with "yours failed to match" — the last one a silent wrong answer. Restart-correctness falls out of the design rather than needing machinery: both sides derive from the same stable string and nothing depends on in-memory state (`worktree/reclaim/026`, a marker written by one process matched by a wholly independent fresh subprocess) — this holds for a same-name **re-created** orchestration and, since the M2.4 persistence fix, for a **restored** one too (a tab restored from a snapshot saved before that fix, or before this field existed, still carries no identity — see M2.4). The happy path (`worktree/reclaim/025`) keeps a worktree owned by the matching identity and excludes a same-repo worktree owned by a different one. `run_worktree_list_cli` stays daemon-free, as required — the owner check reads only the env var and the on-disk marker.

    **Scope of the query surface: `--mine` only.** No `--owner <name>`, and no `reclaim --mine`. This PRD's *"reports and filters by owner"* is satisfied by the OWNER column (M2.3) plus self-filtering — every other owner becomes visible, so filtering on an arbitrary one is a `grep`. `reclaim` in particular is the destructive path whose gating fork #144 hardened deliberately; widening it belongs with fork #175's provisioning work, not here.

    **Trust boundary** *(added PR #215, auditor M1 — recorded now, while true and cheap, so fork #175 does not have to rediscover it)*: **`DOT_AGENT_DECK_WORKTREE_OWNER` is an advisory identity, not an authenticated one.** It is a plain environment variable, settable by the agent and by every descendant process it spawns (a build script, `npm install`, a test runner — anything the orchestration's agent runs inherits and can re-set it for its own children), and it is not evidence of anything. It may gate **display and filtering only**. No destructive or write path may consult it.

    **What it is for, and the complete reachable effect of forging it.** The only thing setting this variable to another orchestration's string does today is list that orchestration's worktrees in the current repo under `--mine` — verified, not assumed: `decide()` (the `reclaim` removal gate) does not take `owner` and cannot see it; `format_reclaim_human` does not render `owner`, so the reclaim confirmation surface is untouched; the filter in `run_worktree_list_cli` is a `reports.retain(...)` applied to an already-computed `Vec<WorktreeReport>`, so it can only narrow, never widen, reorder, or reach outside the cwd repo. And a plain `worktree list` (no `--mine`) already shows every worktree and every owner, so `--mine` discloses nothing a plain `list` did not — weak disclosure at most, no removal-gate bypass.

    **Why it cannot be authoritative, and what would have to change for it to become so:**
    1. A settable env var can never be authoritative on its own. It would need to become a claim the daemon verifies (the caller's pane id → the daemon's own record of that pane's orchestration) — which reintroduces exactly the daemon dependency M3.0's "Why an env var rather than a runtime daemon query" section rejected, for the reasons stated there.
    2. The marker's `created-by:` line is not authenticated either. Fork #144's containment check proves *the deck created this worktree*; it does not prove *which orchestration*, because anything with write access to the worktree's admin dir can rewrite the identity line.
    3. `mark_worktree_owned` is a non-atomic `std::fs::write`. Its own doc comment already names the point at which a torn write stops being cosmetic: once matching is load-bearing (this PR), and more so once authorization is load-bearing (#175). Write-temp-then-rename is a tracked follow-up, not done in this round.

    The authoritative ownership signal for anything destructive remains `ownership_of`'s containment check plus marker presence (fork #144), which this variable does not and must not participate in.

### Phase 4: Ship

- [x] **M4.0** — docs; the deployment precondition below stated in the changelog. **Done.** `docs/orchestration.md` documents `worktree list --mine`, the OWNER column, the `DOT_AGENT_DECK_WORKTREE_OWNER` variable (mirroring the existing `DOT_AGENT_DECK_PANE_ID` documentation), and that the identity now survives a tab close/reopen because it is persisted in the saved session; `changelog.d/166.feature.md` records the feature and both deployment preconditions — a pre-#166 worktree renders a dash in the OWNER column and is never matched by `--mine` until it is recreated, and a session snapshot saved before the M2.4 persistence fix (or before the `owner` field existed at all) restores its orchestration tab with no identity, so `--mine` refuses loudly for that tab until it is reopened fresh. CLAUDE.md rule 1's manual `git worktree add` stays for now — it is replaced by fork #175, not by this PRD.

### Moved to fork #175

Provisioning (`delegate` creating `<orchestration-name>-<change>` in one step), worktree reuse, and refusal when a target is owned by another orchestration. All depend on the identity and ownership this PRD establishes.

## Key Files

- `src/ui.rs` — `FormField::Name` (`:822`), the orchestration `display_title` assignment (`:8264-8266`), `resolve_orchestration_worktree_path` (`:6627`), `validate_orchestration_worktree_slug` (`:6572`), `live_orchestration_cwds` (`:790` — the existing liveness query a uniqueness check can reuse)
- `src/agent_pty.rs` — `TabMembership::Orchestration.display_title` (`:341`) and its contract docs
- `src/worktree_reclaim.rs` — `OWNER_MARKER_FILENAME`, `mark_worktree_owned` (`:602`), `ownership_of`, and the containment protecting `main`; plus the read side and display seam this PRD finishes: `read_marker_owner` (`:502`), `owner_of` (`:545`), `WorktreeReport.owner` (`:168`), `format_list_human` (`:900` — where the OWNER column goes), `SCHEMA_VERSION` (`:37`)
- `src/issue_dispatch_run.rs` — `create_worktree_sync` (`:1058`), `mark_worktree_owned_best_effort` (`:1036`), the `issue-dispatch:<task>#<issue>` creator (`:402`), and the `display_title: None` dispatch sites
- `src/main.rs` — `WorktreeCmd::List` (`:393`, where `--mine` goes), dispatch (`:1235`), `run_worktree_list_cli` (`:1672`)
- `src/agent_pty.rs` — the env-var pattern M2.4 follows: `DOT_AGENT_DECK_PANE_ID` (`:41`), `DOT_AGENT_DECK_AGENT_ID` (`:56`), and the scrub-then-overlay block (`:1073`–`:1079`) that stops a daemon launched inside another deck's pane leaking a stale value
- `prds/140-orchestration-session-partitioning.md` — the `display_title` contract to update
- `tests/CATALOG.md` — `worktree/reclaim/*`, `orchestration/worktree/*`

## Risks and Mitigations

- **Deployment precondition: existing worktrees become obsolete.** They carry a marker but no owner, so no orchestration can claim them. **Drain them before shipping.** They must still resolve `Ours` so `reclaim` keeps working — obsolete for *ownership*, not for cleanup. This needs its own test; it protects every worktree created before this ships.
- **An identity that changes across restarts breaks resume.** The trap `orchestration_id` sets. Mitigated by using the name, and by a test that resumes **after a simulated restart** — a same-session test would pass even with the broken design.
- **Promoting `display_title` silently.** It is documented as cosmetic in PRD #140 and in the field's own docs. Mitigated by updating both, and by rule 12's `.breaking.md`.
- **Ownership becoming a licence to delete.** Ownership answers "may I work here?", not "may I remove this?". `reclaim`'s gates are unchanged and this PRD adds no removal path.
- **`worker-task-<role>.md` still collides per-directory.** `src/state.rs:2059` keys it by role alone — PRD #140's layer 2, still deferred. Since every orchestration here starts in `main`, two *inline* `--task` delegations to the same role could clobber each other; the fork's documented `--task-file` default supplies a unique slug, so the common path is unaffected. Recorded so it is not mistaken for solved. (Its `work-done` twin **is** fixed — `work_done_file_name(role, pane_id)`; the collisions seen 2026-08-09 came from a running daemon predating that fix.)

## Open Questions

- **Is uniqueness scoped to live orchestrations, or to all names ever used?** Live-only is simpler and matches the daemon's existing query, but it lets a name be reused after a tab closes — and that reuse is *exactly* how resume is meant to work. Leaning live-only, with resume as the intended consequence rather than a loophole.
- **What happens to a running orchestration whose name is edited?** Simplest is that names are fixed at tab open.
- **Should `<change>` be supplied or derived?** Supplied is explicit; derived from the task risks unstable or colliding slugs.
- ~~**`SCHEMA_VERSION`** for the added owner field in `worktree list --json` — the field is additive.~~ **Settled 2026-08-10: it stays at `1`.** The `owner` field shipped *at* schema 1 with `skip_serializing_if`, so it is already absent-not-null for older clients; bumping now would announce a change that has already happened silently. See M2.3.
