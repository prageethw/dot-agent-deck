#!/usr/bin/env bash
set -euo pipefail

# Detect worktrees and local/remote branches whose work has already been merged,
# so the release skill can prune them after tagging.
#
# This script is detection-only: it never removes a worktree or deletes a branch.
# It does run `git fetch --prune` to refresh remote-tracking refs, which only
# updates local bookkeeping and never modifies the remote.
#
# Bash 3.2 safe on purpose: macOS ships bash 3.2 as `/bin/bash`, and
# `#!/usr/bin/env bash` resolves to it on the `macos-latest` CI runner. No
# associative arrays, no `${var,,}`/`${var^^}`, no `mapfile`/`readarray`, no
# `declare -n`.

# --- Determine the default branch ---
default_branch="main"
if ref=$(git symbolic-ref --quiet refs/remotes/origin/HEAD 2>/dev/null); then
  default_branch="${ref#refs/remotes/origin/}"
fi

# Branches that are long-lived BY DESIGN and are never cleanup candidates,
# whatever their merge state. `fork-only` is the fork's customisation stack:
# after a sync `main` is reset to it, so it is trivially "merged" while being
# the one branch that must never be deleted (docs/develop/fork-sync-workflow.md).
long_lived="$default_branch
fork-only"
is_long_lived() { printf '%s\n' "$long_lived" | grep -Fxq -- "$1"; }

# --- Refresh remote-tracking refs (drops refs for branches deleted upstream) ---
git fetch --prune --quiet origin 2>/dev/null || true

current_branch=$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")
current_worktree=$(git rev-parse --show-toplevel 2>/dev/null || echo "")

# --- Determine this fork's repository slugs (owner/repo) ---
#
# `gh` resolves a `--repo`-less query against this repository's PARENT when no
# default repo is configured -- the same trap docs/develop/fork-sync-workflow.md
# records for `gh pr create` (fork issue #140, D3). Derive both slugs from the
# actual git remotes so this script stays portable if it is ever copied
# elsewhere, falling back to this fork's own known slugs when a remote is
# unconfigured (not every checkout carries an `upstream` remote) or its URL
# does not parse as a plain "owner/repo" GitHub slug (e.g. a local/bare
# remote, as in this script's own test fixtures).
slug_from_remote() {
  # `|| true`: a missing remote (e.g. no `upstream` configured) must not trip
  # `set -e`/`pipefail` on the caller's `var=$(slug_from_remote ...)` -- an
  # assignment's exit status is the command substitution's, so a real failure
  # here would abort the whole script rather than fall through to the
  # "unconfigured" branch below.
  git remote get-url "$1" 2>/dev/null \
    | sed -E 's#^(git@github\.com:|https://github\.com/)##; s#\.git$##' \
    || true
}
is_owner_repo() {
  printf '%s' "$1" | grep -Eq '^[^/]+/[^/]+$'
}
origin_slug=$(slug_from_remote origin)
is_owner_repo "$origin_slug" || origin_slug="prageethw/dot-agent-deck"
upstream_slug=$(slug_from_remote upstream)
is_owner_repo "$upstream_slug" || upstream_slug="vfarcic/dot-agent-deck"

# --- Gather PR state ---
#
# Merge detection is per-COMMIT, not per-branch-name. A name alone proves
# nothing: Renovate reuses one branch name across many PRs, so a merged
# `renovate/foo` PR leaves that name looking "merged" long after the branch has
# been recreated at a new, unmerged tip for the next open PR. Judging by name
# there offers a live PR's branch up for deletion.
#
# No associative arrays (bash 3.2). `merged_shas_lines` holds "name<TAB>sha"
# lines, queried with an exact-line grep; `open_prs` holds one branch name per
# line, the same shape as `long_lived` above.
merged_shas_lines=""
open_prs=""
pr_state_degraded=false

if command -v gh >/dev/null 2>&1; then
  # Head SHAs of merged same-repo PRs. Covers squash & rebase merges, where the
  # branch's commits never land verbatim on the default branch and so are
  # invisible to an ancestry test. Cross-repo (fork) PRs are excluded: their
  # head names describe branches in the fork, not ours, so a merged fork PR for
  # `fix/thing` says nothing about our own `fix/thing`. Pinned to
  # `$origin_slug` (fork issue #140, D3) -- unpinned, `gh` resolves against
  # this repo's parent.
  if merged_tsv=$(gh pr list --state merged --limit 200 --repo "$origin_slug" \
                    --json headRefName,headRefOid,isCrossRepository \
                    --jq '.[] | select(.isCrossRepository | not) | [.headRefName, .headRefOid] | @tsv' \
                    2>/dev/null); then
    while IFS=$'\t' read -r name sha; do
      [ -z "$name" ] || [ -z "$sha" ] && continue
      line=$(printf '%s\t%s' "$name" "$sha")
      merged_shas_lines="${merged_shas_lines}${line}
"
    done <<< "$merged_tsv"
  else
    pr_state_degraded=true
  fi

  # Any open PR protects its head branch. Deleting the branch closes the PR, so
  # this is the guard that matters most. Queried against BOTH this fork and
  # upstream: an open upstream PR's head name IS our branch name (the fork
  # proposes its own branches upstream -- docs/develop/fork-sync-workflow.md),
  # so matching it can only over-protect (keep a branch we might have
  # pruned), which is the safe direction to err. The limit is higher than the
  # merged query's on purpose: truncating merged PRs just leaves a branch
  # unoffered, but truncating OPEN ones would offer a live PR's branch for
  # deletion.
  if open_fork=$(gh pr list --state open --limit 1000 --repo "$origin_slug" \
                   --json headRefName --jq '.[].headRefName' 2>/dev/null); then
    open_prs="${open_prs}${open_fork}
"
  else
    pr_state_degraded=true
  fi

  if [ -n "$upstream_slug" ] && [ "$upstream_slug" != "$origin_slug" ]; then
    if open_upstream=$(gh pr list --state open --limit 1000 --repo "$upstream_slug" \
                         --json headRefName --jq '.[].headRefName' 2>/dev/null); then
      open_prs="${open_prs}${open_upstream}
"
    else
      pr_state_degraded=true
    fi
  fi
else
  pr_state_degraded=true
fi

open_pr_has() {
  [ -n "$1" ] && printf '%s\n' "$open_prs" | grep -Fxq -- "$1"
}

merged_sha_matches() {
  local needle
  needle=$(printf '%s\t%s' "$1" "$2")
  printf '%s\n' "$merged_shas_lines" | grep -Fxq -- "$needle"
}

# is_merged <branch-name> <ref-to-its-tip>
# Resolves the ref's OWN tip, so a local branch and its same-named remote are
# judged independently -- `origin/foo` may carry unmerged commits that local
# `foo` does not.
is_merged() {
  local name="$1" ref="$2" tip
  if open_pr_has "$name"; then
    return 1
  fi
  tip=$(git rev-parse --verify --quiet "$ref") || return 1
  [ -z "$tip" ] && return 1
  # Real merge or fast-forward: the tip is already reachable from the default.
  if git merge-base --is-ancestor "$tip" "refs/remotes/origin/${default_branch}" 2>/dev/null; then
    return 0
  fi
  # Squash/rebase merge: the tip must still be exactly what the merged PR
  # carried. A recreated or advanced branch has moved on and is not merged.
  merged_sha_matches "$name" "$tip"
}

# --- Worktrees on merged branches (never the current worktree or a long-lived branch) ---
worktrees_out=()
wt_path=""
while IFS= read -r line; do
  case "$line" in
    "worktree "*) wt_path="${line#worktree }" ;;
    "branch refs/heads/"*)
      br="${line#branch refs/heads/}"
      if [ "$wt_path" != "$current_worktree" ] && ! is_long_lived "$br" && is_merged "$br" "refs/heads/${br}"; then
        worktrees_out+=("${wt_path}|${br}")
      fi
      ;;
    "") wt_path="" ;;
  esac
done < <(git worktree list --porcelain 2>/dev/null || true)

# --- Local branches that are merged (never current / long-lived) ---
# A branch checked out in another worktree is still listed here; it is only
# deletable once its worktree is removed, hence the worktree-first ordering in
# the skill's cleanup step.
local_out=()
while IFS= read -r b; do
  [ -z "$b" ] && continue
  is_long_lived "$b" && continue
  [ "$b" = "$current_branch" ] && continue
  is_merged "$b" "refs/heads/${b}" && local_out+=("$b")
done < <(git branch --format='%(refname:short)' 2>/dev/null || true)

# --- Remote branches that are merged (never long-lived) ---
remote_out=()
while IFS= read -r b; do
  b="${b#origin/}"
  [ -z "$b" ] && continue
  [ "$b" = "HEAD" ] && continue
  is_long_lived "$b" && continue
  is_merged "$b" "refs/remotes/origin/${b}" && remote_out+=("$b")
done < <(git branch -r --format='%(refname:short)' 2>/dev/null | grep '^origin/' || true)

# --- Output structured summary ---
echo "DEFAULT_BRANCH=${default_branch}"
if [ "$pr_state_degraded" = "true" ]; then
  echo "PR_STATE_DEGRADED=true"
fi

total=$(( ${#worktrees_out[@]} + ${#local_out[@]} + ${#remote_out[@]} ))
if [ "$total" -eq 0 ]; then
  echo "NOTHING_TO_CLEAN=true"
  exit 0
fi
echo "NOTHING_TO_CLEAN=false"

echo "WORKTREES:"
for w in "${worktrees_out[@]:-}"; do [ -n "$w" ] && echo "  ${w}"; done

echo "LOCAL_BRANCHES:"
for b in "${local_out[@]:-}"; do [ -n "$b" ] && echo "  ${b}"; done

echo "REMOTE_BRANCHES:"
for b in "${remote_out[@]:-}"; do [ -n "$b" ] && echo "  ${b}"; done

exit 0
