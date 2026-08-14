# PRD fork#298: A worktree resolves to a human OR an agent owner, not a marker-existence bool

**GitHub Issues**: [fork #230](https://github.com/prageethw/dot-agent-deck/issues/230), [fork #231](https://github.com/prageethw/dot-agent-deck/issues/231) (both claimed by this branch)

**Priority**: High

**Status** *(2026-08-14)*: **M1/M2/M4 in flight.** M3 deliberately excluded — see Scope.

**Parent**: [fork #166](https://github.com/prageethw/dot-agent-deck/issues/166) (worktree ownership surface, PR [#215](https://github.com/prageethw/dot-agent-deck/pull/215), released v0.37.0) · **Blocked-on for the creation half**: [fork #175](https://github.com/prageethw/dot-agent-deck/issues/175) (delegate provisions the worktree, PRD-only today)

**Related**: fork [#144](https://github.com/prageethw/dot-agent-deck/issues/144) (the P1 that makes `Ours` + merged + clean removable with no prompt — the safety constraint this PRD must not break) · fork [#221](https://github.com/prageethw/dot-agent-deck/issues/221) (closed; the stderr-only disagreement warning) · PRD fork#235 (`issue claim`'s `Identity`, the working two-kind model)

**Fork-only?** **No.** `worktree_reclaim.rs` and `issue_dispatch.rs` are upstream code. Offer upstream per rule 19 once landed — but note fork #266 records that fork#166's own fork-only justification has expired, so that question is already open and should be answered together.

## Problem Statement

The expected model is:

```
Worktree → Owner tracking → Human owner OR Agent owner
```

The product cannot express it. Measured on this repo 2026-08-14: **all 21 worktrees reported `owned: false`**, every JSON row omitted `owner` entirely, `worktree list --mine` refused with exit 1, and the question *"who owns this worktree?"* could only be answered by reading `git log` author — which is not ownership, only authorship.

The ownership feature is shipped, installed and working. It is **unreachable**, for three compounding reasons.

### Root cause 1 — `Ownership` is a two-state binary keyed on one file's existence

```rust
// src/worktree_reclaim.rs:79
pub enum Ownership { Ours, Foreign }

// :550-551
Some(git_dir) if git_dir.join(OWNER_MARKER_FILENAME).is_file() => Ownership::Ours,
_ => Ownership::Foreign,
```

A human-created worktree is not *human-owned*; it is merely *not ours*. There is no vocabulary for a human owner anywhere in this subsystem.

### Root cause 2 — the marker is written only by paths CLAUDE.md rule 1 does not use

`mark_worktree_owned` is called from the interactive orchestration-tab spawn and from issue-dispatch. **Rule 1 mandates the orchestrator run `git worktree add` by hand**, which writes no marker — so the dominant real-world path produces an unmarked worktree, permanently, and there is no retrofit (`worktree` exposes only `list` and `reclaim`).

fork#166 names its own dependency for this:

> *"Replacing CLAUDE.md rule 1's manual `git worktree add` is **fork #175's success criterion**, not this PRD's — this one supplies the identity that makes it possible."*

**fork#175 is unimplemented.** PR #271 merged 2026-08-13 titled "PRD fork#175: delegate provisions the worktree itself", but its entire diff is `prds/fork-175-delegate-provisions-worktree.md +191/-0` — the document, no code. Status `Planning`, 0/14 milestones, and `delegate --help` still offers only `--task`, `--task-file`, `--to`. That status is **accurate, not stale** — worth recording, because seven neighbouring PRDs *were* stale and were corrected in PR #294 the same day.

### Root cause 3 — a working two-kind model exists, in another subsystem, which walked away from the marker

`src/issue_dispatch.rs` already defines exactly the required model:

```rust
pub enum Identity {
    Worktree { path: PathBuf, branch: String, host: String, label: Option<String> },
    Human    { login: String, host: String },
}
```

resolved by a table in `issue_claim.rs:130-154`:

| `DOT_AGENT_DECK_PANE_ID` | Linked worktree? | Identity |
|---|---|---|
| absent | — | `human:<login>@<host>` |
| present | yes | worktree path + branch |
| present | no | refuse |

`issue claim` uses this and it works. But fork#235 round 3 **deliberately removed the marker read**, for precisely this reason — its own docs say *"a marker-less but genuinely linked worktree claims successfully … the orchestrator's own dominant real path under CLAUDE.md rule 1's hand-made `git worktree add` flow."*

So the two subsystems diverged: `issue claim` got a working owner model that ignores the marker; `worktree list` kept a marker nothing writes; nothing connects them.

## The constraint any fix must respect

`Ownership` does double duty:

```rust
// :142-143
Ownership::Ours    => Verdict::Remove,   // bare `reclaim` — no prompt, no path shown
Ownership::Foreign => Verdict::Ask(...)
```

One binary answers **two different questions**: *who owns this?* (reporting) and *may the deck delete it unprompted?* (authority). Marking more worktrees "owned" would make them bare-`reclaim`-removable, reopening the fork#144 P1 that fork#166 explicitly refused to reopen:

> *"Name-based (prefix) ownership was considered and rejected. It would adopt a worktree the deck did not create … fork #144 makes `Ours` + merged + clean removable by a bare `reclaim` with no prompt and no path shown."*

**Therefore: split the two questions. Do not widen the marker.**

## Solution Overview

Reporting gains a three-state owner for every worktree. Removal authority stays keyed strictly on marker proof, byte-for-byte as today.

## Scope

### In scope

- **M1** — `WorktreeOwner { Agent{identity} | Human{login,host} | Unknown{reason} }`, resolved: marker → `Agent` (via the existing `read_marker_owner`, `:604`); no marker → `Human` (reusing `issue_claim`'s resolution shape and `local_hostname`); otherwise `Unknown` with a stated reason, never a silent blank.
- **M2** — surface it: `WorktreeReport.owner_kind` beside the existing `owner`; the human table's OWNER column (built by fork#166 M2.3, renders empty today); `--mine` additionally matching `Human{login}` so it stops refusing outright for a human caller.
- **M4** — close fork #230 (JSON consumers never see the owned/owner disagreement — stderr-only) and fork #231 (the `owned=true, owner=None` mirror case silently excluded from `--mine`). Same defect class, cheap once `WorktreeOwner` exists.

### Out of scope

- **M3 / fork#175** — recording ownership at creation for the hand-made path, and amending rule 1. Excluded by explicit scope decision.
- Widening what a bare `reclaim` removes. **Reporting only.**
- Implicit name-prefix adoption — rejected by fork#166 for cause.

## Milestones

- [ ] **M1.0** — `WorktreeOwner` enum and its resolution function. `Ownership` and `decide` untouched.
- [ ] **M1.1** — the safety pin: a merged, clean, `Human`-owned worktree still yields `Verdict::Ask`, never `Remove`, under a bare `reclaim`.
- [ ] **M2.0** — `WorktreeReport.owner_kind`; `--json` carries owner and kind on every row.
- [ ] **M2.1** — human table OWNER column populated; `--mine` matches a human caller.
- [ ] **M4.0** — fork #230 and #231 closed.
- [ ] **M5.0** — docs + changelog fragment; correct CLAUDE.md rule 23's stale claim that `worktree list --json` "exposes no owner string" (it has carried `owner` since fork#166 — `skip_serializing_if = "Option::is_none"` is why it appears absent).

## Rule 12 — cross-version contract

`WorktreeReport` is a **CLI JSON document** (`schema_version`), not a TUI↔daemon wire type, so adding an optional field is additive and needs no `PROTOCOL_VERSION` bump. Confirm no `OrchestrationSnapshot` field changes; if one becomes necessary, that needs its own rule 12 answer and a manual cross-version run. Record the grep that establishes this, per rule 12 — a milestone ticked on neither a run nor a waiver is the state the rule exists to prevent.

## Success Criteria

**Status against PR #298 as merged** *(annotated 2026-08-14, before merge — do not read this PRD as complete)*:

| # | Criterion | Landed? |
|---|---|---|
| 1 | A deck-created worktree reports `Agent{identity}` matching its marker. | **Yes** — `worktree/reclaim/037` |
| 2 | A hand-made worktree reports `Human{login,host}` — not `Unknown`, and **not** `owned: true`. | **Yes** — `worktree/reclaim/038` |
| 3 | A merged, clean, human-owned worktree is still `Ask`, never `Remove`, under a bare `reclaim`. | **Yes** — `worktree/reclaim/039`, and independently traced by both the reviewer and the auditor |
| 4 | `worktree list --json` carries owner and kind on every row; `--mine` answers rather than refusing for both caller kinds. | **Half.** The `--json` half landed (`worktree/reclaim/040`). **`--mine` still refuses for a human caller** — see below |
| 5 | Ownership survives restart/reload, and a restored `OrchestrationSnapshot.owner` agrees with the marker. | **Partly** — the marker is a file in the worktree's git dir and is inherently durable, but the snapshot-agreement half is not separately asserted |
| 6 | Running `worker-agent-deck worktree list` on this repo names an owner for every worktree instead of an empty column. | **Yes** for the OWNER column, which `format_list_human` already emitted from `r.owner` and which now populates with no display-side change |

### What did NOT land, and why criterion 4 is only half

**M2.1's `--mine`-for-a-human half is not implemented.** Two independent blockers, both in code this PR deliberately did not touch:

1. `owner_filter` is derived **solely** from `DOT_AGENT_DECK_WORKTREE_OWNER` (`src/main.rs`). A human at a terminal has no such variable, so `worktree list --mine` exits `FAILURE` before `examine_worktrees` is ever called — the new human resolution is never consulted for the filter.
2. Even if it were, `is_mine` is `report.owned && report.owner == Some(owner)`. A `Human` worktree is unmarked, so `owned` is `false`, so **no human row can satisfy `is_mine` by construction.**

Closing that gap means changing `is_mine`'s `owned &&` conjunct — a **fail-closed guard PR #215 added deliberately** after its own reviewer F4 / auditor L1 round. That is not a detail to fold into a review-fix commit; it needs its own round with its own review. Tracked as a follow-up.

**M3 (fork#175) was never in scope** — see Scope above. So *"creating a worktree records ownership"* remains true only for deck-created worktrees; a hand-made one now **reports** a resolved human owner but **records** nothing.

### Known follow-ups at merge

- `--mine` for a human caller (criterion 4's second half).
- Structured warning objects rather than prose strings — issue #230's own suggested direction; this PR ships prose.
- Tests for the two untested `Unknown` branches (unresolvable git-dir; `gh` login unresolved). The second is the security-relevant one: it currently renders identically to the adversarial forged-`.git` case.
- The pre-existing stub-`gh` harness duplication between `tests/worktree_reclaim.rs` and `tests/issue_claim.rs`, which tips SonarCloud's new-code duplication gate by **18 lines against a 17.94-line threshold**. Production code has zero duplicated lines; the five new tests appear in no duplication block.
- No bounded wait on any subprocess in `worktree_reclaim.rs` — pre-existing and module-wide, not introduced here.
