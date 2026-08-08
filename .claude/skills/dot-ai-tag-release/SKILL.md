---
name: dot-ai-tag-release
description: Create a release tag based on accumulated changelog fragments, then prune merged worktrees and branches. Run when ready to cut a release.
user-invocable: true
---

# Create Release Tag

Create a semantic version tag based on accumulated changelog fragments.

## When to Use

Run this skill when:
- Multiple PRs have been merged with changelog fragments
- You're ready to cut a release
- After the /prd-done workflow completes (not during it)

## Workflow

### Step 1: Analyze

Run the analysis script bundled with this skill:
```bash
bash .claude/skills/dot-ai-tag-release/analyze.sh
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

### Step 4: Create and Push Tag

After confirmation:
```bash
git tag -a [version] -m "[Brief description summarizing the fragments]"
git push origin [version]
```

### Step 5: Confirm Success

Show the user:
1. The tag created
2. The tag URL on GitHub (if applicable)
3. Note that CI/CD will generate release notes from the fragments

### Step 6: Clean Up Merged Worktrees and Branches

Once the release is tagged, the branches and worktrees whose work it contains are
done. Run the read-only detection script bundled with this skill:
```bash
bash .claude/skills/dot-ai-tag-release/cleanup.sh
```

Interpret the output:
- If the output contains `PR_STATE_DEGRADED=true`, the PR-state or ref-freshness
  data this run relied on could not be fully trusted (a `gh` failure or absence, an
  undetermined repository slug, a truncated result page, or a failed `git fetch`
  — see the `DEGRADED_REASONS:` lines when present) — stop and do not delete
  anything until you can re-run it clean, since an open-PR branch could
  otherwise be offered unprotected.
- If `NOTHING_TO_CLEAN=true`, tell the user there is nothing to clean and finish.
- Otherwise present the `WORKTREES`, `LOCAL_BRANCHES`, and `REMOTE_BRANCHES` lists
  and ask the user to confirm before deleting anything. Each `WORKTREES:` entry
  is `path<TAB>branch` (a literal TAB, not `|` — a branch name may legally
  contain `|`, which would make that separator ambiguous); split on the TAB to
  get the `[worktree_path]` step 1 below removes.

**This step is destructive — always show the full list and get explicit
confirmation first.** `.claude/skills/dot-ai-tag-release/cleanup.sh` already excludes: the ROOT checkout, by its own path — not by matching a branch name, so it stays excluded no matter what branch it happens to be on; `fork-only`, which is trivially "merged" into `main` immediately after every fork/upstream sync while being the one branch that must never be deleted (`docs/develop/fork-sync-workflow.md`); the worktree/local branch you are currently on; and any branch that backs an open PR on either this fork or upstream. The current-branch exclusion applies to the worktree and local-branch guards only — `REMOTE_BRANCHES:` does not exclude your current branch's own `origin/<branch>`, so it can still be offered there if merged.

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

2. Delete each local branch:
   ```bash
   git branch -d [branch]
   ```
   Use `-d` (not `-D`) as a safety net — it refuses unmerged branches. If it
   refuses, surface the warning and ask before forcing with `-D`.

3. Delete each remote branch:
   ```bash
   git push origin --delete [branch]
   ```

Finally, prune stale worktree metadata:
```bash
git worktree prune
```

## Guidelines

- **Don't run during PR workflow**: This is a separate release activity
- **Review fragments first**: Make sure all fragments are accurate before tagging
- **Use semantic versioning**: Follow semver strictly based on fragment types. While the project is pre-1.0 (`v0.x`), the minor digit is the compatibility boundary, so `breaking` fragments bump the minor and `feature`/`bugfix` fragments are patch releases; from `1.0` onward, standard semver applies (`breaking`→major, `feature`→minor, `bugfix`→patch). The `.claude/skills/dot-ai-tag-release/analyze.sh` output already reflects this.
- **Brief tag message**: Summarize the release in 1-2 sentences
- **Never tag [skip ci] commits**: Always create a preparation commit first
- **Clean up only after tagging**: Run the cleanup step (Step 6) once the release
  is cut, never before — and always confirm the detected list with the user, since
  removing worktrees and deleting branches is destructive

