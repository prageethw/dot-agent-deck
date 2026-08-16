# PRD fork#378 — Show the active model alongside the agent-type badge

**Status** *(added 2026-08-16)*: **Merged into the fork** — [PR #379](https://github.com/prageethw/dot-agent-deck/pull/379) (`41e42a9c`), first released in **v0.38.3**. Issue #378 closed. The doc previously had no Status line despite the PRD having shipped.
**Issue:** [prageethw/dot-agent-deck#378](https://github.com/prageethw/dot-agent-deck/issues/378)
**Predecessor:** fork #339 (agent-type badge toggle) — this extends it; it does not replace it.
**Experimental flag:** No (CLAUDE.md rule 9 — extends an existing surface, and stays hidden by default behind the existing toggle).

## Problem

Fork #339 shipped a deck-global agent-type badge on session cards — `ClaudeCode`, `OpenCode`, `Pi`, `Codex`, `Devin` — off by default, toggled with `Ctrl+D` (to command mode) then `Ctrl+M` (or a bare `m`). It shows *which tool* is running but not *which model*, so two panes running Opus and Haiku under the same tool are indistinguishable.

Nothing in the deck knows an agent's model. `config.rs`'s `model` field is `idle_art.model` (the ASCII-art LLM) and is unrelated. Roles carry only an opaque `command = "devbox run claude-opus-devbox"`, so the model is inferable today only by string-matching that command — which this PRD rejects as a hard-coded label.

## Outcome

The existing badge gains a model segment:

```
ClaudeCode (Opus) · my-session
Codex (gpt-5.1-codex-mini) · worker-01
```

Everything else — hidden by default, the `Ctrl+D → Ctrl+M` toggle, immediate application to existing and newly created panes — comes free by reusing #339's single toggle and single render seam rather than adding a second mechanism.

## Decisions

| Question | Decision |
|---|---|
| Model source | **Runtime capture from the agent's hook payload.** Not a config field, not command-string parsing. |
| Experimental flag | **No.** |
| Surface scope | **Card titles only** — `render_session_card`, the one existing seam. No embedded-pane-border work. |
| Protocol version | **No bump expected.** The field is additive; confirmed by the rule 12 cross-version check rather than assumed. |

## Design

### 1. Capture — `src/hook.rs`, `src/event.rs`

The hook payload **already carries the model as a top-level key**. `tests/codex_hook_ingestion.rs:217` is a schema-accurate Codex `PostToolUse` payload containing `"model": "gpt-5.1-codex-mini"`, and Codex posts the same stdin shape as Claude Code (`build_event_typed`'s doc comment). Today it lands in `ClaudeCodeHookInput`'s `#[serde(flatten)] _extra` map and is discarded.

- Add a named `model: Option<String>` field to `ClaudeCodeHookInput` (and `OpenCodeHookInput` if that shape carries one) — a **named top-level field**, *not* a widening of the `_extra`/`metadata` passthrough. The narrow-metadata constraint at `src/hook.rs:356` must be preserved: *"ONE key, only on `SessionStart`, only the one value the repo defines… this cannot become an arbitrary producer-controlled channel."*
- Add `model: Option<String>` to `AgentEvent`, following the `agent_version` precedent exactly (`src/event.rs:599`): `#[serde(default, skip_serializing_if = "Option::is_none")]`, plus an entry in the field-list doc comment above the struct marked "added additively".
- That keeps the wire additive: an older peer decodes a newer payload as `None`, a newer peer decodes an older payload as `None`, and existing producers emit byte-identical JSON. So **no `PROTOCOL_VERSION` bump and no `.breaking.md` fragment**.

Because the key appears on `PostToolUse` and not only `SessionStart`, a mid-session model change is observable — which is what makes the runtime-change requirement satisfiable.

### 2. Store — `src/state.rs`

Add `SessionState.model: Option<String>`, updated in `apply_event`:

- an event carrying `Some(m)` sets it (a later, different `m` overwrites — this is the runtime-change path);
- an event carrying `None` **must not clear** a previously-known model, since most events won't carry one.

### 3. Render — `src/ui.rs` (~19302)

Append the model inside the existing badge span so it stays one registry-coloured, bold segment:

```
model known   →  ClaudeCode (Opus) · my-session
model unknown →  ClaudeCode · my-session          (unchanged)
```

**Shape-matching contract.** The comment at `src/ui.rs:19311` states the `<type> · …` shape *"callers match on (e.g. `Codex ·`, `Pi · orch-01`) stays intact"*. Inserting ` (Model)` between the type and the `·` breaks that for sessions that have a model. Existing synthetic tests use sessions with no model so they should be unaffected — confirm this rather than assume it, and update the comment to describe the new shape.

Keep it compact: no model known means no brackets at all, never an empty `()` or a placeholder.

## Test plan

| Catalog ID | Tier | Scenario | Action |
|---|---|---|---|
| `dashboard/agent-badge/001` | L1 widget snapshot | Toggle on: a session with a known model renders `<Label> (<Model>) · <name>` in the registry colour + BOLD; one with no model renders the bare `<Label> · <name>`; toggle off renders neither. Covers **formatting** + **hidden by default**. | extend |
| `dashboard/agent-badge/002` | pure-data unit | Toggle cycles hidden → shown → hidden with the existing status messages, unchanged by the model segment. Covers **toggle on/off**. | extend |
| `dashboard/agent-badge/003` | L2 PTY (real binary) | Pressing `m` toggles the badge on every card; cards with a model show it. The only default-hidden coverage through the real `render_frame`. Covers **existing + newly created panes**. | extend |
| `dashboard/agent-badge/004` | L2 synthetic | A model arrives on a later event and the badge updates; a second event with a different model updates it again; an event with no model leaves it intact. Covers **runtime model changes**. | create |
| `events/schema/…` | pure-data unit | A payload lacking `model` decodes as `None`; `Some(m)` round-trips; `None` is omitted from the wire, not serialized as `null`. Mirrors `src/event.rs:1097-1180`. | create |

Every `#[spec]` test needs a `/// Scenario:` doc comment (rule 7) or linkage-check fails.

## Verification

- **Rule 12 cross-version check** — this touches hooks and the event wire. Run a daemon from the previous release with an agent under it, run the branch TUI against it, and confirm a `delegate` still routes and hooks still arrive. Isolate `DOT_AGENT_DECK_LOG` along with the sockets, `HOME` and `DOT_AGENT_DECK_STATE_DIR`. Expected: no bump needed — the run is what makes that a finding rather than an assumption.
- **As a user sees it** — launch the deck with a real agent, confirm the badge is hidden at rest, press `Ctrl+D` then `Ctrl+M`, confirm the card reads `ClaudeCode (Opus)`, press again and confirm it hides. Confirm a card created *after* the toggle also shows it.
- **All test runs happen in CI**, never locally (CLAUDE.md rule 5 fork addendum). Local gates before every commit: `cargo fmt --check`, `cargo clippy --workspace --all-targets --features e2e -- -D warnings`, and `cd <worktree> && cargo xtask linkage-check`.
