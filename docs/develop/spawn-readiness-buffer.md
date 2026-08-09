# `DOT_AGENT_DECK_SPAWN_READINESS_BUFFER_MS`

> **Developer / maintainer reference.** This page documents an internal tuning knob and is intentionally excluded from the published documentation site.

Overrides `SPAWN_TIME_READINESS_BUFFER` (`src/ui.rs`), the delay the TUI waits after an orchestration role's agent is judged "ready" (a `SessionStart` observed, or the 10s no-`SessionStart` fallback) before writing its spawn-time / orchestrator seed prompt. The default is 500ms — a brief pause meant to outlast the window where a freshly-spawned agent (large MCP server count, big hook payload, a loaded machine) isn't actually ready to accept input yet, even though it has reported `SessionStart`.

## Who reads it

`spawn_readiness_buffer()` is the sole production reader, called from `deliver_orchestrator_prompt` on every render frame (~62Hz) while a role prompt is pending. The env var is re-read and re-parsed on **every call** — only the warning below is deduplicated, not the parse itself.

## Format and clamp

A non-negative integer number of milliseconds. Invalid values (unparseable, negative) fall back to the 500ms default and log a one-time `WARN`. Valid values are clamped to **[50ms, 30s]** and out-of-range values also log a one-time `WARN` naming the requested and clamped value:

- **Floor (50ms):** `0` no longer disables the buffer outright — it clamps to the floor instead of reopening the timing hole this mechanism exists to close.
- **Ceiling (30s):** keeps the first write comfortably inside `AUTOMATIC_PROMPT_DEADLINE` (60s), leaving room for the write itself, one confirmation retry, and a hook round trip.

## Interaction with the 10s no-`SessionStart` fallback

A role whose agent never reports `SessionStart` still waits out this same buffer (via `should_inject_spawn_time_prompt`) once the 10s fallback engages, rather than firing with a zero-buffer at exactly the 10s mark. A large override therefore delays that fallback path too — bounded by the 30s ceiling above, so it can't push the write past the deadline.

## Nothing sets this today

No test, script, or shipped config currently sets this variable — it exists purely as an escape hatch for a machine or environment where the default 500ms buffer proves too short.
