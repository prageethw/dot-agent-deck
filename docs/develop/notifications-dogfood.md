# Agent-Driven Notifications — Retired ntfy Dogfood (Historical)

> **Retired 2026-07-28.** This page used to document a config-only notification dogfood on *this repo's own* orchestration: a `scripts/notify.sh` helper that `curl`-POSTed to a public [ntfy](https://ntfy.sh) topic, called from per-role notify instructions in every role's `prompt_template`. That setup no longer exists. The script is deleted, the ntfy topic is abandoned, and the per-worker `blocked` pings are gone.

## What replaced it

[PRD #126](../../prds/126-agent-driven-notifications.md) was rescoped from "dogfood, no deck code" into a shipped feature plus a much smaller recipe. Two things came out of it:

- **A daemon-side idle-worker detector** (deck code) — the daemon tracks each outstanding delegation and, after `worker_response_timeout_minutes` (default 120, `0` disables) with no `work-done`, injects one self-describing prompt into the orchestrator. The daemon never notifies; it only reports the condition, and the orchestrator decides what to do.
- **An orchestrator-only notification recipe** — for a while the `orchestrator` role's `prompt_template` in `.dot-agent-deck.toml` sent a short, fire-and-forget Telegram message (via a `telegram` MCP server in `.mcp.json`) at the workflow's five pause-for-human moments, including the daemon's idle-worker event. Workers never notified and never waited on the user: a blocked worker returned its question through `work-done` and the orchestrator escalated.

**The recipe was retired here too (2026-08-06).** The Telegram and Slack MCP servers and the orchestrator's notification protocol were removed from this repo's `.mcp.json`, `.env.vals.yaml` and `.dot-agent-deck.toml`, so nothing in this repository sends messages anywhere any more. The daemon-side idle-worker detector is unaffected and still ships — it is deck code, and `worker_response_timeout_minutes` is still configured here. The worker-escalation topology also survives: workers still never notify and never wait on the user.

The user-facing documentation for both — the feature and an example recipe with placeholders — lives in the published `docs/` tree, not here, and that page is now written as a worked example a reader can adopt rather than as a description of our setup. While the recipe was wired, the orchestrator also kept a minimal expectation log at `.dot-agent-deck/notify-log.md` (gitignored) so that "reached a notify moment but never sent" stayed detectable after a compaction dropped the instruction (PRD #82); that went with the rest of the wiring.

## Why the history is worth keeping

The dogfood is what produced the design, so its findings are recorded in the PRD's [Background](../../prds/126-agent-driven-notifications.md#background-the-dogfood-that-led-here) section rather than repeated here. In short: agent-driven notification works and is genuinely fire-and-forget; "one `.mcp.json` for every agent" is a myth (only Claude reads it natively), which is exactly why orchestrator-only is the right topology; the public ntfy topic was acceptable only because the payload was one status sentence; and the one thing config provably could not do — notice a delegated worker that went silent — is what became the daemon feature.

The full retired setup (script internals, the ntfy topic caveat, the two-record expectation log, the reconciliation procedure) is in this file's git history if you ever need it: `git log --follow -- docs/develop/notifications-dogfood.md`.
