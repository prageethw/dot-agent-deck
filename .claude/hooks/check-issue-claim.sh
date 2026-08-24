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
# CLOSING `gh pr merge` land.
#
# SCOPE (PR #573 fix round, per reviewer/auditor's explicit framing
# recommendation): this is an ACCIDENT-PREVENTER for cooperating
# orchestrations, never a security enforcement boundary. It parses the
# `gh` command line client-side; `gh api` REST calls that perform the same
# writes (`gh api repos/.../issues/.../comments`, `gh api --method PATCH
# repos/.../issues/<n> -f state=closed`, etc.) are entirely outside this
# gate and always will be — widening to `gh api` would drag in every READ
# too, and matching only the write shapes is its own regex-fragility
# problem. A shell variable holding the issue number, or a quoting form
# this tokenizer does not anticipate, can also get past it. That is an
# acceptable, honest scope for #286's actual threat (an orchestration that
# FORGOT to check), not an adversary. Human sessions are gated too — with
# `DOT_AGENT_DECK_PANE_ID` unset, identity resolves to `human:<login>@<host>`
# (see `resolve_caller_identity`), so a human's own `gh issue close` on an
# orchestration-held issue is blocked exactly like an agent's would be.
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
# "permissionDecision":"deny"|"ask","permissionDecisionReason":"..."}}` —
# and exit 0. The current docs (re-verified during the PR #573 fix round,
# not trusted from memory): exit 2 blocks UNCONDITIONALLY and cannot be
# overridden by JSON at all; any OTHER non-zero exit is a NON-BLOCKING
# error and the JSON is ignored (the action proceeds as if nothing ran).
# So exit 0 is REQUIRED for the stdout JSON decision to be honored — it is
# not merely "honored regardless of exit code" as an earlier draft of this
# comment claimed — which is why this script always exits 0 itself and uses
# stdout JSON as the sole signal. `reason` is required for `deny`, optional
# for `ask`, and ignored for `allow` — allowing needs no output at all, so
# an operational-failure note that should stay visible without blocking the
# tool call is written to STDERR instead (grep this script for
# `could not determine` / `could not tokenize` to find every such site).

set -euo pipefail

usage() {
    cat <<'EOF'
Claude Code PreToolUse hook: block gh issue/PR closes not clear to act on.

  check-issue-claim.sh              run as a PreToolUse hook (reads stdin)
  check-issue-claim.sh --self-test  prove the check can block, ask, AND allow

See the comment at the top of this script, and GitHub issue #286.
EOF
    exit "${1:-0}"
}

# Deliberately a fixed literal, NOT `${CLAIM_CHECK_BIN:-worker-agent-deck}`
# read from the ambient environment (auditor A9): on a REAL hook invocation
# an env-controlled override is a silent off-switch — anyone able to set
# `CLAIM_CHECK_BIN=true` on the Claude Code process disables the whole
# check while the hook still appears installed and green. The override
# still exists for `--self-test`, gated behind the private
# `_CLAIM_CHECK_SELF_TEST` flag `self_test()` alone sets when invoking
# itself as a child process — nothing else in this script, and nothing
# outside it, can set that flag, so a real run always resolves the literal
# name via ordinary PATH lookup, exactly like a shell typing the command by
# hand would (and exactly what a stubbed PATH in `--self-test` transparently
# substitutes).
if [ "${_CLAIM_CHECK_SELF_TEST:-}" = "1" ]; then
    CLAIM_CHECK_BIN="${CLAIM_CHECK_BIN:-worker-agent-deck}"
else
    CLAIM_CHECK_BIN="worker-agent-deck"
fi

deny_json() {
    local reason="$1"
    jq -n --arg reason "$reason" \
        '{hookSpecificOutput: {hookEventName: "PreToolUse", permissionDecision: "deny", permissionDecisionReason: $reason}}'
}

# permissionDecision "ask" escalates to the user for a manual permission
# prompt (re-verified against the current docs during the PR #573 fix
# round — this is real, not assumed). Used for Priority 1's tier 2:
# genuinely ambiguous rather than confidently refused (CLAUDE.md rule 14's
# own guidance is to escalate to a human rather than silently adopt).
ask_json() {
    local reason="$1"
    jq -n --arg reason "$reason" \
        '{hookSpecificOutput: {hookEventName: "PreToolUse", permissionDecision: "ask", permissionDecisionReason: $reason}}'
}

# Truncates and delimits an untrusted claim-check reason before it reaches
# `permissionDecisionReason` (auditor A7): `CLAIM_CHECK_REASON` is
# claim-check's combined stdout+stderr, which can embed a holder identity
# PARSED out of a GitHub comment this deck does not author. That string now
# reaches the model's own context via `permissionDecisionReason` — a
# channel that did not exist before this hook (previously it only ever
# reached a human's terminal) — so bound its length and mark it explicitly
# as untrusted rather than passing it through verbatim.
sanitize_reason() {
    local raw="$1"
    printf 'untrusted issue-comment content follows: %s' "${raw:0:256}"
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

# Tokenizes stdin (the raw Bash tool command) into real shell words via
# Python's shlex — POSIX-ish word-splitting and quote-removal with NO
# evaluation of substitutions. This deliberately does NOT use eval/bash -c
# to parse: that would EXECUTE side effects (e.g. `$(...)` command
# substitution) merely by inspecting a command that might end up denied.
# python3 is a safe bet for a hook that only ever runs inside an
# interactive Claude Code session on a dev machine — never in CI (both
# reports independently confirmed: "GitHub Actions never runs Claude Code
# hooks").
#
# This is what PR #573's fix round actually rebuilds (reviewer B3-B5/M1,
# auditor A1-A2): the previous implementation regexed the raw command
# STRING, so a value inside a quoted --body/--comment was scanned into as
# if it were a flag, flags-before-positional defeated a fixed-offset
# "$word (\d+)" match, and a chained command's leftmost match won
# regardless of which segment it actually belonged to. Tokenizing first
# makes each of those a non-issue by construction: a quoted value is ONE
# token, never scanned into; the positional number is found by walking
# tokens and skipping known value-taking flags, not by a fixed offset from
# the subcommand word; and the command is split into independent segments
# on &&/;/|/|& BEFORE any of this runs, so a match in one segment can never
# answer for a different segment's write.
#
# Emits one line per segment that matches a gated verb, fields separated by
# ASCII Unit Separator (0x1F) rather than a tab or space: bash's `read`
# collapses RUNS of any IFS character that is also in the default
# whitespace class (space/tab/newline) and drops empty fields at the
# boundary, even when IFS is explicitly set to nothing but that one
# character — so a tab-separated empty <repo-or-empty> field would
# silently shift every field after it by one. 0x1F is not in that class, so
# `IFS=$'\x1f' read -r ...` on the bash side splits exactly on it with
# empty fields preserved:
#   {issue|merge}<0x1F><repo-or-empty><0x1F><number-or-empty><0x1F>{OK|NONE|AMBIGUOUS}
# OK means <number> is a clean, unambiguous integer. NONE means no
# positional candidate was found at all — the caller resolves `gh pr
# merge`'s current-branch PR before giving up; there is no equivalent
# fallback for the issue verbs. AMBIGUOUS means a candidate token WAS found
# but is not a clean integer (a shell variable like "$N", or any other
# non-numeric positional). Per Priority 2's key behavioral change, a NONE
# (for the issue verbs) or AMBIGUOUS segment must NEVER be silently
# allowed through — the caller asks/denies instead.
#
# Exits 1 with nothing on stdout if the command cannot be tokenized at all
# (unbalanced quoting, e.g. a heredoc body) — the caller treats that
# exactly like python3 being absent: could-not-determine, not "no match".
CLAIM_CHECK_PY=$(cat <<'PYEOF'
import re, shlex, sys

SEPARATORS = {"&&", ";", "|", "|&", ";;"}
ISSUE_VERBS = {"comment", "close", "edit"}
VALUE_FLAGS = {
    "issue": {
        "--repo", "-R", "--comment", "--body", "--body-file", "--reason",
        "--title", "--add-assignee", "--remove-assignee", "--add-label",
        "--remove-label", "--add-project", "--remove-project", "--milestone",
    },
    "merge": {
        "--repo", "-R", "--subject", "--body", "--body-file",
        "--match-head-commit",
    },
}
REPO_RE = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
ENV_ASSIGN_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*=")


def repo_from_value(value):
    return value if REPO_RE.match(value) else None


def extract_repo_and_number(tokens, kind):
    value_flags = VALUE_FLAGS[kind]
    repo = None
    number = None
    status = "NONE"
    i = 0
    n = len(tokens)
    while i < n:
        t = tokens[i]
        if t in ("--repo", "-R"):
            if i + 1 < n:
                r = repo_from_value(tokens[i + 1])
                if r:
                    repo = r
            i += 2
            continue
        if t.startswith("--repo=") or t.startswith("-R="):
            r = repo_from_value(t.split("=", 1)[1])
            if r:
                repo = r
            i += 1
            continue
        if t.startswith("-R") and len(t) > 2 and not t.startswith("--"):
            r = repo_from_value(t[2:])
            if r:
                repo = r
            i += 1
            continue
        if t.startswith("-"):
            if "=" in t:
                i += 1
                continue
            i += 2 if t in value_flags else 1
            continue
        # First non-flag token = the positional candidate. Keep scanning
        # (do not break) so a --repo appearing AFTER the positional is
        # still picked up, but never let a later positional override the
        # first one found.
        if number is None:
            number = t
            status = "OK" if re.fullmatch(r"[0-9]+", number) else "AMBIGUOUS"
        i += 1
    return repo, number, status


def split_segments(tokens):
    segments = []
    current = []
    for t in tokens:
        if t in SEPARATORS:
            segments.append(current)
            current = []
        else:
            current.append(t)
    segments.append(current)
    return segments


def strip_env_assignments(tokens):
    i = 0
    while i < len(tokens) and ENV_ASSIGN_RE.match(tokens[i]):
        i += 1
    return tokens[i:]


def classify(tokens):
    tokens = strip_env_assignments(tokens)
    if len(tokens) < 3 or tokens[0] != "gh":
        return None
    if tokens[1] == "issue" and tokens[2] in ISSUE_VERBS:
        return ("issue",) + extract_repo_and_number(tokens[3:], "issue")
    if tokens[1] == "pr" and tokens[2] == "merge":
        return ("merge",) + extract_repo_and_number(tokens[3:], "merge")
    return None


def main():
    raw = sys.stdin.read()
    try:
        tokens = shlex.split(raw)
    except ValueError as exc:
        print(
            "check-issue-claim.sh: could not tokenize command (unbalanced quoting): {}".format(exc),
            file=sys.stderr,
        )
        return 1
    for seg in split_segments(tokens):
        result = classify(seg)
        if result is None:
            continue
        kind, repo, number, status = result
        print("{}\x1f{}\x1f{}\x1f{}".format(kind, repo or "", number or "", status))
    return 0


sys.exit(main())
PYEOF
)

# Tokenizes $1 (the raw Bash tool command) via the embedded Python helper
# above and fills the global MATCHES array, one element per gated segment
# (0x1F-joined "kind<0x1F>repo<0x1F>number<0x1F>status" — see the Python
# helper's own doc for why not a tab). Returns 1 — MATCHES left
# empty — when python3 is unavailable or the command could not be
# tokenized at all; the caller MUST treat that as Priority 1's tier 3
# (could-not-determine -> allow, with a visible note), never as "nothing
# matched".
extract_gated_segments() {
    local command="$1" out status
    MATCHES=()
    if ! command -v python3 >/dev/null 2>&1; then
        return 1
    fi
    set +e
    out="$(printf '%s' "$command" | python3 -c "$CLAIM_CHECK_PY" 2>/dev/null)"
    status=$?
    set -e
    if [ "$status" -ne 0 ]; then
        return 1
    fi
    [ -n "$out" ] || return 0
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        MATCHES+=("$line")
    done <<<"$out"
    return 0
}

# Run `issue claim-check` for issue $2 of repo $1 (cwd $3). Sets the global
# CLAIM_CHECK_REASON to its combined stdout+stderr. Returns a TIER matching
# `ClaimCheckOutcome`'s own exit-code contract (src/main.rs's
# run_issue_claim_check_cli — do not change one side of this without the
# other):
#   0 = clear, safe to proceed
#   1 = confident lock violation (deny)
#   2 = ambiguous — identity unknown (ask, or deny if ask is unsupported)
#   3 = could not determine (operational failure — allow, but surface why)
# Any OTHER exit code (binary missing -> 127, a future CLI change, a crash)
# also collapses to tier 3 — an unexpected exit is an operational surprise,
# never a confident refusal. This is B1/A5's actual fix: the hook used to
# treat ANY non-zero exit as a refusal, which fails closed on exactly the
# cases fail-open was meant to cover (the binary missing entirely on a
# fresh clone, `gh` unauthenticated, or the caller being an agent-shaped
# pane in the root checkout — CLAUDE.md rule 17's normal orchestrator case).
run_claim_check() {
    local repo="$1" issue="$2" cwd="$3" out status
    set +e
    out="$(cd "$cwd" && "$CLAIM_CHECK_BIN" issue claim-check "$issue" --repo "$repo" 2>&1)"
    status=$?
    set -e
    CLAIM_CHECK_REASON="$out"
    case "$status" in
    0 | 1 | 2) return "$status" ;;
    *) return 3 ;;
    esac
}

# Every issue number the closing-keyword regex (CLAUDE.md rule 8) finds in
# $1 (typically a PR's title, body, and commit messageHeadline/messageBody
# lines, newline-joined). Widened past bare `#N` to also catch `GH-N`,
# `owner/repo#N`, and a full issue URL — GitHub's closing-keyword parser
# honors all four forms (auditor A3 / reviewer M2); the previous pattern,
# copied from CLAUDE.md rule 8's own hand-run audit commands, implemented
# only the first. Dedup'd, one per output line. Empty means none.
#
# Two passes, deliberately NOT collapsed into one keyword-bound extraction:
# pass 1 (`grep -iE` with the KEYWORD prefix) decides which LINES qualify
# at all — a line needs at least one closing keyword followed by a
# reference in any of the four forms; pass 2 (`grep -oE` on
# `$REF_PATTERN` ALONE, no keyword prefix) then pulls out EVERY
# reference-shaped token from each qualifying line, keyword-bound or not.
# This preserves the auditor A11 / reviewer M3 over-extraction property
# (a QUALIFYING line naming a second, unrelated number — "fixes #1, see
# also #999" — still yields BOTH 1 and 999) across all four forms, not
# just bare `#N`: collapsing to a single keyword-bound pass would have
# silently narrowed that property for the three new forms, which the task
# was explicit is NOT wanted (it over-BLOCKS, never under-blocks — the
# right direction for a hook whose whole purpose is not missing a closing
# reference). Each reference-shaped match always ENDS in the digits, so a
# third pass pulling the trailing run of digits off each one recovers the
# number. Do not "fix" the over-extraction into a tighter per-match
# result without re-reading auditor A11 first.
extract_closing_issue_numbers() {
    local text="$1"
    local ref_pattern='((GH-|#)[0-9]+|[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+#[0-9]+|https://github\.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+/issues/[0-9]+)'
    printf '%s\n' "$text" \
        | grep -iE "(clos|fix|resolv)[a-z]*[[:space:]]+${ref_pattern}" \
        | grep -oE "$ref_pattern" \
        | grep -oE '[0-9]+$' \
        | sort -un || true
}

# Runs claim-check for issue $2 of repo $1 (cwd $3) and reacts per its tier
# (see run_claim_check's doc): tier 0 returns (nothing to do — the caller's
# loop moves to the next match); tier 1 denies and exits; tier 2 asks (or
# would deny if a future hook contract ever dropped "ask" — re-verify
# against the docs before assuming it still applies) and exits; tier 3
# allows but leaves a visible note on stderr and returns. $4 is a note
# prefix for the deny/ask reason text ("merging this PR would close issue
# #N, and " for the merge path, empty for a direct gh issue write).
gate_or_allow() {
    local repo="$1" issue="$2" cwd="$3" note="$4" tier
    if run_claim_check "$repo" "$issue" "$cwd"; then
        tier=0
    else
        tier=$?
    fi
    case "$tier" in
    0)
        return 0
        ;;
    1)
        deny_json "blocked by check-issue-claim.sh (issue #286): ${note}\`issue claim-check\` refused — $(sanitize_reason "$CLAIM_CHECK_REASON")"
        exit 0
        ;;
    2)
        ask_json "check-issue-claim.sh (issue #286): ${note}\`issue claim-check\` could not confirm — issue #$issue of $repo is labelled in-progress but no claim comment names a holder (identity unknown). Confirm this is safe before proceeding. $(sanitize_reason "$CLAIM_CHECK_REASON")"
        exit 0
        ;;
    *)
        echo "check-issue-claim.sh: \`issue claim-check\` for issue #$issue of $repo could not determine an answer (operational failure — binary missing, gh auth/network issue, or the caller is not in a linked worktree) — allowing without a claim check: $CLAIM_CHECK_REASON" >&2
        return 0
        ;;
    esac
}

main_hook() {
    local input tool_name command cwd

    # This hook must be robust on a machine with only git/gh — jq missing
    # would otherwise error on EVERY Bash tool call, not just gated ones
    # (reviewer L3), since jq is used below just to parse the hook's own
    # stdin contract. Fail open, loudly, once, before any other jq call.
    if ! command -v jq >/dev/null 2>&1; then
        echo "check-issue-claim.sh: jq not found — allowing without a claim check" >&2
        exit 0
    fi

    input="$(cat)"
    tool_name="$(jq -r '.tool_name // empty' <<<"$input" 2>/dev/null)"

    # Not Bash at all: nothing this hook cares about can appear here. Allow
    # fast, no-op — do not slow down or interfere with unrelated tool calls.
    if [ "$tool_name" != "Bash" ]; then
        exit 0
    fi

    command="$(jq -r '.tool_input.command // empty' <<<"$input" 2>/dev/null)"
    cwd="$(jq -r '.cwd // empty' <<<"$input" 2>/dev/null)"
    [ -n "$cwd" ] || cwd="$PWD"
    [ -n "$command" ] || exit 0

    if ! extract_gated_segments "$command"; then
        # Could not tokenize (no python3, or unparseable quoting/heredoc) —
        # Priority 1 tier 3: could-not-determine. Allow, but say so
        # visibly on stderr so this is never a silent bypass.
        echo "check-issue-claim.sh: could not tokenize command for claim checking (python3 missing, or unparseable quoting) — allowing without a claim check: $command" >&2
        exit 0
    fi
    [ "${#MATCHES[@]}" -gt 0 ] || exit 0

    local m kind repo number ext_status
    for m in "${MATCHES[@]}"; do
        IFS=$'\x1f' read -r kind repo number ext_status <<<"$m"

        [ -n "$repo" ] || repo="$(derive_repo_slug "$cwd")"
        if [ -z "$repo" ]; then
            # Cannot even name the repo — nothing to check against. Fail
            # open rather than block on an ambiguity this hook cannot
            # resolve; the underlying `worker-agent-deck issue
            # claim`/`claim-check` commands would refuse just as loudly if
            # run by hand with no derivable repo.
            continue
        fi

        if [ "$kind" = "merge" ] && [ "$ext_status" = "NONE" ]; then
            # No PR number in the merge invocation itself — resolve the
            # current branch's open PR before giving up (same
            # repo-derivation logic as everything else here).
            local resolved
            resolved="$(cd "$cwd" && gh pr view --repo "$repo" --json number --jq '.number' 2>/dev/null)" || resolved=""
            if [ -z "$resolved" ]; then
                # Could not even look up the current branch's PR (no PR, no
                # gh auth, network hiccup) — nothing to gate on; allow. A
                # real merge attempt will hit the same `gh` failure itself.
                continue
            fi
            number="$resolved"
            ext_status="OK"
        fi

        if [ "$ext_status" != "OK" ]; then
            # Matches a gated verb, but the issue/PR number could not be
            # unambiguously determined from the command — Priority 2's key
            # behavioral change: never silently allow this through.
            local verb_desc
            if [ "$kind" = "merge" ]; then
                verb_desc="gh pr merge"
            else
                verb_desc="gh issue comment/close/edit"
            fi
            ask_json "check-issue-claim.sh (issue #286): this command matches a gated \`${verb_desc}\` form, but the issue/PR number could not be unambiguously determined from it — refusing to guess rather than risk checking the wrong one. Confirm this is safe, or re-run it with an explicit, literal issue/PR number. Command: $command"
            exit 0
        fi

        if [ "$kind" = "merge" ]; then
            local text numbers n
            text="$(cd "$cwd" && gh pr view "$number" --repo "$repo" --json title,body,commits \
                --jq '.title, .body, (.commits[].messageHeadline), (.commits[].messageBody)' 2>/dev/null)" || text=""
            if [ -z "$text" ]; then
                # Could not even look up the PR (bad number, no gh auth,
                # network hiccup) — nothing to gate on; allow. A real
                # merge attempt will hit the same `gh` failure itself.
                continue
            fi
            numbers="$(extract_closing_issue_numbers "$text")"
            [ -n "$numbers" ] || continue
            while IFS= read -r n; do
                [ -n "$n" ] || continue
                gate_or_allow "$repo" "$n" "$cwd" "merging this PR would close issue #$n, and "
            done <<<"$numbers"
        else
            gate_or_allow "$repo" "$number" "$cwd" ""
        fi
    done
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

    # --- Scenario 1: claim-check refuses (held by someone else) -> DENY ---
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
    out="$(PATH="$fake_bin:$PATH" bash "$0" <<<"$input_blocked")"
    local decision reason
    decision="$(jq -r '.hookSpecificOutput.permissionDecision // empty' <<<"$out" 2>/dev/null)"
    reason="$(jq -r '.hookSpecificOutput.permissionDecisionReason // empty' <<<"$out" 2>/dev/null)"
    if [ "$decision" != "deny" ] || [[ "$reason" != *"held by"* ]]; then
        echo "self-test FAILED: expected a deny decision naming the holder; got:" >&2
        printf '%s\n' "$out" >&2
        fail=1
    else
        echo "self-test ok: a claim-check refusal (tier 1) blocks the tool call with the refusal reason surfaced"
    fi

    # --- Scenario 2: claim-check is clear -> ALLOW (no output) ---
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
echo "ok to proceed on issue #999 of acme/widgets as \`human:dana@host\`"
exit 0
EOF
    chmod +x "$fake_bin/worker-agent-deck"

    out="$(PATH="$fake_bin:$PATH" bash "$0" <<<"$input_blocked")"
    if [ -n "$out" ]; then
        echo "self-test FAILED: expected no output (allow) when claim-check is clear; got:" >&2
        printf '%s\n' "$out" >&2
        fail=1
    else
        echo "self-test ok: a clear claim-check (tier 0) allows the tool call (no output)"
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
    out="$(PATH="$fake_bin:$PATH" bash "$0" <<<"$input_unrelated")"
    if [ -n "$out" ]; then
        echo "self-test FAILED: expected no output for an unrelated command; got:" >&2
        printf '%s\n' "$out" >&2
        fail=1
    else
        echo "self-test ok: an unrelated Bash command is allowed without ever shelling out to claim-check"
    fi

    # --- Scenario 4: `gh pr merge` closing an issue by keyword -> DENY,
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
    out="$(PATH="$fake_bin:$PATH" bash "$0" <<<"$input_merge")"
    decision="$(jq -r '.hookSpecificOutput.permissionDecision // empty' <<<"$out" 2>/dev/null)"
    reason="$(jq -r '.hookSpecificOutput.permissionDecisionReason // empty' <<<"$out" 2>/dev/null)"
    if [ "$decision" != "deny" ] || [[ "$reason" != *"#777"* ]]; then
        echo "self-test FAILED: expected a deny decision naming issue #777 (from the closing keyword, not PR #573); got:" >&2
        printf '%s\n' "$out" >&2
        fail=1
    else
        echo "self-test ok: \`gh pr merge\` closing an issue by keyword is checked against THAT issue and blocked"
    fi
    rm -f "$fake_bin/gh"

    # --- Scenario 5: claim-check binary cannot be found/run (operational
    # failure — e.g. a fresh clone with no fork build installed per
    # CLAUDE.md rule 21, reviewer B1/auditor A5) -> ALLOW, with a visible
    # stderr note, never a silent bypass and never a deny. Points
    # CLAIM_CHECK_BIN at a guaranteed-absent absolute path via the private
    # self-test-only override, rather than relying on PATH not already
    # having a real `worker-agent-deck` on it (this IS a fork dev machine
    # per rule 21, so that assumption would not hold). ---
    local stderr5 out5 err5
    stderr5="$tmp/stderr5"
    out5="$(PATH="$fake_bin:$PATH" _CLAIM_CHECK_SELF_TEST=1 CLAIM_CHECK_BIN="$fake_bin/does-not-exist" bash "$0" <<<"$input_blocked" 2>"$stderr5")"
    err5="$(cat "$stderr5")"
    if [ -n "$out5" ]; then
        echo "self-test FAILED: expected no blocking decision (allow) when claim-check cannot be found/run; got:" >&2
        printf '%s\n' "$out5" >&2
        fail=1
    elif [[ "$err5" != *"could not determine"* ]]; then
        echo "self-test FAILED: an operational failure should leave a visible note on stderr, not be silent; got stderr: $err5" >&2
        fail=1
    else
        echo "self-test ok: claim-check being unavailable (tier 3, operational failure) allows the tool call, with a visible reason on stderr"
    fi

    # --- Scenario 6: RefuseNoIdentity (labelled, no claim comment names a
    # holder) -> ASK, not silently deny or allow (reviewer M5). ---
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
echo "issue claim-check: issue #999 of acme/widgets is labelled in-progress but no claim comment names a holder — refusing (identity unknown)" >&2
exit 2
EOF
    chmod +x "$fake_bin/worker-agent-deck"

    out="$(PATH="$fake_bin:$PATH" bash "$0" <<<"$input_blocked")"
    decision="$(jq -r '.hookSpecificOutput.permissionDecision // empty' <<<"$out" 2>/dev/null)"
    reason="$(jq -r '.hookSpecificOutput.permissionDecisionReason // empty' <<<"$out" 2>/dev/null)"
    if [ "$decision" != "ask" ] || [[ "$reason" != *"identity unknown"* ]]; then
        echo "self-test FAILED: expected an ask decision noting identity unknown; got:" >&2
        printf '%s\n' "$out" >&2
        fail=1
    else
        echo "self-test ok: a RefuseNoIdentity claim-check result (tier 2) asks rather than silently denying or allowing"
    fi

    # --- Scenario 7: flags-before-positional (`gh issue close --repo r/r
    # 999`) is correctly detected, not silently allowed (reviewer B4). ---
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
if [ "$3" != "999" ] || [ "$5" != "r/r" ]; then
    echo "self-test FAILED: claim-check called with issue=$3 repo=$5, expected issue=999 repo=r/r" >&2
    exit 1
fi
echo "issue claim-check: issue #999 of r/r is held by \`orch-a\` — held by another agent" >&2
exit 1
EOF
    chmod +x "$fake_bin/worker-agent-deck"

    local input_flags_before
    input_flags_before=$(jq -n --arg cwd "$tmp/repo" '{
        tool_name: "Bash",
        tool_input: {command: "gh issue close --repo r/r 999"},
        cwd: $cwd
    }')
    out="$(PATH="$fake_bin:$PATH" bash "$0" <<<"$input_flags_before")"
    decision="$(jq -r '.hookSpecificOutput.permissionDecision // empty' <<<"$out" 2>/dev/null)"
    reason="$(jq -r '.hookSpecificOutput.permissionDecisionReason // empty' <<<"$out" 2>/dev/null)"
    if [ "$decision" != "deny" ] || [[ "$reason" != *"#999"* ]]; then
        echo "self-test FAILED: flags-before-positional (--repo before the issue number) must still be checked; got:" >&2
        printf '%s\n' "$out" >&2
        fail=1
    else
        echo "self-test ok: flags-before-positional (\`gh issue close --repo r/r 999\`) is correctly detected and checked"
    fi

    # --- Scenario 8: a chained command checks BOTH segments, not just the
    # first (reviewer B5). Issue 1 clear, issue 2 held. ---
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
case "$3" in
1)
    echo "ok to proceed on issue #1 of acme/widgets as \`human:x@h\`"
    exit 0
    ;;
2)
    echo "issue claim-check: issue #2 of acme/widgets is held by \`orch-a\` — held by another agent" >&2
    exit 1
    ;;
*)
    echo "self-test FAILED: unexpected issue $3" >&2
    exit 1
    ;;
esac
EOF
    chmod +x "$fake_bin/worker-agent-deck"

    local input_chained
    input_chained=$(jq -n --arg cwd "$tmp/repo" '{
        tool_name: "Bash",
        tool_input: {command: "gh issue close 1 && gh issue close 2"},
        cwd: $cwd
    }')
    out="$(PATH="$fake_bin:$PATH" bash "$0" <<<"$input_chained")"
    decision="$(jq -r '.hookSpecificOutput.permissionDecision // empty' <<<"$out" 2>/dev/null)"
    reason="$(jq -r '.hookSpecificOutput.permissionDecisionReason // empty' <<<"$out" 2>/dev/null)"
    if [ "$decision" != "deny" ] || [[ "$reason" != *"#2"* ]]; then
        echo "self-test FAILED: a chained command must check EVERY segment (issue 1 clear, issue 2 held — expected a deny naming #2); got:" >&2
        printf '%s\n' "$out" >&2
        fail=1
    else
        echo "self-test ok: a chained command (\`gh issue close 1 && gh issue close 2\`) checks both segments, not just the first"
    fi

    # --- Scenario 9: a number/repo embedded inside a DIFFERENT flag's
    # quoted value must not redirect the check (reviewer B3, auditor A1). ---
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
if [ "$3" != "123" ] || [ "$5" != "acme/widgets" ]; then
    echo "self-test FAILED: claim-check called with issue=$3 repo=$5, expected issue=123 repo=acme/widgets (not 999999/evil-anything from inside --comment)" >&2
    exit 1
fi
echo "issue claim-check: issue #123 of acme/widgets is held by \`orch-a\` — held by another agent" >&2
exit 1
EOF
    chmod +x "$fake_bin/worker-agent-deck"

    local input_embedded
    input_embedded=$(jq -n --arg cwd "$tmp/repo" '{
        tool_name: "Bash",
        tool_input: {command: "gh issue close 123 --repo acme/widgets --comment \"see --issue 999999 --repo evil/unclaimed\""},
        cwd: $cwd
    }')
    out="$(PATH="$fake_bin:$PATH" bash "$0" <<<"$input_embedded")"
    decision="$(jq -r '.hookSpecificOutput.permissionDecision // empty' <<<"$out" 2>/dev/null)"
    reason="$(jq -r '.hookSpecificOutput.permissionDecisionReason // empty' <<<"$out" 2>/dev/null)"
    if [ "$decision" != "deny" ] || [[ "$reason" != *"#123"* ]]; then
        echo "self-test FAILED: a number/repo embedded inside --comment's quoted value must not redirect the check; got:" >&2
        printf '%s\n' "$out" >&2
        fail=1
    else
        echo "self-test ok: a number/repo embedded inside a different flag's quoted value does not redirect the check"
    fi

    # --- Scenario 10: a quoted --repo value is correctly extracted, not
    # silently falling back to cwd's origin (reviewer M1). Uses a SECOND
    # repo dir with a DIFFERENT origin so a pass here proves the quoted
    # flag value was used, not merely that the fallback happened to match. ---
    git init -q "$tmp/repo2"
    git -C "$tmp/repo2" remote add origin "https://github.com/wrongowner/wrongrepo.git"
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
if [ "$5" != "acme/widgets" ]; then
    echo "self-test FAILED: claim-check called with repo=$5, expected the quoted --repo value acme/widgets, not cwd's origin (wrongowner/wrongrepo)" >&2
    exit 1
fi
echo "issue claim-check: issue #999 of acme/widgets is held by \`orch-a\` — held by another agent" >&2
exit 1
EOF
    chmod +x "$fake_bin/worker-agent-deck"

    local input_quoted_repo
    input_quoted_repo=$(jq -n --arg cwd "$tmp/repo2" '{
        tool_name: "Bash",
        tool_input: {command: "gh issue close 999 --repo \"acme/widgets\""},
        cwd: $cwd
    }')
    out="$(PATH="$fake_bin:$PATH" bash "$0" <<<"$input_quoted_repo")"
    decision="$(jq -r '.hookSpecificOutput.permissionDecision // empty' <<<"$out" 2>/dev/null)"
    reason="$(jq -r '.hookSpecificOutput.permissionDecisionReason // empty' <<<"$out" 2>/dev/null)"
    if [ "$decision" != "deny" ] || [[ "$reason" != *"acme/widgets"* ]]; then
        echo "self-test FAILED: a quoted --repo value must be extracted correctly, not fall back to cwd's origin; got:" >&2
        printf '%s\n' "$out" >&2
        fail=1
    else
        echo "self-test ok: a quoted --repo \"acme/widgets\" value is correctly extracted, not silently falling back to cwd's origin"
    fi

    if [ "$fail" -ne 0 ]; then
        exit 1
    fi
    echo "self-test ok: all 10 scenarios passed — check-issue-claim.sh blocks a refused claim-check, asks on ambiguity, and allows on a clear or could-not-determine result"
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
