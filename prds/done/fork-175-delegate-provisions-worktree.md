# PRD fork#175: `delegate` provisions the worktree itself

**GitHub Issue**: [fork #175](https://github.com/prageethw/dot-agent-deck/issues/175)

**Priority**: Medium

**Status** *(corrected 2026-08-16)*: **Merged into the fork** — [PR #271](https://github.com/prageethw/dot-agent-deck/pull/271) (issue #175 closed). The doc previously still read "Planning" despite the PRD having shipped.

**Parent**: [fork #166](https://github.com/prageethw/dot-agent-deck/issues/166) — this is its second half, split out so the riskier first half could land alone. fork#166 is **complete and released in v0.37.0**; its own M4.0 note says CLAUDE.md rule 1's manual `git worktree add` *"is replaced by fork #175, not by this PRD."* This is that replacement.

**Related**: fork [#122](https://github.com/prageethw/dot-agent-deck/issues/122) (per-orchestration worktree creation and the hardened path validation reused here) · fork [#144](https://github.com/prageethw/dot-agent-deck/issues/144) (the ownership marker) · fork [#192](https://github.com/prageethw/dot-agent-deck/issues/192) (the unique name that makes ownership meaningful) · PRD #120 (issue-dispatch, which already provisions its own worktrees this way) · [upstream #220](https://github.com/vfarcic/dot-agent-deck/issues/220) (the upstream cousin) · fork [#74](https://github.com/prageethw/dot-agent-deck/issues/74) (the motivating collision) · fork [#218](https://github.com/prageethw/dot-agent-deck/issues/218) (the marker write is not atomic — reachable from here)

## Fork-only — but the old justification has expired, so here is the correct one

fork#166 and fork#175 both justified fork-only status mechanically: *"`src/worktree_reclaim.rs` **does not exist** on `upstream/main` and `worktree_slug` has **0** occurrences."*

**That test no longer holds.** Verified against `upstream/main` @ `3496e7ab` on 2026-08-13: `src/worktree_reclaim.rs` **does exist** upstream — the reclaim half landed via upstream PR [#427](https://github.com/vfarcic/dot-agent-deck/pull/427).

The conclusion is unchanged but the reason must be restated, because a justification that has silently expired is worse than none — the next person re-runs the old check, sees it fail, and does not know whether the answer changed. Re-verified on the same commit:

| Primitive this PRD builds on | On `upstream/main` |
|---|---|
| `mark_worktree_owned` | **absent** |
| `owner_of` | **absent** |
| `sanitize_marker_creator` | **absent** |
| `DOT_AGENT_DECK_WORKTREE_OWNER` | **absent** |

Upstream took the *reclaim* half and not the *ownership* half. Since ownership is the entire basis on which this PRD reuses, refuses and attributes a worktree, it remains **fork-only** — on the grounds that the mechanism is absent upstream, not that the file is.

**Flagged as follow-up work, not done here:** the ownership half is now a coherent, self-contained addition to a file upstream already has, which makes it a far more offerable change than it was when fork#166 declared it permanently forked. Rule 19's default should be re-applied to it deliberately rather than inherited from a stale finding.

## Problem Statement

Creating a worktree is a **human step**, and CLAUDE.md rule 1 mandates it: the orchestrator runs `git worktree add` by hand, unsets the upstream, and states the absolute path, exact SHA and branch name in every task it delegates. Rule 16 exists precisely because that supply obligation kept being forgotten.

The cost is documented, repeatedly, as fork **#74**:

> a second orchestration's task named no worktree path, so its worker found the first orchestration's existing branch and reasonably joined it — writing production code into a worktree it did not own, then pushing to it and cancelling the first orchestration's in-flight CI run.

More than once. **The goal is autonomous operation** — spinning up agents without a human provisioning directories first.

fork#166 built the identity that makes this answerable: a required, unique orchestration name, recorded as the owner of each worktree, surviving a restart, queryable via `worktree list --mine`. What is still missing is the step that *uses* it.

## Solution Overview

**`delegate` provisions the worktree in one step and returns its absolute path**, so the task the worker receives carries the path, branch and SHA without a human typing them.

Three properties, and the last two are what make it safe to call repeatedly:

1. **Provision.** Create `<orchestration-name>-<change>`, reusing fork #122's hardened `resolve_orchestration_worktree_path` validation and `issue_dispatch_run::create_worktree_sync` — **not** a second implementation of either. Record the delegating orchestration as owner.
2. **Reuse.** Delegating the same change twice returns the **existing owned** worktree rather than re-creating it or failing. A retried delegation must be a no-op, not an error.
3. **Refusal.** A target owned by a *different* orchestration is refused, with a reason **naming the owner** — never raw git output. This is fork #74's exact failure, converted from a silent join into a loud refusal.

### PROBE RESULT (2026-08-13) — the decision below was WRONG, and here is what replaces it

A feasibility probe tested the premise before any code was written. Full evidence: `.dot-agent-deck/findings-175-probe.md` in the root checkout.

**The propagation half is fine.** `DOT_AGENT_DECK_WORKTREE_OWNER` would reach a pane's shell subprocess — verified live, and `src/main.rs:890`'s `Delegate` handler already reads `DOT_AGENT_DECK_PANE_ID` from its own environment today, so this is a working mechanism rather than a theory.

**The premise is false anyway.** `creator` is only computed on the branch of the New Pane form that *creates a worktree* — an **optional** "Worktree:" field (`resolve_orchestration_worktree_request`, `src/ui.rs:7708`), where blank is valid and never assigns `creator`. So an orchestration opened the ordinary way carries no owner identity at all.

Measured against the running deck, not inferred:

| Check | Result |
|---|---|
| Orchestrator-shaped panes carrying `DOT_AGENT_DECK_WORKTREE_OWNER` | **0 of 8** |
| Those panes' `cwd` | all 8 = the root checkout |
| `dot-agent-deck-owner` marker files anywhere in the workspace | **zero** |
| Persisted orchestrations in the real `session.toml` with an `owner` | **0 of 2** |

That is not an edge case, it is the normal workflow — and it matches CLAUDE.md rule 1's model exactly (orchestrator in the root checkout, worktrees hand-created per task). CLI-side provisioning would therefore hit *"absent identity, refuse loudly"* on **nearly every real invocation**, defeating this PRD's entire goal.

### The replacement decision: make the identity unconditional, from the required unique name

The identity problem is upstream of the CLI-vs-daemon question, and it already has an answer that shipped: **fork#192 made the orchestration Name required and unique** (released in v0.37.0). Every orchestration has one. What is missing is that the owner string is derived from it **only on the worktree-creating branch** of the form.

So: compute the creator identity for **every** orchestration pane, from the typed unique name, rather than only when the form happens to create a worktree. `orchestration_creator_string` (`src/ui.rs`) already builds `format!("orchestration:{typed_name}")` and already applies `sanitize_marker_creator`; the work is to reach it on every spawn path, not to invent anything.

**Why this over the alternatives the probe offered:**

- **Daemon-side resolution from `TabMembership`/`orch_config.name`** would work, but fork#166 M2.4 explicitly prohibits reconstructing the owner daemon-side. The probe rightly notes that prohibition was written for `--mine`'s *read-only listing* and that whether it should bind a *provisioning* decision deserves an explicit ruling rather than inheritance. It is still the wrong trade here: it adds a daemon round trip, **flips this PRD's rule 12 answer to a protocol change**, and leaves the identity a derived guess rather than the value the user actually typed.
- **Passing the identity as an explicit `delegate` argument** re-opens rule 16's supply problem at exactly the seam this PRD exists to close — the orchestrator would once again be hand-supplying a value nothing guarantees.

**Fallback, stated so it is not rediscovered:** if reaching `orchestration_creator_string` on every spawn path turns out to be structurally impossible, fall back to daemon-side resolution **and accept the `PROTOCOL_VERSION` bump** — the conditional framing in the rule 12 section below already anticipates exactly this trigger. Do not quietly keep the CLI-side answer while the identity is absent.

### Also established by the probe, and independently important

- **`resolve_orchestration_worktree_path` (`src/ui.rs:7792`) is private.** Its body is pure path/string logic with no TUI or daemon state, so this is a one-word visibility change (`fn` → `pub(crate) fn`), not a relocation. Stated precisely here rather than left as "callable as-is".
- **`git worktree add` does NOT safely serialise the reuse path.** For `-b` (new branch) git's ref-lock genuinely serialises — 3-way race, 1 winner, 2 clean failures. For **attaching an existing branch**, which is exactly M2's reuse case, a 2-way race over 25 trials corrupted twice (**~8%**): both processes exit 0 and git registers two admin entries for the identical path. The Risk table's guess was right for creation and wrong for reuse. **M2 needs a real lock**, and this affects issue-dispatch today, not only this PRD — filed as fork [#282](https://github.com/prageethw/dot-agent-deck/issues/282).
- **Issue-dispatch synthesises its own identity** (`issue-dispatch:{task_name}#{issue}`) rather than reading the env var, so a `delegate` inside a dispatch-spawned pane would name the dispatch task rather than a human orchestration. A gap this PRD does not currently address; decide it in M1.

### Decision (SUPERSEDED — retained as the record): provisioning happens CLI-side, not daemon-side

The `Delegate` command already runs **inside the orchestrator's own pane** (`src/main.rs:885`), which is where `DOT_AGENT_DECK_WORKTREE_OWNER` lives — fork#166 M2.4 put it there, beside `DOT_AGENT_DECK_PANE_ID`, threaded from one computed value through both spawn paths.

So the identity needed to stamp and match an owner is already in the calling process's environment. Provisioning there:

- **needs no new daemon RPC**, so no `PROTOCOL_VERSION` bump (see Rule 12 below);
- matches `run_worktree_list_cli`, which fork#166 M3.0 deliberately kept **daemon-free** for the same reason;
- keeps the daemon out of git operations that can block for seconds, which `WORKTREE_GIT_TIMEOUT` exists to bound.

Reconstructing the owner daemon-side from `TabMembership` / `display_title` is the shortcut fork#166 M2.4 explicitly prohibits. Do not reintroduce it here.

### The identity must fail loudly, exactly as `--mine` does

`worktree list --mine` already refuses on an absent variable, on the `orchestration:unknown` sentinel, and on an exported-but-empty one — `std::env::var` returns `Ok("")` for the last, which without a guard became the filter and produced a confident wrong answer. **Provisioning inherits all three refusals**, and for a stronger reason: `--mine` returning a wrong list is a bad answer, while provisioning under a wrong identity **stamps** it onto disk, where the next run reads it back as truth.

Normalise through `sanitize_marker_creator` exactly once and use that single string for both the stamp and the comparison — comparing a raw value against an always-sanitized marker is the bug fork#166's round-4 fixup had to correct.

## Scope

### In Scope

- `delegate` gaining worktree provisioning, with the path returned to the caller.
- Reuse and refusal semantics, including the owner-naming refusal message.
- Threading the resolved path (and branch, and SHA) into the delegated task so rule 16's supply obligation is satisfied **mechanically** rather than by the orchestrator remembering.
- Docs: `docs/orchestration.md`, plus the CLAUDE.md rule 1 amendment that retires the manual step.
- A PTY-attached L2 test (rule 4) and a real-agent test.

### Out of Scope

- **Removing rule 1's supply obligations.** The path, SHA and branch still reach the worker; they are simply no longer typed by hand. Rule 16 is unchanged.
- **Reclaiming or removing worktrees** — PRD #422, already shipped.
- **Making the marker write atomic** — fork #218. Reachable from here and *worsened* by it (this path writes markers far more often), but a separate change. Note it in the PR.
- **Offering the ownership half upstream** — flagged above, not done here.
- **Changing how issue-dispatch provisions.** It already does this; this PRD reuses its function rather than replacing its call site.

## Milestones

### M1 — provisioning

- [ ] `delegate` resolves a worktree path from the orchestration identity plus a change slug, via `resolve_orchestration_worktree_path`.
- [ ] Creation goes through `create_worktree_sync`, so branch-probe, timeout classification and best-effort cleanup behaviour are inherited, not re-written.
- [ ] The owner is stamped from `DOT_AGENT_DECK_WORKTREE_OWNER`, sanitized once.
- [ ] Absent / sentinel / empty identity **refuses loudly**, naming which of the three it was.

### M2 — reuse and refusal

- [ ] Same change delegated twice → the existing owned worktree is returned; no second `git worktree add`, no error.
- [ ] A target owned by a different orchestration → refused, message **names the owner**, no raw git output leaks.
- [ ] A target that exists but carries **no** marker → decide and document one behaviour. `owner_of` resolves this as `Ours` with owner unknown (containment and presence stay authoritative), so the safe reading is *reuse*; state it explicitly rather than letting it fall out of the implementation.

### M3 — the task contract

- [ ] The returned absolute path, branch and SHA reach the delegated task automatically.
- [ ] CLAUDE.md rule 1 is amended: the manual `git worktree add` is retired, the supply obligation is not.

### M4 — proof

- [ ] Unit coverage for reuse, refusal and each identity-failure mode.
- [ ] **A PTY-attached L2 test** driving the real flow end to end — rule 4 requires one for a user-facing surface, and this is one.
- [ ] A real-agent test on a cheap model, with a uniquely-named sentinel file, proving an agent genuinely runs in the provisioned worktree rather than a stand-in proving only the plumbing.

### M5 — ship

- [ ] `docs/orchestration.md` and a changelog fragment.
- [ ] Rule 12 answer recorded (below).

## Success Criteria

1. An orchestration delegates work without any human running `git worktree add`.
2. Delegating the same change twice is a no-op that returns the same path.
3. A worktree owned by another orchestration is refused with its owner named — fork #74's mechanism, made impossible rather than discouraged.
4. A wrong or missing identity refuses; it never stamps a guess onto disk.
5. Rule 1 no longer asks a human for the step, and rule 16 still holds.

## Key Files

- `src/main.rs:885` — the `Delegate` command, where provisioning is added.
- `src/ui.rs:7792` — `resolve_orchestration_worktree_path`, reused.
- `src/issue_dispatch_run.rs:1363` — `create_worktree_sync`, reused, and `mark_worktree_owned_best_effort` (`:1341`).
- `src/worktree_reclaim.rs:634` — `owner_of`; `:692` — `mark_worktree_owned`; `:743` — `sanitize_marker_creator`.
- `src/agent_pty.rs` — where `DOT_AGENT_DECK_WORKTREE_OWNER` is set.

## Rule 12 — cross-version contract

**Expected answer: no `PROTOCOL_VERSION` bump, no `.breaking.md`.** Provisioning is CLI-side and daemon-free by the decision above, so the `DelegateSignal` frame is unchanged.

**That expectation is conditional and must be re-answered if the implementation moves.** If provisioning ends up needing a daemon round trip — to resolve the identity, to serialise concurrent provisioning, or to return the path — then the frame changes and this answer is void. fork#197's M4 recorded exactly this shape of conditional answer and it is the right pattern: state the expectation, name what would invalidate it, and re-check before the PR rather than inheriting it.

**The manual cross-version run is required regardless**, since this is orchestration-path work and the run is what catches a semantic break behind a stable wire. Isolate `DOT_AGENT_DECK_LOG` along with the sockets, `HOME` and state dir.

## Risks and Mitigations

| Risk | Mitigation |
|---|---|
| Auto-provisioning makes worktrees accumulate faster than they are reclaimed | PRD #422's `worktree list` / `reclaim` already exists and is the answer; verify `--mine` sees provisioned worktrees, and say so in the docs. |
| The marker write is non-atomic (fork #218) and this path writes markers far more often | Out of scope, but the exposure genuinely increases. Note it in the PR and link #218 so the two are not triaged independently. |
| Two orchestrations race to provision the same target | The refusal is the guard, but it is read-then-write and therefore racy in principle. Establish whether `git worktree add` itself is the serialisation point — `AddOutcome::AlreadyClaimed` suggests it is — and if so, say that the refusal is a *better message*, not the safety mechanism. |
| A slug derived from user text escapes the intended parent directory | `resolve_orchestration_worktree_path` is fork #122's hardened validation and is reused precisely so this is not re-litigated. Do not add a second sanitiser beside it. |
| Retiring rule 1's manual step reads as retiring rule 16's supply obligation | M3 amends rule 1 to say the opposite in as many words. |
