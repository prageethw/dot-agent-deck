#!/usr/bin/env bash
#
# Phase 0 of /verify-pr: everything learnable about a PR WITHOUT running its
# code. Runs from the main checkout, creates nothing, touches no worktree.
#
# Usage: scan.sh <pr-number|pr-url>
#
# Emits KEY=value metadata plus the changed files classified into buckets. The
# EXEC_ON_CLONE and CI_SECRETS buckets are why this script runs before any
# other phase: those paths execute code on the REVIEWER's machine (agent hooks,
# build.rs, cargo config, shell scripts) or inside CI with repository
# credentials, so the diff has to be read before a worktree exists and before
# any cargo command is invoked.

set -uo pipefail

if [ $# -lt 1 ]; then
  echo "ERROR=true"
  echo "MESSAGE=Usage: scan.sh <pr-number|pr-url>"
  exit 0
fi

# Accept "123", "#123", and a full PR URL.
pr="${1#\#}"
pr="${pr##*/}"

if ! [[ "$pr" =~ ^[0-9]+$ ]]; then
  echo "ERROR=true"
  echo "MESSAGE=Could not parse a PR number from '$1'"
  exit 0
fi

if ! command -v gh >/dev/null 2>&1; then
  echo "ERROR=true"
  echo "MESSAGE=GitHub CLI (gh) is required: https://cli.github.com/"
  exit 0
fi

# `authorAssociation` is NOT a `gh pr view --json` field — it exists only on the
# REST endpoint, queried separately below. Fork detection uses
# `isCrossRepository` rather than comparing repo names, which is what GitHub
# itself keys on.
fields='number,title,url,state,isDraft,author,isCrossRepository,maintainerCanModify,baseRefName,headRefName,headRefOid,headRepository,headRepositoryOwner,mergeable,mergeStateStatus,additions,deletions,changedFiles,labels,createdAt,updatedAt'

if ! meta=$(gh pr view "$pr" --json "$fields" --jq '
  "PR_NUMBER=\(.number)",
  "PR_TITLE=\(.title)",
  "PR_URL=\(.url)",
  "PR_STATE=\(.state)",
  "PR_DRAFT=\(.isDraft)",
  "PR_AUTHOR=\(.author.login)",
  "PR_IS_FORK=\(.isCrossRepository)",
  "PR_MAINTAINER_CAN_MODIFY=\(.maintainerCanModify)",
  "PR_HEAD_REPO=\(.headRepositoryOwner.login // "unknown")/\(.headRepository.name // "unknown")",
  "PR_BASE_BRANCH=\(.baseRefName)",
  "PR_HEAD_BRANCH=\(.headRefName)",
  "PR_HEAD_SHA=\(.headRefOid)",
  "PR_MERGEABLE=\(.mergeable)",
  "PR_MERGE_STATE=\(.mergeStateStatus)",
  "PR_ADDITIONS=\(.additions)",
  "PR_DELETIONS=\(.deletions)",
  "PR_CHANGED_FILES=\(.changedFiles)",
  "PR_LABELS=\([.labels[].name] | join(","))",
  "PR_CREATED=\(.createdAt)",
  "PR_UPDATED=\(.updatedAt)"
' 2>&1); then
  echo "ERROR=true"
  echo "MESSAGE=gh pr view $pr failed: ${meta}"
  exit 0
fi

echo "$meta"

# REST-only field. Drives the "read the whole diff before running anything"
# decision for authors outside the org.
association=$(gh api "repos/{owner}/{repo}/pulls/${pr}" --jq '.author_association' 2>/dev/null || echo UNKNOWN)
echo "PR_AUTHOR_ASSOCIATION=${association}"
case "$association" in
  MEMBER | OWNER | COLLABORATOR) echo "TRUSTED_AUTHOR=true" ;;
  *) echo "TRUSTED_AUTHOR=false" ;;
esac

# --- Classify the changed files -------------------------------------------

if ! files=$(gh pr diff "$pr" --name-only 2>&1); then
  echo "ERROR=true"
  echo "MESSAGE=gh pr diff $pr --name-only failed: ${files}"
  exit 0
fi

# First match wins, so the paths that execute code are tested first.
classify() {
  case "$1" in
    .claude/* | .cursor/* | AGENTS.md | CLAUDE.md) echo EXEC_ON_CLONE ;;
    build.rs | */build.rs) echo EXEC_ON_CLONE ;;
    .cargo/* | rust-toolchain* | .envrc) echo EXEC_ON_CLONE ;;
    xtask/* | scripts/* | *.sh) echo EXEC_ON_CLONE ;;
    devbox.json | devbox.lock | Taskfile* | Makefile) echo EXEC_ON_CLONE ;;
    .github/*) echo CI_SECRETS ;;
    Cargo.toml | Cargo.lock | */Cargo.toml) echo DEPS ;;
    src/daemon*.rs | src/hook*.rs | src/connect.rs | src/remote.rs) echo PROTOCOL ;;
    src/dispatch.rs | src/issue_dispatch*.rs | src/scheduler.rs) echo PROTOCOL ;;
    src/orchestrator_ext.rs | src/mode_manager.rs | src/spawn.rs) echo PROTOCOL ;;
    src/agent_pty.rs | src/agent_registry.rs | src/wrap.rs) echo PROTOCOL ;;
    src/*hooks_manage.rs | src/schedule_cli.rs | src/event.rs) echo PROTOCOL ;;
    src/ui.rs | src/pane*.rs | src/tab*.rs | src/terminal_widget.rs) echo UI ;;
    src/embedded_pane.rs | src/keybindings.rs | src/palette.rs) echo UI ;;
    src/ascii_art.rs | src/features.rs | src/hyperlink.rs) echo UI ;;
    tests/snapshots/*) echo UI_SNAPSHOT ;;
    tests/CATALOG.md) echo CATALOG ;;
    tests/*) echo TESTS ;;
    changelog.d/*) echo CHANGELOG ;;
    prds/*) echo PRD ;;
    docs/develop/*) echo DOCS_DEVELOP ;;
    docs/* | site/* | *.md) echo DOCS ;;
    src/*) echo SRC ;;
    *) echo OTHER ;;
  esac
}

declare -A bucket_files=()
while IFS= read -r f; do
  [ -z "$f" ] && continue
  b=$(classify "$f")
  bucket_files["$b"]+="  ${f}"$'\n'
done <<<"$files"

# A hard gate the reviewing agent must honour: these paths run outside the test
# command, so their diff has to be read before anything is built or executed.
read_first=""
for b in EXEC_ON_CLONE CI_SECRETS; do
  [ -n "${bucket_files[$b]:-}" ] && read_first+="${b} "
done
echo "READ_DIFF_BEFORE_RUNNING=${read_first:-none}"

# Rule 12 (cross-version contract) and rule 4 (TUI test ladder) triggers.
[ -n "${bucket_files[PROTOCOL]:-}" ] && echo "RULE_12_TRIGGERED=true" || echo "RULE_12_TRIGGERED=false"
[ -n "${bucket_files[UI]:-}${bucket_files[UI_SNAPSHOT]:-}" ] && echo "RULE_4_TRIGGERED=true" || echo "RULE_4_TRIGGERED=false"
[ -n "${bucket_files[CHANGELOG]:-}" ] && echo "CHANGELOG_FRAGMENT_PRESENT=true" || echo "CHANGELOG_FRAGMENT_PRESENT=false"
[ -n "${bucket_files[TESTS]:-}" ] && echo "TESTS_TOUCHED=true" || echo "TESTS_TOUCHED=false"
[ -n "${bucket_files[DEPS]:-}" ] && echo "DEPS_TOUCHED=true" || echo "DEPS_TOUCHED=false"

for b in EXEC_ON_CLONE CI_SECRETS DEPS PROTOCOL UI UI_SNAPSHOT CATALOG TESTS \
  CHANGELOG PRD DOCS_DEVELOP DOCS SRC OTHER; do
  if [ -n "${bucket_files[$b]:-}" ]; then
    echo "--- ${b} ---"
    printf '%s' "${bucket_files[$b]}"
  fi
done

# --- Signal that already exists on the PR ---------------------------------

echo "--- CI CHECKS (gh pr checks) ---"
# Exits non-zero when a check is failing or pending, which is information, not
# an error for this script.
gh pr checks "$pr" 2>&1 || true

# Workflow runs held for approval. GitHub withholds Actions runs on a fork PR
# from a first-time contributor until a maintainer approves them, so a PR can
# look "checked" (only `label` / a review bot ran) while every real CI job —
# Windows, macOS, `cargo audit` — never executed. Measured on #334: CI and Docs
# sat at `action_required` on every head commit for two days and nobody noticed,
# which made a local Linux-only run the sole verification. Phase 1b decides
# whether approving them is safe.
head_sha=$(printf '%s\n' "$meta" | sed -n 's/^PR_HEAD_SHA=//p')
awaiting=$(gh api "repos/{owner}/{repo}/actions/runs?head_sha=${head_sha}" \
  --jq '[.workflow_runs[] | select(.conclusion == "action_required" or .status == "action_required")] | length' 2>/dev/null || echo unknown)
echo "WORKFLOWS_AWAITING_APPROVAL=${awaiting:-unknown}"
if [ "${awaiting:-0}" != "0" ] && [ "${awaiting:-unknown}" != "unknown" ]; then
  echo "--- RUNS AWAITING APPROVAL (id / name) ---"
  gh api "repos/{owner}/{repo}/actions/runs?head_sha=${head_sha}" \
    --jq '.workflow_runs[] | select(.conclusion == "action_required" or .status == "action_required") | "  \(.id)\t\(.name)"' 2>/dev/null || true
  echo "APPROVE_WITH=gh api --method POST repos/{owner}/{repo}/actions/runs/<run_id>/approve"
fi

# Rule 8: a green check board is not evidence a review actually happened —
# read the surface that carries the result. Fetch inline PR review comments
# explicitly in case any reviewer left findings there.
inline=$(gh api "repos/{owner}/{repo}/pulls/${pr}/comments" --paginate --jq '.[].id' 2>/dev/null | wc -l | tr -d ' ')
echo "INLINE_REVIEW_COMMENTS=${inline:-unknown}"
echo "READ_INLINE_WITH=gh api repos/{owner}/{repo}/pulls/${pr}/comments --paginate"

echo "SUCCESS=true"
