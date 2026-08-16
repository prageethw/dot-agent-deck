# Upstream contribution policy — what goes upstream, what stays forked

This fork carries two kinds of divergence from `vfarcic/dot-agent-deck`, and they have **opposite economics**. Treating them as one thing is what produced the backlog this document exists to drain. [`fork-sync-workflow.md`](fork-sync-workflow.md) covers the *mechanics* of syncing; this covers the *policy* of what should be diverging at all.

## The decision test

Ask one question about every change: **would upstream want this?**

- **No — it is a preference.** It stays forked, permanently. Badge removal and the "worker-deck" rename, the role→model assignments, the devbox wrapper commands, the Telegram/Slack removal, the fork's release-CI skips, this fork's CLAUDE.md rules and PRD documents, `cleanup.sh`'s fork-specific evolution. This is the fork's identity. Upstream would reject it and should.
- **Yes — it is an improvement that merely happens to have been written here.** It goes upstream. A bugfix, a test-determinism fix, a genuine feature, a correction to an upstream doc.

If the honest answer is "yes, but not yet" — that is a "yes" with an unscheduled second step, and the second step is the one that historically does not happen. See the backlog below for what that cost. Under the fork-first ordering this document now prescribes, "not yet" is the *expected* state between merging here and offering upstream, which is exactly why that gap has to be closed by a tracked issue rather than by intention.

## Why the second category must not linger here

`fork-only` is a rebaseable commit stack that is re-applied onto `upstream/main` at **every** sync. A fix parked in that stack is re-applied, re-conflict-resolved and re-verified on every sync, forever — to keep a fix upstream would have taken for free. That is permanent rebase tax for zero benefit.

It also costs real, duplicated work when the offer comes late. `55021c3` (PRD #333, tab status colours) was **dropped entirely** in the 2026-08-09 sync: upstream merged its own version as PR #356 only after the maintainer requested behaviour changes, so upstream's version had evolved past the fork's. The resolution was to take upstream's version whole and discard the fork's diff. That work was done twice because it was offered late rather than early.

## The default for new work is fork-first, offer-after

*(Changed 2026-08-14 by maintainer decision. This section previously mandated upstream-first — branch from `upstream/main`, open the PR upstream, and let the fix arrive here by sync. That is withdrawn: it delayed this fork's own users for a benefit they never receive. The **decision test** above is unchanged; only the **ordering** flipped. CLAUDE.md rule 19 carries the same change and is the authority.)*

For anything that is **not** a preference divergence:

1. Fix it here, on a fork branch, against `origin/main`.
2. Review it, merge it, ship it. This fork's users have the fix at this point, and nothing further is required for them.
3. **Then** open the upstream PR from the merged work, and file the offer issue that tracks it.

**This fork is never blocked on an upstream decision.** Not on a review, not on requested changes, not on a merge. An open upstream PR is a gift already given — it is not a dependency and never belongs in a status report as blocking work.

**The risk moves; it does not disappear.** Under this ordering the *offer* is the step that silently does not happen — which is precisely what built the backlog below. "Offer later" depending on someone remembering is what failed before, so it must be tracked rather than remembered: **file the upstream-offer issue at merge time**, naming what to port, the matching upstream issue, and anything that makes the port non-trivial. [prageethw/dot-agent-deck#322](https://github.com/prageethw/dot-agent-deck/issues/322) is the pattern to copy. A merge that owed an offer issue and did not produce one is a defect, not a judgement call — the next session will not know the offer was ever owed.

Offering promptly still matters for the reason the PRD #333 failure above shows: the longer the gap, the more upstream's version and the fork's can drift apart. "After merge" means the same day, not "eventually".

## Purifying `fork-only` is a byproduct, not a project

Do **not** schedule a separate pass to extract upstream-worthy commits out of the stack. **Once a commit lands upstream, the next rebase drops it automatically** — it reduces to an empty commit and `--empty=drop` removes it.

This is observed behaviour, not theory. It has happened twice:

- `ab71a28` (PRD #336, split toggle) — dropped 2026-08-08 once upstream merged the same work as PR #342.
- `55021c3` (PRD #333, tab colours) — dropped 2026-08-09.

A hand-run purification pass would do by hand what the sync does for free, and would do it as one large rewrite of the branch that `main` is reset to — high blast radius — instead of letting the same outcome arrive incrementally and safely.

**The genuine exceptions** are commits that are *half* fork-only and cannot be offered as-is. These need splitting, but that is per-commit work done when you reach them, not a phase that blocks anything. *(Table widened from "two" to five 2026-08-16, seventh re-curation — the count itself is not load-bearing, only the pattern is.)*

| Commit | Split |
|---|---|
| `54066ca` | the `ps`-sampling half rides on fork-only `bf38bc1`; the hydration-race half is an upstream defect |
| `b06b85b` | two test fixes are upstream-worthy; the Greptile note is fork-only |
| `fd709c18` | the features-config ancestor-walk hardening (bounding a world-writable ancestor, open-then-fstat) is an upstream `resolve_features` defect; the declined-ancestor diagnostics call the fork-only `sanitize_path_for_terminal_display` and extend the fork-only `features status` subcommand |
| `99dd8ae4` | the shallow-shared-repo detection extends fork-only `worktree_reclaim.rs`'s own CLI; the `create_worktree_sync` attach-race lock is unverified against upstream's own (differently-named) worktree-creation path in `issue_dispatch_run.rs` — not confirmed either way, see the row's own note in `fork-sync-workflow.md` |
| `5576bde1` | the `Event::Paste` gate fix closes a defect verified identical on `upstream/main`; the new persistent LOCKED/UNLOCKED chip has no upstream equivalent at all |

## Write access is permission to merge, not to skip review

As of 2026-08-09 this fork's maintainer has **write** access on `vfarcic/dot-agent-deck` (`vfarcic` remains `admin`). That removes the friction that caused the backlog — but write access only helps if the offer gets *made*. It is the tracked offer issue, not the access, that closes the gap.

Keep the review discipline. Upstream's Greptile genuinely works, unlike this fork, which has no automated code reviewer at all (CLAUDE.md rule 8) — and upstream review has been substantive: on PR #419 the maintainer verified the change locally against the full e2e tier, identified three specific defects, and filed two follow-up issues (#434, #435) rather than waving it through. That is a better gate than the fork currently has. Open the PR and let the review run; merge it when it is approved.

**That review runs on upstream's clock, and an open upstream PR costs this fork nothing.** PR [#506](https://github.com/vfarcic/dot-agent-deck/pull/506) is the worked example: the fork's own fix for the same defect merged here as PR #240 on 2026-08-11 and shipped to this fork's users immediately, while the upstream PR sat green under a standing `CHANGES_REQUESTED` for days with every requested item already addressed. Nothing on this side waited. Do not chase an open upstream PR, do not re-ping the maintainer on a schedule, and do not report "the upstream PR has not merged" as unfinished work.

## Order of operations

This ordering governs **draining the historical backlog** below. It does **not** govern new work, and nothing in it may delay a fork fix or a fresh offer — see the fork-first default above.

1. **A stalled upstream queue never blocks new work.** *(Corrected 2026-08-14; this previously read "Finish the open upstream queue before extending it.")* Open upstream PRs are waiting on someone else's click, so treating them as a queue to drain first would hand upstream a veto over this fork's pace — exactly what the fork-first default exists to prevent. Fix here, merge here, offer, move on, however many offers are already open.
2. **Decide the closed PRs.** Re-offer or explicitly abandon — limbo is the worst state. These are the ones that genuinely need a decision, because nobody else will make it.
3. **Drain the backlog** in themed batches, splitting the two MIXED commits as you reach them. This is opportunistic work, not a phase that blocks anything.
4. **Re-verify before offering.** The classification below is hand-maintained and hand-maintained classifications drift. Confirm a commit still applies to current `upstream/main` before spending a PR on it — and per CLAUDE.md rule 20, search upstream **PRs** as well as issues, `--state all`, since an offer already made is invisible from `upstream/main`.

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
| `f44b13c5` | Bound the shutdown grace-window's EINTR retry against the deadline, restore the ECHILD alarm, and bound the hook CLI's stdin read (fork #145, #217) |
| `fb81d36b` | `register_orchestration_role` never removed a stale `orchestrator_pane_ids` flag when a pane_id was reused for a worker role, wrongly excluding it as a delegate candidate (fork #361) — verified identical on `upstream/main` |

### Test and CI determinism

`9dd02ee` (delegate_011 clock-independence), `4b35b48` (fixed-budget PTY/grid flake class), `fa038c1` (manager_016 side-pane settle), `70b3eca` (ingest_event broadcast/apply atomicity), `3ced4ba` and `ad9c20c` (idle_worker_011), `8841019` (lint the e2e-gated test files instead of compiling them away — upstream's CI has the same blind spot).

### Docs

`9fbd83a` — corrects an upstream doc's real-agent e2e file count from 4 to 19.

## Offered upstream and awaiting review

**These are not backlog and must not be re-offered.** They are open PRs on `vfarcic/dot-agent-deck`, green and mergeable, waiting on the maintainer's approving review — the fork's maintainer is the author and cannot self-approve, and the `main-protected` ruleset has no bypass actors. Nothing here needs work; it needs someone else's click.

*(Table re-verified against GitHub 2026-08-14. #390, #419 and #427 had all **merged** and were still listed here as awaiting review — the exact drift the "Keep it current" note below warns about, in the direction that makes the queue look more stalled than it is. Four genuinely-open PRs were missing. Re-verify with `gh pr list --repo vfarcic/dot-agent-deck --state open --author prageethw` rather than trusting this table.)*

| Upstream PR | Opened | Review state | What |
|---|---|---|---|
| [#471](https://github.com/vfarcic/dot-agent-deck/pull/471) | 2026-08-09 | `CHANGES_REQUESTED` | Claim dispatched issues and triage them on dispatch (upstream #421) |
| [#506](https://github.com/vfarcic/dot-agent-deck/pull/506) | 2026-08-11 | `CHANGES_REQUESTED` | Identify Claude Code hook rules by command, not by the binary's name (upstream #516, #517). **All three requested items were addressed and pushed 2026-08-13; all 10 checks green.** The fork's own fix merged here as PR #240 on 2026-08-11. |
| [#520](https://github.com/vfarcic/dot-agent-deck/pull/520) | 2026-08-12 | `REVIEW_REQUIRED` | Resolve the deck's command name from `current_exe()` rather than the crate literal (fork #253) |
| [#539](https://github.com/vfarcic/dot-agent-deck/pull/539) | 2026-08-13 | `REVIEW_REQUIRED` | Suggest and enforce unique orchestration names on the new-orchestration form (fork #192) |
| [#556](https://github.com/vfarcic/dot-agent-deck/pull/556) | 2026-08-14 | `REVIEW_REQUIRED` | De-duplicate two PRD files and repoint their references |

A `CHANGES_REQUESTED` row is **not** automatically outstanding work — check whether the requested items have already been pushed, as on #506, where the standing review is the only thing left and the author has already responded. Re-review happens on the maintainer's clock.

**Why this table exists.** On 2026-08-10 an orchestration asked to offer PRD #421 upstream searched both trackers' *issues*, correctly found upstream #421 open and unimplemented on `upstream/main`, and planned the entire port — which #471 had already delivered the day before. Nothing in this repository recorded that the offer had been made, so the only way to discover it was to query GitHub for PRs. That absence is what made the near-duplicate possible; this table is the fix, and CLAUDE.md rule 20 now requires a `gh pr list --state all` search over both trackers as well as the issue search.

**Keep it current.** Add a row the moment a PR is opened upstream, not when it merges — the whole point is that the record exists during the window when the work is invisible from `upstream/main`. Delete the row when it merges; the next rebase deletes the commit.

**A stalled queue is NOT a reason to withhold the next offer.** *(Corrected 2026-08-14; this previously read "A stalled queue is a reason not to extend it… adding a fifth just moves the pile.")* Several PRs sitting here at once is normal and costs this fork nothing — each is already-finished work parked on someone else's clock. Withholding a new offer until the queue drains would make upstream's review latency set this fork's contribution rate, which is the veto the fork-first default exists to remove. Open the fifth. The rows below are a record, not a debt.

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
