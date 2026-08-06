# Orchestrator context is generated — edit the TOML, not the Markdown

> **Developer / maintainer reference.** Not published to the documentation site; it renders as plain Markdown here on GitHub.

`.dot-agent-deck.toml` is the source of truth for role prompts and for the orchestrator's workflow text. Everything a role says to its agent — the orchestrator's numbered workflow, the notification recipe, each worker's standing instructions — lives in that file's `prompt_template` fields and nowhere else.

## `.dot-agent-deck/orchestrator-context.md` is a generated artifact

On every orchestration start, `prepare_orchestrator_prompt()` (`src/ui.rs`) writes `.dot-agent-deck/orchestrator-context.md` from `build_orchestrator_context(config)` and injects a one-liner telling the orchestrator to read that file. The path sits inside the gitignored `.dot-agent-deck/` directory and is overwritten in full each time — the file is a render target, not an input.

**Therefore: editing the `.md` is pointless.** Any change you make to `.dot-agent-deck/orchestrator-context.md` is silently reverted the next time an orchestration starts, with no warning and no diff to notice. Edit `.dot-agent-deck.toml` instead, and let the next start regenerate the Markdown. (This is non-obvious enough that it has already cost one session a wasted round of edits.)

## Not everything in the file comes from config

`build_orchestrator_context()` composes three parts: the start role's `prompt_template` verbatim, then hard-coded **"Available agents"** and **"Delegation protocol"** sections, then a hard-coded **"Important"** section. Only the first part is configurable — the agent list is derived from the non-start roles' `name`/`description` fields, and the delegation-protocol and Important text are string literals in `src/ui.rs`. Changing those requires a code change and a rebuild; no amount of TOML editing will move them.

Read `build_orchestrator_context()` and `prepare_orchestrator_prompt()` in `src/ui.rs` to confirm any of the above rather than taking this page's word for it.
