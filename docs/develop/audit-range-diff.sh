#!/usr/bin/env bash
# Audits whether fork-only commits from an OLD point in history genuinely
# survive (by content, not by SHA) at a NEW point — typically "main right
# before a sync" vs. "main right now" or "main right after that sync".
#
# WHY THIS EXISTS, AND WHY IT IS NOT A SIMPLE `git merge-base --is-ancestor`
# LOOP OVER MERGED PRS: this fork's `fork-only` branch is periodically
# rebased onto upstream/main and `main` is reset to match it (see "The sync
# procedure" above in this doc), so every fork-only commit gets a brand-new
# SHA on every re-curation. Measured 2026-08-17 while auditing issue #443:
#   - 0 of 20 randomly-sampled fork-only PRs' GitHub-recorded
#     `mergeCommit.oid` were ancestors of current `main`.
#   - 156 of this doc's own 203 stack-table rows cited a SHA that is not an
#     ancestor of current `main` either.
# Both are expected, not evidence of loss: this doc's own stack-table intro
# already warns "a sync rewrites every SHA below the base", and a row's SHA
# is a label frozen at whatever re-curation last touched that row, not
# something kept literally current. A `gh pr list` + ancestor-check sweep
# over merged PRs is therefore not a meaningful signal on this fork.
#
# WHAT WORKS: `git range-diff` matches commits between two ranges by patch
# similarity, not by position or SHA, so it can say "this commit's diff has
# no reasonable match anywhere in the new range" even after N rebases have
# rewritten every SHA in between. Three outcomes per old-range commit:
#   '='  identical patch survives verbatim (no SHA change at all — the
#        commit sits before whatever first diverged)
#   '!'  correlated (same commit slot, matched by message/context) but the
#        PATCH CONTENT differs — inspect the diff-of-diff; a large deletion
#        here is the exact shape issue #443's DeliveryPhase case was (PR
#        #270's merge commit correlated cleanly by message while its whole
#        `DeliveryPhase` implementation was removed)
#   '<'  no match found in the new range at all — the strongest automated
#        signal of a genuine drop
# Neither '!' nor '<' is proof by itself: '!' can be a legitimate rewrite
# (message reworded, tests renamed) and '<' can be a commit whose content
# was folded into a differently-shaped commit elsewhere (verified false
# positives found 2026-08-17: 9a7f2af8/binary_name(), 10762e0f/
# DelegateResponse — both showed as unmatched '<' yet their functionality
# is fully present in current main under evolved names). Always finish by
# grepping current `src/`/`tests/` for the commit's actual symbols before
# concluding anything is lost.
#
# Usage:
#   docs/develop/audit-range-diff.sh <old-tip> <new-tip> [<upstream-ref>]
# <upstream-ref> defaults to upstream/main and is only used to find the
# shared base for both ranges via merge-base — override it if upstream/main
# has moved on since the point you actually want to compare against.
set -euo pipefail

old_tip="${1:?usage: audit-range-diff.sh <old-tip> <new-tip> [<upstream-ref>]}"
new_tip="${2:?usage: audit-range-diff.sh <old-tip> <new-tip> [<upstream-ref>]}"
upstream_ref="${3:-upstream/main}"

for sha in "$old_tip" "$new_tip"; do
  git cat-file -e "$sha" 2>/dev/null || {
    echo "error: $sha is not present locally — git fetch first" >&2
    exit 2
  }
done

base_old="$(git merge-base "$old_tip" "$upstream_ref")"
base_new="$(git merge-base "$new_tip" "$upstream_ref")"
if [ "$base_old" != "$base_new" ]; then
  echo "warning: old-tip and new-tip share different merge-bases with $upstream_ref" >&2
  echo "  old base: $base_old" >&2
  echo "  new base: $base_new" >&2
  echo "  proceeding with old-tip's base for both ranges; results may be noisier." >&2
fi
base="$base_old"

echo "base:     $base"
echo "old tip:  $old_tip"
echo "new tip:  $new_tip"
echo "---"

out="$(git range-diff --no-color "$base..$old_tip" "$base..$new_tip")"

echo "$out" | grep -E "^ *[0-9]+: +[0-9a-f]+ < " | sed -E 's/^/UNMATCHED       /' || true
echo "$out" | grep -E "^ *[0-9]+: +[0-9a-f]+ ! +[0-9]+: +[0-9a-f]+ / " > /dev/null 2>&1 || true
echo "$out" | grep -E "^ *[0-9]+: +[0-9a-f]+ ! " | sed -E 's/^/CORRELATED-DIFF /' || true

echo "---"
echo "UNMATCHED rows have no match at all in the new range — investigate first."
echo "CORRELATED-DIFF rows matched by message/context but their patch content"
echo "differs — read the diff-of-diff (rerun without piping to grep) for large"
echo "deletions before ruling them safe."
echo "Either verdict must be confirmed by grepping current src//tests/ for the"
echo "commit's actual symbols — a SHA-only verdict produces false positives"
echo "(see the header comment in this script)."
