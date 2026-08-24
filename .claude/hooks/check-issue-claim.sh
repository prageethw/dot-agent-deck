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
# not trusted from memory): with valid JSON that passes schema validation,
# JSON decision fields are honored and the exit code is IGNORED — only
# plain-text or invalid-JSON stdout makes a non-zero exit a non-blocking
# error (round-2 fix, reviewer L1/R8: an earlier draft of this comment
# claimed the opposite, that any non-zero exit other than 2 discards the
# JSON). Exit 2 remains special: it blocks UNCONDITIONALLY and cannot be
# overridden by JSON at all. `reason` is REQUIRED for both `deny` and `ask`
# (round-2 fix, reviewer L2 — not "optional for ask" as an earlier draft
# claimed), optional only for `allow`, which needs no output at all. This
# script always exits 0 itself and uses stdout JSON as the sole signal
# regardless. A fail-open (allow) path that should stay visible without
# blocking the tool call is surfaced via the top-level `systemMessage` JSON
# field (round-2 fix, reviewer R4: stderr from a hook that exits 0 reaches
# only the debug log, never the transcript, so a stderr-only note is a
# fully silent bypass in practice even though it "prints something" —
# `systemMessage` is documented as shown in the transcript). Grep this
# script for `add_note` to find every site that contributes to it.

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
# round — this is real, not assumed). Used for Priority 1's tier 4:
# genuinely ambiguous rather than confidently refused (CLAUDE.md rule 14's
# own guidance is to escalate to a human rather than silently adopt).
ask_json() {
    local reason="$1"
    jq -n --arg reason "$reason" \
        '{hookSpecificOutput: {hookEventName: "PreToolUse", permissionDecision: "ask", permissionDecisionReason: $reason}}'
}

# Truncates and delimits a claim-check reason before it reaches
# `permissionDecisionReason` or `systemMessage` (auditor A7, generalized in
# the round-2 fix per reviewer L3): `CLAIM_CHECK_REASON` is claim-check's
# combined stdout+stderr, which on a genuine refusal embeds a holder
# identity PARSED out of a GitHub comment this deck does not author — but
# on an OPERATIONAL failure (a stale binary's clap usage error, a missing
# binary, a network hiccup) it is the checker's OWN diagnostic text, not
# issue-comment content at all. Round 1's label ("untrusted issue-comment
# content follows") asserted the former unconditionally; reworded generically
# so it is accurate either way rather than mislabeling clap's own error text
# as something a stranger wrote in a GitHub comment.
sanitize_reason() {
    local raw="$1"
    printf 'unvalidated checker output follows (may include untrusted GitHub comment text): %s' "${raw:0:256}"
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
#
# Round-2 fix (reviewer M2 / auditor R7): `parse_github_owner_repo`'s four
# literal prefixes miss ordinary, real remote configurations — an SSH
# host-alias form (`git@github.com-work:owner/repo.git`, from a
# `~/.ssh/config Host github.com-work` entry) and a URL carrying userinfo
# (`https://user@github.com/owner/repo.git`) both measured falling through
# and taking a completely unchecked issue write with them. Rather than
# growing an ever-longer list of literal prefixes, fall back to `gh`'s own
# repo resolution, which already handles every remote shape `gh` itself
# understands — asking `gh` "what repo is this" is more robust than
# re-deriving the answer from the remote URL a second time.
derive_repo_slug() {
    local dir="$1" url slug
    url="$(git -C "$dir" remote get-url origin 2>/dev/null)" || url=""
    if [ -n "$url" ]; then
        slug="$(parse_github_owner_repo "$url")"
        if [ -n "$slug" ]; then
            printf '%s\n' "$slug"
            return
        fi
    fi
    (cd "$dir" && gh repo view --json nameWithOwner --jq '.nameWithOwner' 2>/dev/null) || true
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
# Round-2 fix (reviewer B1-B5/M1, auditor R1-R2): round 1 rebuilt tokenizing
# from a raw-string regex; THIS round rebuilds the SEGMENT SPLITTING, which
# had its own class of bypass. `shlex.split` (plain, `punctuation_chars`
# unset) treats a newline as ordinary whitespace and never separates `;`/
# `&&`/`||`/`&` from an adjacent word with no surrounding space — so
# `cd /tmp; gh issue close 123` (unspaced) and a genuinely multi-line Bash
# command (the single most common way an agent issues two commands) both
# collapsed into ONE token stream with no separator at all, and the
# classifier's `tokens[0] == "gh"` requirement then meant a chain led by
# anything other than `gh` (`cd`, `echo`, `timeout`, ...) produced NO match
# whatsoever — a fully silent bypass, verified as a measured regression
# against round 1's own (less precise but accidentally broader) regex.
#
# The fix is two independent pieces, per both reports' own tested fix
# direction:
#   1. Split the RAW command on literal newlines FIRST, and tokenize each
#      resulting line independently — `shlex` has no notion of "this
#      newline is a separator", so the split has to happen before it runs.
#   2. Use `shlex.shlex(..., punctuation_chars=True)` with
#      `whitespace_split = True` instead of the plain `shlex.split`
#      convenience wrapper: this makes shlex recognize `;`, `&&`, `||`,
#      `&`, `|`, `|&`, `(`, `)` as their OWN standalone tokens even with no
#      surrounding whitespace, for free — closing the unspaced-operator
#      half of the same bug in one change. A quoted occurrence of any of
#      these (`--comment "a; b"`) still tokenizes as ONE token, never split.
#
# `classify()` no longer requires `tokens[0] == "gh"` either: it scans a
# SEGMENT (never across a separator — see `split_segments`) for `gh` at ANY
# index whose next two tokens are `issue <verb>` or `pr merge`. This is what
# actually fixes B2 (a non-`gh`-led chain, `cd X; gh …`) and, as a
# documented side effect, L6 (wrapper commands: `timeout 30 gh …`,
# `sudo gh …`, `command gh …` all now match too) — with no separate
# wrapper-stripping list to maintain. A quoted occurrence
# (`"gh issue close 1"` as a single string argument to something else)
# cannot false-positive here since it is one token, not three. Segmenting
# BEFORE this scan (rather than scanning the whole unsegmented stream) is
# what keeps a later segment's `--repo`/positional from leaking into an
# earlier segment's extraction — each segment's tokens are handed to
# `extract_repo_and_number` in isolation.
#
# Emits one line per gated match, fields separated by ASCII Unit Separator
# (0x1F) rather than a tab or space: bash's `read` collapses RUNS of any IFS
# character that is also in the default whitespace class (space/tab/
# newline) and drops empty fields at the boundary, even when IFS is
# explicitly set to nothing but that one character — so a tab-separated
# empty <repo-or-empty> field would silently shift every field after it.
# 0x1F is not in that class, so `IFS=$'\x1f' read -r ...` on the bash side
# splits exactly on it with empty fields preserved:
#   {issue|merge}<0x1F><repo-or-empty><0x1F><number-or-empty><0x1F>{OK|NONE|AMBIGUOUS}
# OK means <number> is a clean, unambiguous integer. NONE means no
# positional candidate was found at all — the caller resolves `gh pr
# merge`'s current-branch PR before giving up; there is no equivalent
# fallback for the issue verbs. AMBIGUOUS means either a candidate token WAS
# found but is not a clean integer (a shell variable like "$N"), OR an
# unrecognized flag of unknown arity was seen before the positional was
# identified (round-2 fix, reviewer M1 / auditor R2 — see
# `extract_repo_and_number`'s own doc). Per Priority 2's key behavioral
# change, a NONE (for the issue verbs) or AMBIGUOUS segment must NEVER be
# silently allowed through — the caller asks/denies instead.
#
# Exits 1 with nothing on stdout if ANY line of the command cannot be
# tokenized at all (unbalanced quoting) — the caller treats that exactly
# like python3 being absent: could-not-determine, not "no match". A single
# unparseable line fails the WHOLE command's tokenization rather than
# silently skipping just that line, since a partial result here could
# discard a genuine match on another line of the same logical command.
CLAIM_CHECK_PY=$(cat <<'PYEOF'
import re, shlex, sys

SEPARATORS = {"&&", ";", "||", "&", "|", "|&", ";;"}
ISSUE_VERBS = {"comment", "close", "edit"}

# Every value-taking flag `gh issue close|comment|edit` and `gh pr merge`
# accept, long AND short forms, verified directly against `gh --help`
# output for each subcommand (gh 2.97.0) rather than assumed (round-2 fix,
# reviewer M1 / auditor R2: round 1's redesign listed only long forms, so a
# short flag's VALUE — e.g. `-m 42` for `--milestone`, or `-c "text"` for
# `--comment` — was mistaken for the positional issue number).
VALUE_FLAGS = {
    "issue": {
        "--repo", "-R",
        "--comment", "-c",
        "--body", "-b",
        "--body-file", "-F",
        "--reason", "-r",
        "--title", "-t",
        "--milestone", "-m",
        "--duplicate-of",
        "--parent",
        "--type",
        "--add-assignee",
        "--remove-assignee",
        "--add-label",
        "--remove-label",
        "--add-project",
        "--remove-project",
        "--add-blocked-by",
        "--add-blocking",
        "--add-sub-issue",
        "--remove-blocked-by",
        "--remove-blocking",
        "--remove-sub-issue",
    },
    "merge": {
        "--repo", "-R",
        "--subject", "-t",
        "--body", "-b",
        "--body-file", "-F",
        "--match-head-commit",
        "--author-email", "-A",
    },
}

# Every BOOLEAN (non-value-taking) flag for the same subcommands, likewise
# verified against `gh --help`. Kept as an explicit allowlist rather than
# "anything not in VALUE_FLAGS is boolean" so a genuinely unrecognized flag
# (a future `gh` release, or a form neither report anticipated) is treated
# as unknown-arity below, not silently assumed safe.
BOOLEAN_FLAGS = {
    "issue": {
        "--create-if-none",
        "--delete-last",
        "--edit-last",
        "--editor", "-e",
        "--web", "-w",
        "--yes",
        "--remove-milestone",
        "--remove-parent",
        "--remove-type",
        "--help",
    },
    "merge": {
        "--admin",
        "--auto",
        "--delete-branch", "-d",
        "--disable-auto",
        "--merge", "-m",
        "--rebase", "-r",
        "--squash", "-s",
        "--help",
    },
}
REPO_RE = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")


def repo_from_value(value):
    return value if REPO_RE.match(value) else None


def extract_repo_and_number(tokens, kind):
    """Walk a single segment's post-verb tokens for the positional
    issue/PR number and an optional --repo/-R value. Round-2 fix (reviewer
    M1 / auditor R2): an unrecognized flag of unknown arity, seen BEFORE the
    positional has been identified, forces the final status to AMBIGUOUS
    rather than being assumed boolean (which risked treating its actual
    VALUE as the positional — the exact redirection primitive round 1's A1
    fixed for the flags this round's VALUE_FLAGS/BOOLEAN_FLAGS lists
    happen to cover, reopened for anything they don't). Once forced,
    nothing downstream un-flags it — a later, perfectly clean integer
    found after an unknown flag does not restore confidence, since we
    cannot tell in general whether that integer was itself an unknown
    flag's swallowed value.
    """
    value_flags = VALUE_FLAGS[kind]
    boolean_flags = BOOLEAN_FLAGS[kind]
    repo = None
    number = None
    forced_ambiguous = False
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
                # A `--flag=value` form always carries its value inline,
                # regardless of whether the flag is recognized — safe to
                # treat as one token either way.
                i += 1
                continue
            if t in value_flags:
                i += 2
                continue
            if t in boolean_flags:
                i += 1
                continue
            # Unrecognized flag, arity unknown. If the positional has not
            # been found yet, we cannot tell whether the NEXT token is this
            # flag's value or the real positional — force ambiguous rather
            # than guess either direction.
            if number is None:
                forced_ambiguous = True
            i += 1
            continue
        if number is None:
            number = t
        i += 1
    if forced_ambiguous:
        status = "AMBIGUOUS"
    elif number is None:
        status = "NONE"
    else:
        status = "OK" if re.fullmatch(r"[0-9]+", number) else "AMBIGUOUS"
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


def find_gh_verb(tokens):
    """Scan ONE segment's tokens for a `gh issue <verb>` or `gh pr merge`
    invocation at ANY index — round-2 fix (reviewer B2, auditor R1):
    dropping the old `tokens[0] == "gh"` requirement is what actually
    fixes a non-`gh`-led chain (`cd X; gh issue close N`, now correctly
    segmented by `split_segments` into its own segment, but still needing
    this to not require `gh` to lead ITS segment) and, as a bonus, wrapper
    commands (`timeout 30 gh …`, `sudo gh …`) that legitimately have `gh`
    somewhere other than index 0 within one segment. Returns None if this
    segment contains no gated verb at all.
    """
    n = len(tokens)
    i = 0
    while i + 2 < n:
        if tokens[i] == "gh":
            if tokens[i + 1] == "issue" and tokens[i + 2] in ISSUE_VERBS:
                return ("issue",) + extract_repo_and_number(tokens[i + 3:], "issue")
            if tokens[i + 1] == "pr" and tokens[i + 2] == "merge":
                return ("merge",) + extract_repo_and_number(tokens[i + 3:], "merge")
        i += 1
    return None


def tokenize_line(line):
    lex = shlex.shlex(line, posix=True, punctuation_chars=True)
    lex.whitespace_split = True
    return list(lex)


def main():
    raw = sys.stdin.read()
    for line in raw.split("\n"):
        try:
            tokens = tokenize_line(line)
        except ValueError as exc:
            print(
                "check-issue-claim.sh: could not tokenize command (unbalanced quoting): {}".format(exc),
                file=sys.stderr,
            )
            return 1
        for seg in split_segments(tokens):
            result = find_gh_verb(seg)
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
#   3 = could not determine (operational failure — allow, but surface why)
#   4 = ambiguous — identity unknown (ask, or deny if ask is unsupported)
# Code 2 is deliberately never returned here (round-2 fix, reviewer B5 /
# auditor R3): it is clap's OWN reserved usage-error code, so any
# `worker-agent-deck` binary that predates the `claim-check` subcommand
# answers with exit 2 from a clap parse failure, not from a real outcome —
# and the previous round's hook treated bare exit 2 as tier 2 (ask) with a
# reason string that ASSERTED a specific claim state nothing had actually
# determined. As defense in depth beyond simply renumbering the Rust side
# (which closes the collision going forward but not against a binary that
# has not been rebuilt yet), a bare exit 2 is gated on
# `run_issue_claim_check_cli`'s own message prefix ("issue claim-check: ",
# which every non-`Clear` outcome carries) before being trusted as tier 4 —
# an exit 2 WITHOUT that prefix (clap's usage error, or any future
# accidental collision) demotes to tier 3 rather than fabricating an ask.
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
    0 | 1) return "$status" ;;
    2)
        if [[ "$out" == *"issue claim-check:"* ]]; then
            return 4
        fi
        return 3
        ;;
    4) return 4 ;;
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

# Global note accumulator (round-2 fix, reviewer R4/M2 / auditor R4/R7): a
# fail-open path (operational failure, unresolvable repo, unresolvable PR
# lookup) must never be a COMPLETELY silent allow. The current docs confirm
# stderr from an exit-0 hook reaches only the debug log, never the
# transcript — so the genuinely visible channel is the top-level
# `systemMessage` JSON field, not stderr. Every fail-open site calls
# `add_note` instead of writing to stderr directly; all notes collected
# across the WHOLE hook run (there can be more than one gated segment) are
# flushed as ONE combined `systemMessage` right before the final exit,
# since a PreToolUse hook gets exactly one JSON response per invocation.
NOTES=()

add_note() {
    NOTES+=("$1")
    # Keep the stderr line too — cheap, and useful for anyone tailing the
    # debug log directly; but per the fix above, never rely on this alone.
    echo "check-issue-claim.sh: $1" >&2
}

flush_notes_and_exit() {
    if [ "${#NOTES[@]}" -gt 0 ]; then
        local joined
        joined="$(printf '%s\n' "${NOTES[@]}")"
        jq -n --arg msg "$joined" '{systemMessage: $msg}'
    fi
    exit 0
}

# Runs claim-check for issue $2 of repo $1 (cwd $3) and reacts per its tier
# (see run_claim_check's doc): tier 0 returns (nothing to do — the caller's
# loop moves to the next match); tier 1 denies and exits; tier 4 asks (or
# would deny if a future hook contract ever dropped "ask" — re-verify
# against the docs before assuming it still applies) and exits; tier 3
# allows but leaves a visible note (via `add_note`) and returns. $4 is a
# note prefix for the deny/ask reason text ("merging this PR would close
# issue #N, and " for the merge path, empty for a direct gh issue write).
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
    4)
        ask_json "check-issue-claim.sh (issue #286): ${note}\`issue claim-check\` could not confirm — $(sanitize_reason "$CLAIM_CHECK_REASON")"
        exit 0
        ;;
    *)
        add_note "\`issue claim-check\` for issue #$issue of $repo could not determine an answer (operational failure — binary missing, gh auth/network issue, or the caller is not in a linked worktree) — allowing without a claim check: $(sanitize_reason "$CLAIM_CHECK_REASON")"
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
    # jq itself being absent is the one fail-open path that cannot route
    # through `add_note`/`flush_notes_and_exit` (both need jq to build
    # valid JSON) — hand-write a fixed, content-free JSON literal instead.
    if ! command -v jq >/dev/null 2>&1; then
        echo "check-issue-claim.sh: jq not found - allowing without a claim check" >&2
        printf '{"systemMessage":"check-issue-claim.sh: jq not found - allowing without a claim check"}\n'
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
        # visibly (systemMessage, not just stderr) so this is never a
        # silent bypass.
        add_note "could not tokenize command for claim checking (python3 missing, or unparseable quoting) — allowing without a claim check: $command"
        flush_notes_and_exit
    fi
    if [ "${#MATCHES[@]}" -eq 0 ]; then
        flush_notes_and_exit
    fi

    local m kind repo number ext_status
    for m in "${MATCHES[@]}"; do
        IFS=$'\x1f' read -r kind repo number ext_status <<<"$m"

        [ -n "$repo" ] || repo="$(derive_repo_slug "$cwd")"
        if [ -z "$repo" ]; then
            # Cannot even name the repo — nothing to check against. Fail
            # open rather than block on an ambiguity this hook cannot
            # resolve; the underlying `worker-agent-deck issue
            # claim`/`claim-check` commands would refuse just as loudly if
            # run by hand with no derivable repo. Round-2 fix (reviewer
            # M2 / auditor R7): this used to be a fully silent `continue`.
            add_note "could not derive a repo slug for a gated \`gh\` command — cwd's origin remote is unrecognized (even after the \`gh repo view\` fallback) and the command itself carries no --repo/-R — allowing without a claim check. Command: $command"
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
                # Round-2 fix (reviewer M2 / auditor R7): this used to be a
                # fully silent `continue`.
                add_note "could not resolve the current branch's pull request via \`gh pr view\` for a gated \`gh pr merge\` with no explicit PR number/URL/branch — allowing without a claim check. Command: $command"
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
                # Round-2 fix (reviewer M2 / auditor R7): this used to be a
                # fully silent `continue`.
                add_note "could not look up PR #$number of $repo via \`gh pr view\` to check for closing-keyword references — allowing without a claim check. Command: $command"
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
    flush_notes_and_exit
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
    # number itself. Stubs `gh` too, since this path calls `gh pr view`.
    # The `gh` stub fails unless the `--json` argument names `title`,
    # pinning that the closing-keyword scan really does query the PR
    # TITLE (not just body/commits) — round-2 fix, reviewer M3: the
    # previous stub answered ANY `pr view` call, so it could not tell a
    # title-scanning implementation apart from one that never asked. ---
    cat >"$fake_bin/gh" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
    found_title_field=0
    for arg in "$@"; do
        case "$arg" in
        *title*) found_title_field=1 ;;
        esac
    done
    if [ "$found_title_field" -ne 1 ]; then
        echo "self-test FAILED: gh pr view was not asked for --json title: $*" >&2
        exit 1
    fi
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
        echo "self-test ok: \`gh pr merge\` closing an issue by keyword is checked against THAT issue and blocked, and the lookup genuinely queried --json title"
    fi
    rm -f "$fake_bin/gh"

    # --- Scenario 5: claim-check binary cannot be found/run (operational
    # failure — e.g. a fresh clone with no fork build installed per
    # CLAUDE.md rule 21, reviewer B1/auditor A5) -> ALLOW, with a visible
    # systemMessage note (round-2 fix, reviewer/auditor R4: stderr from an
    # exit-0 hook never reaches the transcript, so the note must be in the
    # JSON, not just stderr — this scenario now asserts on
    # `.systemMessage`, not merely "no output"). Points CLAIM_CHECK_BIN at
    # a guaranteed-absent absolute path via the private self-test-only
    # override, rather than relying on PATH not already having a real
    # `worker-agent-deck` on it (this IS a fork dev machine per rule 21, so
    # that assumption would not hold). ---
    local stderr5 out5 err5 sysmsg5 decision5
    stderr5="$tmp/stderr5"
    out5="$(PATH="$fake_bin:$PATH" _CLAIM_CHECK_SELF_TEST=1 CLAIM_CHECK_BIN="$fake_bin/does-not-exist" bash "$0" <<<"$input_blocked" 2>"$stderr5")"
    err5="$(cat "$stderr5")"
    decision5="$(jq -r '.hookSpecificOutput.permissionDecision // empty' <<<"$out5" 2>/dev/null)"
    sysmsg5="$(jq -r '.systemMessage // empty' <<<"$out5" 2>/dev/null)"
    if [ -n "$decision5" ]; then
        echo "self-test FAILED: expected no BLOCKING decision (allow) when claim-check cannot be found/run; got:" >&2
        printf '%s\n' "$out5" >&2
        fail=1
    elif [[ "$sysmsg5" != *"could not determine"* ]]; then
        echo "self-test FAILED: an operational failure should leave a visible systemMessage note, not rely on stderr alone; got out: $out5 stderr: $err5" >&2
        fail=1
    else
        echo "self-test ok: claim-check being unavailable (tier 3, operational failure) allows the tool call, with a visible systemMessage reason"
    fi

    # --- Scenario 6: RefuseNoIdentity (labelled, no claim comment names a
    # holder) -> ASK, not silently deny or allow (reviewer M5). Exit code
    # is now 4, renumbered off clap's reserved 2 (round-2 fix, reviewer
    # B5/auditor R3). ---
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
echo "issue claim-check: issue #999 of acme/widgets is labelled in-progress but no claim comment names a holder — refusing (identity unknown)" >&2
exit 4
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
        echo "self-test ok: a RefuseNoIdentity claim-check result (tier 4) asks rather than silently denying or allowing"
    fi

    # --- Scenario 6b: a BARE exit 2 (clap's own usage-error code, e.g. a
    # stale binary predating the `claim-check` subcommand) WITHOUT the
    # `issue claim-check: ` message prefix must degrade to tier 3
    # (allow + note), never be trusted as tier 4/ask — round-2 fix,
    # reviewer B5 / auditor R3's defense-in-depth. ---
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
echo "error: unrecognized subcommand 'claim-check'" >&2
echo "" >&2
echo "  tip: a similar subcommand exists: 'claim'" >&2
exit 2
EOF
    chmod +x "$fake_bin/worker-agent-deck"

    local out6b decision6b sysmsg6b
    out6b="$(PATH="$fake_bin:$PATH" bash "$0" <<<"$input_blocked")"
    decision6b="$(jq -r '.hookSpecificOutput.permissionDecision // empty' <<<"$out6b" 2>/dev/null)"
    sysmsg6b="$(jq -r '.systemMessage // empty' <<<"$out6b" 2>/dev/null)"
    if [ -n "$decision6b" ]; then
        echo "self-test FAILED: a bare clap-shaped exit 2 (no 'issue claim-check: ' prefix) must NOT be trusted as tier 4/ask (it would fabricate a claim-state reason nothing determined); got:" >&2
        printf '%s\n' "$out6b" >&2
        fail=1
    elif [[ "$sysmsg6b" != *"could not determine"* ]]; then
        echo "self-test FAILED: a bare exit 2 should degrade to tier 3 (allow + visible note); got: $out6b" >&2
        fail=1
    else
        echo "self-test ok: a bare exit 2 with no claim-check message prefix (clap's usage-error collision) degrades to tier 3, never fabricating an ask"
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

    # --- Scenarios 11-15: the round-2 regression class itself (reviewer
    # B1/B2/B3, auditor R1) — every measured silent bypass must now DENY,
    # not silently allow. Issue 123 is HELD; any OTHER issue number (the
    # chained scenarios' leading `gh issue close 1`) is CLEAR — so a chain
    # that only checks the first segment and stops would wrongly ALLOW
    # rather than accidentally deny for the wrong reason, and a genuine
    # pass here proves segment 2 (issue 123) was reached and checked too. ---
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
if [ "$3" = "123" ]; then
    echo "issue claim-check: issue #123 of acme/widgets is held by \`orch-a\` — held by another agent" >&2
    exit 1
fi
echo "ok to proceed on issue #$3 of acme/widgets as \`human:x@h\`"
exit 0
EOF
    chmod +x "$fake_bin/worker-agent-deck"

    run_bypass_scenario() {
        local label="$1" cmd="$2"
        local inp o dec rsn
        inp=$(jq -n --arg cwd "$tmp/repo" --arg cmd "$cmd" '{
            tool_name: "Bash",
            tool_input: {command: $cmd},
            cwd: $cwd
        }')
        o="$(PATH="$fake_bin:$PATH" bash "$0" <<<"$inp")"
        dec="$(jq -r '.hookSpecificOutput.permissionDecision // empty' <<<"$o" 2>/dev/null)"
        rsn="$(jq -r '.hookSpecificOutput.permissionDecisionReason // empty' <<<"$o" 2>/dev/null)"
        if [ "$dec" != "deny" ] || [[ "$rsn" != *"#123"* ]]; then
            echo "self-test FAILED ($label): expected a deny decision naming issue #123; command was: $cmd; got:" >&2
            printf '%s\n' "$o" >&2
            fail=1
        else
            echo "self-test ok: $label is correctly denied, not silently allowed"
        fi
    }

    run_bypass_scenario "a newline-separated chain" "$(printf 'gh issue close 1\ngh issue close 123 --repo acme/widgets')"
    run_bypass_scenario "an unspaced ; chain led by a non-gh command" "cd /tmp; gh issue close 123 --repo acme/widgets"
    run_bypass_scenario "an unspaced ; chain between two gh commands" "gh issue close 1;gh issue close 123 --repo acme/widgets"
    run_bypass_scenario "an unspaced && chain" "gh issue close 1&&gh issue close 123 --repo acme/widgets"
    run_bypass_scenario "a || chain" "gh issue close 1 || gh issue close 123 --repo acme/widgets"
    run_bypass_scenario "a & background chain" "gh issue close 1 & gh issue close 123 --repo acme/widgets"
    run_bypass_scenario "a non-gh-led chain (echo; gh ...)" "echo hi; gh issue close 123 --repo acme/widgets"
    run_bypass_scenario "a wrapper command (timeout 30 gh ...)" "timeout 30 gh issue close 123 --repo acme/widgets"

    # --- Scenario 16: extraction-ambiguity -> ASK, the PR body's own
    # description of "the key behavioral change" — previously untested
    # (reviewer M3). ---
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
echo "self-test FAILED: worker-agent-deck should never be invoked when the number could not be determined" >&2
exit 1
EOF
    chmod +x "$fake_bin/worker-agent-deck"

    local input_ambiguous out16 decision16 reason16
    input_ambiguous=$(jq -n --arg cwd "$tmp/repo" '{
        tool_name: "Bash",
        tool_input: {command: "gh issue comment $N --body x"},
        cwd: $cwd
    }')
    out16="$(PATH="$fake_bin:$PATH" bash "$0" <<<"$input_ambiguous")"
    decision16="$(jq -r '.hookSpecificOutput.permissionDecision // empty' <<<"$out16" 2>/dev/null)"
    reason16="$(jq -r '.hookSpecificOutput.permissionDecisionReason // empty' <<<"$out16" 2>/dev/null)"
    if [ "$decision16" != "ask" ]; then
        echo "self-test FAILED: an unresolvable positional (\$N, not a literal integer) must ask, never silently allow or guess; got:" >&2
        printf '%s\n' "$out16" >&2
        fail=1
    else
        echo "self-test ok: an extraction ambiguity (\`gh issue comment \$N --body x\`) asks rather than guessing, and never calls claim-check"
    fi

    # --- Scenario 17: a short value-taking flag (`-m`, --milestone) no
    # longer misdirects the check to its VALUE instead of the real
    # positional (reviewer M1's redirection concern, now fixed by
    # completing VALUE_FLAGS). ---
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
if [ "$3" != "123" ]; then
    echo "self-test FAILED: claim-check called with issue=$3, expected 123 (not 42, -m's value)" >&2
    exit 1
fi
echo "issue claim-check: issue #123 of acme/widgets is held by \`orch-a\` — held by another agent" >&2
exit 1
EOF
    chmod +x "$fake_bin/worker-agent-deck"

    local input_short_flag out17 decision17 reason17
    input_short_flag=$(jq -n --arg cwd "$tmp/repo" '{
        tool_name: "Bash",
        tool_input: {command: "gh issue edit -m 42 123 --repo acme/widgets"},
        cwd: $cwd
    }')
    out17="$(PATH="$fake_bin:$PATH" bash "$0" <<<"$input_short_flag")"
    decision17="$(jq -r '.hookSpecificOutput.permissionDecision // empty' <<<"$out17" 2>/dev/null)"
    reason17="$(jq -r '.hookSpecificOutput.permissionDecisionReason // empty' <<<"$out17" 2>/dev/null)"
    if [ "$decision17" != "deny" ] || [[ "$reason17" != *"#123"* ]]; then
        echo "self-test FAILED: \`-m 42\` (short --milestone) must not misdirect the check to issue 42; expected a deny naming #123; got:" >&2
        printf '%s\n' "$out17" >&2
        fail=1
    else
        echo "self-test ok: a short value-taking flag (\`-m 42\`) no longer misdirects the check to its own value"
    fi

    # --- Scenario 18: an unrecognized flag of unknown arity forces ASK
    # rather than being assumed boolean (reviewer M1 / auditor R2's
    # general guard). ---
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
echo "self-test FAILED: worker-agent-deck should never be invoked when an unknown-arity flag forces ambiguity" >&2
exit 1
EOF
    chmod +x "$fake_bin/worker-agent-deck"

    local input_unknown_flag out18 decision18
    input_unknown_flag=$(jq -n --arg cwd "$tmp/repo" '{
        tool_name: "Bash",
        tool_input: {command: "gh issue edit --frobnicate 42 123 --repo acme/widgets"},
        cwd: $cwd
    }')
    out18="$(PATH="$fake_bin:$PATH" bash "$0" <<<"$input_unknown_flag")"
    decision18="$(jq -r '.hookSpecificOutput.permissionDecision // empty' <<<"$out18" 2>/dev/null)"
    if [ "$decision18" != "ask" ]; then
        echo "self-test FAILED: an unrecognized flag of unknown arity (--frobnicate) must force ask, not be assumed boolean; got:" >&2
        printf '%s\n' "$out18" >&2
        fail=1
    else
        echo "self-test ok: an unrecognized flag of unknown arity forces ask rather than guessing its arity"
    fi

    # --- Scenario 19: the repo-underivable and PR-lookup-failure paths
    # (reviewer M2 / auditor R7) now surface a visible systemMessage note
    # instead of a fully silent allow. ---
    local tmp_norigin input_norepo out19 decision19 sysmsg19
    tmp_norigin="$tmp/repo-no-origin"
    git init -q "$tmp_norigin"
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
echo "self-test FAILED: worker-agent-deck should never be invoked when the repo cannot be derived" >&2
exit 1
EOF
    chmod +x "$fake_bin/worker-agent-deck"
    cat >"$fake_bin/gh" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "repo" ] && [ "$2" = "view" ]; then
    exit 1
fi
echo "self-test FAILED: unexpected gh invocation: $*" >&2
exit 1
EOF
    chmod +x "$fake_bin/gh"
    input_norepo=$(jq -n --arg cwd "$tmp_norigin" '{
        tool_name: "Bash",
        tool_input: {command: "gh issue close 123"},
        cwd: $cwd
    }')
    out19="$(PATH="$fake_bin:$PATH" bash "$0" <<<"$input_norepo")"
    decision19="$(jq -r '.hookSpecificOutput.permissionDecision // empty' <<<"$out19" 2>/dev/null)"
    sysmsg19="$(jq -r '.systemMessage // empty' <<<"$out19" 2>/dev/null)"
    if [ -n "$decision19" ]; then
        echo "self-test FAILED: a gated command with no derivable repo must allow (nothing to check against), not block; got:" >&2
        printf '%s\n' "$out19" >&2
        fail=1
    elif [[ "$sysmsg19" != *"could not derive a repo slug"* ]]; then
        echo "self-test FAILED: an undeliverable repo must leave a visible systemMessage note, not a silent allow; got: $out19" >&2
        fail=1
    else
        echo "self-test ok: a gated command with no derivable repo allows with a visible systemMessage note, not silently"
    fi
    rm -f "$fake_bin/gh"

    # --- Scenario 20: `gh pr merge` with no explicit number, where
    # resolving the current branch's PR via `gh pr view --json number`
    # fails — must allow with a visible systemMessage note (reviewer M2 /
    # auditor R7), not a fully silent continue. ---
    local input_nonum out20 decision20 sysmsg20
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
echo "self-test FAILED: worker-agent-deck should never be invoked when the current-branch PR cannot be resolved" >&2
exit 1
EOF
    chmod +x "$fake_bin/worker-agent-deck"
    cat >"$fake_bin/gh" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
    exit 1
fi
echo "self-test FAILED: unexpected gh invocation: $*" >&2
exit 1
EOF
    chmod +x "$fake_bin/gh"
    input_nonum=$(jq -n --arg cwd "$tmp/repo" '{
        tool_name: "Bash",
        tool_input: {command: "gh pr merge --squash"},
        cwd: $cwd
    }')
    out20="$(PATH="$fake_bin:$PATH" bash "$0" <<<"$input_nonum")"
    decision20="$(jq -r '.hookSpecificOutput.permissionDecision // empty' <<<"$out20" 2>/dev/null)"
    sysmsg20="$(jq -r '.systemMessage // empty' <<<"$out20" 2>/dev/null)"
    if [ -n "$decision20" ]; then
        echo "self-test FAILED: an unresolvable current-branch PR lookup must allow, not block; got:" >&2
        printf '%s\n' "$out20" >&2
        fail=1
    elif [[ "$sysmsg20" != *"could not resolve the current branch's pull request"* ]]; then
        echo "self-test FAILED: an unresolvable current-branch PR lookup must leave a visible systemMessage note, not a silent allow; got: $out20" >&2
        fail=1
    else
        echo "self-test ok: \`gh pr merge\` with no explicit number, when the current-branch PR cannot be resolved, allows with a visible systemMessage note, not silently"
    fi
    rm -f "$fake_bin/gh"

    # --- Scenario 21: a resolvable PR number/branch, but the
    # title/body/commits lookup used for closing-keyword scanning fails —
    # must allow with a visible systemMessage note (reviewer M2 / auditor
    # R7), not a fully silent continue. ---
    local input_lookupfail out21 decision21 sysmsg21
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
echo "self-test FAILED: worker-agent-deck should never be invoked when the PR body/title lookup fails" >&2
exit 1
EOF
    chmod +x "$fake_bin/worker-agent-deck"
    cat >"$fake_bin/gh" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
    exit 1
fi
echo "self-test FAILED: unexpected gh invocation: $*" >&2
exit 1
EOF
    chmod +x "$fake_bin/gh"
    input_lookupfail=$(jq -n --arg cwd "$tmp/repo" '{
        tool_name: "Bash",
        tool_input: {command: "gh pr merge 573 --squash"},
        cwd: $cwd
    }')
    out21="$(PATH="$fake_bin:$PATH" bash "$0" <<<"$input_lookupfail")"
    decision21="$(jq -r '.hookSpecificOutput.permissionDecision // empty' <<<"$out21" 2>/dev/null)"
    sysmsg21="$(jq -r '.systemMessage // empty' <<<"$out21" 2>/dev/null)"
    if [ -n "$decision21" ]; then
        echo "self-test FAILED: an unresolvable PR title/body/commits lookup must allow, not block; got:" >&2
        printf '%s\n' "$out21" >&2
        fail=1
    elif [[ "$sysmsg21" != *"could not look up PR"* ]]; then
        echo "self-test FAILED: an unresolvable PR title/body/commits lookup must leave a visible systemMessage note, not a silent allow; got: $out21" >&2
        fail=1
    else
        echo "self-test ok: a \`gh pr merge <n>\` whose PR body/title lookup fails allows with a visible systemMessage note, not silently"
    fi
    rm -f "$fake_bin/gh"

    if [ "$fail" -ne 0 ]; then
        exit 1
    fi
    echo "self-test ok: all scenarios passed — check-issue-claim.sh blocks a refused claim-check, asks on ambiguity, allows on a clear or could-not-determine result, and (round-2 fix) no longer silently bypasses a newline/unspaced-operator/non-gh-led chain, a short-flag redirection, or an unrecognized-flag ambiguity"
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
