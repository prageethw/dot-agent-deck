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
# settings.json itself. See docs/develop/issue-claim-check-hook.md for a
# contributor-facing overview of what this hook does and how to read a
# deny/ask/systemMessage note.
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
# overridden by JSON at all. `reason` is OPTIONAL for both `deny` and `ask`
# (round-3 fix, reviewer N7 — round 2's "REQUIRED for both" was itself
# wrong; this script supplies one on every gated path anyway, so this has
# no functional effect, only a documentation one). Its AUDIENCE differs by
# decision: for `allow`/`ask` the reason is shown to the USER only, never to
# Claude; for `deny` it is shown to Claude; for the fourth `permissionDecision`
# value, `defer` (not currently emitted by this script), the reason is
# ignored entirely. That distinction matters for `sanitize_reason` below —
# untrusted GitHub-comment text reaches Claude's own context only via the
# `deny` path, never via `ask`. This script always exits 0 itself and uses
# stdout JSON as the sole signal regardless. A fail-open (allow) path that
# should stay visible without blocking the tool call is surfaced via the
# top-level `systemMessage` JSON field (round-2 fix, reviewer R4: stderr
# from a hook that exits 0 reaches only the debug log, never the
# transcript, so a stderr-only note is a fully silent bypass in practice
# even though it "prints something" — `systemMessage` is documented as
# shown in the transcript, though on `PreToolUse` specifically it is
# user-visible only, never seen by the model itself). Grep this script for
# `add_note` to find every site that contributes to it. Hook output
# strings, `systemMessage` included, are capped by Claude Code itself at
# 10,000 characters; overflow is spilled to a file and replaced with a
# preview (round-3 fix, reviewer N7).
#
# KNOWN LIMITATIONS (round-3 fix, reviewer/auditor Priority 3 — real,
# measured, and deliberately not chased further; this is an
# accident-preventer for cooperating orchestrations, never an enforcement
# boundary, per the SCOPE paragraph above):
#   - An absolute or otherwise path-qualified invocation of `gh`
#     (`/usr/bin/gh issue close 123`) defeats the `tokens[i] == "gh"` match
#     entirely — allowed, silently, with no note.
#   - `bash -c '...'`-wrapped commands, and any `eval`, are opaque to this
#     tokenizer — the wrapped string is never inspected.
#   - `GH_REPO=other/repo gh issue close 123` is not recognized as an
#     alternate repo source; the check still resolves the repo from cwd's
#     origin (or an explicit `--repo`/`-R`), never from `GH_REPO`. A
#     natural follow-up once N2's "unrecognized --repo forces ambiguous"
#     fix has landed (recognizing `GH_REPO` the same way), not a separate
#     defect to chase in this round.
#   - (Round-2/round-3 residual, now closed) A `systemMessage` note from an
#     earlier segment used to be dropped whenever a LATER segment denied or
#     asked, because the old code exited immediately on the first blocking
#     verdict. `finish_hook`'s round-3 rewrite (reviewer N3) evaluates
#     every segment before responding, so a `deny`/`ask` decision now
#     carries any accumulated notes as `systemMessage` in the SAME
#     response — see `finish_hook`'s own doc.
#   - Cross-repo closing references (`Closes
#     https://github.com/other/elsewhere/issues/N` inside a PR merging
#     into a different repo) are checked against the MERGE TARGET's repo,
#     not the referenced repo's — over-blocking, the deliberately safe
#     direction, not a bug.
#   - `gh api` REST calls that perform the same writes remain permanently
#     out of scope — see the SCOPE paragraph above.
#   - A hook TIMEOUT (the 15-second budget in `.claude/settings.json`) is
#     the one fail-open path that CANNOT be made visible: per the current
#     docs, a timed-out hook is canceled, its entire output (including any
#     `systemMessage`) is discarded, and the tool call proceeds through
#     the normal permission flow as if the hook had never run. Every other
#     fail-open path in this script announces itself; this one structurally
#     cannot. The 15s budget is shared across the whole invocation — one
#     `derive_repo_slug` `gh repo view` call per gated segment with no
#     `--repo`, up to two `gh pr view` calls for a merge, and one
#     `worker-agent-deck issue claim-check` per closing reference (capped —
#     see `MAX_CLOSING_REFS_PER_MERGE` below) — with no per-subprocess
#     timeout of its own.

set -euo pipefail

# Round-3 fix (reviewer N5): caps how many closing references a single
# `gh pr merge` invocation gates — see the call site in `main_hook` and the
# header's KNOWN LIMITATIONS for why this bounds network round trips
# rather than latency per call.
MAX_CLOSING_REFS_PER_MERGE=10

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

# permissionDecision "ask" escalates to the user for a manual permission
# prompt (re-verified against the current docs during the PR #573 fix
# round — this is real, not assumed). Used for Priority 1's tier 4:
# genuinely ambiguous rather than confidently refused (CLAUDE.md rule 14's
# own guidance is to escalate to a human rather than silently adopt). Both
# `deny` and `ask` JSON responses are now built directly in `finish_hook`
# (round-3 fix, reviewer N3) rather than by dedicated `deny_json`/`ask_json`
# helpers, since a single response may also need to carry accumulated
# `NOTES` as `systemMessage` alongside the decision — see that function's
# own doc.
#
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
# Round-3 fix (reviewer N1, a regression this round's own newline-pre-split
# introduced): splitting the raw command on EVERY literal newline, before
# tokenizing, is wrong when a newline falls INSIDE a quoted argument (a
# multi-line `--body "..."`, this repo's own commonest gated shape per
# CLAUDE.md rule 25) or after a backslash line-continuation (this repo's
# own house style for long `gh` invocations) — either one splits a quote in
# half, fails to tokenize, and used to fail the WHOLE command's
# tokenization, silently allowing every segment on every line including
# ones that had nothing wrong with them. `split_logical_lines` below fixes
# this at the source: it walks the raw string tracking quote state (are we
# currently inside a `'...'` or `"..."`?) and only treats a bare newline as
# a split point when it is OUTSIDE any quote; a backslash immediately
# before such a newline is a continuation and is dropped, joining the two
# physical lines into one logical line. Outside quotes, an escaped quote
# character (`\'`/`\"`) is also recognized so it cannot be mistaken for the
# start of a quoted region. This is deliberately NOT a full bash-grammar
# reimplementation — see the KNOWN LIMITATIONS list at the top of this
# script — it only has to get quote-vs-newline right, since `shlex` (via
# `tokenize_line`) does the real word-splitting on each resulting logical
# line afterward.
#
# Round-3 fix (auditor A5): a logical line that STILL fails to tokenize
# (genuinely unbalanced quoting) is now recovered PER LINE, not
# whole-command — `main()` emits a `tokfail` record for that one line (the
# bash side turns it into a visible `systemMessage` note) and keeps
# processing every other logical line normally. Per-line is the narrowest
# recovery this design can offer: segmentation (`split_segments`) only runs
# on tokens a line successfully produced, so a line that fails to tokenize
# at all has no segments to recover independently — but one prose line with
# an apostrophe (a heredoc-built PR/issue comment is the common real case)
# no longer defeats every OTHER line of the same multi-line command, which
# is what the previous whole-command failure did.
CLAIM_CHECK_PY=$(cat <<'PYEOF'
import re, shlex, sys

# Round-3 fix (reviewer N4): `(` and `)` are added here so a second `gh`
# invocation inside the same segment — a genuine command substitution
# (`$(gh issue close 123 …)`) or two parenthesized commands back to back —
# is split into its own segment rather than silently never reached by
# `find_gh_verb`, which (deliberately) returns only the first match per
# segment. Verified this does not disturb the subshell (`(gh issue close
# 123 …)`) or loop-body cases that already worked: those become their own
# segment either way, before or after this change.
SEPARATORS = {"&&", ";", "||", "&", "|", "|&", ";;", "(", ")"}

# Round-3 fix (auditor A4): the original three verbs were the ones round 1
# happened to pick; `gh issue --help` (2.97.0) lists eight further mutating
# subcommands that take the same `<number>` positional and therefore need
# no extraction changes. CLAUDE.md rule 14 defines the gated action as
# "any write — a comment, a close, a label, an assignee", and `delete` is
# irreversible, so all eight are gated rather than carved out.
ISSUE_VERBS = {
    "comment", "close", "edit",
    "reopen", "delete", "lock", "unlock", "pin", "unpin", "transfer", "develop",
}

# Round-3 fix (auditor A2): every punctuation-class token `punctuation_chars
# =True` can emit as its own standalone token — a shell redirect
# (`2>/dev/null`) is the case that matters: it tokenizes as `2`, `>`,
# `/dev/null`, and without this check the bare fd number `2` is mistaken
# for the positional issue/PR number while the real write lands elsewhere.
# `extract_repo_and_number` below forces AMBIGUOUS the moment it sees ANY
# token made entirely of these characters — see that function's own doc for
# why this is safe even though the fd number is examined first (a
# left-to-right scan) and gets provisionally assigned as `number` before
# the operator token is reached.
PUNCTUATION_CHARS = set("();<>|&")

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
# Two-segment form only (`OWNER/REPO`) and gh's own documented
# `[HOST/]OWNER/REPO` three-segment form — round-3 fix (reviewer N2).
REPO_RE = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
HOST_REPO_RE = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")


def repo_from_value(value):
    """Validate a --repo/-R value. Returns (repo_or_None, recognized).

    Round-3 fix (reviewer N2): `gh`'s own help for every gated subcommand
    documents `-R, --repo [HOST/]OWNER/REPO` — a three-segment value when a
    host is given (`github.com/acme/widgets`). The old version of this
    function only matched two segments and returned a bare `None` on
    anything else, which every call site then read as "no --repo was
    given" and silently fell through to deriving the repo from cwd's
    origin remote instead — a write to a HELD issue in the actually-named
    repo was allowed because the check ran against a completely different
    repository, with zero output. `recognized` is the actual fix: it lets
    a caller distinguish "this value parsed to `repo_or_None`" from "this
    value does not parse as a --repo at all", so an unparseable
    --repo/-R VALUE can force AMBIGUOUS instead of silently vanishing.
    """
    if REPO_RE.match(value):
        return value, True
    m = HOST_REPO_RE.match(value)
    if m:
        # Drop the host segment; this is exactly gh's own rule for
        # [HOST/]OWNER/REPO, not a guess.
        return "/".join(value.split("/")[1:]), True
    return None, False


def extract_repo_and_number(tokens, kind, repo=None):
    """Walk a single segment's post-verb tokens for the positional
    issue/PR number and an optional --repo/-R value. `repo` seeds the
    result with a value already captured from FLAGS BEFORE the verb word
    (round-3 fix, auditor A3 — see `find_gh_verb`'s `_skip_flags_before_verb`
    helper); a --repo/-R found here, after the verb, overrides it, matching
    gh's own last-flag-wins behavior.

    Round-2 fix (reviewer M1 / auditor R2): an unrecognized flag of unknown
    arity, seen BEFORE the positional has been identified, forces the final
    status to AMBIGUOUS rather than being assumed boolean (which risked
    treating its actual VALUE as the positional — the exact redirection
    primitive round 1's A1 fixed for the flags this round's
    VALUE_FLAGS/BOOLEAN_FLAGS lists happen to cover, reopened for anything
    they don't). Once forced, nothing downstream un-flags it — a later,
    perfectly clean integer found after an unknown flag does not restore
    confidence, since we cannot tell in general whether that integer was
    itself an unknown flag's swallowed value.

    Round-3 fix (reviewer N2): an unparseable --repo/-R VALUE now forces
    AMBIGUOUS the same way, rather than silently discarding the value and
    falling back to a different repo — see `repo_from_value`'s own doc.

    Round-3 fix (auditor A2): a token made entirely of punctuation
    characters (`>`, `>>`, `<`, `<<`, `&&`, …) reaching here means
    `punctuation_chars=True` split a shell redirect into its own token —
    most commonly `2>/dev/null` tokenizing as `2`, `>`, `/dev/null`, where
    the bare fd number `2` would otherwise be mistaken for the positional.
    The check below forces `forced_ambiguous = True` unconditionally the
    moment such a token is SEEN, regardless of whether `number` already
    holds a value from an earlier token in this same left-to-right scan —
    that is deliberate, not a bug: `forced_ambiguous` is checked FIRST in
    the status computation at the end of this function, so it overrides
    whatever `number` was provisionally set to. This is what makes `gh
    issue close 2>/dev/null 123` come out AMBIGUOUS even though `number`
    gets assigned "2" three tokens before the disqualifying `>` is reached.
    """
    value_flags = VALUE_FLAGS[kind]
    boolean_flags = BOOLEAN_FLAGS[kind]
    number = None
    forced_ambiguous = False
    i = 0
    n = len(tokens)
    while i < n:
        t = tokens[i]
        if t in ("--repo", "-R"):
            if i + 1 < n:
                r, recognized = repo_from_value(tokens[i + 1])
                if recognized:
                    repo = r
                else:
                    forced_ambiguous = True
            else:
                forced_ambiguous = True
            i += 2
            continue
        if t.startswith("--repo=") or t.startswith("-R="):
            r, recognized = repo_from_value(t.split("=", 1)[1])
            if recognized:
                repo = r
            else:
                forced_ambiguous = True
            i += 1
            continue
        if t.startswith("-R") and len(t) > 2 and not t.startswith("--"):
            r, recognized = repo_from_value(t[2:])
            if recognized:
                repo = r
            else:
                forced_ambiguous = True
            i += 1
            continue
        if t and all(ch in PUNCTUATION_CHARS for ch in t):
            forced_ambiguous = True
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


def _skip_flags_before_verb(tokens, start):
    """From index `start` (right after `gh issue`/`gh pr`), skip a run of
    flag-shaped tokens before the actual verb word. Round-3 fix (auditor
    A3): real `gh`/Cobra accepts a flag between the subcommand and the
    verb (`gh issue --repo acme/widgets close 123`), but the old
    `find_gh_verb` required `gh`/`issue`/`<verb>` to sit at three
    consecutive indices, so this shape reached ZERO checks with no note.

    `--repo`/`-R` is the one flag common to every gated subcommand and
    known in advance to take a value, so it is captured here and returned
    alongside the new scan index; the caller seeds `extract_repo_and_number`
    with it. Any OTHER flag-shaped token here is skipped assuming it is
    boolean — this cannot misdirect the check in the dangerous direction:
    if it were actually a value-taking flag we don't recognize, its value
    is simply examined next as a candidate "verb" word, fails to match
    ISSUE_VERBS/"merge", and this whole `gh` occurrence is correctly
    treated as not a match rather than mis-parsed. An unrecognized --repo
    value found HERE (pre-verb) is a narrow, accepted residual — it is not
    forced ambiguous the way a post-verb one is (see `repo_from_value`),
    since a pre-verb flag is already the edge case this function exists
    for; if the command also carries no --repo after the verb, `derive_
    repo_slug`'s cwd-origin fallback applies exactly as it would if no
    --repo had been given at all.
    """
    i = start
    n = len(tokens)
    repo = None
    while i < n and tokens[i].startswith("-"):
        t = tokens[i]
        if t in ("--repo", "-R"):
            if i + 1 < n:
                r, recognized = repo_from_value(tokens[i + 1])
                if recognized:
                    repo = r
            i += 2
            continue
        if t.startswith("--repo=") or t.startswith("-R="):
            r, recognized = repo_from_value(t.split("=", 1)[1])
            if recognized:
                repo = r
            i += 1
            continue
        if t.startswith("-R") and len(t) > 2 and not t.startswith("--"):
            r, recognized = repo_from_value(t[2:])
            if recognized:
                repo = r
            i += 1
            continue
        i += 1
    return i, repo


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

    Round-3 fix (auditor A3): `gh`/`issue`|`pr` no longer has to be
    IMMEDIATELY followed by the verb word — `_skip_flags_before_verb`
    tolerates a flag (most importantly --repo/-R) in between, matching
    real `gh`'s own Cobra-based parsing.
    """
    n = len(tokens)
    i = 0
    while i < n:
        if tokens[i] == "gh" and i + 1 < n and tokens[i + 1] in ("issue", "pr"):
            sub = tokens[i + 1]
            j, pre_repo = _skip_flags_before_verb(tokens, i + 2)
            if j < n:
                verb = tokens[j]
                if sub == "issue" and verb in ISSUE_VERBS:
                    return ("issue",) + extract_repo_and_number(tokens[j + 1:], "issue", repo=pre_repo)
                if sub == "pr" and verb == "merge":
                    return ("merge",) + extract_repo_and_number(tokens[j + 1:], "merge", repo=pre_repo)
        i += 1
    return None


def tokenize_line(line):
    lex = shlex.shlex(line, posix=True, punctuation_chars=True)
    lex.whitespace_split = True
    # Round-3 fix (auditor A1): shlex.shlex defaults `commenters` to '#',
    # which the `shlex.split()` convenience wrapper this round replaced
    # does NOT do (it sets commenters=''). Bash only treats `#` as a
    # comment introducer at the START of a word; shlex without this reset
    # treats it mid-word too, so `echo a#b; gh issue close 123` silently
    # discarded everything from the `#` onward — a fully silent, zero-note
    # allow of a write to a held issue, and a regression against round 1,
    # which correctly denied this exact string.
    lex.commenters = ""
    return list(lex)


def split_logical_lines(raw):
    """Split `raw` into the logical lines `tokenize_line` should each
    process independently — the way bash itself decides where a "line"
    ends, not a naive `str.split("\\n")` (round-3 fix, reviewer N1, a
    regression this round's own naive newline-pre-split introduced — see
    this script's header KNOWN LIMITATIONS / the doc above `CLAIM_CHECK_PY`
    for the full incident). A bare newline is a split point only when it
    occurs OUTSIDE any `'...'`/`"..."` quoted region — inside one, it is
    preserved verbatim as part of that logical line, exactly as bash
    itself would never split a multi-line quoted argument. A backslash
    immediately before such an outside-quote newline is a line
    continuation: both characters are dropped, joining the next physical
    line onto the current logical one. Outside quotes, a backslash before
    ANY other character (including a quote character) is also consumed
    together with it, so an escaped quote (`\\'`/`\\"`) outside a quoted
    region is never mistaken for the start of one. This does not attempt
    to fully reproduce bash's own escaping rules inside double quotes —
    only enough to correctly decide whether a `"` closes the region, which
    is all a caller that hands the reassembled line to `shlex` afterward
    needs.
    """
    lines = []
    current = []
    quote = None
    i = 0
    n = len(raw)
    while i < n:
        c = raw[i]
        if quote:
            current.append(c)
            if c == quote:
                quote = None
            elif quote == '"' and c == "\\" and i + 1 < n:
                current.append(raw[i + 1])
                i += 2
                continue
            i += 1
            continue
        if c == "\\" and i + 1 < n:
            nxt = raw[i + 1]
            if nxt == "\n":
                # Backslash-newline continuation, outside any quote: drop
                # both, keep going on the same logical line.
                i += 2
                continue
            current.append(c)
            current.append(nxt)
            i += 2
            continue
        if c == "'" or c == '"':
            quote = c
            current.append(c)
            i += 1
            continue
        if c == "\n":
            lines.append("".join(current))
            current = []
            i += 1
            continue
        current.append(c)
        i += 1
    lines.append("".join(current))
    return lines


def main():
    raw = sys.stdin.read()
    for line in split_logical_lines(raw):
        try:
            tokens = tokenize_line(line)
        except ValueError as exc:
            # Round-3 fix (auditor A5): recover PER LOGICAL LINE, not
            # whole-command — a `tokfail` record lets the bash side surface
            # a visible note for just this one line while every other line
            # is still gated normally. The kind/repo/number/status shape is
            # reused so the bash reader needs no second record format.
            print("tokfail\x1f\x1f\x1funbalanced quoting on one logical line: {}".format(exc))
            continue
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

# Round-3 fix (reviewer N3): a `deny`/`ask` verdict is now RECORDED here
# instead of immediately printed and exited. `main_hook`'s per-match loop
# used to `exit 0` on the very first `ask`/`deny` it produced, so a chain
# whose FIRST gated segment was merely ambiguous never even evaluated a
# LATER segment's confident `deny` (a real held-issue violation) — the user
# got prompted about the wrong thing, and approving that prompt ran the
# whole command, violation included. `finish_hook` (below) evaluates every
# segment first, then emits exactly one verdict using the precedence
# `deny > ask > allow`, matching Claude Code's own documented decision
# precedence (`deny > defer > ask > allow`) — this script does not use
# `defer`, so the effective order here is `deny > ask > allow`. Reasons
# from every segment that produced one are merged into that single
# response, rather than only the first segment reached.
DENY_REASONS=()
ASK_REASONS=()

add_note() {
    NOTES+=("$1")
    # Keep the stderr line too — cheap, and useful for anyone tailing the
    # debug log directly; but per the fix above, never rely on this alone.
    echo "check-issue-claim.sh: $1" >&2
}

record_deny() {
    DENY_REASONS+=("$1")
}

record_ask() {
    ASK_REASONS+=("$1")
}

# Round-3 fix (reviewer N3): the single exit point for `main_hook`, called
# once after every segment of the command has been evaluated. `deny` beats
# `ask` beats a plain allow — see `DENY_REASONS`/`ASK_REASONS`'s own doc
# above for why this can no longer be "whichever segment came first".
#
# Round-3 fix (reviewer's own Priority 3 residual, closed as a side effect
# of N3 rather than deferred): a `deny`/`ask` decision now carries any
# `NOTES` accumulated from OTHER segments as `systemMessage` in the SAME
# response, instead of discarding them. Before N3, `deny_json`/`ask_json`
# `exit`ed immediately on the first blocking verdict, so a note from an
# earlier fail-open segment (an unresolvable repo, an unresolvable `gh pr
# view`) was silently dropped whenever a later segment denied or asked.
# Now that every segment is evaluated before any response is built, both
# can be reported together — the decision is the thing that blocks or
# prompts; the notes are the record of what ELSE happened in the same
# command that a human reading the transcript should also see.
finish_hook() {
    local decision="" reason="" notes_joined=""
    if [ "${#DENY_REASONS[@]}" -gt 0 ]; then
        decision="deny"
        reason="blocked by check-issue-claim.sh (issue #286):
$(printf '%s\n\n' "${DENY_REASONS[@]}")"
        reason="${reason%$'\n\n'}"
    elif [ "${#ASK_REASONS[@]}" -gt 0 ]; then
        decision="ask"
        reason="check-issue-claim.sh (issue #286):
$(printf '%s\n\n' "${ASK_REASONS[@]}")"
        reason="${reason%$'\n\n'}"
    fi
    if [ "${#NOTES[@]}" -gt 0 ]; then
        notes_joined="$(printf '%s\n' "${NOTES[@]}")"
    fi
    if [ -n "$decision" ] && [ -n "$notes_joined" ]; then
        jq -n --arg dec "$decision" --arg reason "$reason" --arg msg "$notes_joined" \
            '{hookSpecificOutput: {hookEventName: "PreToolUse", permissionDecision: $dec, permissionDecisionReason: $reason}, systemMessage: $msg}'
    elif [ -n "$decision" ]; then
        jq -n --arg dec "$decision" --arg reason "$reason" \
            '{hookSpecificOutput: {hookEventName: "PreToolUse", permissionDecision: $dec, permissionDecisionReason: $reason}}'
    elif [ -n "$notes_joined" ]; then
        jq -n --arg msg "$notes_joined" '{systemMessage: $msg}'
    fi
    exit 0
}

# Runs claim-check for issue $2 of repo $1 (cwd $3) and reacts per its tier
# (see run_claim_check's doc): tier 0 does nothing (the caller's loop moves
# to the next match); tier 1 records a deny reason (`record_deny`); tier 4
# records an ask reason (`record_ask`) — round-3 fix (reviewer N3): neither
# exits immediately any more, both defer to `finish_hook` once every
# segment has been evaluated; tier 3 allows but leaves a visible note (via
# `add_note`). $4 is a note prefix for the deny/ask reason text ("merging
# this PR would close issue #N, and " for the merge path, empty for a
# direct gh issue write).
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
        record_deny "${note}\`issue claim-check\` refused — $(sanitize_reason "$CLAIM_CHECK_REASON")"
        ;;
    4)
        record_ask "${note}\`issue claim-check\` could not confirm — $(sanitize_reason "$CLAIM_CHECK_REASON")"
        ;;
    *)
        add_note "\`issue claim-check\` for issue #$issue of $repo could not determine an answer (operational failure — binary missing, gh auth/network issue, or the caller is not in a linked worktree) — allowing without a claim check: $(sanitize_reason "$CLAIM_CHECK_REASON")"
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
    # through `add_note`/`finish_hook` (both need jq to build valid JSON) —
    # hand-write a fixed, content-free JSON literal instead.
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
        # python3 missing entirely, or the tokenizer crashed outright — the
        # per-LOGICAL-LINE unbalanced-quoting case is now recovered inside
        # the Python helper itself (round-3 fix, auditor A5 — see the
        # `tokfail` record handled in the loop below) and never reaches
        # here. Priority 1 tier 3: could-not-determine. Allow, but say so
        # visibly (systemMessage, not just stderr) so this is never a
        # silent bypass.
        add_note "could not run the tokenizer for claim checking (python3 missing, or the tokenizer failed) — allowing without a claim check: $command"
        finish_hook
    fi
    if [ "${#MATCHES[@]}" -eq 0 ]; then
        finish_hook
    fi

    local m kind repo number ext_status
    for m in "${MATCHES[@]}"; do
        IFS=$'\x1f' read -r kind repo number ext_status <<<"$m"

        if [ "$kind" = "tokfail" ]; then
            # Round-3 fix (auditor A5): one logical line failed to tokenize
            # (genuinely unbalanced quoting) — allow just that line without
            # a claim check, visibly, and keep evaluating every OTHER
            # match already extracted from lines that tokenized fine. This
            # is what makes the recovery per-line rather than
            # whole-command: a stray apostrophe in one heredoc's prose no
            # longer defeats a `gh issue close` on an entirely different
            # line of the same command.
            add_note "could not tokenize one logical line of the command (unbalanced quoting) — allowing that line without a claim check while any other lines/segments are still checked normally: $ext_status"
            continue
        fi

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
            # Round-3 fix (reviewer N3): record and move on to the NEXT
            # match instead of exiting immediately — a later segment in
            # the same command may still resolve to a confident `deny`,
            # which must not be skipped just because an earlier one was
            # merely ambiguous. `finish_hook` picks the strongest verdict
            # across everything recorded.
            local verb_desc
            if [ "$kind" = "merge" ]; then
                verb_desc="gh pr merge"
            else
                verb_desc="gh issue comment/close/edit/…"
            fi
            record_ask "this command matches a gated \`${verb_desc}\` form, but the issue/PR number could not be unambiguously determined from it — refusing to guess rather than risk checking the wrong one. Confirm this is safe, or re-run it with an explicit, literal issue/PR number. Command: $command"
            continue
        fi

        if [ "$kind" = "merge" ]; then
            local text numbers n count
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
            # Round-3 fix (reviewer N5): bound the number of closing
            # references gated per `gh pr merge` invocation — each one is a
            # full `worker-agent-deck issue claim-check` subprocess, itself
            # `gh issue view` + `gh api user`, all sequential and sharing
            # the hook's single 15s timeout budget (see the header's KNOWN
            # LIMITATIONS). A PR body naming many closing references could
            # otherwise turn one tool call into an unbounded number of
            # sequential network round trips. Anything past the cap is
            # named in a visible note rather than silently skipped.
            count=0
            while IFS= read -r n; do
                [ -n "$n" ] || continue
                count=$((count + 1))
                if [ "$count" -gt "$MAX_CLOSING_REFS_PER_MERGE" ]; then
                    add_note "PR #$number of $repo names more closing references than the $MAX_CLOSING_REFS_PER_MERGE this hook checks per merge — only the first $MAX_CLOSING_REFS_PER_MERGE were checked; verify the remainder manually before merging."
                    break
                fi
                gate_or_allow "$repo" "$n" "$cwd" "merging this PR would close issue #$n, and "
            done <<<"$numbers"
        else
            gate_or_allow "$repo" "$number" "$cwd" ""
        fi
    done
    finish_hook
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

    # --- Round-3 fix round scenarios (PR #573 fix round 4, issue #286) ---
    # Reuse the "issue 123 HELD, everything else CLEAR" stub scenarios
    # 11-15 established.
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

    # --- Scenario 22 (reviewer N1a): a multi-line quoted --body argument
    # is no longer split in half by the newline pre-split. ---
    run_bypass_scenario "a multi-line quoted --body argument (N1a)" \
        "$(printf 'gh issue close 123 --repo acme/widgets --body "line one\nline two"')"

    # --- Scenario 23 (reviewer N1b): a backslash line-continuation, this
    # repo's own house style for long gh invocations, is joined rather
    # than split. ---
    run_bypass_scenario "a backslash-continued command (N1b)" \
        "$(printf 'gh issue close 123 \\\n  --repo acme/widgets')"

    # --- Scenario 24 (reviewer N1c): a report-then-close two-command
    # sequence — CLAUDE.md rule 25's own \`gh pr comment\` merge-report
    # shape (not itself gated) followed by a genuinely gated close on a
    # later line. ---
    run_bypass_scenario "a report-then-close sequence (N1c)" \
        "$(printf 'gh pr comment 5 --body "Merge report\n\n- CI green\n"\ngh issue close 123 --repo acme/widgets')"

    # --- Scenario 25 (auditor A5): a per-LINE tokenization failure (a
    # genuinely unbalanced quote on a LATER line) no longer discards a
    # perfectly well-formed gated command on an EARLIER line of the same
    # multi-line command — the old whole-command failure would have
    # silently allowed the close below. ---
    run_bypass_scenario "per-line recovery when a later line is unbalanced (A5)" \
        "$(printf 'gh issue close 123 --repo acme/widgets\necho "unterminated')"

    # --- Scenario 26 (reviewer N2): a --repo value that is genuinely
    # unparseable (not even gh's own [HOST/]OWNER/REPO shape) must force
    # ASK, never silently fall back to a different repo. ---
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
echo "self-test FAILED: worker-agent-deck should never be invoked when --repo is unparseable" >&2
exit 1
EOF
    chmod +x "$fake_bin/worker-agent-deck"
    local input_badrepo out26 decision26
    input_badrepo=$(jq -n --arg cwd "$tmp/repo" '{
        tool_name: "Bash",
        tool_input: {command: "gh issue close 123 --repo not_a_valid_repo_value!!!"},
        cwd: $cwd
    }')
    out26="$(PATH="$fake_bin:$PATH" bash "$0" <<<"$input_badrepo")"
    decision26="$(jq -r '.hookSpecificOutput.permissionDecision // empty' <<<"$out26" 2>/dev/null)"
    if [ "$decision26" != "ask" ]; then
        echo "self-test FAILED: an unparseable --repo value must force ask, never silently fall back to a different repo; got:" >&2
        printf '%s\n' "$out26" >&2
        fail=1
    else
        echo "self-test ok: an unparseable --repo value (N2) forces ask rather than silently checking a different repo"
    fi

    # --- Scenario 27 (reviewer N2): gh's own [HOST/]OWNER/REPO three-
    # segment --repo form is correctly recognized, and the check runs
    # against THAT repo, not cwd's origin — repo2's origin is a DIFFERENT
    # repo (scenario 10's fixture), so a pass here proves the
    # host-qualified value was used, not merely that the fallback happened
    # to match. ---
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
if [ "$5" != "acme/widgets" ]; then
    echo "self-test FAILED: claim-check called with repo=$5, expected the host-qualified --repo value acme/widgets (host stripped), not cwd's origin (wrongowner/wrongrepo)" >&2
    exit 1
fi
echo "issue claim-check: issue #999 of acme/widgets is held by \`orch-a\` — held by another agent" >&2
exit 1
EOF
    chmod +x "$fake_bin/worker-agent-deck"
    local input_hostrepo out27 decision27 reason27
    input_hostrepo=$(jq -n --arg cwd "$tmp/repo2" '{
        tool_name: "Bash",
        tool_input: {command: "gh issue close 999 --repo github.com/acme/widgets"},
        cwd: $cwd
    }')
    out27="$(PATH="$fake_bin:$PATH" bash "$0" <<<"$input_hostrepo")"
    decision27="$(jq -r '.hookSpecificOutput.permissionDecision // empty' <<<"$out27" 2>/dev/null)"
    reason27="$(jq -r '.hookSpecificOutput.permissionDecisionReason // empty' <<<"$out27" 2>/dev/null)"
    if [ "$decision27" != "deny" ] || [[ "$reason27" != *"acme/widgets"* ]]; then
        echo "self-test FAILED: a HOST/OWNER/REPO --repo value must be recognized and checked against the right repo; got:" >&2
        printf '%s\n' "$out27" >&2
        fail=1
    else
        echo "self-test ok: gh's own [HOST/]OWNER/REPO --repo form (N2) is correctly extracted, not silently falling back to cwd's origin"
    fi

    # --- Scenario 28 (auditor A1): shlex's default '#' comment character
    # no longer truncates the tokenizer mid-command. ---
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
    run_bypass_scenario "a mid-word # no longer truncates the command (A1)" "echo a#b; gh issue close 123 --repo acme/widgets"

    # --- Scenario 29 (auditor A2): a shell redirect before the positional
    # (2>/dev/null) no longer aims the check at the bare fd number — it
    # must force ask, never silently check the wrong issue. ---
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
echo "self-test FAILED: worker-agent-deck should never be invoked when a redirect precedes the positional" >&2
exit 1
EOF
    chmod +x "$fake_bin/worker-agent-deck"
    local input_redirect out29 decision29
    input_redirect=$(jq -n --arg cwd "$tmp/repo" '{
        tool_name: "Bash",
        tool_input: {command: "gh issue close 2>/dev/null 123 --repo acme/widgets"},
        cwd: $cwd
    }')
    out29="$(PATH="$fake_bin:$PATH" bash "$0" <<<"$input_redirect")"
    decision29="$(jq -r '.hookSpecificOutput.permissionDecision // empty' <<<"$out29" 2>/dev/null)"
    if [ "$decision29" != "ask" ]; then
        echo "self-test FAILED: a redirect before the positional (2>/dev/null 123) must force ask, never silently check the fd number instead; got:" >&2
        printf '%s\n' "$out29" >&2
        fail=1
    else
        echo "self-test ok: a shell redirect before the positional (A2) forces ask rather than checking the wrong (fd) number"
    fi

    # --- Scenario 30 (auditor A3): a flag between the subcommand and the
    # verb (gh issue --repo r/r close N) is no longer missed entirely. ---
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
if [ "$3" != "123" ] || [ "$5" != "acme/widgets" ]; then
    echo "self-test FAILED: claim-check called with issue=$3 repo=$5, expected issue=123 repo=acme/widgets" >&2
    exit 1
fi
echo "issue claim-check: issue #123 of acme/widgets is held by \`orch-a\` — held by another agent" >&2
exit 1
EOF
    chmod +x "$fake_bin/worker-agent-deck"
    run_bypass_scenario "a flag between the subcommand and the verb (A3)" "gh issue --repo acme/widgets close 123"

    # --- Scenario 31 (auditor A4): the newly-gated verbs (reopen/delete/
    # transfer/…) are checked, not skipped. ---
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
if [ "$3" != "123" ]; then
    echo "self-test FAILED: claim-check called with issue=$3, expected 123" >&2
    exit 1
fi
echo "issue claim-check: issue #123 of acme/widgets is held by \`orch-a\` — held by another agent" >&2
exit 1
EOF
    chmod +x "$fake_bin/worker-agent-deck"
    run_bypass_scenario "a newly-gated verb, gh issue delete (A4)" "gh issue delete 123 --repo acme/widgets"
    run_bypass_scenario "a newly-gated verb, gh issue reopen (A4)" "gh issue reopen 123 --repo acme/widgets"
    run_bypass_scenario "a newly-gated verb, gh issue transfer (A4)" "gh issue transfer 123 other/repo --repo acme/widgets"

    # --- Scenario 32 (reviewer N4): two gh invocations inside the same
    # segment via command substitution are BOTH reached now that ( and )
    # are separators, not just the first. ---
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
case "$3" in
1)
    echo "ok to proceed on issue #1 of acme/widgets as \`human:x@h\`"
    exit 0
    ;;
123)
    echo "issue claim-check: issue #123 of acme/widgets is held by \`orch-a\` — held by another agent" >&2
    exit 1
    ;;
*)
    echo "self-test FAILED: unexpected issue $3" >&2
    exit 1
    ;;
esac
EOF
    chmod +x "$fake_bin/worker-agent-deck"
    run_bypass_scenario "command substitution reaches the inner gh call too (N4)" \
        'gh issue close 1 --repo acme/widgets $(gh issue close 123 --repo acme/widgets)'

    # --- Scenario 33 (reviewer N3): an earlier AMBIGUOUS segment no longer
    # suppresses a later segment's confident DENY — the whole command must
    # still deny, naming the real violation. ---
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
if [ "$3" = "123" ]; then
    echo "issue claim-check: issue #123 of acme/widgets is held by \`orch-a\` — held by another agent" >&2
    exit 1
fi
echo "self-test FAILED: worker-agent-deck should not be invoked for any issue but 123 in this scenario" >&2
exit 1
EOF
    chmod +x "$fake_bin/worker-agent-deck"
    local input_ask_then_deny out33 decision33 reason33
    input_ask_then_deny=$(jq -n --arg cwd "$tmp/repo" '{
        tool_name: "Bash",
        tool_input: {command: "gh issue comment $N --body x --repo acme/widgets; gh issue close 123 --repo acme/widgets"},
        cwd: $cwd
    }')
    out33="$(PATH="$fake_bin:$PATH" bash "$0" <<<"$input_ask_then_deny")"
    decision33="$(jq -r '.hookSpecificOutput.permissionDecision // empty' <<<"$out33" 2>/dev/null)"
    reason33="$(jq -r '.hookSpecificOutput.permissionDecisionReason // empty' <<<"$out33" 2>/dev/null)"
    if [ "$decision33" != "deny" ] || [[ "$reason33" != *"#123"* ]]; then
        echo "self-test FAILED: an earlier ambiguous segment must not suppress a later confident deny (deny > ask precedence); got:" >&2
        printf '%s\n' "$out33" >&2
        fail=1
    else
        echo "self-test ok: an earlier ambiguous segment does not suppress a later segment's confident deny (N3's deny > ask > allow precedence)"
    fi

    # --- Scenario 34 (round-2 M3 / round-3 N6): the three closing-keyword
    # forms beyond bare #N — GH-N, owner/repo#N, and the full issue URL —
    # are pinned, not just described in prose. Same title-pinning gh stub
    # discipline as scenario 4. ---
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
    case "$PR_VIEW_BODY_MARKER" in
    GH) echo "Fixed GH-778" ;;
    OWNERREPO) echo "resolves acme/widgets#778" ;;
    URL) echo "Closes https://github.com/acme/widgets/issues/778" ;;
    esac
    exit 0
fi
echo "self-test FAILED: unexpected gh invocation: $*" >&2
exit 1
EOF
    chmod +x "$fake_bin/gh"
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
if [ "$3" != "778" ]; then
    echo "self-test FAILED: claim-check called with issue $3, expected 778" >&2
    exit 1
fi
echo "issue claim-check: issue #778 of acme/widgets is held by \`orch-a\` — held by another agent" >&2
exit 1
EOF
    chmod +x "$fake_bin/worker-agent-deck"

    local closing_form input_closing out_closing decision_closing reason_closing
    for closing_form in GH OWNERREPO URL; do
        input_closing=$(jq -n --arg cwd "$tmp/repo" '{
            tool_name: "Bash",
            tool_input: {command: "gh pr merge 574 --squash"},
            cwd: $cwd
        }')
        out_closing="$(PATH="$fake_bin:$PATH" PR_VIEW_BODY_MARKER="$closing_form" bash "$0" <<<"$input_closing")"
        decision_closing="$(jq -r '.hookSpecificOutput.permissionDecision // empty' <<<"$out_closing" 2>/dev/null)"
        reason_closing="$(jq -r '.hookSpecificOutput.permissionDecisionReason // empty' <<<"$out_closing" 2>/dev/null)"
        if [ "$decision_closing" != "deny" ] || [[ "$reason_closing" != *"#778"* ]]; then
            echo "self-test FAILED: closing-keyword form $closing_form must be recognized and checked against issue #778; got:" >&2
            printf '%s\n' "$out_closing" >&2
            fail=1
        else
            echo "self-test ok: the $closing_form closing-keyword form is recognized and checked against the right issue (N6)"
        fi
    done
    rm -f "$fake_bin/gh"

    # --- Scenario 35 (auditor's structural suggestion): pin the
    # DOCUMENTED scope limits (header KNOWN LIMITATIONS) as expected-ALLOW
    # — asserted here, not merely described in prose, so a future
    # tokenizer change cannot silently move one of these from a safe allow
    # to a wrong-target check without a self-test scenario going red. ---
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
echo "self-test FAILED: worker-agent-deck should never be invoked for a documented out-of-scope shape" >&2
exit 1
EOF
    chmod +x "$fake_bin/worker-agent-deck"

    run_scope_limit_scenario() {
        local label="$1" cmd="$2"
        local inp o dec
        inp=$(jq -n --arg cwd "$tmp/repo" --arg cmd "$cmd" '{
            tool_name: "Bash",
            tool_input: {command: $cmd},
            cwd: $cwd
        }')
        o="$(PATH="$fake_bin:$PATH" bash "$0" <<<"$inp")"
        dec="$(jq -r '.hookSpecificOutput.permissionDecision // empty' <<<"$o" 2>/dev/null)"
        if [ -n "$dec" ]; then
            echo "self-test FAILED ($label): a documented out-of-scope shape must never produce a blocking decision; command was: $cmd; got:" >&2
            printf '%s\n' "$o" >&2
            fail=1
        else
            echo "self-test ok: $label stays within the documented scope limit (allow, never a blocking decision)"
        fi
    }

    run_scope_limit_scenario "an absolute path to gh" "/usr/bin/gh issue close 123"
    run_scope_limit_scenario "a bash -c wrapped command" "bash -c 'gh issue close 123'"
    run_scope_limit_scenario "a gh api REST call" "gh api repos/acme/widgets/issues/123/comments -f body=hi"

    # --- Scenario 36 (regression guard): a genuinely unbalanced quote,
    # with no other well-formed gated command anywhere in the input, still
    # correctly fails to tier 3 (allow + visible note) — N1's quote-aware
    # newline handling must not accidentally start ACCEPTING malformed
    # input. ---
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
echo "self-test FAILED: worker-agent-deck should never be invoked for a genuinely unparseable command" >&2
exit 1
EOF
    chmod +x "$fake_bin/worker-agent-deck"
    local input_unbalanced out36 decision36 sysmsg36
    input_unbalanced=$(jq -n --arg cwd "$tmp/repo" '{
        tool_name: "Bash",
        tool_input: {command: "gh issue close 123 --body \"unterminated"},
        cwd: $cwd
    }')
    out36="$(PATH="$fake_bin:$PATH" bash "$0" <<<"$input_unbalanced")"
    decision36="$(jq -r '.hookSpecificOutput.permissionDecision // empty' <<<"$out36" 2>/dev/null)"
    sysmsg36="$(jq -r '.systemMessage // empty' <<<"$out36" 2>/dev/null)"
    if [ -n "$decision36" ]; then
        echo "self-test FAILED: a genuinely unbalanced quote must allow (nothing to gate on confidently), not block; got:" >&2
        printf '%s\n' "$out36" >&2
        fail=1
    elif [[ "$sysmsg36" != *"unbalanced quoting"* ]]; then
        echo "self-test FAILED: a genuinely unbalanced quote must leave a visible systemMessage note, not a silent allow; got: $out36" >&2
        fail=1
    else
        echo "self-test ok: a genuinely unbalanced quote still correctly fails to tier 3 (allow + visible note), not silently and not by denying"
    fi

    # --- Scenario 37 (reviewer N5): a PR naming more closing references
    # than MAX_CLOSING_REFS_PER_MERGE is visibly capped — the rest are
    # named in a note, not silently skipped and not turned into an
    # unbounded number of sequential network round trips. ---
    cat >"$fake_bin/gh" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
    printf 'fixes #1, fixes #2, fixes #3, fixes #4, fixes #5, fixes #6, fixes #7, fixes #8, fixes #9, fixes #10, fixes #11\n'
    exit 0
fi
echo "self-test FAILED: unexpected gh invocation: $*" >&2
exit 1
EOF
    chmod +x "$fake_bin/gh"
    cat >"$fake_bin/worker-agent-deck" <<'EOF'
#!/usr/bin/env bash
echo "ok to proceed on issue #$3 as human:x@h"
exit 0
EOF
    chmod +x "$fake_bin/worker-agent-deck"
    local input_manyrefs out37 decision37 sysmsg37
    input_manyrefs=$(jq -n --arg cwd "$tmp/repo" '{
        tool_name: "Bash",
        tool_input: {command: "gh pr merge 575 --squash"},
        cwd: $cwd
    }')
    out37="$(PATH="$fake_bin:$PATH" bash "$0" <<<"$input_manyrefs")"
    decision37="$(jq -r '.hookSpecificOutput.permissionDecision // empty' <<<"$out37" 2>/dev/null)"
    sysmsg37="$(jq -r '.systemMessage // empty' <<<"$out37" 2>/dev/null)"
    if [ -n "$decision37" ]; then
        echo "self-test FAILED: capping closing references must not itself block the merge; got:" >&2
        printf '%s\n' "$out37" >&2
        fail=1
    elif [[ "$sysmsg37" != *"only the first $MAX_CLOSING_REFS_PER_MERGE were checked"* ]]; then
        echo "self-test FAILED: more closing references than the cap must produce a visible note naming the cap; got: $out37" >&2
        fail=1
    else
        echo "self-test ok: a PR naming more closing references than the $MAX_CLOSING_REFS_PER_MERGE cap is visibly capped, not silently unbounded (N5)"
    fi
    rm -f "$fake_bin/gh"

    if [ "$fail" -ne 0 ]; then
        exit 1
    fi
    echo "self-test ok: all scenarios passed — check-issue-claim.sh blocks a refused claim-check, asks on ambiguity, allows on a clear or could-not-determine result, and (round-2/round-3 fixes) no longer silently bypasses a newline/unspaced-operator/non-gh-led chain, a short-flag redirection, an unrecognized-flag ambiguity, a multi-line quoted argument, a backslash continuation, a mid-word #, a redirect before the positional, an unparseable/host-qualified --repo value, a flag between subcommand and verb, or an ungated verb — and evaluates every segment before responding rather than stopping at the first ask"
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
