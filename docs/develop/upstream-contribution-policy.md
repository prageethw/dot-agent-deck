# Upstream contribution policy — what goes upstream, what stays forked

This fork carries two kinds of divergence from `vfarcic/dot-agent-deck`, and they have **opposite economics**. Treating them as one thing is what produced the backlog this document exists to drain. [`fork-sync-workflow.md`](fork-sync-workflow.md) covers the *mechanics* of syncing; this covers the *policy* of what should be diverging at all.

## The decision test

Ask one question about every change: **would upstream want this?**

- **No — it is a preference.** It stays forked, permanently. Badge removal and the "worker-deck" rename, the role→model assignments, the devbox wrapper commands, the Telegram/Slack removal, the fork's release-CI skips, this fork's CLAUDE.md rules and PRD documents, `cleanup.sh`'s fork-specific evolution. This is the fork's identity. Upstream would reject it and should.
- **Yes — it is an improvement that merely happens to have been written here.** It goes upstream. A bugfix, a test-determinism fix, a genuine feature, a correction to an upstream doc.

If the honest answer is "yes, but not yet" — that is a "yes" with an unscheduled second step, and the second step is the one that historically does not happen. See the backlog below for what that cost.

## Why the second category must not linger here

`fork-only` is a rebaseable commit stack that is re-applied onto `upstream/main` at **every** sync. A fix parked in that stack is re-applied, re-conflict-resolved and re-verified on every sync, forever — to keep a fix upstream would have taken for free. That is permanent rebase tax for zero benefit.

It also costs real, duplicated work when the offer comes late. `55021c3` (PRD #333, tab status colours) was **dropped entirely** in the 2026-08-09 sync: upstream merged its own version as PR #356 only after the maintainer requested behaviour changes, so upstream's version had evolved past the fork's. The resolution was to take upstream's version whole and discard the fork's diff. That work was done twice because it was offered late rather than early.

## The default for new work is upstream-first

For anything that is **not** a preference divergence: branch from `upstream/main`, open the PR upstream, merge, and let it arrive here through the normal sync.

**Do not** default to fork-first-then-offer. That pattern is what created the backlog — "offer later" depends on someone remembering, nothing enforces it, and it did not happen fourteen times. It also maximises the window in which upstream's version and the fork's can drift apart, which is exactly the PRD #333 failure above.

Fork-first remains correct when the change genuinely needs the fork's environment to be developed or validated — but the PR upstream is then opened as soon as it works, not "eventually".

## Purifying `fork-only` is a byproduct, not a project

Do **not** schedule a separate pass to extract upstream-worthy commits out of the stack. **Once a commit lands upstream, the next rebase drops it automatically** — it reduces to an empty commit and `--empty=drop` removes it.

This is observed behaviour, not theory. It has happened twice:

- `ab71a28` (PRD #336, split toggle) — dropped 2026-08-08 once upstream merged the same work as PR #342.
- `55021c3` (PRD #333, tab colours) — dropped 2026-08-09.

A hand-run purification pass would do by hand what the sync does for free, and would do it as one large rewrite of the branch that `main` is reset to — high blast radius — instead of letting the same outcome arrive incrementally and safely.

**The two genuine exceptions** are commits that are *half* fork-only and cannot be offered as-is. These need splitting, but that is per-commit work done when you reach them, not a phase that blocks anything:

| Commit | Split |
|---|---|
| `54066ca` | the `ps`-sampling half rides on fork-only `bf38bc1`; the hydration-race half is an upstream defect |
| `b06b85b` | two test fixes are upstream-worthy; the Greptile note is fork-only |

## Write access is permission to merge, not to skip review

As of 2026-08-09 this fork's maintainer has **write** access on `vfarcic/dot-agent-deck` (`vfarcic` remains `admin`). That removes the friction that caused the backlog — but only if the default above actually changes. Write access with an unchanged fork-first habit just produces a backlog you *could* merge and still do not.

Keep the review discipline. Upstream's Greptile genuinely works — it is **not** credit-limited the way this fork's is (CLAUDE.md rule 8) — and upstream review has been substantive: on PR #419 the maintainer verified the change locally against the full e2e tier, identified three specific defects, and filed two follow-up issues (#434, #435) rather than waving it through. That is a better gate than the fork currently has. Open the PR, let the review run, then merge.

## Order of operations

1. **Finish the open upstream queue before extending it.** Adding to a stalled queue just moves the pile.
2. **Decide the closed PRs.** Re-offer or explicitly abandon — limbo is the worst state.
3. **Then drain the backlog** in themed batches, splitting the two MIXED commits as you reach them.
4. **Re-verify before offering.** The classification below is hand-maintained and hand-maintained classifications drift. Confirm a commit still applies to current `upstream/main` before spending a PR on it.

## The backlog — upstream-worthy, never offered

Tagged **UPSTREAM-WORTHY** in [`fork-sync-workflow.md`](fork-sync-workflow.md)'s stack table, and cross-referenced against every upstream PR ever opened from this fork: these have **no upstream PR at all**. Recorded here so it does not have to be re-derived.

### Product code

| Commit | What |
|---|---|
| `861424f` | Root each orchestration tab in its own worktree (fork #122) — the largest, and the fix for panes sharing one `.dot-agent-deck/` |
| `d11ed6c` | Kill the git process group on timeout so hook grandchildren cannot outlive it (fork #133) |
| `ab6a1ff` | Move the post-SIGKILL reap off the render loop (fork #136) |
| `e0a5544` | Re-hydrate a fresh snapshot on every reconnect — an upstream reconnect defect, not a fork customisation |
| `87ab3b4` | Enforce `PROTOCOL_VERSION` on the local daemon attach path — an upstream protocol-gate defect |
| `a8cbc29` | Fix the Pi start-role spawn-order race (fork #92) |
| `20c1055` | Pass the hook socket endpoint explicitly instead of through the environment (fork #102) |

### Test and CI determinism

`9dd02ee` (delegate_011 clock-independence), `4b35b48` (fixed-budget PTY/grid flake class), `fa038c1` (manager_016 side-pane settle), `70b3eca` (ingest_event broadcast/apply atomicity), `3ced4ba` and `ad9c20c` (idle_worker_011), `8841019` (lint the e2e-gated test files instead of compiling them away — upstream's CI has the same blind spot).

### Docs

`9fbd83a` — corrects an upstream doc's real-agent e2e file count from 4 to 19.

## Offered upstream but closed without merging

These are **not** in the backlog above — they were offered and did not land. Each needs an explicit decision to re-offer or abandon.

| Upstream PR | Commit | What |
|---|---|---|
| #411 | `205272c` | codex-hooks `write_atomic` preserves destination mode |
| #409 | `9e0c79d` | work-done output-path collisions — this is fork **#76**, whose symptom (one worker's report archiving another's) recurs in practice |
| #408 | `10039b9` | make daemon rejections and confirmations visible to the `delegate` caller |
| #392 | `54066ca` | hydration idle-edge race — opened as WIP |

## Keeping this current

When a backlog row is offered upstream, move it out of the backlog table and note its PR number. When it merges, delete the row — the next sync deletes the commit. If a row turns out **not** to be upstream-worthy on re-verification, delete it and correct the tag in [`fork-sync-workflow.md`](fork-sync-workflow.md)'s stack table, so the two documents cannot disagree.
