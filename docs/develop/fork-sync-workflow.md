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

Oldest to newest, rooted at an upstream base commit. Always re-verify against the live branch before trusting exact SHAs — run `git log origin/fork-only --oneline -20`, since a sync rewrites every SHA below the base:

| SHA | Commit | Status |
| --- | --- | --- |
| `9ca7de1` | `docs(claude): correct rule 8's required-status-check claim` | **base** (upstream commit) |
| `ab71a28` | `fix(prd-336): toggle orchestration pane-column split ratio [CI shadow] (#2)` | **PERMANENT** fork-only |
| `ac43b4e` | `feat(prd-333): color orchestration tab labels by highest-priority pane status (#3)` | **TEMPORARY** — see watch-item below |
| `349e895` | `fork-only: remove agent-type badge from cards, rename title to worker-deck (#4)` | **PERMANENT** fork-only |
| `bbad18a` | `fork-only: auto-focus active orchestration tab on WaitingForInput pane (#5)` | **PERMANENT** fork-only |
| `f98dd56` | `test(fork): cover the auto_focus_waiting_pane render-loop wiring (L1) (#6)` | **PERMANENT** fork-only |
| `d87f995` | `fork-only: swap worker role devbox commands to claude-sonnet-devbox/codex-devbox wrappers (#7)` | **PERMANENT** fork-only |
| `e42b288` | `fork-only: add devbox.json/.dot-agent-deck.toml backups + restore doc (#8)` | **PERMANENT** fork-only |
| `e034c45` | `fork-only: carry the sync-workflow doc onto this branch too` | **PERMANENT** fork-only (this doc) |
| `86d13e2` | `fork-only: document the active-config check before delegating` | **PERMANENT** fork-only (this doc) |
| `30c5f79` | `feat(prd-371): three-stage Ctrl+l pane-split toggle (Default/Narrow/Hidden) (#10)` | **PERMANENT** fork-only |
| `8c07d10` | `fix(prd-372): clear WaitingForInput on the approved tool's own ToolStart (#11)` | **PERMANENT** fork-only |
| `26255e8` | `feat(prd-374): lock command entry to the orchestrator pane, Ctrl+e to unlock (#12)` | **PERMANENT** fork-only |
| `35780ef` | `fork-only: reassign orchestrator/coder to opus, release to haiku (#13)` | **PERMANENT** fork-only |
| `fc45d01` | `docs(prd-370): create PRD #370 - shell activity working status [skip ci]` | **PERMANENT** fork-only (doc) |
| `703b4d2` | `docs(prd-383): create PRD #383 - blocked-keystroke reset for the Orchestration inactivity timer [skip ci]` | **PERMANENT** fork-only (doc) |
| `d6e1d21` | `feat(prd-370): treat underlying shell activity as Working status inside a worker pane (#14)` | **PERMANENT** fork-only |
| `08a9402` | `fork-only: run the L2 e2e tier in CI as an informational, non-blocking job` | **PERMANENT** fork-only |

The base is `9ca7de1` — `upstream/main`'s tip at the time of this sync (2026-08-05). Every commit above it was verified as genuinely fork-only before inclusion: none of the symbols/behaviors they introduce (`SplitStage`, `command_entry_locked`, `ToggleOrchestrationSplit`, the shell-activity status change, `claude-sonnet-devbox`) exist anywhere in `upstream/main`. This sync also picked up 7 commits (`30c5f79` through `d6e1d21`) that had landed directly on `main` since the previous sync without ever being added to `fork-only` — a reminder that `fork-only` needs to be kept current as fork-specific work lands on `main`, not just rebuilt at sync time. Consider re-curating it (or running the sync procedure) more often than "whenever it's badly out of date" to keep this drift small.

### Watch-item: PRD #333 is temporary

`ac43b4e` (PRD #333, colour orchestration tab labels by status) is **not** a permanent fork feature. It already has an open upstream PR — **#356** on `vfarcic/dot-agent-deck`, still open/unmerged as of this sync (2026-08-05), blocked on upstream maintainer merge rights, the same situation as PRs #352/#346. It sits in the stack only because fork-only commit #5 (`bbad18a`, auto-focus) structurally depends on the `pane_status_for_tabs` code PRD #333 introduces.

**When PR #356 actually merges upstream:** the next `git rebase upstream/main` should find `73b233c`'s changes already present natively. Git will likely reduce it to an empty/no-op commit — drop it at that point (`git rebase --skip` when it stops on the empty commit, or rebase with `--empty=drop`). Until #356 merges, leave it in.

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
