#!/usr/bin/env bash
#
# Claude Code PreToolUse hook — issue #286.
#
# Two AI orchestrations collided on issue #257 because neither ran the
# MUTATING `worker-agent-deck issue claim` before writing to it (CLAUDE.md
# rule 14's original incident). `issue claim` fixes that for an orchestrator
# that remembers to run it — this hook makes running it unnecessary to
# remember: it shells out to the READ-ONLY `worker-agent-deck issue
# claim-check` before letting a `gh issue comment`/`close`/`edit` or a
# CLOSING `gh pr merge` land, and blocks the tool call outright if
# claim-check says the calling identity is not clear to act.
#
# Deliberately wired into the TRACKED .claude/settings.json, not
# settings.local.json: issue #286's own text called this "machine-local",
# but a tracked hook protects every future orchestration that clones this
# repo, not just one machine — which is the actual point of #286 (mechanical
# enforcement that doesn't depend on anyone remembering). See the PR body
# for the fuller note; JSON has no comment syntax to carry it inline in
# settings.json itself.
#
# Usage:
#   check-issue-claim.sh              run as a PreToolUse hook (reads stdin)
#   check-issue-claim.sh --self-test  prove the check can actually block AND
#                                     actually allow, with no real network
#                                     calls (mirrors scripts/check-symlinks.sh
#                                     --self-test's convention: a fabricated
#                                     scenario, pass/fail printed clearly, so
#                                     a green self-test is never the vacuous
#                                     kind).
#
# stdin contract (Claude Code PreToolUse, docs.claude.com/en/docs/claude-code
# /hooks as of 2026-08): a JSON object with at least `tool_name`,
# `tool_input.command` (for the Bash tool) and `cwd`. To block, print JSON on
# stdout — `{"hookSpecificOutput":{"hookEventName":"PreToolUse",
# "permissionDecision":"deny","permissionDecisionReason":"..."}}` — and exit
# 0; the JSON decision is honored regardless of exit code, which is why this
# script always exits 0 itself and uses stdout JSON as the sole signal.
# Allowing needs no output at all.

set -euo pipefail

usage() {
    cat <<'EOF'
Claude Code PreToolUse hook: block gh issue/PR closes not clear to act on.

  check-issue-claim.sh              run as a PreToolUse hook (reads stdin)
  check-issue-claim.sh --self-test  prove the check can block AND allow

See the comment at the top of this script, and GitHub issue #286.
EOF
    exit "${1:-0}"
}

# Deliberately resolved at hook-run time, not hardcoded, so a stubbed PATH in
# --self-test transparently substitutes a fake binary — and so a real run
# picks up whichever binary is actually first on PATH, the same resolution
# the shell itself would use.
CLAIM_CHECK_BIN="${CLAIM_CHECK_BIN:-worker-agent-deck}"

deny_json() {
    local reason="$1"
    jq -n --arg reason "$reason" \
        '{hookSpecificOutput: {hookEventName: "PreToolUse", permissionDecision: "deny", permissionDecisionReason: $reason}}'
}

# owner/name from a GitHub remote URL (HTTPS, git@ SSH, or ssh:// SSH) —
# mirrors src/worktree_reclaim.rs's parse_github_owner_repo exactly: same
# four prefixes, same .git-suffix strip, same "no more than two segments"
# rule. Empty output (not an error) means "could not parse" — callers must
# check for that themselves, `set -e` will not catch it.
parse_github_owner_repo() {
    local url="$1" rest
    for prefix in "git@github.com:" "ssh://git@github.com/" "https://github.com/" "http://github.com/"; do
        if [[ "$url" == "$prefix"* ]]; then
            rest="${url#"$prefix"}"
            rest="${rest%.git}"
            if [[ "$rest" =~ ^([^/]+)/([^/]+)$ ]]; then
                printf '%s/%s\n' "${BASH_REMATCH[1]}" "${BASH_REMATCH[2]}"
            fi
            return
        fi
    done
}

# owner/name derived from $1's origin remote — the fallback when the gh
# command itself carries no --repo/-R. Empty output means "could not derive".
derive_repo_slug() {
    local dir="$1" url
    url="$(git -C "$dir" remote get-url origin 2>/dev/null)" || return 0
    parse_github_owner_repo "$url"
}

# --repo/-R's value out of a gh command line, if present. Empty means absent.
extract_repo_flag() {
    local command="$1"
    if [[ "$command" =~ (--repo|-R)[[:space:]=]+([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+) ]]; then
        printf '%s\n' "${BASH_REMATCH[2]}"
    fi
}

# The issue/PR number out of `gh issue comment|close|edit <n> ...` or
# `gh pr merge <n> ...` — the first bare positional integer after the
# subcommand word, or an explicit --issue <n> if the command carries one
# (gh's own issue subcommands take the number positionally; --issue is
# supported defensively per the task's own wording). Empty means "could not
# extract" — callers must treat that as "allow" (nothing to check), never as
# a match on issue/PR 0.
extract_number() {
    local command="$1" subcommand_word="$2" last
    if [[ "$command" =~ --issue[[:space:]=]+([0-9]+) ]]; then
        printf '%s\n' "${BASH_REMATCH[1]}"
        return
    fi
    # $subcommand_word may itself contain a capturing group (the
    # "(comment|close|edit)" alternation) — bash's ERE has no non-capturing
    # group syntax, so the number is not reliably group 1. It IS reliably
    # the LAST group, since "([0-9]+)" is always the rightmost parenthesis
    # in the pattern regardless of how many groups $subcommand_word adds.
    if [[ "$command" =~ $subcommand_word[[:space:]]+([0-9]+) ]]; then
        last=$((${#BASH_REMATCH[@]} - 1))
        printf '%s\n' "${BASH_REMATCH[$last]}"
    fi
}

# Run claim-check for issue $2 of repo $1 (cwd $3). Echoes nothing; sets
# the global CLAIM_CHECK_REASON on refusal. Returns 0 = clear to proceed,
# 1 = refused.
run_claim_check() {
    local repo="$1" issue="$2" cwd="$3" out status
    set +e
    out="$(cd "$cwd" && "$CLAIM_CHECK_BIN" issue claim-check "$issue" --repo "$repo" 2>&1)"
    status=$?
    set -e
    if [ "$status" -ne 0 ]; then
        CLAIM_CHECK_REASON="$out"
        return 1
    fi
    return 0
}

# Every issue number the closing-keyword regex (CLAUDE.md rule 8) finds in
# $1 (typically a PR's body plus its commit messageHeadline/messageBody
# lines, newline-joined). Dedup'd, one per output line. Empty means none.
extract_closing_issue_numbers() {
    local text="$1"
    printf '%s\n' "$text" \
        | grep -inE '(clos|fix|resolv)[a-z]*[[:space:]]+#[0-9]+' \
        | grep -oE '#[0-9]+' \
        | tr -d '#' \
        | sort -un || true
}

main_hook() {
    local input tool_name command cwd
    input="$(cat)"
    tool_name="$(jq -r '.tool_name // empty' <<<"$input")"

    # Not Bash at all: nothing this hook cares about can appear here. Allow
    # fast, no-op — do not slow down or interfere with unrelated tool calls.
    if [ "$tool_name" != "Bash" ]; then
        exit 0
    fi

    command="$(jq -r '.tool_input.command // empty' <<<"$input")"
    cwd="$(jq -r '.cwd // empty' <<<"$input")"
    [ -n "$cwd" ] || cwd="$PWD"

    # Fast, unconditional allow for anything that isn't one of the four
    # commands this hook exists to gate.
    if ! [[ "$command" =~ gh[[:space:]]+issue[[:space:]]+(comment|close|edit) ]] \
        && ! [[ "$command" =~ gh[[:space:]]+pr[[:space:]]+merge ]]; then
        exit 0
    fi

    local repo issue
    repo="$(extract_repo_flag "$command")"
    [ -n "$repo" ] || repo="$(derive_repo_slug "$cwd")"
    if [ -z "$repo" ]; then
        # Cannot even name the repo — nothing to check against. Fail open
        # rather than block on an ambiguity this hook cannot resolve; the
        # underlying `worker-agent-deck issue claim`/`claim-check` commands
        # would refuse just as loudly if run by hand with no derivable repo.
        exit 0
    fi

    if [[ "$command" =~ gh[[:space:]]+pr[[:space:]]+merge ]]; then
        issue="$(extract_number "$command" "merge")"
        if [ -z "$issue" ]; then
            exit 0
        fi
        local body commits text numbers
        text="$(cd "$cwd" && gh pr view "$issue" --repo "$repo" --json body,commits \
            --jq '.body, (.commits[].messageHeadline), (.commits[].messageBody)' 2>/dev/null)" || {
            # Could not even look up the PR (bad number, no gh auth in this
            # environment, network hiccup) — nothing to gate on; allow. A
            # real merge attempt will hit the same `gh` failure itself.
            exit 0
        }
        numbers="$(extract_closing_issue_numbers "$text")"
        if [ -z "$numbers" ]; then
            # The merge closes nothing by keyword — nothing to check.
            exit 0
        fi
        while IFS= read -r n; do
            [ -n "$n" ] || continue
            if ! run_claim_check "$repo" "$n" "$cwd"; then
                deny_json "blocked by check-issue-claim.sh (issue #286): merging this PR would close issue #$n, and \`issue claim-check\` refused — $CLAIM_CHECK_REASON"
                exit 0
            fi
        done <<<"$numbers"
        exit 0
    fi

    # gh issue comment|close|edit
    issue="$(extract_number "$command" "(comment|close|edit)")"
    if [ -z "$issue" ]; then
        exit 0
    fi
    if ! run_claim_check "$repo" "$issue" "$cwd"; then
        deny_json "blocked by check-issue-claim.sh (issue #286): \`issue claim-check\` refused — $CLAIM_CHECK_REASON"
    fi
    exit 0
}

self_test() {
    local tmp fake_bin fail=0

    tmp="$(mktemp -d)"
    # shellcheck disable=SC2064  # expand $tmp now, not at trap time
    trap "rm -rf '$tmp'" EXIT
    fake_bin="$tmp/bin"
    mkdir -p "$fake_bin"
    git init -q "$tmp/repo"
    git -C "$tmp/repo" remote add origin "https://github.com/acme/widgets.git"

    # --- Scenario 1: claim-check refuses (held by someone else) -> BLOCK ---
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
echo "issue claim-check: issue #999 of acme/widgets is held by \`orch-a\` — held by another agent" >&2
exit 1
EOF
    chmod +x "$fake_bin/worker-agent-deck"

    local input_blocked out
    input_blocked=$(jq -n --arg cwd "$tmp/repo" '{
        tool_name: "Bash",
        tool_input: {command: "gh issue comment 999 --body \"working on this\""},
        cwd: $cwd
    }')
    out="$(PATH="$fake_bin:$PATH" CLAIM_CHECK_BIN=worker-agent-deck bash "$0" <<<"$input_blocked")"
    local decision reason
    decision="$(jq -r '.hookSpecificOutput.permissionDecision // empty' <<<"$out" 2>/dev/null)"
    reason="$(jq -r '.hookSpecificOutput.permissionDecisionReason // empty' <<<"$out" 2>/dev/null)"
    if [ "$decision" != "deny" ] || [[ "$reason" != *"held by"* ]]; then
        echo "self-test FAILED: expected a deny decision naming the holder; got:" >&2
        printf '%s\n' "$out" >&2
        fail=1
    else
        echo "self-test ok: a claim-check refusal blocks the tool call with the refusal reason surfaced"
    fi

    # --- Scenario 2: claim-check is clear -> ALLOW (no output) ---
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
echo "ok to proceed on issue #999 of acme/widgets as \`human:dana@host\`"
exit 0
EOF
    chmod +x "$fake_bin/worker-agent-deck"

    out="$(PATH="$fake_bin:$PATH" CLAIM_CHECK_BIN=worker-agent-deck bash "$0" <<<"$input_blocked")"
    if [ -n "$out" ]; then
        echo "self-test FAILED: expected no output (allow) when claim-check is clear; got:" >&2
        printf '%s\n' "$out" >&2
        fail=1
    else
        echo "self-test ok: a clear claim-check allows the tool call (no output)"
    fi

    # --- Scenario 3: unrelated Bash command -> ALLOW, and never even calls claim-check ---
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
echo "self-test FAILED: worker-agent-deck should never have been invoked for an unrelated command" >&2
exit 1
EOF
    chmod +x "$fake_bin/worker-agent-deck"

    local input_unrelated
    input_unrelated=$(jq -n --arg cwd "$tmp/repo" '{
        tool_name: "Bash",
        tool_input: {command: "cargo fmt --check"},
        cwd: $cwd
    }')
    out="$(PATH="$fake_bin:$PATH" CLAIM_CHECK_BIN=worker-agent-deck bash "$0" <<<"$input_unrelated")"
    if [ -n "$out" ]; then
        echo "self-test FAILED: expected no output for an unrelated command; got:" >&2
        printf '%s\n' "$out" >&2
        fail=1
    else
        echo "self-test ok: an unrelated Bash command is allowed without ever shelling out to claim-check"
    fi

    # --- Scenario 4: `gh pr merge` closing an issue by keyword -> BLOCK,
    # driven by the extracted closing-keyword issue number, not the PR
    # number itself. Stubs `gh` too, since this path calls `gh pr view`. ---
    cat >"$fake_bin/gh" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
    echo "fixes #777"
    exit 0
fi
echo "self-test FAILED: unexpected gh invocation: $*" >&2
exit 1
EOF
    chmod +x "$fake_bin/gh"
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
# Assert the issue number claim-check receives is the one the CLOSING
# KEYWORD named (777), never the merged PR's own number (573).
if [ "$3" != "777" ]; then
    echo "self-test FAILED: claim-check called with issue $3, expected 777" >&2
    exit 1
fi
echo "issue claim-check: issue #777 of acme/widgets is held by \`orch-a\` — held by another agent" >&2
exit 1
EOF
    chmod +x "$fake_bin/worker-agent-deck"

    local input_merge
    input_merge=$(jq -n --arg cwd "$tmp/repo" '{
        tool_name: "Bash",
        tool_input: {command: "gh pr merge 573 --squash"},
        cwd: $cwd
    }')
    out="$(PATH="$fake_bin:$PATH" CLAIM_CHECK_BIN=worker-agent-deck bash "$0" <<<"$input_merge")"
    decision="$(jq -r '.hookSpecificOutput.permissionDecision // empty' <<<"$out" 2>/dev/null)"
    reason="$(jq -r '.hookSpecificOutput.permissionDecisionReason // empty' <<<"$out" 2>/dev/null)"
    if [ "$decision" != "deny" ] || [[ "$reason" != *"#777"* ]]; then
        echo "self-test FAILED: expected a deny decision naming issue #777 (from the closing keyword, not PR #573); got:" >&2
        printf '%s\n' "$out" >&2
        fail=1
    else
        echo "self-test ok: \`gh pr merge\` closing an issue by keyword is checked against THAT issue and blocked"
    fi

    if [ "$fail" -ne 0 ]; then
        exit 1
    fi
    echo "self-test ok: check-issue-claim.sh blocks a refused claim-check and allows a clear one"
}

case "${1:-}" in
--self-test)
    self_test
    ;;
-h | --help)
    usage
    ;;
"")
    main_hook
    ;;
*)
    echo "unknown option: $1" >&2
    usage 1 >&2
    ;;
esac
