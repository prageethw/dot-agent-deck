---
name: tag-release
description: Create a release tag based on accumulated changelog fragments, then prune merged worktrees and branches using SHA-vetted merge detection. Run when ready to cut a release.
user-invocable: true
---

# Create Release Tag

Create a semantic version tag based on accumulated changelog fragments.

This skill is **project-local and owned here** (CLAUDE.md rule 13), forked from the `dot-ai-tag-release` mirror. It differs from that mirror in one deliberate way: the cleanup step (Step 7) uses a SHA-vetted merge check and `git branch -D` instead of a plain `git branch -d` ancestry test — see that step for why. It does **not** adopt upstream's workflow-dispatch automation for Steps 1–6 (`.github/workflows/tag-release.yml`, issue #1089 upstream): that automation exists to avoid a human bypassing a branch-protection ruleset when pushing the version-pin commit and tag, and this fork's own `main` carries no such ruleset at all (CLAUDE.md rule 8), so the problem it solves does not apply here. `docs/develop/fork-sync-workflow.md`'s 14th-sync entry records this decision.

## When to Use

Run this skill when:
- Multiple PRs have been merged with changelog fragments
- You're ready to cut a release
- After the /prd-done workflow completes (not during it)

## Workflow

### Step 1: Analyze

Run the analysis script bundled with this skill:
```bash
bash .claude/skills/tag-release/analyze.sh
```

If the script fails (non-zero exit) or the output contains `ERROR=true`, show the `MESSAGE` to the user and stop.

If the output contains `NO_FRAGMENTS=true`, inform the user there's nothing to release and stop.

### Step 2: Propose Version

Present the script output to the user:
1. Current version (`CURRENT_VERSION`)
2. Fragments found (the `FRAGMENTS` list with their types)
3. Proposed next version (`PROPOSED_VERSION`) based on bump type (`BUMP_TYPE`)
4. Ask for confirmation or allow override

### Step 3: Handle [skip ci]

If `SKIP_CI=true`, inform the user that tagging HEAD would prevent the release workflow from running. Create a preparation commit:
```bash
git commit --allow-empty -m "chore: prepare release [version]"
git push origin HEAD
```

### Step 4: Sync the Flake Version Pin

Do this **before** creating the tag, so the tagged tree carries the right pin.

`flake.nix` pins the released version in a `version = "..."` let-binding and hands it to the build as `DAD_VERSION`. It has to: a Nix source build gets a tarball with no `.git` for `build.rs` to describe, so with no pin the binary reports the `0.1.0` placeholder from `Cargo.toml`. `release.yml` hard-fails the release when the pin and the tag disagree, and it does so in `prepare`, which runs *after* the tag has been pushed. A missed bump therefore costs you a deleted remote tag, not just a re-run.

The pin carries **no leading `v`**. The tag is `v0.35.7`; the pin is `0.35.7`. Strip it.

```bash
# [version] is the tag as everywhere else in this skill (e.g. v0.35.8).
# [version-no-v] is that same string with the leading `v` removed (0.35.8).
sed -i.bak -E 's/^([[:space:]]*)version = "[^"]+";$/\1version = "[version-no-v]";/' flake.nix && rm flake.nix.bak
git diff flake.nix
```

The diff must show exactly one changed line: `release.yml` also refuses to release unless `flake.nix` contains exactly one anchored `version = "...";` line. Then commit and push:

```bash
git commit -am "chore: pin flake version to [version]"
git push origin HEAD
```

If the repository has no `flake.nix`, skip this step.

### Step 5: Create and Push Tag

After confirmation:
```bash
git tag -a [version] -m "[Brief description summarizing the fragments]"
git push origin [version]
```

### Step 6: Confirm Success

Show the user:
1. The tag created
2. The tag URL on GitHub (if applicable)
3. Note that CI/CD will generate release notes from the fragments

### Step 7: Clean Up Merged Worktrees and Branches

Once the release is tagged, the branches and worktrees whose work it contains are
done. Run the read-only detection script bundled with this skill:
```bash
bash .claude/skills/tag-release/cleanup.sh
```

Interpret the output:
- If the output contains `PR_STATE_DEGRADED=true`, the PR-state or ref-freshness
  data this run relied on could not be fully trusted (a `gh` failure or absence, an
  undetermined repository slug, a truncated open-PR result page, a failed `git fetch`,
  or an undeterminable main-worktree path — see the `DEGRADED_REASONS:` lines when
  present) — stop and do not delete anything until you can re-run it clean, since an
  open-PR branch could otherwise be offered unprotected.
- If the output contains `MERGED_LIST_TRUNCATED=true`, the merged-PR query hit its
  own page limit — this is informational, not a degradation: it only means a later
  merged branch may not have been offered this run, never that something unsafe was
  offered. Proceed normally.
- If `NOTHING_TO_CLEAN=true`, tell the user there is nothing to clean and finish.
- Otherwise present the `WORKTREES`, `LOCAL_BRANCHES`, and `REMOTE_BRANCHES` lists
  and ask the user to confirm before deleting anything. Each `WORKTREES:` entry
  is `path<TAB>branch` (a literal TAB, not `|` — a branch name may legally
  contain `|`, which would make that separator ambiguous); a worktree's own path
  can itself contain a TAB, but a branch name never can, so split on the LAST TAB
  to get the `[worktree_path]` step 1 below removes. `cleanup.sh` reads
  `git worktree list --porcelain -z` internally, so a path containing a raw
  newline is no longer truncated (fork issue #152 A-N2) — but that also means
  such an entry can visibly span more than one output line. Do not assume one
  `WORKTREES:` entry is one line; if an entry looks unusual, inspect it closely
  before running `git worktree remove` on it. Each `LOCAL_BRANCHES:`/
  `REMOTE_BRANCHES:` entry is `<branch> <sha>`, where the SHA is the tip
  `cleanup.sh` actually vetted as merged — the delete step below uses it.

**This step is destructive — always show the full list and get explicit confirmation first.** Never touch the default branch (`DEFAULT_BRANCH`) — neither the root checkout nor any other worktree or branch backing it. `.claude/skills/tag-release/cleanup.sh` already excludes: the ROOT checkout, by its own path — not by matching a branch name, so it stays excluded no matter what branch it happens to be on; the default branch by name, so a second, LINKED worktree checked out on it is excluded too, not only the root checkout; `fork-only`, which is trivially "merged" into `main` immediately after every fork/upstream sync while being the one branch that must never be deleted (`docs/develop/fork-sync-workflow.md`); the worktree/local branch you are currently on; and any branch that backs an open PR on either this fork or upstream. The current-branch exclusion applies to the worktree and local-branch guards only — `REMOTE_BRANCHES:` does not exclude your current branch's own `origin/<branch>`, so it can still be offered there if merged.

After confirmation, process the items **in this order**:

1. Remove each worktree (must come before deleting its branch — a branch checked
   out in a worktree cannot be deleted):
   ```bash
   git worktree remove [worktree_path]
   ```
   If a worktree has uncommitted changes git refuses; report it and skip rather
   than using `--force`, unless the user explicitly asks. If it instead fails with
   `fatal: '<path>' is a main working tree` (exit 128 — `--force` does not
   override this either), stop and report it rather than working around it: this
   should never happen once the exclusion guard above is working, and
   improvising something like `rm -rf` on that path is the exact destructive
   mistake this whole step exists to prevent.

2. Delete each local branch. Each entry is `<branch> <sha>`; confirm the branch
   still points at the vetted SHA and then use `-D`:
   ```bash
   [ "$(git rev-parse [branch])" = "[sha]" ] && git branch -D [branch]
   ```

   **`-D`, not `-d`.** `git branch -d` tests *ancestry* — is this tip reachable
   from the branch's upstream, or from `HEAD` — and a squash merge never lands
   the branch's commits on `main`, so it refuses every correctly squash-merged
   branch and hints the operator straight to `-D` anyway. Measured 2026-09-14
   upstream: it would have refused all 13 correctly-merged dispatch branches in
   a real sample. A check that is wrong that often does not make anyone
   careful — it teaches them to type `-D` on everything, including the one
   branch that genuinely was not merged. `cleanup.sh` already made the real
   check, from PR state rather than ancestry, before ever offering a branch: it
   offers one only when its tip is either reachable from `origin/<default>` or
   is exactly the head SHA of a merged, same-repo PR, and never when an open PR
   points at that name. The SHA comparison above is what carries that verdict
   through to the delete — it is what catches the branch having moved since the
   scan, which is the one thing the scan cannot see. Delete only names
   `cleanup.sh` printed, and re-run it rather than reusing a stale list.

3. Delete each remote branch, gated the same way against its vetted SHA:
   ```bash
   [ "$(git rev-parse refs/remotes/origin/[branch])" = "[sha]" ] && git push origin --delete [branch]
   ```
   That compares the remote-tracking ref `cleanup.sh` refreshed with its own
   `git fetch --prune`, so it catches a list gone stale in your hands but not
   the remote advancing since that fetch. Re-run `cleanup.sh` rather than
   working from an old list.

Finally, prune stale worktree metadata:
```bash
git worktree prune
```

## Guidelines

- **Don't run during PR workflow**: This is a separate release activity
- **Review fragments first**: Make sure all fragments are accurate before tagging
- **Use semantic versioning**: Follow semver strictly based on fragment types. While the project is pre-1.0 (`v0.x`), the minor digit is the compatibility boundary, so `breaking` fragments bump the minor and `feature`/`bugfix` fragments are patch releases; from `1.0` onward, standard semver applies (`breaking`→major, `feature`→minor, `bugfix`→patch). The `.claude/skills/tag-release/analyze.sh` output already reflects this.
- **Brief tag message**: Summarize the release in 1-2 sentences
- **Never tag [skip ci] commits**: Always create a preparation commit first
- **Clean up only after tagging**: Run the cleanup step (Step 7) once the release
  is cut, never before — and always confirm the detected list with the user, since
  removing worktrees and deleting branches is destructive
