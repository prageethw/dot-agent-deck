#!/usr/bin/env bash
#
# Read-only discovery for /review-pr-upstream: every open PR in a repo that is
# genuinely waiting on the authenticated user's review. Creates nothing,
# checks out nothing, posts nothing.
#
# Usage: list-waiting.sh [owner/repo]
#   With no argument, resolves the repo from the 'upstream' git remote of the
#   current checkout.
#
# "Waiting on me" = open, not authored by me, and either:
#   - I'm currently a requested reviewer, or
#   - I've reviewed before (CHANGES_REQUESTED / COMMENTED / APPROVED) and at
#     least one commit landed after my last review's submittedAt — the two
#     cases GitHub's UI never re-flags as a fresh request: my prior feedback
#     not yet re-examined, and an approval silently invalidated by
#     dismiss_stale_reviews_on_push.
#
# Output: KEY=value prelude, then one compact JSON object per in-scope PR
# (JSON Lines), then WAITING_COUNT=<n> and SUCCESS=true. On any failure,
# ERROR=true plus a MESSAGE=, exit 0 — never a bare non-zero a caller has to
# interpret.

set -uo pipefail

repo_arg="${1:-}"
if [ -z "$repo_arg" ]; then
  url="$(git remote get-url upstream 2>/dev/null || true)"
  if [ -z "$url" ]; then
    echo "ERROR=true"
    echo "MESSAGE=No repo given and no 'upstream' git remote in this checkout. Usage: list-waiting.sh <owner/name>"
    exit 0
  fi
  # Strips the transport (ssh or https) and a trailing .git, leaving owner/name.
  repo_arg="$(printf '%s' "$url" | sed -E 's#^(git@|https://)([^:/]+)[:/](.+?)(\.git)?$#\3#')"
fi

case "$repo_arg" in
  */*) ;;
  *)
    echo "ERROR=true"
    echo "MESSAGE=Could not parse owner/name from '${repo_arg}'"
    exit 0
    ;;
esac

if ! command -v gh >/dev/null 2>&1; then
  echo "ERROR=true"
  echo "MESSAGE=GitHub CLI (gh) is required: https://cli.github.com/"
  exit 0
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "ERROR=true"
  echo "MESSAGE=jq is required"
  exit 0
fi

me="$(gh api user --jq .login 2>/dev/null || true)"
if [ -z "$me" ]; then
  echo "ERROR=true"
  echo "MESSAGE=Could not resolve the authenticated gh user (check: gh auth status)"
  exit 0
fi

owner="${repo_arg%%/*}"
name="${repo_arg##*/}"

echo "REPO=${repo_arg}"
echo "ME=${me}"

# --- Candidate PR numbers ---------------------------------------------------
#
# review-requested:  I'm currently in the requested-reviewers list.
# reviewed-by:        I've left a review before, possibly now stale — GitHub
#                     has no search qualifier for "stale", so every PR I've
#                     ever reviewed is a candidate and gets classified below.
requested_nums="$(gh pr list --repo "$repo_arg" --state open \
  --search "review-requested:${me}" --json number --jq '.[].number' 2>/dev/null || true)"
reviewed_nums="$(gh pr list --repo "$repo_arg" --state open \
  --search "reviewed-by:${me}" --json number --jq '.[].number' 2>/dev/null || true)"

all_nums="$(printf '%s\n%s\n' "$requested_nums" "$reviewed_nums" | grep -E '^[0-9]+$' | sort -n -u || true)"

if [ -z "$all_nums" ]; then
  echo "WAITING_COUNT=0"
  echo "SUCCESS=true"
  exit 0
fi

# --- Per-PR classification --------------------------------------------------
#
# gh pr view --json reviewRequests entries carry `login` for a User reviewer
# or `name` for a Team; match on (.login // .name). gh api's own --jq cannot
# take --arg, so $me is threaded through a separate `jq --arg` pass instead of
# being inlined into a --jq expression anywhere in this script.
classify_filter='
  (.author.login == $me) as $isMine |
  (.reviewRequests // [] | map(.login // .name) | index($me) != null) as $requested |
  ((.reviews // []) | map(select(.author.login == $me)) | sort_by(.submittedAt) | last) as $lastReview |
  ((.commits // []) | map(.committedDate) | sort | last) as $lastCommitAt |
  {
    number, title, url, isDraft,
    author: .author.login,
    lastCommitAt: $lastCommitAt,
    myLastReviewState: ($lastReview.state // null),
    myLastReviewAt: ($lastReview.submittedAt // null)
  } + (
    if $isMine then
      {reason: null}
    elif $requested then
      {reason: "review-requested"}
    elif ($lastReview != null
          and ($lastReview.state == "CHANGES_REQUESTED" or $lastReview.state == "COMMENTED" or $lastReview.state == "APPROVED")
          and $lastCommitAt != null and $lastReview.submittedAt != null
          and $lastCommitAt > $lastReview.submittedAt) then
      {reason: ("stale-review:" + $lastReview.state)}
    else
      {reason: null}
    end
  )
'

waiting=0
for n in $all_nums; do
  info="$(gh pr view "$n" --repo "$repo_arg" \
    --json number,title,url,isDraft,author,reviewRequests,reviews,commits 2>/dev/null || true)"
  [ -z "$info" ] && continue

  classified="$(printf '%s' "$info" | jq -c --arg me "$me" "$classify_filter" 2>/dev/null || true)"
  [ -z "$classified" ] && continue

  reason="$(printf '%s' "$classified" | jq -r '.reason // "null"')"
  [ "$reason" = "null" ] && continue

  # Unresolved-thread count, via GraphQL — not expressible in `gh pr view
  # --json`. Page past 100 threads (checking hasNextPage) before trusting a
  # zero on a large PR; unpaged, a zero is unproven, not confirmed.
  threads_raw="$(gh api graphql -f query='
    query($owner:String!, $repo:String!, $pr:Int!, $cursor:String) {
      repository(owner:$owner, name:$repo) {
        pullRequest(number:$pr) {
          reviewThreads(first:100, after:$cursor) {
            totalCount
            pageInfo { hasNextPage endCursor }
            nodes { isResolved }
          }
        }
      }
    }' -F owner="$owner" -F repo="$name" -F pr="$n" 2>/dev/null || true)"

  unresolved=0
  cursor=""
  if [ -n "$threads_raw" ]; then
    page="$threads_raw"
    while :; do
      unresolved=$((unresolved + $(printf '%s' "$page" | jq '[.data.repository.pullRequest.reviewThreads.nodes[] | select(.isResolved | not)] | length' 2>/dev/null || echo 0)))
      more="$(printf '%s' "$page" | jq -r '.data.repository.pullRequest.reviewThreads.pageInfo.hasNextPage' 2>/dev/null || echo false)"
      [ "$more" != "true" ] && break
      cursor="$(printf '%s' "$page" | jq -r '.data.repository.pullRequest.reviewThreads.pageInfo.endCursor' 2>/dev/null || echo null)"
      [ "$cursor" = "null" ] && break
      page="$(gh api graphql -f query='
        query($owner:String!, $repo:String!, $pr:Int!, $cursor:String) {
          repository(owner:$owner, name:$repo) {
            pullRequest(number:$pr) {
              reviewThreads(first:100, after:$cursor) {
                pageInfo { hasNextPage endCursor }
                nodes { isResolved }
              }
            }
          }
        }' -F owner="$owner" -F repo="$name" -F pr="$n" -F cursor="$cursor" 2>/dev/null || true)"
      [ -z "$page" ] && break
    done
  fi

  waiting=$((waiting + 1))
  printf '%s' "$classified" | jq -c --argjson unresolved "$unresolved" '. + {unresolvedThreads: $unresolved}'
done

echo "WAITING_COUNT=${waiting}"
echo "SUCCESS=true"
