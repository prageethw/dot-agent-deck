# Experimental Flag

> **Developer / maintainer reference.** This page documents an internal development mechanism and is intentionally excluded from the published documentation site.

`dot-agent-deck` can hide in-flight, work-in-progress surfaces behind a single boolean feature flag named `experimental`. It is **off by default**, so a normal install never shows half-finished features. Enable it only when you want to test a surface that a PRD has explicitly marked as experimental.

## What the flag does

The flag is a **presentation switch**, not a behaviour switch. It controls only whether certain *new, user-visible surfaces* (a pane, field, command, tab, footer, or keybinding) are shown. The underlying code paths run regardless — the flag just decides whether you can see and reach the new surface.

A feature is gated by the flag only when its PRD says so. Surfaces that are not marked experimental are always visible and are unaffected by this flag.

## How to enable it

There are two ways to turn it on. **The environment variable wins over the file** — if it is set, the file value for this field is ignored.

**1. Config file (`.dot-agent-deck.toml`)**

Add a `[features]` table to your project's `.dot-agent-deck.toml`:

```toml
[features]
experimental = true
```

**Which file is that, exactly?** At startup the deck walks up from its own working directory and uses the first `.dot-agent-deck.toml` it finds — so launching from a subdirectory of the project picks up the project root's file, not a non-existent one beside you. `DOT_AGENT_DECK_FEATURES_CONFIG` overrides the search entirely and is what the tests use.

Two consequences worth knowing, because both are silent:

- **A deck launched outside the project tree still resolves the flag OFF.** There is nothing to walk up to from, say, `$HOME`, so every experimental surface stays hidden however the project config reads. That is fork issue [#303](https://github.com/prageethw/dot-agent-deck/issues/303). Phase 1 concluded a structural per-project/per-pane fix is over-engineering for the three dashboard-chrome consumers this flag currently gates, so it stayed process-global; Phase 2 (below) instead makes the OFF-by-silence case visible rather than resolving it. **#303 stays open** — not for that structural fix, which the design phase narrowed away, but tracking [#353](https://github.com/prageethw/dot-agent-deck/issues/353): a config split-brain where `features_config_path()`'s ancestor walk and `load_project_config(dir)` can now resolve against different directories. Until then, launch the deck from inside the project, run `worker-agent-deck features status` to see exactly what resolved and why, or set `DOT_AGENT_DECK_FEATURES_CONFIG`.
- **A project living on a mount where every directory reports world-writable has its config declined, and experimental features silently stay OFF.** FAT32/exFAT USB media, some CIFS/SMB mounts, and a Docker volume mounted with `umask=0` are the common cases. This is the intended trade-off of fork issue [#309](https://github.com/prageethw/dot-agent-deck/issues/309): a world-writable directory could have had its `.dot-agent-deck.toml` planted by another user, so it is never trusted, however well-formed the file is. The deck prints a `declining <path> in features-config search: ...` warning to stderr at startup naming exactly what was declined, and `worker-agent-deck features status` reports the same declines (and, in the degenerate case where nothing anywhere is trustworthy, says so plainly instead of guessing). If the flag is unexpectedly OFF, checking for that warning — or running `features status` — is the fastest way to tell "the file was never trusted" apart from "the file was never found" or "the file is malformed."
- **The walk runs once, at startup, and the resolved path is then fixed.** Editing *that* file while the deck is running takes effect live — within a couple of seconds, no restart needed, and setting it back to `false` (or removing the table) hides the experimental surfaces again. But *creating* a config the walk did not find will not be picked up until the deck restarts, because the watcher polls the path resolved at startup rather than repeating the search.

**2. Environment variable (`DOT_AGENT_DECK_EXPERIMENTAL`)**

```bash
DOT_AGENT_DECK_EXPERIMENTAL=1 dot-agent-deck
```

The value is case-insensitive: `1` or `true` enables the flag; any other value (or leaving it unset) disables it.

> **Environment overrides the file.** When `DOT_AGENT_DECK_EXPERIMENTAL` is set, it decides the flag's state and edits to `[features] experimental` in `.dot-agent-deck.toml` are ignored until you unset the variable. Set the variable to `1`/`true` to force the experimental surfaces on regardless of the file, or to `0`/`false` to force them off.

## Default and precedence

| Source | Value | Result |
|---|---|---|
| Nothing set | — | **Off** (default) |
| `[features] experimental = true` in `.dot-agent-deck.toml` | file | On |
| `DOT_AGENT_DECK_EXPERIMENTAL=1` (or `true`) | env | On — wins over the file |
| `DOT_AGENT_DECK_EXPERIMENTAL=0` (or `false`/other) | env | Off — wins over the file |

Both the TUI and the background daemon call `init_and_watch` and each independently resolves the flag from the same `.dot-agent-deck.toml`. As of fork issue #303's Phase 1 investigation, that is a weaker guarantee than it sounds: every consumer of the flag (`show_experimental_footer`, `show_issue_dispatch_authoring`, `show_command_entry_lock`) lives in `src/ui.rs`, so the value the daemon installs into its own process-global state is currently read by nothing. Treat "both processes resolve the same algorithm" as true and "the two processes therefore agree on anything" as not yet a meaningful guarantee — it would need a real daemon-side (or pane-scoped) consumer to become one.

On startup each process logs a single line naming the resolved value **and the path it came from** — `experimental flag: ON (config path: …)` — which surfaces only when file logging is enabled (`DOT_AGENT_DECK_LOG`). The path is in that line precisely so a wrong-file mismatch is diagnosable from the log without resorting to `lsof` on a running deck.

Two ways to check the resolution on demand, without touching `DOT_AGENT_DECK_LOG` at all:

- **`worker-agent-deck features status`** reports, whether or not the deck is running, the resolved config path, whether it exists, the resolved value, and which source won (env override, project file, or default) — including whether a project file that exists actually supplied the value, or the value fell back to a default because the file could not be used (malformed TOML, an unreadable or non-regular target, an oversized file).
- **The missing-config startup warning** (fork issue #303 Phase 2): when the ancestor walk finds no `.dot-agent-deck.toml` anywhere above the launch directory, the TUI prints a warning to stderr before the alternate screen takes over. Unlike the log line above, this needs neither `DOT_AGENT_DECK_LOG` nor a restart to see, and stays completely silent once a config is found.

> **One flag for everything.** There is exactly one experimental toggle. If two unrelated experimental surfaces are in flight at once, they are shown or hidden together — there are no per-feature toggles.

## Why surfaces are gated

This lets work-in-progress code merge to `main` without exposing unfinished UI during normal use. Each gated surface is wired behind a small wrapper function so that, once the feature is finished ("graduates"), the gating is removed mechanically and the surface becomes visible to everyone. Until then, it stays behind `experimental`.

## Currently gated

| Wrapper (in `src/features.rs`) | Surface | PRD | Graduation |
|---|---|---|---|
| `show_experimental_footer()` | The experimental dashboard footer | #139 | — |
| `show_issue_dispatch_authoring()` | The new-pane `schedule: issues` modal authoring option (PRD #120 creation UX) | #120 | `graduate-issue-dispatch` |

## Graduated

| Surface | PRD | Graduated |
|---|---|---|
| The new-pane `dispatcher` Mode-cycler option (PRD #220) — its `show_dispatcher()` wrapper is deleted and the branch inlined to `true`. The `dispatch` CLI verb and its daemon handler were never gated. Documented for users at [`docs/dispatcher-mode.md`](../dispatcher-mode.md). | #220 | in #220's own PR, before shipping |
| The orchestration command-entry lock: the `Ctrl+E` binding, the keystroke gate on a focused worker pane, and the waiting-pane focus steering (PRD #393) — its `show_command_entry_lock()` wrapper is deleted and all three `src/ui.rs` seams inlined to unconditional. The lock now works regardless of where the deck is launched from. | #393 | fork #346 |

> **`show_issue_dispatch_authoring()` is a render seam, like the others (redesigned 2026-06-24).** An earlier iteration gated `issue_dispatch` *behaviour* (the daemon's schedule-fire activation seam) — that is **gone**. A configured `issue_dispatch` task now runs **unconditionally**; the flag, config parsing, and the `schedule add --repo …` CLI are all flag-free. The wrapper now gates ONLY the new-pane Mode-cycler `schedule: issues` authoring option (a render/input seam in `src/ui.rs`) — i.e. the experimental *creation UX* for the task type, not the task type itself. This keeps the flag presentation-only, consistent with the default model.
