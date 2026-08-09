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

tab="$(printf '\t')"

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
is_long_lived() { grep -Fxq -- "$1" <<< "$long_lived"; }

# --- Degradation tracking ---
#
# Any time the script cannot fully trust its own PR-state or ref-freshness
# data, it sets `pr_state_degraded=true` rather than silently proceeding as if
# everything were clean (fork issue #140). `mark_degraded` also records a
# short, human-readable reason so the marker tells an operator something
# actionable instead of just "something, somewhere, failed".
pr_state_degraded=false
degraded_reasons=""

# A truncated MERGED-PR page is informational, not a degradation (fork issue
# #152 S4) -- kept separate from `pr_state_degraded` so it never blocks
# SKILL.md Step 6's "delete nothing until it comes back clean".
merged_list_truncated=false
mark_degraded() {
  pr_state_degraded=true
  if [ -n "${1:-}" ]; then
    degraded_reasons="${degraded_reasons}${1}
"
  fi
}

# Two reusable scratch files for capturing a command's stdout and stderr
# separately without a command-substitution subshell swallowing the reason
# (`$(...)` runs in a subshell, so a variable set inside it never reaches the
# caller). Reused sequentially across every `git fetch`/`gh` call below; each
# call truncates and re-reads them before the next one runs.
_stdout_file=$(mktemp)
_stderr_file=$(mktemp)
trap 'rm -f "$_stdout_file" "$_stderr_file"' EXIT

# capture_cmd <command...>: runs the command, leaving its stdout in
# $_stdout_file and, on failure, a short reason in $last_err (A2). Returns the
# command's own exit status.
last_err=""
capture_cmd() {
  if "$@" >"$_stdout_file" 2>"$_stderr_file"; then
    last_err=""
    return 0
  fi
  last_err=$(head -n1 "$_stderr_file")
  return 1
}

# --- Refresh remote-tracking refs (drops refs for branches deleted upstream) ---
if ! capture_cmd git fetch --prune --quiet origin; then
  mark_degraded "git fetch --prune failed: ${last_err:-unknown error}"
fi

current_branch=$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")
current_worktree=$(git rev-parse --show-toplevel 2>/dev/null || echo "")

# The MAIN working tree's own path — the root checkout, as opposed to any
# linked worktree. Excluding by this identity (rather than by matching branch
# name) means the root checkout stays excluded no matter what branch it
# happens to be on (fork issue #140 target behaviour 1) — a name-based check
# only catches it while it sits on `main`.
#
# Two independent ways this derivation can go wrong (fork issue #152 S2),
# both verified by direct measurement against a throwaway repo:
#   1. Pre-2.31 git does not recognise `--path-format`, and unlike most
#      subcommands `git rev-parse` echoes an unrecognised flag straight back
#      instead of failing -- so `git_common_dir` ends up holding the flag
#      text plus a relative `.git`, on two lines, exit 0. Fed unguarded to
#      `dirname`, that two-line string is one argument starting with `--`,
#      which `dirname` refuses (non-zero exit) -- and with no `||`/`if`
#      guard on that assignment, `set -e` killed the whole script before any
#      output was printed.
#   2. A relocated git directory (`git init --separate-git-dir`, or an
#      equivalent real-world setup) makes `git rev-parse --git-common-dir`
#      succeed and return the RELOCATED directory's own absolute path --
#      `dirname` of that is the relocated directory's PARENT, not the actual
#      worktree. Silently wrong, no crash, no degradation.
#
# Both failure modes are handled the same way: derive a candidate, then
# VERIFY it by asking git itself whether that candidate is genuinely a
# working tree (`--is-inside-work-tree`, run FROM the candidate). A
# crash-avoided-but-still-wrong candidate from failure mode 1, and the
# wrong-parent candidate from failure mode 2, both fail this check and
# degrade the run -- rather than either crashing with no output or silently
# trusting a path that was never confirmed.
git_common_dir=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null || echo "")
main_worktree_path=""
if [ -n "$git_common_dir" ]; then
  candidate=""
  if candidate=$(dirname "$git_common_dir" 2>/dev/null); then
    if [ "$(git -C "$candidate" rev-parse --is-inside-work-tree 2>/dev/null || echo "")" != "true" ]; then
      candidate=""
    fi
  else
    candidate=""
  fi
  main_worktree_path="$candidate"
fi
if [ -z "$main_worktree_path" ]; then
  mark_degraded "could not reliably determine the main working tree's own path (old git flag handling or a relocated git directory can make the derivation crash or silently resolve to the wrong path)"
fi

# --- Determine this fork's repository slugs (owner/repo) ---
#
# `gh` resolves a `--repo`-less query against this repository's PARENT when no
# default repo is configured -- the same trap docs/develop/fork-sync-workflow.md
# records for `gh pr create` (fork issue #140, D3). Derive both slugs from the
# actual git remotes so this script stays portable if it is ever copied
# elsewhere. A remote whose slug cannot be determined is never guessed at: the
# corresponding query is skipped and, since a missing PR-state query means the
# output can no longer be trusted as complete, the run is marked degraded
# (fork issue #140 target behaviour 2) -- except a genuinely UNCONFIGURED
# `upstream` remote, which is legitimately optional and not a degradation
# (target behaviour 3).
#
# `git config --get remote.<name>.url` reads the raw configured value. This is
# deliberately NOT `git remote get-url`, which APPLIES `url.<base>.insteadOf`
# rewriting (verified against git 2.55.0) and would silently derive an
# `insteadOf`-configured mirror's slug instead of the intended GitHub one.
is_owner_repo() {
  grep -Eq -- '^[^/]+/[^/]+$' <<< "$1"
}
slug_from_url() {
  sed -E 's#^(ssh://|git://|https?://)?([^@/]*@)?github\.com[:/]##; s#\.git$##' <<< "$1"
}

origin_slug=""
if origin_url=$(git config --get remote.origin.url 2>/dev/null); then
  candidate=$(slug_from_url "$origin_url")
  if is_owner_repo "$candidate"; then
    origin_slug="$candidate"
  else
    # Name the remote, never the URL: `slug_from_url`'s userinfo-stripping
    # only fires on the github.com branch, so a non-github.com remote (a GHES
    # host, say) can carry a raw, credential-bearing URL straight into this
    # message -- and DEGRADED_REASONS: is printed to stdout on every run
    # (fork issue #152 S1/S5). Inspect the URL with `git config --get
    # remote.origin.url` directly instead.
    mark_degraded "origin remote URL does not resolve to a GitHub owner/repo slug (run 'git config --get remote.origin.url' to inspect it)"
  fi
else
  mark_degraded "origin remote is not configured"
fi

upstream_slug=""
if upstream_url=$(git config --get remote.upstream.url 2>/dev/null); then
  candidate=$(slug_from_url "$upstream_url")
  if is_owner_repo "$candidate"; then
    upstream_slug="$candidate"
  else
    # Same redaction as the origin branch above (fork issue #152 S1/S5).
    mark_degraded "upstream remote URL does not resolve to a GitHub owner/repo slug (run 'git config --get remote.upstream.url' to inspect it)"
  fi
fi
# A missing `upstream` remote is left as-is here (empty, no `mark_degraded`
# call) -- not every checkout carries one, and that is a routine, not
# degraded, state (target behaviour 3).

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

if [ -z "$origin_slug" ]; then
  : # slug derivation above already marked this run degraded; nothing to query
elif ! command -v gh >/dev/null 2>&1; then
  mark_degraded "gh is not installed or not on PATH"
else
  # Head SHAs of merged same-repo PRs. Covers squash & rebase merges, where the
  # branch's commits never land verbatim on the default branch and so are
  # invisible to an ancestry test. Cross-repo (fork) PRs are excluded: their
  # head names describe branches in the fork, not ours, so a merged fork PR for
  # `fix/thing` says nothing about our own `fix/thing`. Pinned to
  # `$origin_slug` (fork issue #140, D3) -- unpinned, `gh` resolves against
  # this repo's parent.
  #
  # `--limit 200` bounds what THIS call fetches from the API; the
  # `isCrossRepository` select then drops rows client-side, so counting
  # survivors here (as the pre-#152 code did) counts AFTER the filter and can
  # under-count a genuinely full raw page (fork issue #152 S3). A second
  # query below asks for the COMPLEMENTARY partition -- cross-repo rows only,
  # same `--limit` -- and its count is added back in: same-repo survivors +
  # cross-repo survivors reconstructs the raw fetched-page size the two
  # queries together saw, which is what determines whether the page could
  # have been truncated.
  merged_same_repo_count=0
  if capture_cmd gh pr list --state merged --limit 200 --repo "$origin_slug" \
                  --json headRefName,headRefOid,isCrossRepository \
                  --jq '.[] | select(.isCrossRepository | not) | [.headRefName, .headRefOid] | @tsv'; then
    merged_tsv=$(cat "$_stdout_file")
    merged_same_repo_count=$(awk 'END { print NR }' "$_stdout_file")
    while IFS=$'\t' read -r name sha; do
      [ -z "$name" ] || [ -z "$sha" ] && continue
      line=$(printf '%s\t%s' "$name" "$sha")
      merged_shas_lines="${merged_shas_lines}${line}
"
    done <<< "$merged_tsv"
  else
    mark_degraded "gh pr list --state merged --repo $origin_slug failed: ${last_err:-unknown error}"
  fi

  merged_cross_repo_count=0
  if capture_cmd gh pr list --state merged --limit 200 --repo "$origin_slug" \
                  --json headRefName,headRefOid,isCrossRepository \
                  --jq '.[] | select(.isCrossRepository) | [.headRefName, .headRefOid] | @tsv'; then
    merged_cross_repo_count=$(awk 'END { print NR }' "$_stdout_file")
  else
    mark_degraded "gh pr list --state merged --repo $origin_slug (cross-repo count) failed: ${last_err:-unknown error}"
  fi

  merged_raw_count=$((merged_same_repo_count + merged_cross_repo_count))
  if [ "$merged_raw_count" -ge 200 ]; then
    # Fork issue #152 S4: at 200+ merged PRs this repo hits a full page on
    # EVERY run (91/200 today), which would make `PR_STATE_DEGRADED=true`
    # permanent and disable SKILL.md Step 6's "delete nothing until it comes
    # back clean" for good. Truncation here only means a later merged branch
    # goes unoffered (benign, under-offers) -- surface it informationally
    # instead of degrading.
    merged_list_truncated=true
  fi

  # Any open PR protects its head branch. Deleting the branch closes the PR, so
  # this is the guard that matters most. Queried against BOTH this fork and
  # upstream: an open upstream PR's head name IS our branch name (the fork
  # proposes its own branches upstream -- docs/develop/fork-sync-workflow.md),
  # so matching it can only over-protect (keep a branch we might have
  # pruned), which is the safe direction to err. The limit is higher than the
  # merged query's on purpose: truncating merged PRs just leaves a branch
  # unoffered, but truncating OPEN ones would offer a live PR's branch for
  # deletion -- so a full page here degrades too (target behaviour 4).
  if capture_cmd gh pr list --state open --limit 1000 --repo "$origin_slug" \
                  --json headRefName --jq '.[].headRefName'; then
    open_fork=$(cat "$_stdout_file")
    open_fork_count=$(awk 'END { print NR }' "$_stdout_file")
    open_prs="${open_prs}${open_fork}
"
    if [ "$open_fork_count" -ge 1000 ]; then
      mark_degraded "gh pr list --state open --repo $origin_slug returned a full page ($open_fork_count rows); results may be truncated"
    fi
  else
    mark_degraded "gh pr list --state open --repo $origin_slug failed: ${last_err:-unknown error}"
  fi

  if [ -n "$upstream_slug" ] && [ "$upstream_slug" != "$origin_slug" ]; then
    if capture_cmd gh pr list --state open --limit 1000 --repo "$upstream_slug" \
                    --json headRefName --jq '.[].headRefName'; then
      open_upstream=$(cat "$_stdout_file")
      open_upstream_count=$(awk 'END { print NR }' "$_stdout_file")
      open_prs="${open_prs}${open_upstream}
"
      if [ "$open_upstream_count" -ge 1000 ]; then
        mark_degraded "gh pr list --state open --repo $upstream_slug returned a full page ($open_upstream_count rows); results may be truncated"
      fi
    else
      mark_degraded "gh pr list --state open --repo $upstream_slug failed: ${last_err:-unknown error}"
    fi
  fi
fi

open_pr_has() {
  [ -n "$1" ] && grep -Fxq -- "$1" <<< "$open_prs"
}

merged_sha_matches() {
  local needle
  needle=$(printf '%s\t%s' "$1" "$2")
  grep -Fxq -- "$needle" <<< "$merged_shas_lines"
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

# --- Worktrees on merged branches (never the current worktree, the main
# working tree, or a long-lived branch name) ---
worktrees_out=()
wt_path=""
while IFS= read -r line; do
  case "$line" in
    "worktree "*) wt_path="${line#worktree }" ;;
    "branch refs/heads/"*)
      br="${line#branch refs/heads/}"
      if [ "$wt_path" != "$current_worktree" ] && [ "$wt_path" != "$main_worktree_path" ] \
        && ! is_long_lived "$br" && is_merged "$br" "refs/heads/${br}"; then
        worktrees_out+=("${wt_path}${tab}${br}")
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
  if [ -n "$degraded_reasons" ]; then
    echo "DEGRADED_REASONS:"
    while IFS= read -r reason; do
      [ -n "$reason" ] && echo "  ${reason}"
    done <<< "$degraded_reasons"
  fi
fi
if [ "$merged_list_truncated" = "true" ]; then
  echo "MERGED_LIST_TRUNCATED=true"
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
