# The `issue claim-check` PreToolUse hook

`.claude/hooks/check-issue-claim.sh` is a Claude Code `PreToolUse` hook, wired into the **tracked** `.claude/settings.json` — so it runs for every contributor, on every clone, not just on one machine. It exists to make CLAUDE.md rule 14's `worker-agent-deck issue claim` check unnecessary to remember: before a `gh issue comment`/`close`/`edit`/`reopen`/`delete`/`lock`/`unlock`/`pin`/`unpin`/`transfer`/`develop` or a CLOSING `gh pr merge` reaches the shell, the hook parses the command, works out which issue it would write to, and shells out to the read-only `worker-agent-deck issue claim-check` to ask whether that issue is safe to act on.

## What triggers it

Every `Bash` tool call's raw command string is tokenized (Python's `shlex`, no shell evaluation — see the script's own header for why `eval`/`bash -c` are deliberately never used to parse). The tokenizer looks for a `gh issue <verb>` or `gh pr merge` invocation anywhere in the command, including inside a chain (`;`, `&&`, `||`, `&`, `|`, subshells, command substitution) and inside wrapper commands (`timeout 30 gh …`, `sudo gh …`). A command with no `gh` invocation at all — the overwhelming majority of `Bash` tool calls — is allowed instantly and never shells out to anything.

## What you'll see

The hook always exits 0 itself; its JSON on stdout carries the actual decision (`permissionDecision`) plus a `systemMessage` for anything worth surfacing without blocking:

- **`deny`** — `issue claim-check` refused: the issue is held by a different identity (worktree path, branch, host). The reason is shown to Claude, so the model can react to it directly (e.g. by not retrying the same write).
- **`ask`** — either `issue claim-check` could not confirm the identity (no claim comment names a holder), or the hook itself could not determine unambiguously which issue/PR the command targets (an unresolvable positional, an unrecognized `--repo` value, a shell redirect before the positional, …). The reason is shown to the user only, not to Claude — this is a genuine escalation, matching CLAUDE.md rule 14's own guidance to ask a human rather than silently adopt an ambiguous claim.
- **A visible `systemMessage`, no blocking decision** — the hook could not run the check at all (the `worker-agent-deck` binary is missing or stale, `gh`/network failed, the repo could not be derived) and is allowing the command through, but wants a human reading the transcript to know it didn't actually check anything. Stderr from an exit-0 hook never reaches the transcript, so this is the only visible channel for that.
- **Nothing at all** — the command was either unrelated to `gh`, or `issue claim-check` came back clear.

When a command contains more than one gated invocation, every one is evaluated before any response is built, and the single strongest verdict wins (`deny` beats `ask` beats a plain allow) — an earlier ambiguous segment never suppresses a later confident `deny`.

## Scope — read this before treating a green run as proof

This is an **accident-preventer for cooperating orchestrations, never a security enforcement boundary** — the exact framing CLAUDE.md rule 14's own incident (#257) calls for. It parses the `gh` CLI client-side; `gh api` REST calls that perform the same writes are entirely out of scope and always will be, along with `bash -c`-wrapped commands, an absolute/path-qualified `gh` invocation, and a shell variable holding the issue number. The script's own header carries the current, authoritative KNOWN LIMITATIONS list — read it there rather than here, since this page is not regenerated when that list changes.

## If it blocks or asks you unexpectedly

Read the reason text — it names the holder (for a `deny`) or the specific ambiguity (for an `ask`). If the claim is stale (the holding orchestration has genuinely stopped), CLAUDE.md rule 23 covers `worker-agent-deck issue claim --takeover --confirm-stopped`. If the hook is simply wrong about which issue a command targets — an unusual quoting form, a flag it doesn't recognize — that is exactly the residual scope this hook accepts; re-run with an explicit, literal issue/PR number, or proceed by hand.

## Testing it yourself

```sh
.claude/hooks/check-issue-claim.sh --self-test
```

Runs entirely offline against fabricated stubs (mirrors `scripts/check-symlinks.sh --self-test`'s convention) and is exercised on every push in both the Linux `build` job and the macOS `build-macos` job (the latter added specifically to settle bash-3.2 portability empirically rather than by inspection). If you touch the script, run this locally before pushing — see CLAUDE.md rule 2 for why this stays a local lint/build check rather than joining the CI-only test tiers.
