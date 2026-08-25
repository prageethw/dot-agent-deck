# PRD fork#603 — Orchestration name uniqueness scoped to (directory, name), not name alone

**Issue:** [prageethw/dot-agent-deck#603](https://github.com/prageethw/dot-agent-deck/issues/603)
**Priority:** High
**Status:** Planning
**Related:** `prds/done/fork-192-orchestration-name-as-identity.md` (built the name-only system this extends), fork issue #201 (the daemon claim registry this touches was built to close its TOCTOU race), `prds/done/fork-544-*` shared-clone/workspace-path architecture (confirmed orthogonal — see Problem Statement), PRD #140 (the same-cwd warning this reuses a comparison helper from)
**Fork-only?** No — the new-pane form and the daemon claim registry are upstream-shared code (fork#192's own M2.0 already offered the name-only system's form-level commit upstream as [vfarcic/dot-agent-deck#539](https://github.com/vfarcic/dot-agent-deck/pull/539), still open). Fix here first per CLAUDE.md rule 19; offer upstream once shipped.

## Problem Statement

Orchestration tab-name uniqueness is enforced **globally by name alone**, at two independent layers, neither of which considers the working directory an orchestration runs in:

1. **Client-side (advisory) check** — `suggest_orchestration_name()` (`src/ui.rs:1723`) and `name_collision()` (`src/ui.rs:1819`) compare a candidate against `NewPaneFormState.live_orchestration_names: Vec<String>` (`src/ui.rs:1598`), a flat, directory-blind list.
2. **Daemon-side (authoritative) claim** — `AgentPtyRegistry::claim_orchestration_name(&self, name: &str, pane_id: &str) -> bool` (`src/agent_pty.rs:3735`) keys `orchestration_name_claims: Mutex<HashMap<String, String>>` (`src/agent_pty.rs:3051`) by name only. The wire message it serves, `AttachRequest::ClaimOrchestrationName { name, token }` (`src/daemon_protocol.rs:641`), carries no directory field at all. This is the layer that actually decides on submit — it was added specifically (fork #201) to close a race the client-side check alone can't, so a fix that only touches layer 1 produces a contradictory UX: the form allows a submission the daemon then refuses anyway.

**The invariant that matters is narrower than what's enforced today.** The original motivating incident (fork #74) is about two live orchestrations *sharing one directory* being indistinguishable from each other. Nothing about that incident requires two orchestrations in **unrelated** directories to be forced into different names. Today's global check is stricter than the product needs, and the fix is to widen the uniqueness key from `name` to the compound **`(directory, name)`** pair.

**This is not a new shape in the codebase.** `state::OrchestrationIdentity::NameCwd { name, cwd }` (`src/state.rs:503-521`) already uses exactly this compound key, for a different purpose (delegate/work-done message *routing*, via `pane_orchestration_map: HashMap<String, OrchestrationIdentity>`, `src/state.rs:788`) — not creation-time uniqueness. The two mechanisms should stay structurally separate (see Decisions), but the precedent confirms the shape is already trusted in this codebase.

**Directory is already tracked and in scope everywhere this matters** — nothing needs to be plumbed from nowhere:
- `Tab::Orchestration.cwd: String` / `Tab::Mode.cwd: String` (`src/tab.rs:132`, `:117`).
- `TabMembership::Orchestration.orchestration_cwd: Option<String>` (`src/agent_pty.rs:503-524`), already returned per-record by the daemon's `ListAgents` response, paired with `name`/`display_title`.
- At both `ClaimOrchestrationName` send sites (`src/ui.rs:10721`, `:13445`), the directory (`req.dir`, `saved_pane.dir`) is already in local scope one line above the call.
- The one place the pairing is actually destroyed: `live_orchestration_cwds_and_titles()` (`src/ui.rs:1207-1239`) reads `resp.agent_records` where each record carries `(orchestration_cwd, name)` together, then immediately shreds them into two independently-deduplicated lists (`cwds`, `titles`) before they reach `NewPaneFormState`. That's the exact point the association needs to survive instead.

### Resolving the prior "rename collision" decision (must not be silently overridden)

fork#192's "Open Questions" explicitly considered and rejected per-directory scoping:

> "What is `N` counted over — live orchestrations in this cwd, or all live orchestrations? ... global-over-live is the safer default: uniqueness is what the name is for, and a per-cwd counter could suggest `N=1` in two different directories that then happen to share a basename later (e.g. after a rename), producing a real collision."

That concern was about a **per-directory *suggestion counter*** operating underneath **global name-only *uniqueness***: two empty directories independently suggest `N=1`, one is later renamed to share the other's basename, and the *then-existing* name-only check would treat the resulting identical names as a real collision. **This does not transfer to a compound uniqueness *key*.** The directory half of the key is never a cached basename or a suggestion-time snapshot — it is the live `cwd` string reported fresh by the daemon's *current* `ListAgents` response (client side) or the caller's *current* `req.dir`/`saved_pane.dir` (daemon side), re-read and re-compared on every check. After a rename, a directory's `cwd` value simply reflects its new path on the very next read; there is no stale text anywhere that "used to be right." Two directories renamed into apparent basename agreement still have distinct absolute paths. **Verdict: the rename-collision concern is specific to a per-cwd counter sitting under a global check, and does not apply once uniqueness itself is `(dir, name)`.** This PRD supersedes that Open Question rather than silently ignoring it.

### Confirmed orthogonal: workspace/worktree path derivation

`resolve_workspace_path(dir, segment)` (`src/ui.rs:9024`) already derives the physical worktree path from the *picked directory's parent* (`dir.with_file_name("{dir_basename}-{segment}")`), so two orchestrations with an identical typed Name in two different directories never collide on disk today, regardless of this PRD. What collides today is purely the logical claim/suggestion layer. This PRD does not touch workspace provisioning or `src/issue_dispatch_run.rs`.

## Decisions

| Question | Decision |
|---|---|
| Uniqueness key | Compound `(directory, name)`, not name alone, at **both** enforcement layers. |
| Suggestion-counter scope (`N` in `{basename}-orchestrator-N`) | **Per-directory**, filtered the same way the collision check is. fork#192's rejection of this is now moot, not merely weakened: it was rejecting a per-cwd counter specifically *because* it sat under global name-only uniqueness, where two directories' suggestions really could collide. Once uniqueness itself is per-directory, keeping the counter global would be strictly worse — a brand-new directory could be offered `-orchestrator-3` because an unrelated directory happens to hold three live orchestrations, with no correctness benefit. |
| New daemon-side key type | A dedicated struct, `OrchestrationClaimKey { name: String, cwd: Option<String> }` — **not** a reuse of `state::OrchestrationIdentity`. That enum's `Instance` variant is meaningless for this purpose and reusing it would blur two structurally separate mechanisms (routing identity vs. creation-time claim). |
| Wire shape change | Additive: `ClaimOrchestrationName` gains `#[serde(default)] cwd: Option<String>`. No `PROTOCOL_VERSION` bump (see Backward compatibility below) — confirm this via the mandatory rule-12 cross-version manual test rather than asserting it. |
| Backward compatibility semantics | A claim with `cwd: None` (an old TUI, or explicitly a caller that doesn't know its directory) is treated as a **global wildcard**: it conflicts with a claim of the same name from *any* directory, and is conflicted-with by any `Some(cwd)` claim of the same name. This preserves an old client's own assumption that its claim was exclusive everywhere, and means an old-TUI/new-daemon pairing is byte-for-byte unchanged from the user's perspective; a new-TUI/old-daemon pairing silently drops the field and falls back to the daemon's original global-only behavior, which can only over-refuse relative to the new design, never under-refuse (never let a real same-`(dir,name)` collision through). |
| Directory-equality comparison | Extract a single shared helper reused by **both** the existing same-cwd warning (`live_orchestration_in_same_cwd`, `src/ui.rs:1138`) and the new suggestion/collision filter, rather than let two subtly different notions of "same directory" (literal string vs. canonicalized) drift apart. Canonicalize once, at the sender, before the claim is sent — the daemon-side comparison stays a plain string equality. |

## Design

### M1 — Client-side: carry `(cwd, name)` pairs through the form

**File: `src/ui.rs`**

1. `live_orchestration_cwds_and_titles()` (`:1207-1239`): stop shredding the pairing. Change return shape from `(Vec<String>, Vec<String>)` to `Vec<(String /* cwd */, String /* title */)>`, deduped on the whole pair (two role panes of the same orchestration still collapse to one entry — a strict generalization of the current per-list dedup).
2. At the one call site (`:8806-8810`), derive the flat cwd list `with_live_orchestration_cwds` still needs from this same paired vec (e.g. `pairs.iter().map(|(cwd, _)| cwd.clone()).collect()`), so that builder itself is untouched.
3. `NewPaneFormState.live_orchestration_names: Vec<String>` (`:1598`) becomes `live_orchestration_identities: Vec<(String /* cwd */, String /* name */)>`. Replace `with_live_orchestration_names(names: Vec<String>)` (`:1711`) with `with_live_orchestration_identities(identities: Vec<(String, String)>)`.
4. Extract the directory-equality logic already inside `live_orchestration_in_same_cwd` (`:1138-1151`, literal-match fast path then canonicalize-both-sides fallback) into a shared `fn cwd_matches(form_cwd: &Path, candidate: &str) -> bool`, used by both the existing same-cwd warning and the new filter below.
5. `suggest_orchestration_name()` (`:1723-1758`): filter `live_orchestration_identities` to entries where `cwd_matches(&self.dir, cwd)` before building the exclusion set for `{basename}-orchestrator-{n}`.
6. `name_collision()` (`:1819-1822`): change to `self.live_orchestration_identities.iter().any(|(cwd, name)| cwd_matches(&self.dir, cwd) && name == t.trim())`.
7. Update the doc comments that currently assert name-only global semantics (the "counted globally... not reopened here" language) to point at this PRD, following this file's own convention of narrating *why* a prior decision changed rather than silently changing the code under it.

### M2 — Daemon-side: compound claim key + wire field

**Files: `src/agent_pty.rs`, `src/daemon_protocol.rs`**

1. Wire type (`src/daemon_protocol.rs:641`): add `#[serde(default)] cwd: Option<String>` to `ClaimOrchestrationName`. No `deny_unknown_fields` exists on `AttachRequest` today (matches the established additive-field pattern already used for `TabMembership::Orchestration::orchestration_cwd`), so this needs no companion attribute change.
2. Registry key (`src/agent_pty.rs:3051`): `orchestration_name_claims: Mutex<HashMap<String, String>>` → `Mutex<HashMap<OrchestrationClaimKey, String>>` where `OrchestrationClaimKey { name: String, cwd: Option<String> }` (new, dedicated type — see Decisions).
3. `claim_orchestration_name` (`:3735-3744`): new signature `pub fn claim_orchestration_name(&self, name: &str, cwd: Option<&str>, pane_id: &str) -> bool`. Cannot stay a plain `get`/`insert` because of the wildcard semantics decided above — implement as a scan:
   ```rust
   pub fn claim_orchestration_name(&self, name: &str, cwd: Option<&str>, pane_id: &str) -> bool {
       let mut claims = self.orchestration_name_claims.lock().unwrap();
       let conflict = claims.iter().any(|(k, holder)| {
           k.name == name
               && holder != pane_id
               && (k.cwd.is_none() || cwd.is_none() || k.cwd.as_deref() == cwd)
       });
       if conflict {
           return false;
       }
       claims.insert(
           OrchestrationClaimKey { name: name.to_string(), cwd: cwd.map(str::to_string) },
           pane_id.to_string(),
       );
       true
   }
   ```
   The existing idempotent-reclaim-by-same-holder behavior (pinned by `identity_022`-region tests) is preserved via the `holder != pane_id` guard.
4. `release_orchestration_name` (`:3751-3756`) and `confirm_orchestration_claim` (`:3770-3780`) both operate purely on the *value* (holder string) — `claims.retain(|_, holder| holder != pane_id)` and `claims.iter().find(|(_, holder)| holder == token)` — so neither needs a signature change, only the key type they index over changes.
5. Handler (`src/daemon_protocol.rs:2519`): pass the new field through — `registry.claim_orchestration_name(&name, cwd.as_deref(), &token)`.

### M3 — Call sites: supply `cwd` at both claim points

**File: `src/ui.rs`**

1. Live spawn path (`:10711-10727`): `req.dir` is already in local scope one line before the existing `resolve_orchestration_name(&orch_config.name, &req.dir)` call. Canonicalize once here (`req.dir.canonicalize().unwrap_or_else(|_| req.dir.clone())`) and add `cwd: Some(...)` to the `ClaimOrchestrationName` literal at `:10721`.
2. Restore/reconnect path (`:13431-13448`): same treatment using `saved_pane.dir`, already used two lines above (`:13438-13439`) to build `restore_claim_name`.
3. `ConfirmOrchestrationClaim`/`ReleaseOrchestrationName` never carried name or cwd and still don't — the `cwd` rides only on the initial claim.

### Rule 12 mechanics (CLAUDE.md)

- Run the mandatory cross-version manual test: this branch's TUI against a **previous-release daemon**, and a daemon built from this branch against an **old TUI** build. Confirm delegate/hooks still route (unaffected, but rule 12 requires checking the core-flow list regardless of what changed), and specifically confirm: (a) old TUI → new daemon still refuses a same-name collision globally (wildcard semantics, unchanged from the user's perspective), and (b) new TUI → old daemon degrades to global-only refusal (stricter, never weaker, than the new design).
- Isolate `DOT_AGENT_DECK_LOG` alongside sockets/`HOME` for the sandboxed run, per rule 12's explicit warning about shared log pollution across two daemons.
- Record the manual test's result in this PRD's milestone evidence. If it contradicts the "no bump, no `.breaking.md`" conclusion above, add `changelog.d/603.breaking.md` and/or bump `PROTOCOL_VERSION` at that point instead of before.
- Rule 9 (experimental flag) does not apply — this changes enforcement scope on an existing surface, not a new user-visible one.

## Milestones

- [ ] M1 — Client-side `(cwd, name)` pairing: `live_orchestration_cwds_and_titles()` returns paired data, `NewPaneFormState` carries `live_orchestration_identities`, shared `cwd_matches` helper extracted, `suggest_orchestration_name`/`name_collision` both directory-filtered.
- [ ] M2 — Daemon-side compound claim key: `OrchestrationClaimKey` type, `claim_orchestration_name`'s wildcard-aware conflict scan, wire field addition on `ClaimOrchestrationName`.
- [ ] M3 — Both claim call sites (`:10721`, `:13445`) supply a canonicalized `cwd`.
- [ ] M4 — Existing tests updated (builder/signature shape only, same intent): `orchestration/identity/003`, `/004`, `orchestration/guard/002`, `/003` (`src/ui.rs`, ~34528+/35044+/35159+) move from `.with_live_orchestration_names(vec![...])` to `.with_live_orchestration_identities(vec![(dir, name), ...])`; the `identity/02x` daemon claim-race tests (`src/agent_pty.rs`) move to the 3-arg `claim_orchestration_name` signature.
- [ ] M5 — New tests (previously-impossible surface, not modifications): client-side proof that two different directories both suggesting/holding `<basename>-orchestrator-1` do not collide; daemon-side proof that `claim("x", Some("/a"), p1)` and `claim("x", Some("/b"), p2)` both succeed while `claim("x", Some("/a"), p3)` is refused; daemon-side backward-compat proof that a `None`-cwd claim conflicts with any `Some(cwd)` claim of the same name in both directions; an e2e test opening two different fixture directories that resolve to the same suggested name and confirming both land un-blocked. `tests/CATALOG.md` gets entries for every new `#[spec(...)]` id.
- [ ] M6 — Rule 12 cross-version manual test run and recorded (see above), `PROTOCOL_VERSION`/`.breaking.md` decision confirmed or corrected based on its result.
- [ ] M7 — Offer upstream per rule 19 once merged here, alongside/against the still-open fork#192 upstream PR (#539), since both touch the same new-pane-form surface.

## Test plan

Mix of L1 (unit tests on `NewPaneFormState`/`AgentPtyRegistry`, fast and deterministic) and one L2/e2e case for the cross-directory suggestion flow end to end. No real-agent test is needed — this is pure naming/claim logic, not spawn/attach behavior. Existing-test updates (M4) are mechanical (signature/builder shape) and should be reviewed separately from the genuinely new surface (M5) in the PR description, so churn volume doesn't obscure what's actually new behavior.

## Out of scope

- Workspace/worktree path provisioning (`resolve_workspace_path`, `src/issue_dispatch_run.rs`) — confirmed orthogonal above.
- Renaming a running orchestration — unchanged from fork#192's own scoping decision.
- Uniqueness across all names ever used, not just live ones — unchanged from fork#192; a name is still reusable once its tab closes.
- Any restriction on where orchestration tabs start — unchanged (PRD #140's advisory-only stance).
