# PRD fork#235: Make the issue claim a lock that refuses, and make it say who holds it

**GitHub Issue**: [fork #235](https://github.com/prageethw/dot-agent-deck/issues/235)

**Priority**: High

**Status**: In progress — M1–M4 implemented and green at `375285d`, then **re-scoped after review**. Reviewer and auditor both returned blocking verdicts on the marker-based identity (see "Identity, round 2"). Round-2 fixes in flight.

## Threat model — read this before the design

**What this defends against: accidental collision between cooperating agents.** Fork #74 was an accident — a second orchestration that did not check a label nobody made it check. Every agent involved was trying to do the right thing. That is the whole of the problem this PRD exists to solve.

**What it explicitly does NOT defend against**, recorded as non-goals rather than left implied:

- **Replay.** The identity is published in a public claim comment, so it is a record, not a secret. Anyone who can read the issue can post a comment reproducing it. A deck reading that back sees "held by me" and proceeds.
- **Forgery.** Anyone who can comment on the issue can write a `Claimed by …` line. There is no author check on claim comments.
- **A hostile local process.** The ownership marker and the pane environment are both writable by anything running as the same user.

Calling this a "lock" oversells it: it is **cooperative claim coordination**. Prose in this PRD, the CLI help, and the changelog should say "claim", and reserve "lock" for something that could survive an adversary. The auditor's F2/F6/F8 are correct *as adversarial findings* and are out of scope by this definition — they are recorded here so a future reader knows they were considered, not overlooked.

**Fork-only**, and intended to stay so. Builds on PRD #421 (the claim), fork #192 (orchestration names as identity) and fork #144/#425/#166 (the worktree ownership marker) — none of which exist upstream, where [#421](https://github.com/vfarcic/dot-agent-deck/issues/421) is still open and unimplemented. Per `docs/develop/upstream-contribution-policy.md` this is not an offer-later case.

**Related**: fork [#201](https://github.com/prageethw/dot-agent-deck/issues/201) (name uniqueness is advisory — the defect this PRD routes around) · fork [#222](https://github.com/prageethw/dot-agent-deck/issues/222) (owner-identity collision edges) · fork [#74](https://github.com/prageethw/dot-agent-deck/issues/74) (the motivating collision) · fork #184, #218, #164 (the marker this reads) · upstream [#482](https://github.com/vfarcic/dot-agent-deck/issues/482) (nothing clears a claim), #483, #486, #484 (neighbouring issue-dispatch defects, not fixed here)

## Problem Statement

Two orchestrations working the same issue is not hypothetical. On 2026-08-07/08 (fork #74/#89) one orchestration claimed issue #89 and labelled it `in-progress`; a second delegated a coder on the same issue **eight minutes later** without checking, joined the first one's worktree, and cancelled its in-flight CI run with a same-branch push. Nothing *required* the second orchestration to look.

The mechanical path is genuinely gated. `dispatch_decision` reads three signals — worktree on disk, open PR on `agent/issue-<n>`, and the `in-progress` label — so dispatch never collides with dispatch. **The hole is everything else.** A human-started orchestration claims by hand-typing `gh issue edit --add-label in-progress`: no check, no attribution, and no way to fail. CLAUDE.md rule 14 asks it to check, in prose — and prose is what failed in #74.

PRD #421 built most of the record. Three gaps remain in it as a statement of *current* ownership:

1. **It names the cron entry, not the agent.** The claim comment names `ScheduledTask.name` — a hand-written config string like `nightly-triage`. When the dispatch opens an orchestration, the orchestration's own name (required and unique since fork #192) never reaches GitHub.
2. **Nothing is assigned.** `assignee` / `--add-assignee` / `assignees` have **zero** occurrences across `src/`, `.github/workflows/` and `tests/`. A machine-claimed issue is invisible in every human-facing view that keys on assignment.
3. **A handoff never updates the record.** `claim_issue` has exactly one caller, and a labelled issue is skipped (`dispatch/015`). Work moving from orchestration A to B leaves the claim naming A forever.

Gap 3 is the odd one, because the machinery for it already exists and has nothing driving it. `parse_claim_comment` (`src/issue_dispatch_run.rs:596`) takes the **last** matching comment, documented as review fix C2:

> the PRD deliberately APPENDS rather than edits in place precisely so a succession of claimants is preserved when one hands off to another.

An append-only log with newest-wins semantics, and no writer that ever produces a succession.

## Solution Overview

**A record does not stop anyone. A refusal does.** The centrepiece is a first-class claim command that fails when someone else holds the issue:

```
dot-agent-deck issue claim <n> [--repo <owner/name>] [--takeover] [--confirm-stopped]
```

| State | Flags | Result |
|---|---|---|
| Unlabelled | — | claim, exit 0 |
| Held by **this** identity | — | idempotent refresh, exit 0 |
| Held by a **different** identity | — | **refuse**, exit ≠ 0, naming holder, host and when |
| Labelled, **no claim comment** | — | **refuse** — identity unknown; the hand-typed rule 14 claim |
| Held by a different identity | `--takeover` | **still refuses**, instructing `--confirm-stopped` |
| Held by a different identity | `--takeover --confirm-stopped` | claim; append + assignee replace |

**The exit code is the mechanism.** A non-zero exit is what an agent's shell actually notices; a warning printed to stdout is not a gate. **The two-step override is deliberate friction, not a confirmation prompt** — `--takeover` alone must fail, so an agent cannot satisfy the override in the same breath it discovers the conflict.

On a successful claim the three surfaces divide the record:

| Surface | Answers |
|---|---|
| `in-progress` label | *that* it is held |
| **Assignee** | *who owns it now* — replaced on every claim |
| Comment log | *who has held it, in order* — appended, never edited |

The assignee has a defined meaning: **the human who owns the agent working the issue at that moment.** Not who filed it, not an accumulated history. That definition is what makes replace-to-one correct rather than merely convenient.

## Identity must be an instance, not a name

This decides whether the lock works at all, and it is the reason this PRD is not a small one.

Fork **#201** records that orchestration-name uniqueness is **advisory**: `name_collision()` reads a list captured once at form-open and never refreshed, so two forms open simultaneously are suggested the same name and *neither submit is refused*. Its own text names the consequence:

> **This is the case #74 is actually about.** #74's collision was two orchestrations running concurrently … it does not close the concurrent window.

Fork **#222** adds two more equality collisions: `sanitize_marker_creator` truncates at 200 characters while `ScheduledTask.name` is unbounded, so two task names sharing a 185-character prefix collapse to one identity; and an orchestration literally named `unknown` is byte-identical to the no-name sentinel.

A lock comparing bare names would therefore read two *distinct* holders as **equal**, hit the "held by this identity → idempotent refresh, exit 0" row, and wave both through — failing in precisely the scenario it exists for. So identity carries **name + host + worktree digest**:

| Form | Composition |
|---|---|
| `orchestration:<name>@<host>:<wt>` | name from the owner marker (`src/ui.rs:8850`); `<wt>` = first 8 hex of a digest of the worktree's absolute path |
| `issue-dispatch:<task>#<issue>@<host>:<wt>` | as today (`src/issue_dispatch_run.rs:402`), plus the same suffix |
| `human:<login>@<host>` | a human claiming outside any orchestration; no worktree, so no `<wt>` |

Comparison is on the **whole string**, never the name alone.

This **neutralises both halves of #222 as a side effect**: even when two long names truncate identically, or two orchestrations are both called `unknown`, their worktree paths differ — rule 1 mandates one worktree per change — so the composed identities differ. It does **not** fix #201; it makes the lock correct despite #201, which stays open.

**The path is digested, never emitted raw.** A claim comment is public, and `/Users/<name>/workspace/…` leaks the OS username and local directory layout. Eight hex of a digest preserves comparison and leaks nothing; #222 proposes exactly this technique for its truncation half, so it is already blessed in-repo. The host is already published in today's claim comment — accepted precedent, unchanged.

## Identity, round 2 — keyed on the pane, not the marker

The round-1 design derived identity from the worktree ownership marker. Reviewer and auditor both found it defeated, from opposite ends:

- **The marker is almost never there.** `owned_git_dir` requires the git-dir to sit under `<common>/worktrees`, which the root checkout's `.git` can never satisfy, and **CLAUDE.md rule 1's mandated flow is the orchestrator creating worktrees by hand with `git worktree add`** — which writes no marker. So the dominant real path is "pane env present, marker absent → refuse", and the orchestrator, the caller M5 rewrites rules 14/23 around, could never claim at all. This is rule 16's exact shape: a consumed value with no named supplier, in a PRD that listed "apply rule 16" as a step.
- **`human:<login>@<host>` carries no instance component**, so two orchestrations started by one person on one machine compare **equal**, take the idempotent-refresh row, and both proceed — #74 verbatim. With the orchestration form unreachable, that was the form people would actually hit.
- **The pane env's value was discarded** (`Ok(_)`), so identity came entirely from `cwd` — any agent could assume another orchestration's identity by `cd`-ing into its worktree, deliberately or by accident.

**Round 2 keyed the instance on `DOT_AGENT_DECK_PANE_ID`. That was also wrong**, and for a reason already written down: CLAUDE.md **rule 23**, verified 2026-08-10, records that those values are *"small daemon-scoped integers … and they recycle across a daemon restart."* Confirmed in code — `next_pane_id` (`src/spawn.rs:718`) increments `PANE_COUNTER`, a process-global atomic that resets with the daemon. So after a restart a new orchestration reusing pane id `6` compares **equal** to the previous holder, takes the idempotent-refresh row, and proceeds. That is the fork #160/#163/#166 incident rule 23 exists to prevent.

Rule 23 was invisible for rounds 1 and 2 because the harness injects `CLAUDE.md` from the **root checkout**, which sits at `4a68720` with 21 rules while `origin/main` carries 23 (filed as fork [#242](https://github.com/prageethw/dot-agent-deck/issues/242)).

**Round 3: the anchor is the worktree path and branch** — rule 23's own answer, for rule 16's own reason:

> The claim comment names the **worktree path and branch** you created for the issue, per rule 1 … Those are the right identifiers because rule 1 **already obliges you to create and name them**, so this rule invents no new mechanism and consumes nothing rule 1 does not already supply.

| Form | Compared string |
|---|---|
| agent working a worktree | the worktree's absolute path + its branch |
| human at a plain terminal | `human:<login>@<host>` |

The rendered claim matches rule 23's existing prose format exactly, so the mechanised claim and the hand-written one are **one artefact**, not two competing formats feeding the same `.rfind` parser:

```
Claimed by the orchestration working `/Users/…/dot-agent-deck-prd235` on branch `prd-235-issue-claim-lock`.
```

**Three consequences that reverse earlier decisions:**

1. **The digest is dropped, and `issue/claim/008` with it.** Rule 23 publishes the raw path deliberately: the check it enables is a human running `git worktree list`, and a digest is uncheckable. Publishing the path is already the status quo on this repo's issues.
2. **"`cd` changes your identity" stops being a defect and becomes the definition.** The worktree *is* the unit of work; one orchestration entering another's is the rule 1 violation itself, not an identity flaw.
3. **Stable across daemon restarts**, which is what rounds 1 and 2 both failed at from opposite ends — a value that is never written, then a value that is rewritten.

**Equality is on the worktree path and branch, and nothing else.** Not the orchestration name — that would reintroduce fork #201's advisory-uniqueness hole. Not the marker — that is the round-1 supply gap. Not the pane ID — that is the round-2 recycling bug. Host, timestamp and login all appear in the comment for other reasons and none of them is compared.

**The exact comment format**, extending rule 23's sentence with the assignee bookkeeping this PRD needs:

```
Claimed by the orchestration working `<worktree-path>` on branch `<branch>` at <ts>, for @<login>.
```

and for a claimant outside any worktree:

```
Claimed by @<login> working from `<host>` at <ts>.
```

Three constraints on that format, each load-bearing:

1. **It must still begin `Claimed by `.** `parse_claim_comment` finds claims by `.rfind` on `CLAIM_COMMENT_PREFIX`; a body that does not start with it is invisible, and the deck would go on believing a superseded holder still holds the issue.
2. **The path and branch are the identity clause** — rule 23's own wording, unchanged, so a hand-written claim and a machine-written one are the same artefact and a human can check either against `git worktree list`.
3. **`for @<login>` is bookkeeping, not identity.** The assignee replace-to-one logic parses the prior login back out of this clause. It is *not* part of the compared string, and it must be re-validated with `validate_gh_login` on the way back in (reviewer F8 / auditor F5).

**Known consequence, accepted:** every pane working inside one worktree shares that worktree's identity, so an orchestrator and the workers it delegates into the same worktree all claim as the same actor. That is correct — rule 1 makes the worktree the unit of a change, and they *are* one actor working one change. Two orchestrations can never share it, because rule 1 forbids exactly that.

**Where it is weaker than rounds 1–2 would have been:** a claim outside any worktree — a human at a plain terminal, or an agent in the root checkout — falls back to `human:<login>@<host>`, which has no instance component. Two such claimants on one machine still compare equal. That residual is accepted: rule 1 already forbids working in the root checkout, and a human racing themselves across two terminals can see both.

## Milestones

### M1 — Identity (`src/issue_dispatch.rs`)

- Compose the three identity forms above. Promote `sanitize_marker_creator` to shared visibility if `worktree_reclaim` keeps it private — do not duplicate it. `human:<login>` is the one form that is not free text: a validated `gh` login needs only `validate_gh_login`.
- `claim_comment_body` renders `Claimed by <identity> on \`<host>\` at <ts>, for @<login>.` The `issue-dispatch:` form's existing prefix stays **byte-identical** so `dispatch/010`'s assertion and `CLAIM_COMMENT_PREFIX` both still hold.
- **The takeover comment must keep the `Claimed by ` prefix.** Wording it `Taken over by …` would make it invisible to `.rfind`, leaving the system convinced the *previous* holder still holds the issue — a silent regression of the whole feature. Provenance goes in the tail: `…, for @human2, taking over from \`orchestration:orch-A@host-1:a1b2c3d4\`.` Pinned by a unit test, not a comment.
- Extend `parse_claim_comment` to return the **held-by identity, its host, and the prior login**. One function feeds the lock decision and its refusal message (M3) and the assignee replacement (M2). A refusal must name *which machine* may still be running the other agent, or the human cannot act on it.
- New pure builders: `gh_current_login_argv()` → `gh api user --jq .login`; `issue_edit_assignee_argv(repo, issue, add, remove)` mirroring `issue_edit_add_label_argv`, `--` separator included.
- `validate_gh_login` — accept only `^[A-Za-z0-9][A-Za-z0-9-]*$`. The value reaches a public comment body *and* an argv.

### M2 — The claim writer (`src/issue_dispatch_run.rs`)

`claim_issue(repo, issue, identity, login, notifier)`, three writes in order:

1. `--add-label in-progress` (unchanged).
2. **Assignee, replace-to-one:** `gh issue edit --add-assignee <current> --remove-assignee <prior>`, where `prior` is the login parsed from the newest claim comment — the deck's own receipt. Skipped when no login resolved.
3. **Append** the claim comment. Never edit in place; the log is the history.

- Resolve the login **once per run** beside `ensure_claim_label`, via `run_capture_args`.
- Derive the identity from the bound spawn handle. The dispatch call site **discards** it today (`if let Err(e) = spawn(req, …)`), and `SpawnKind` (`src/spawn.rs:97`) already carries the branch: `Orchestration { name }` → `orchestration:…`, `SingleAgent` → `issue-dispatch:…`. Preserve the existing error arm verbatim (`take_worktree`; worktree left for the next fire).
- **Dispatch calls this writer directly, bypassing M3's refusal** — its own three-signal gate has already decided. Do not double-gate.
- **Best-effort discipline is non-negotiable.** A failed `gh api user` warns and the run continues unassigned; a failed assignee write emits `NotifyEvent::IssueClaimFailed`, worded distinctly from the label and comment failures. Neither may turn a successful dispatch into `IssueDispatchFailed` — the exact defect #421's Risks section names and review fix C3 corrected.
- Implementation trap: GitHub **silently drops** an assignee lacking repo access and `gh` may still exit 0. Word the notification as what was *attempted*.

### M3 — The lock (`src/main.rs`)

**Caller identity — two local signals, no daemon:**

| `DOT_AGENT_DECK_PANE_ID` | Owner marker | Identity |
|---|---|---|
| absent | — | `human:<login>@<host>` |
| present | present | `orchestration:<name>@<host>:<wt>` |
| **present** | **absent** | **refuse, exit non-zero** |

That third row is the one to get right. `write_owner_marker` is **best-effort by design** — "a worktree that fails to record its own ownership is still a perfectly good worktree" — so a missing marker does *not* imply a human. If such a pane fell back to `human:<login>`, every agent on one deck would resolve to the **same** identity, the lock would read "held by me", and it would wave them all through while appearing to work. The env var makes agent-vs-human unambiguous; the marker only names *which* orchestration. This also covers fork #164 (an invisible failed marker write) safely: it degrades to a refusal, not a false claim.

`owner_of` is `pub(crate)` and takes `(repo_dir, worktree_path)`; both derive from cwd via `git rev-parse --git-dir` and `--git-common-dir`. Promote it or add a cwd wrapper rather than reimplementing the containment check.

**Resolving locally is deliberate.** A daemon lookup would add a protocol request, forcing a `PROTOCOL_VERSION` bump, a minor release, and rule 12's cross-version manual test — real cost for authority in a case rule 1 already forbids (hand-made worktrees).

Both refusals name the holder **and its host**: `held by \`orchestration:orch-A@host-1:a1b2c3d4\` since <ts>`.

### M4 — Tests

| Catalog ID | Action | Scenario |
|---|---|---|
| `scheduler/dispatch/010` | modify | Single-agent dispatch: label, assignee, one comment — wording otherwise unchanged. |
| `scheduler/dispatch/021` | create | Orchestration dispatch names the **orchestration** in the claim, not the scheduled task. |
| `scheduler/dispatch/022` | create | `gh api user` fails: label and comment still land, no assignee, dispatch **not** reported failed. **GREEN today** — see below. |
| `issue/claim/001` | create | **The lock:** issue held by A, `issue claim` from B **exits non-zero**, writes nothing, names A and A's host. |
| `issue/claim/002` | create | `--takeover` alone **still refuses** — nothing written, message instructs `--confirm-stopped`. |
| `issue/claim/003` | create | `--takeover --confirm-stopped` succeeds: log holds both in order, `.rfind` returns B, assignee is B's human only, new comment still starts `Claimed by `. |
| `issue/claim/004` | create | Labelled with **no claim comment** → refused, identity unknown. |
| `issue/claim/005` | create | No `DOT_AGENT_DECK_PANE_ID` → claims as `human:<login>`; a later orchestration claim is refused, naming the human. |
| `issue/claim/006` | create | Pane env set, marker **absent** → **refuse**; specifically does not fall back to `human:<login>`. |
| `issue/claim/007` | create | **Two orchestrations with the SAME name, different worktrees** (fork #201) — the second is **refused**, not treated as an idempotent self-refresh. |
| `issue/claim/008` | create | The claim comment carries a worktree **digest**, never a raw path — no `/Users/`, no `/home/`. |
| `scheduler/dispatch/014` | verify unchanged | A failed spawn still leaves the issue completely unmarked. |
| `scheduler/dispatch/015` | verify unchanged | Dispatch still skips a labelled issue — M3 must not alter the mechanical gate. |

**Two tiers, deliberately.** `scheduler/dispatch/*` stays in `tests/e2e_issue_dispatch.rs` (e2e-gated, informational in CI). `issue/claim/*` is a **new sub-area** in a **new fast-tier file `tests/issue_claim.rs`** — following `tests/daemon_status.rs` (fork #47) and `tests/worktree_reclaim.rs` (PRD #422), neither of which carries `#![cfg(feature = "e2e")]`, and both of which drive the **real `dot-agent-deck` binary as a subprocess**. That is the correct shape for CLI exit-code assertions, and it puts the lock — the part that matters — in the tier CI actually blocks on, with a ~1–2 minute round trip rather than ~9–12.

Until the `issue claim` subcommand exists, these fail via clap's own "unrecognized subcommand" error rather than any assertion the tests make — the same honest RED `daemon_status.rs` documents for itself. `dispatch/004` remains the reference for the orchestration-vs-single-agent fixture split on the e2e side. Each test carries a `/// Scenario:` comment (rule 7) and a `tests/CATALOG.md` entry. None is demo-reel-eligible (no real agent).

**`issue/claim/007` is the single most important test in the set** and its Scenario comment must state the #201 reasoning, not merely assert a refusal — it is the regression guard against anyone later "simplifying" the identity back to a bare name.

**Two corrections made during the RED round**, recorded rather than silently applied:

1. **The e2e IDs are `021`/`022`, not `016`/`017`.** This PRD originally named `016`/`017`; both are already taken in `tests/e2e_issue_dispatch.rs` by unrelated PRD #421 tests (`dispatch_016_externally_labelled_issue_skip_reports_no_claimant`, `dispatch_017_skip_causes_render_distinguishably`), as are `018`–`020`. The PRD was written without checking the file — the same "didn't look before naming" shape rule 20 exists to catch, one directory over instead of one repo over.
2. **`scheduler/dispatch/022` is GREEN from the start, not RED-first.** Today's `claim_issue` never calls `gh api user` and writes no assignee at all, so "no assignee written, dispatch not reported failed" holds trivially against unmodified code. It is a regression guard that only becomes load-bearing once M2's assignee write lands — the same pattern as this file's existing `012`/`014`/`019`. Recorded in its catalog entry so it is not later mistaken for a test that failed to wire up.

**RED confirmed on CI** at `fb55e2b`/`a43c05c` (PR #236): 1946 tests run, **1938 passed, 8 failed, 0 skipped** on both Linux and macOS — exactly the eight `issue/claim/*` tests, each failing at `assert_recognized_subcommand` with clap's `unrecognized subcommand 'issue'`, and nothing collateral. `dispatch/010` and `021` fail on their assertions in the e2e tier (read from the job log, not the check colour — the `e2e:` job is `continue-on-error: true`, so its green run reports nothing about test outcomes).

### M5 — Docs and close-out

- `changelog.d/235.feature.md`.
- Annotate `prds/421-automatic-issue-labelling.md`'s "Decisions taken during implementation" in place — its E1 record explicitly anticipated this second identity — matching how #373/#374 record later reversals.
- Rewrite **CLAUDE.md rules 14 and 23** around `dot-agent-deck issue claim`: the check stops being something an orchestrator must remember and becomes a command that fails. Apply **rule 16** while doing it — naming a command a worker must run obliges naming who supplies the issue number and repo.
- **Rule 12:** no TUI↔daemon wire change; that is precisely why identity resolves locally. Patch release, not minor. If `NotifyEvent` turns out to cross the protocol boundary, that is a `PROTOCOL_VERSION` question — report back rather than deciding.
- **Rule 9:** no new TUI surface, so no `experimental` flag.
- Leave `.github/workflows/issue-label-hygiene.yml` alone: stripping `in-progress` on close is deliberate, and the assignee should **survive** close as the record of who did the work.

## Risks

- **Check-then-write, not a distributed lock.** It closes the eight-minute window that caused #74; it does not close a genuinely simultaneous race. The word "lock" must not be allowed to imply more than that.
- **A takeover is not a preemption.** It rewrites the record; it does not stop the other deck's agent. human1's agent can keep working after human2 takes over, and the record will cleanly claim only one holds it — the #74 failure mode wearing a correct-looking claim. `--confirm-stopped` makes a human assert they handled it; nothing verifies the assertion.
- **Assignee replacement can overwrite a manual assignment.** Semantically correct given the field's defined meaning — a hand-set assignee is not "the human owning the agent currently working it" — but still a behaviour change for anyone using the field conventionally. Mitigated by only removing the login parsed from the deck's *own* claim comment. Reviewer and auditor should be given the definition alongside the behaviour, so any disagreement is about the definition rather than the implementation.
- **A cross-human assignee removal can fail on permissions**, leaving the comment naming orch-B/human2 while the assignee still reads human1 — correct behaviour, silently inconsistent record. Surface through the `Notifier` seam, not `tracing::warn!` alone; the same reasoning as review fix C3.
- **The lock works despite fork #201, not because it is fixed.** Name uniqueness remains advisory; the instance suffix routes around it. `issue/claim/007` is the guard.
- **A stale marker names a stale orchestration.** The marker records the name at worktree-creation time, so a renamed orchestration resolves to the old name. Accepted — renaming is not a supported flow, and the host+digest suffix keeps the identity distinct regardless.
- **Neighbouring open issues touch this code**: upstream #482 (nothing clears a claim — `--takeover --confirm-stopped` is arguably the release path it asks for), #483, #486, #484. Not fixed here; flag any accidental overlap in the PR body so the trackers stay honest.
