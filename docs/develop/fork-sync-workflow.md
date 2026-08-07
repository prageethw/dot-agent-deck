# Fork ↔ upstream sync workflow

This repository is a **fork**: `origin` is `prageethw/dot-agent-deck` and `upstream` is `vfarcic/dot-agent-deck`. Over time the fork has accumulated a handful of fork-only customisations that must survive every future pull of upstream's work. This document is the exact, copy-pasteable procedure for syncing with upstream without losing (or silently mangling) those customisations.

If you only read one section, read [The sync procedure](#the-sync-procedure).

## Verify the active config before delegating work

Before delegating any work from a checkout (root checkout, worktree, or PR review branch), verify that the active `.dot-agent-deck.toml` role commands map to the fork's wrapper scripts, not upstream's original per-model scripts:

```bash
grep -A1 'name = "coder"' .dot-agent-deck.toml
# expect: command = "devbox run claude-sonnet-devbox"
```

If that command instead reads `devbox run agent-coder`, `devbox run pi-sol`, `devbox run oc-big`, `devbox run codex-big`, or similar upstream-style names, the checkout predates the fork's devbox-wrapper customisation (PR #7) and delegated workers will silently launch with the wrong model/tool — no error, just wrong behaviour observable only by noticing the wrong model running.

**Why this can happen silently:** any checkout on a branch/commit that predates the fork's config customisation — or any branch built directly off an old upstream point — carries the pre-fork config. There is no validation step that catches the mismatch; delegation just proceeds with whatever command the checkout's `.dot-agent-deck.toml` happens to specify.

**Fix/mitigation:** always delegate work from `main` or `fork-only`'s current tip (both carry the fork's config as of the most recent sync), and re-run the check above after any branch switch — especially after switching to review a PR, check out an older branch, or create a fresh worktree.

## The two-branch model, and why

There are two long-lived branches:

- **`fork-only`** — holds *only* the fork's customisations, as a clean, linear, rebaseable commit stack on top of an upstream base commit. This is the branch that actually syncs with upstream.
- **`main`** — what everything else (CI, worktrees, day-to-day work) builds from. `main` is **never** synced with upstream directly. After each sync it is simply reset to match `fork-only`.

Why `main` can't just merge upstream itself: `main`'s history already contains the *old*, pre-`fork-only` versions of the fork customisations (they landed directly on `main` as individual squash-merged PRs — #4–#8 — before the `fork-only` branch existed). So `main` is not a clean mirror of upstream and can never fast-forward cleanly from it. Its fork-only commits are also interleaved with upstream's own history across high-churn files (`src/ui.rs`, `src/tab.rs`, `tests/CATALOG.md`), so a `git merge upstream/main` straight into `main` would smear conflicts across that mixed history, or drift silently. `fork-only`, by contrast, is a tidy stack whose conflicts (if any) surface **once, per commit, in one place** during a rebase.

The customisations really do collide with upstream in practice — two concrete examples that had to be resolved by hand while building this workflow, kept here only as illustration of *why* the rebase can conflict:

- Fork-only badge removal (PR #4) vs. upstream PRD #339 (moved card Last/Tools stats to the bottom border) — both edit `render_session_card` / `truncate_styled_segments` in `src/ui.rs` plus four snapshot files.
- Fork-only PRD #336 (Ctrl+l pane-split toggle) vs. upstream PRD #341 ("one funnel per key event", which collapsed the inline key-dispatch cascade into a single `handle_key_event(...)`). PRD #336's `ToggleOrchestrationSplit` orchestration-tab-scoping guard had to be re-anchored inside the new `handle_key_event` function.

You don't need to reproduce these; they're just what "resolve the conflicts with the same rigor" looks like in the wild.

## The sync procedure

`main` never talks to `upstream`. Only `fork-only` rebases onto upstream, then `main` is reset to it.

```bash
git fetch upstream
git checkout fork-only
git rebase upstream/main          # conflicts resolved HERE — once, per-commit
# ... resolve any conflicts with full rigor: read BOTH sides, don't guess, verify
#     functional overlap beyond the visible conflict hunk, and regenerate + visually
#     verify any snapshot content that changed ...
git push --force-with-lease origin fork-only
git checkout main
git reset --hard fork-only
git push --force origin main   # force-with-lease won't help here: we didn't fetch origin, so its tracking ref for main may be stale
```

**This rewrites history on both branches, intentionally.** The rebase gives `fork-only`'s commits new SHAs, and `main` is force-pushed to match. That is expected for this workflow, not an accident. Anyone holding a stale local clone of either branch must **hard-reset to the new history** after a sync (`git fetch && git reset --hard origin/<branch>`) — a plain `git pull` will produce a tangled merge, not the intended state.

## The current `fork-only` stack

Oldest to newest, rooted at an upstream base commit. Always re-verify against the live branch before trusting exact SHAs — run `git log origin/fork-only --oneline -50`, since a sync rewrites every SHA below the base:

| SHA | Commit | Status |
| --- | --- | --- |
| `9ca7de1` | `docs(claude): correct rule 8's required-status-check claim` | **base** (upstream commit) |
| `ab71a28` | `fix(prd-336): toggle orchestration pane-column split ratio [CI shadow] (#2)` | **PERMANENT** fork-only, but **as an ancestor only** — superseded by `30c5f79`, see note below |
| `ac43b4e` | `feat(prd-333): color orchestration tab labels by highest-priority pane status (#3)` | **TEMPORARY** — see watch-item below |
| `349e895` | `fork-only: remove agent-type badge from cards, rename title to worker-deck (#4)` | **PERMANENT** fork-only |
| `bbad18a` | `fork-only: auto-focus active orchestration tab on WaitingForInput pane (#5)` | **PERMANENT** fork-only |
| `f98dd56` | `test(fork): cover the auto_focus_waiting_pane render-loop wiring (L1) (#6)` | **PERMANENT** fork-only |
| `d87f995` | `fork-only: swap worker role devbox commands to claude-sonnet-devbox/codex-devbox wrappers (#7)` | **PERMANENT** fork-only |
| `e42b288` | `fork-only: add devbox.json/.dot-agent-deck.toml backups + restore doc (#8)` | **PERMANENT** fork-only |
| `e034c45` | `fork-only: carry the sync-workflow doc onto this branch too` | **PERMANENT** fork-only (this doc) |
| `86d13e2` | `fork-only: document the active-config check before delegating` | **PERMANENT** fork-only (this doc) |
| `30c5f79` | `feat(prd-371): three-stage Ctrl+l pane-split toggle (Default/Narrow/Hidden) (#10)` | **PERMANENT** fork-only — supersedes `ab71a28`'s mechanism, see note below |
| `8c07d10` | `fix(prd-372): clear WaitingForInput on the approved tool's own ToolStart (#11)` | **PERMANENT** fork-only |
| `26255e8` | `feat(prd-374): lock command entry to the orchestrator pane, Ctrl+e to unlock (#12)` | **PERMANENT** fork-only — mechanism retained, but two of its *decisions* are reversed by PRD #393; see note below |
| `35780ef` | `fork-only: reassign orchestrator/coder to opus, release to haiku (#13)` | **PERMANENT** fork-only |
| `fc45d01` | `docs(prd-370): create PRD #370 - shell activity working status [skip ci]` | **PERMANENT** fork-only (doc) |
| `703b4d2` | `docs(prd-383): create PRD #383 - blocked-keystroke reset for the Orchestration inactivity timer [skip ci]` | **PERMANENT** fork-only (doc) — **describes behaviour that no longer exists**: PRD #393 deleted the inactivity timer this PRD's reset applied to; see note below |
| `d6e1d21` | `feat(prd-370): treat underlying shell activity as Working status inside a worker pane (#14)` | **PERMANENT** fork-only, but **as an ancestor only** — its `tcgetpgrp` body never fired and was replaced by `296b9d1` (PRD #386); see watch-item below |
| `a8987e6` | `fork-only: update the fork-sync-workflow stack table after the 2026-08-05 sync` | **PERMANENT** fork-only (this doc) |
| `08a9402` | `fork-only: run the L2 e2e tier in CI as an informational, non-blocking job` | **PERMANENT** fork-only |
| `0348ebb` | `docs(claude): correct rule 5's stale never-in-CI e2e claim for this fork` | **PERMANENT** fork-only (rules) |
| `d3c278f` | `docs(claude): correct rule 4's stale never-in-CI e2e claim for this fork` | **PERMANENT** fork-only (rules) |
| `166cb8b` | `docs: make "regression runs in CI, not locally" an explicit fork rule` | **PERMANENT** fork-only (rules) |
| `f7c9e6d` | `fork-only: align worker prompt templates with "regression runs in CI"` | **PERMANENT** fork-only (prompts) |
| `03ba9e9` | `fork-only: condition step 6 on local casts, add checkpoint-findings rule, document generated orchestrator context` | **PERMANENT** fork-only (prompts) |
| `e80c4a0` | `docs(prd-370): record that the tcgetpgrp signal never fired; superseded by #386` | **PERMANENT** fork-only (doc) |
| `7692861` | `docs(prd-370): mark the PRD superseded by #386 in its status line` | **PERMANENT** fork-only (doc) |
| `9baf503` | `fork-only: require every work-done report to lead with a Summary naming the PRD/issue` | **PERMANENT** fork-only (prompts) |
| `6b00f5e` | `fork-only: require a progress table in orchestrator status reports` | **PERMANENT** fork-only (prompts) |
| `0451799` | `feat(prd-373): auto-return focus to the orchestrator pane (#18)` | **PERMANENT** fork-only — but **half of it is dead**: `6df86c1` (PRD #393) deletes its inactivity snap-back; see note below |
| `91c245a` | `feat(prd-387): unified, deck-global, command-mode-scoped Ctrl+L split toggle (#19)` | **PERMANENT** fork-only — generalises `30c5f79` |
| `09ccc53` | `chore: remove Telegram and Slack MCP wiring and the orchestrator notification protocol (#22)` | **PERMANENT** fork-only (config/prompts) |
| `296b9d1` | `feat(prd-386): descendant-scan shell-activity signal (#23)` | **TEMPORARY** — open upstream as PR #390; see watch-item below |
| `c43266f` | `test/docs: repair card-badge assertions, v0.35.8 changelog, notification-recipe tense (#24, #25)` | **PERMANENT** fork-only — the assertions follow `349e895`'s badge removal |
| `3154929` | `fix(prd-386): preserve synthetic-working provenance across hydration (#34, #37, #38)` | **TEMPORARY** — rides on `296b9d1` / PR #390 |
| `4a21e02` | `ci/docs: stop the e2e job flaking under runner contention; move all test runs to CI (#40, #42)` | **PERMANENT** fork-only (CI/rules) |
| `60634fd` | `fix: close the hydration idle-edge race and harden shell-activity ps sampling (#43, #44)` | **MIXED** — the `ps`-sampling half rides on `296b9d1`; the hydration-race half is an upstream-worthy bug fix, see the upstream-candidate note below |
| `beaf579` | `docs(fork): worktree-per-fix default, CI-only testing in the role prompts, PRD #361 record (#48, #52, #53)` | **PERMANENT** fork-only (rules/prompts) |
| `af0a0f1` | `test/docs: de-flake manager_010, quote generated shims, record Greptile's credit-limit mode (#54, #55, #59)` | **MIXED** — the two test fixes are upstream-worthy; the Greptile note is fork-only |
| `6df86c1` | `feat(prd-393): command-mode-scoped, deck-global command-entry lock and lock-governed focus (#51)` | **PERMANENT** fork-only — amends `26255e8`, deletes `0451799`'s timer; see note below |
| `5eddb54` | `test(e2e): make delegate_011 clock-independent and wait on the widget, not a proxy string (#60, #61)` | **UPSTREAM-WORTHY** — generic e2e determinism, no fork-specific content |
| `de8c161` | `docs(prd-393): revive M7 - the upstream proposal is open as PR #404 (#66)` | **PERMANENT** fork-only (doc) |
| `6553f44` | `fix(orchestration): guard the waiting-focus move against queued input (#69)` | **PERMANENT** fork-only — fixes `bbad18a`/`6df86c1`'s focus chain |
| `7e27ec3` | `fix: re-hydrate a fresh snapshot on every reconnect, not just at bootstrap (#49, #28)` | **UPSTREAM-WORTHY** — upstream reconnect defect, not a fork customisation |
| `e2d77a3` | `fix: enforce PROTOCOL_VERSION on the local daemon attach path (#17)` | **UPSTREAM-WORTHY** — upstream protocol-gate defect, not a fork customisation |
| `4e11558` | `fork-only: skip upstream-only Homebrew/Scoop publishing on fork releases (#35)` | **PERMANENT** fork-only (release CI) |
| `c04ab81` | `docs(changelog): key the command-entry-lock fragment to fork issue #68 (#72)` | **PERMANENT** fork-only (changelog) |
| `c50f931` | `chore(config): run the orchestrator in plan mode, the coder and release roles on sonnet (#77)` | **PERMANENT** fork-only — amends `35780ef`/`d87f995` |
| `3e1d4ec` | `docs: correct the real-agent e2e file count from 4 to 19 (fork #26)` | **UPSTREAM-WORTHY** — corrects an upstream doc/rule count |
| `ac12948` | `fork-only: skip the docs publish job on fork releases (#71)` | **PERMANENT** fork-only (release CI) |
| `665ed77` | `test(daemon): pin ingest_event's broadcast/apply atomicity (fork #31)` | **UPSTREAM-WORTHY** — generic daemon regression test |
| `352a782` | `docs(fork-sync): record the open-upstream-PR branch-deletion hazard (#83)` | **PERMANENT** fork-only (doc) |
| `819c5c1` | `test(harness): fix the fixed-budget PTY/grid observation flake class (fork #81, #82)` | **UPSTREAM-WORTHY** — generic e2e/PTY timing determinism, no fork-specific content |
| `aed6343` | `feat(delegate): make daemon rejections and confirmations visible to the caller (#84)` | **UPSTREAM-WORTHY** — `delegate` is upstream functionality; the touched surfaces (`handle_delegate`, the hook socket reply, the CLI) carry no fork-only symbols |
| `7441b0f` | `ci: add a Semgrep CE scan publishing SARIF to GitHub code scanning (#85, #86)` | **PERMANENT** fork-only (CI) |
| `a9ef53e` | `ci: add SonarQube Cloud analysis, gated on the SONAR_TOKEN secret (#88)` | **PERMANENT** fork-only (CI) |

The base is `9ca7de1` — `upstream/main`'s tip at the time of the 2026-08-05 sync. Every commit above it was verified as genuinely fork-only before inclusion: none of the symbols/behaviors they introduce (`SplitStage`, `command_entry_locked`, `ToggleOrchestrationSplit`, the shell-activity status change, `claude-sonnet-devbox`) exist anywhere in `upstream/main`.

**Re-curated 2026-08-07.** The 2026-08-05 sync closed with `main` reset to `fork-only`; two days later `main` was **184 commits ahead** and running the documented `git reset --hard fork-only` would have discarded every one of them. Rows `0451799` onward are that re-curation: the 184 commits squashed into 22 logically-grouped commits, in `main`'s own topological order, each one's tree taken verbatim from `main` at a PR-merge boundary. Because `main` never takes upstream changes directly, the re-curation is verifiable by a single check — `git diff fork-only origin/main` must be **empty** apart from this file's own table update. It was, at `665ed77`.

**Keep it current.** This is the second time the drift has been discovered rather than prevented (the 2026-08-05 sync picked up 7 uncurated commits; this one picked up 184). Re-curate whenever a PR merges to `main`, or at minimum on a fixed cadence — not "whenever it's badly out of date". Every re-curation that waits makes the squash grouping coarser and the next upstream rebase harder to reason about.

### Upstream candidates: what is on this stack that is not a fork customisation

Six rows above are marked **UPSTREAM-WORTHY** or **MIXED**. They are on `fork-only` because they are on `main` and the invariant demands it, not because the fork wants to keep them diverged. They fix defects or flakes that exist in `upstream/main` just as much as here:

- `7e27ec3` — the TUI never re-hydrates after a mid-session subscription loss (fork #49/#28).
- `e2d77a3` — the local daemon attach path never checks `PROTOCOL_VERSION` (fork #17). Note this one carries a `changelog.d/17.breaking.md` fragment.
- `665ed77` — `ingest_event`'s broadcast/apply atomicity is unpinned (fork #31).
- `5eddb54` — `delegate_011` depends on paused-clock tick alignment; e2e waits keyed on proxy strings sample torn frames (#402, #395).
- `af0a0f1` — `manager_010`'s `[Submit]` torn-frame flake, and developer-controlled paths interpolated into generated `/bin/sh` shims (#54, #59).
- `3e1d4ec` — the real-agent e2e file count in the rules is stale (fork #26).
- The hydration-race half of `60634fd` (`send_replace` on the hydration gate, closing the reconnect snapshot/subscribe window — fork #36).

**Offering these upstream would shrink the stack.** Each one that merges upstream becomes a duplicate the next rebase can drop, exactly like PRD #333's row. Worth doing before the next sync rather than after.

### Supersession: PRD #371's `SplitStage` replaces PRD #336's `split_narrow`

`ab71a28` (PRD #336) and `30c5f79` (PRD #371) are **not** two parallel fork features that happen to overlap — they are two generations of one feature. #336 introduced a two-stage `split_narrow: bool` on `Tab::Orchestration`; #371 replaced it with a three-stage `SplitStage` enum carried by both `Tab::Dashboard` and `Tab::Orchestration`. Verified on `main`: `split_narrow` has **zero** occurrences anywhere under `src/`, while `SplitStage` has 14 hits in `src/tab.rs` and 44 in `src/ui.rs`. So `ab71a28` is no longer *independently* PERMANENT — nothing it added survives on the fork in its own right. It stays in the stack because `30c5f79` is written on top of it: it is a required ancestor, not a feature to preserve on its own terms.

This matters at sync time because upstream PR #342 is #336's mechanism continued upstream, and it has evolved past `ab71a28` — on that branch the split became global and gained a `scope_orchestration_split` helper in `src/ui.rs`, neither of which exists on the fork. **When #342 merges,** `upstream/main` gains `split_narrow: bool` plus `scope_orchestration_split`, and the next `git rebase upstream/main` will surface both against the fork's `SplitStage`. **Resolve that as a supersession, not a merge** — a mechanical three-way merge would happily leave *both* `split_narrow` and `SplitStage` alive:

- **Take the fork's `SplitStage`**, and drop upstream's `split_narrow` field and all of its call sites entirely. A `bool` and a three-variant enum are the same feature at two granularities, and the enum is strictly the later generation.
- **Take upstream's `scope_orchestration_split` as the seed**, rather than reinventing it — it is the reviewed artefact, and the one genuinely still-wanted thing to come out of #336/#342. PRD #387 generalises it into `scope_split_stage`.
- **Update this table in the same pass:** once `split_narrow` is gone from the merged tree, `ab71a28`'s row is purely historical.

### Amendment: PRD #393 reverses two of PRD #374's decisions, and deletes half of #373

**This is deliberately *not* filed as a supersession, and the distinction matters at conflict-resolution time.** Unlike `ab71a28` — whose mechanism `30c5f79` replaced outright, leaving nothing of its own alive — `26255e8`'s mechanism **survives intact**. `command_entry_locked`, `Action::ToggleOrchestrationLock` and `gate_pane_input_key` all still exist and still do what #374 built them to do. What PRD #393 changes are two *decisions about* that mechanism, plus one addition:

- **Per-tab → deck-global.** `command_entry_locked` moved from a field on `Tab::Orchestration` to a single field on `UiState`. Note carefully: **only the storage moved.** The gate's reach is unchanged — Orchestration tabs only; Dashboard and Mode tabs are still never gated. Anyone reading "deck-global" as "the lock now covers every tab type" will resolve a conflict wrongly.
- **Any-mode → command-mode only.** `Ctrl+E` is now claimed only in `UiMode::Normal`, via a pure `scope_command_entry_lock` that mirrors PRD #387's `scope_split_stage`. #374 deliberately made the chord mode-independent; that reasoning was reversed because it meant a focused role pane's PTY never received `0x05`, so readline's `end-of-line` never reached the agent — the same conflict class `Ctrl+W` (#218/#241) and `Ctrl+L` (#387) already resolved this way.
- **Added: a `WaitingForInput` carve-out**, so an agent that has stopped and asked can be answered without unlocking — with a fail-closed guard (`build_pane_status_for_gate`) that denies the exemption whenever two sessions collide on one `pane_id`.

So `26255e8` stays **independently PERMANENT**: it is a feature to preserve on its own terms, not merely a required ancestor. Its row is annotated rather than downgraded.

**`703b4d2` is the one that genuinely lost its subject.** PRD #383's blocked-keystroke reset existed to keep #373's 30-second inactivity timer from misreading a locked pane as idle. PRD #393 **deleted that timer entirely** — along with `auto_focus_after_inactivity`, `last_role_pane_activity_at` and all six of its stamp sites, the `DOT_AGENT_DECK_INACTIVITY_TIMEOUT_SECS` test seam, and eleven tests. The doc commit stays in the stack as an ancestor, but nothing it describes is live behaviour any more.

**Drift resolved 2026-08-07:** PRD #373's *implementation* used to be missing from this table — it landed directly on `main` after the 2026-08-05 sync without being curated in. It is now curated in as `0451799`. It was **not** split into its surviving and deleted halves, deliberately: `main` merged all of #373 as one PR (#18), and carving M1's all-clear focus move out of M2/M3's inactivity snap-back would have meant hunk-level surgery on a tree that no commit on `main` ever had. The stack therefore replays history faithfully — `0451799` adds the timer, `6df86c1` (PRD #393) deletes it — and this note is how a future conflict resolver knows that roughly half of `0451799` is dead on arrival. **If a rebase conflicts inside `0451799` on `auto_focus_after_inactivity`, `last_role_pane_activity_at`, or `DOT_AGENT_DECK_INACTIVITY_TIMEOUT_SECS`, resolve it however is cheapest** — `6df86c1` removes all three a few commits later. Only `0451799`'s all-clear focus move is worth resolving with care.

**None of this changes upstream conflict risk**, because none of it exists upstream: `command_entry_locked`, `auto_focus_*`, `ToggleOrchestrationLock` and `gate_pane_input_key` all have **zero** occurrences on `upstream/main`, verified during #393. #373 and #374 were both closed upstream as not-planned. See PRD #393's Upstream section for why a future contribution would be a net-new proposal rather than a port, and issue #369 for the maintainer's recorded position on the feature itself.

### Watch-item: PRD #333 is temporary

`ac43b4e` (PRD #333, colour orchestration tab labels by status) is **not** a permanent fork feature. It already has an open upstream PR — **#356** on `vfarcic/dot-agent-deck`, still open/unmerged as of 2026-08-07, blocked on upstream maintainer merge rights, the same situation as PRs #352/#346. It sits in the stack only because fork-only commit #5 (`bbad18a`, auto-focus) structurally depends on the `pane_status_for_tabs` code PRD #333 introduces.

**When PR #356 actually merges upstream:** the next `git rebase upstream/main` should find `73b233c`'s changes already present natively. Git will likely reduce it to an empty/no-op commit — drop it at that point (`git rebase --skip` when it stops on the empty commit, or rebase with `--empty=drop`). Until #356 merges, leave it in.

### Watch-item: PRD #386 is temporary too

`296b9d1` (PRD #386, the descendant-scan shell-activity signal) is the same shape as PRD #333: real fork work that is already **offered upstream as PR #390** on `vfarcic/dot-agent-deck`, still open/unmerged as of 2026-08-07. Two rows ride on it — `3154929` (synthetic-working provenance across hydration) and the `ps`-sampling half of `60634fd` — and `d6e1d21` (PRD #370) is its dead predecessor.

**When PR #390 merges upstream,** do *not* expect a single clean empty commit the way #333 will produce: #390 carries the M1–M3 core, while `3154929` and `60634fd` are fork-side follow-ups authored after the PR was opened. Rebase `296b9d1` first, drop whatever git reduces to empty, then rebase the two follow-ups on top of upstream's merged version and keep only the hunks upstream does not already have. `d6e1d21` becomes purely historical at that point.

### Watch-item: this stack now contains upstream-worthy work

Unlike the two above, the rows marked **UPSTREAM-WORTHY** in the table have **no upstream PR at all**. Nothing will make them go away on their own — they will be replayed by every future rebase until someone offers them upstream. See the upstream-candidate note under the table for the list and why it is worth clearing.

### Caution: never delete a fork branch that backs an open upstream PR

While pruning `origin`'s branch list from 47 down to 7 on 2026-08-07, four branches turned out to be the head branch of an **open PR against `vfarcic/dot-agent-deck` (upstream)**. GitHub auto-closes a cross-repo pull request when its head branch is deleted, so deleting any of these would have silently killed an open upstream proposal:

| Fork branch | Open upstream PR |
| --- | --- |
| `feat/orchestration-command-entry-lock` | #404 — orchestration command-entry lock |
| `prd-386-shell-activity-descendant-scan` | #390 — shell-activity descendant scan |
| `prd-333-orchestration-tab-status-color` | #356 — orchestration tab status color |
| `prd-336-toggle-orchestration-pane-split-ratio` | #342 — pane-column split ratio |

This table is a 2026-08-07 snapshot and will go stale as these upstream PRs merge or close — regenerate it before trusting it, the same way [The current `fork-only` stack](#the-current-fork-only-stack) tells you to re-verify its SHAs against the live branch rather than trust the table: `gh pr list --repo vfarcic/dot-agent-deck --author prageethw --state open --json number,headRefName`.

**Why this trap is not obvious:** `prd-333` and `prd-336` each have a **merged fork PR** (#3 and #2 respectively — see the `fork-only` stack table above). By the fork-PR signal alone they look completely finished and safe to delete. Only the upstream check keeps them alive. This is a structural consequence of the fork's own workflow, not an edge case: the fork lands a change on `main` first, then proposes the same branch upstream, so "merged here, still open there" is the *normal* state for any upstream candidate — exactly the situation the [Watch-item: PRD #333 is temporary](#watch-item-prd-333-is-temporary) and [Watch-item: PRD #386 is temporary too](#watch-item-prd-386-is-temporary-too) sections already track from the stack side.

**`git branch -r --merged origin/main` is not a safe filter for this cleanup.** Every fork PR is squash-merged, so a squash-merged branch is never an ancestor of `main` — of the 47 branches, only 4 reported as merged by ancestry, while 36 had a `MERGED` fork PR. Ancestry under-reports; PR state, not ancestry, is the signal to use. Deleting a merged PR's branch loses nothing on its own — GitHub retains the merged commits against the PR (spot-checked on fork PR #79 after the prune: it still resolved with its original 2-file diff) — the hazard is specifically the *open upstream PR* case above.

The safe procedure used for the 2026-08-07 prune:

1. `git fetch origin --prune`
2. Build the keep-list from three sources, not one: the long-lived branches (`main`, `fork-only`); any branch with live uncommitted/unmerged work or an attached worktree (`git worktree list`); and every head branch of an open upstream PR (`gh pr list --repo vfarcic/dot-agent-deck --author prageethw --state open --json number,headRefName`).
3. Capture a rollback record before deleting anything: `git for-each-ref --format='%(refname:strip=3) %(objectname)' refs/remotes/origin`. Restoring a branch is `git push origin <sha>:refs/heads/<name>`.
4. Delete in batches of roughly 10 rather than one large push, so a single rejected ref does not obscure which of the others succeeded.
5. Verify afterwards that the open upstream PRs are **still OPEN** — this is the check that actually matters. If one flipped to CLOSED, restore its head branch from the rollback record and reopen the PR.

### Caution: don't assume a commit is a redundant duplicate

When curating this stack, verify claimed duplicates directly against `upstream/main` before excluding anything. Two candidate ancestors were considered:

- `483fe3d` (the fork's *local* PRD #311 commit) is a genuine duplicate of upstream's `f86c37b` — same content, different SHA, because upstream merged its own PR #334. **Correctly excluded.**
- PRD #336's commit was *briefly and wrongly* assumed to be a duplicate too — but upstream has **zero** trace of `ToggleOrchestrationSplit`. It is genuinely fork-only and **is** included (`ddbac1b`).

The lesson: a catalog-ID or filename collision can look like a false-positive "duplicate". Confirm with a direct content check before dropping a commit, e.g.:

```bash
git show upstream/main:src/ui.rs | grep ToggleOrchestrationSplit   # empty ⇒ genuinely fork-only, keep it
```

## Relationship to the config-backup files

[`fork-config-backups.md`](fork-config-backups.md) documents the `.fork-backup` snapshots of `devbox.json` / `.dot-agent-deck.toml` and a manual diff-and-restore procedure. That doc predates this workflow. Now that those two files are carried through `fork-only`'s rebase like any other fork commit (`6e20ca7`), the rebase is the **primary** mechanism that preserves them. The `.fork-backup` files and their diff-and-restore steps become a **secondary, belt-and-suspenders** safety net for detecting an accidental override — not the main line of defence. Keep them fresh per that doc, but treat `fork-only` as the source of truth.
