# PRD fork#339: A command-mode toggle that shows the agent-type badge on session cards

**GitHub Issue**: [fork #339](https://github.com/prageethw/dot-agent-deck/issues/339)

**Priority**: Medium

**Status** *(corrected 2026-08-16)*: **Merged into the fork — M1–M8 complete, every resume-checklist step discharged.** PR [#342](https://github.com/prageethw/dot-agent-deck/pull/342) merged 2026-08-15 as `a9ca72cc`, first released in **v0.38.2**. The `reviewer` and `auditor` findings named below were resolved before merge (step 7); step 8 (`release` → `/prd-done`) is done; step 9 — the rule 19 upstream-offer issue — is discharged as [fork #347](https://github.com/prageethw/dot-agent-deck/issues/347), which carries the reframing the next paragraph describes. Nothing remains open on this PRD.

*(This line read "In progress … Draft PR #342 open" until 2026-08-16 — the PR had merged the day before, so the file advertised work that was already shipped. Corrected while clearing the PRD queue; the stale status is what made the PRD look actionable in a queue sweep.)*

**Branch prefix — one deliberate deviation from the resume checklist below.** Step 2 prescribes `fix/<n>-…`. This branch is `feat/339-…` instead, because [fork #340](https://github.com/prageethw/dot-agent-deck/issues/340)'s tier-2 supplier maps `fix/` → `bug` while this change ships a `.feature.md` fragment (M8), so `work-type-check`'s R0 would fail on tier disagreement the moment that gate lands. `feat/` is the correct prefix for a `prd`-type change.

**Fork-only?** **No — upstream-worthy.** The badge removal (`370b6228`) is a fork preference, but *a toggle for it* is a genuine feature in upstream's own card renderer. Per CLAUDE.md rule 19 the ordering is: build it here, merge it here, then file the upstream-offer issue **at merge time**. Note the offer would need reframing for upstream, where the badge is shown unconditionally — there the same toggle hides it rather than reveals it.

---

## Problem Statement

The request as received was *"we previously removed model names from agent panes; add a toggle to show them again."* Research established that **both halves of that premise are wrong in ways that change the work**, and the corrections are the reason this PRD exists rather than a one-line fix.

### 1. No model name was ever displayed, or ever removed

Fork-only commit **`370b6228`** (*"fork-only: remove agent-type badge from cards, rename title to worker-deck (#4)"*, 2026-08-03) removed the **agent-type badge** — `ClaudeCode` / `OpenCode` / `Codex` / `Pi`, in the registry colour, bold, as segment 2 of 3 in the card title row. It was introduced by `286b6889` (PRD #20 M5, "coloured identity badge"). Duplicate SHAs of the same change across rebases: `82bc90dc`, `b33cec2c`, `432a2f6f`, `1eefeadb`.

The pre-removal card read:

```
┌ 1 ClaudeCode · friendly-claude ──────── ● Thinking ┐
```

**No model string has ever appeared on a card, and none exists in the data model.** `SessionState` (`src/state.rs:307-368`), the `SessionSnapshot` wire type (`src/state.rs:269`) and `AgentSpawnOptions` (`src/pane.rs:247-271`) carry no model field. The only place a model exists in this repo is buried inside a launch command — and here it is hidden in a devbox wrapper *name*, not a `--model` flag:

```toml
command = "devbox run claude-opus-devbox"     # .dot-agent-deck.toml:95, orchestrator
command = "devbox run claude-sonnet-devbox"   # :184 coder, :205 tester, :228 release
```

So showing a real model name would mean either parsing a wrapper name heuristically, or plumbing a new field spawn → daemon → `SessionState` — an additive `#[serde(default)]` wire change, which is a **TUI↔daemon contract change under CLAUDE.md rule 12** with its own cross-version manual test. **The user chose the badge**; a true model field is out of scope here and, if still wanted, is a separate issue with its own protocol work.

### 2. There is no `Ctrl+D` prefix or chord state

The requested `CTRL+D, then CTRL+M` assumed a tmux-style prefix. There is none — `grep -E 'pending_key|pending_chord|prefix_pending|awaiting_'` over `src/` returns zero hits. `Ctrl+D` maps to `KbAction::Dashboard` → `Action::DetachToNormal` (`src/keybindings.rs:124-130`, `src/ui.rs:8597`), which toggles `UiState::mode` into `UiMode::Normal` — **command mode**.

"`Ctrl+D` then X" is therefore an established idiom meaning *X is a command-mode binding*, and the codebase says so in as many words at `src/ui.rs:8717-8721` (*"the user pays one extra `Ctrl+D` rather than losing the chord entirely. Deliberate pattern, not a one-off"*) and in the lock status string at `src/ui.rs:6102`. The requested shortcut is buildable exactly as asked — it just is not the mechanism the request assumed.

### 3. The shortcut conflict, which is real and silent

`Ctrl+M` is byte `0x0d`. Two independent consequences:

| Context | What happens | Evidence |
|---|---|---|
| Enhanced terminal (kitty, Ghostty, WezTerm, iTerm2+CSIu) | arrives as `Char('m') + CONTROL` — the binding works | `push_keyboard_enhancement`, `src/ui.rs:11286-11303`, pushes `DISAMBIGUATE_ESCAPE_CODES` |
| **tmux / Terminal.app / legacy** | arrives as **`KeyCode::Enter`** — already bound to `FocusPane` (`src/ui.rs:7530`) and `ModeTabFocus` (`src/ui.rs:8798`), so it silently focuses a pane instead | the push self-disables under tmux (`src/ui.rs:11277-11280`); crossterm decodes `0x0d` as `Enter` (decoder table read at `src/ui.rs:5719`) |
| **Inside a pane** (`UiMode::PaneInput`) | `Ctrl+M` is the **CR submit byte** for every supported agent | `keyevent_to_bytes` `src/ui.rs:5768-5774` → `ctrl_c0_byte('m')`; `user_byte_submits_input_box` `src/ui.rs:5885`; pinned by `src/agent_pty.rs:5831-5834` |

**The keybinding config's own conflict detector cannot see the first row.** `resolve_conflicts` (`src/keybindings.rs:744-760`) keys on `normalize_chord`, and `(Char('m'),CONTROL) != (Enter,NONE)`. So a naive `Ctrl+m` binding would ship looking clean and be dead on tmux.

Hence two requirements that are not optional: a **bare `m` alias** (the door that works everywhere), and **command-mode scoping** (or the binding steals agent submit from everyone on a modern terminal — the highest-severity failure mode in this change).

---

## Solution Overview

Restore the exact segments `370b6228` deleted, behind a deck-global boolean that is **off by default**, toggled from command mode by `Ctrl+M` or a bare `m`.

The pieces to reuse rather than rebuild — both are still in the tree with **zero production consumers**, which is why this is a restoration and not a new mechanism:

- `AgentSpec::badge_color` (`src/agent_registry.rs:90`), populated for all six specs (`:182` LightMagenta, `:196` LightGreen, `:210` LightCyan, `:230` LightYellow, `:270` LightBlue, `:288` DarkGray). Confirmed dead: `grep -rn badge_color src/` shows only the definition, the six literals, and two assertions at `:577,580`.
- `impl Display for AgentType` (`src/ui.rs:81`), which renders `agent_registry::spec(self).label` — exactly what `format!("{}", session.agent_type)` used.

### Decisions taken (do not re-litigate on resume)

| # | Decision | Rationale |
|---|---|---|
| D1 | Show the **agent-type badge**, not a model name | What was actually removed; reuses existing code; no protocol change. A true model field was offered and declined for this change. |
| D2 | Bind **`Ctrl+M` *and* a bare `m`** | `Ctrl+M` is what was asked for and works on enhanced terminals; `m` is the only door on tmux/legacy. `m` is unbound in command mode today. |
| D3 | **No experimental feature flag** | The toggle is off by default, which already provides the safety a flag would. Avoids a `src/features.rs` wrapper and a `graduate-` follow-up. Note the flag is ON in this fork's committed config anyway (`ad93333`), so gating would not have hidden it here. |
| D4 | **Skip the badge on placeholder cards** (`AgentType::None`) | `370b6228` rendered the type unconditionally and `spec(&None).label` is `"No agent"`, so verbatim restoration yields `┌ 1 No agent · pane-x ── ● No agent ┐`. `is_placeholder` is already computed at `src/ui.rs:19144`. This is a deliberate refinement of "the previous display style", flagged to the user. |
| D5 | The `m` alias is a **hardcoded fallback**, not a second `ActionSpec` | An alias is by definition not remappable; a second spec would pollute the help overlay, the `[global]` docs table and conflict warnings. Precedent: `src/ui.rs:7554` (`|| key.code == KeyCode::Char('S')`), and the `Down`/`Up` aliases at `:7492,:7503`. |
| D6 | Scope via a **one-line arm in `global_action_for_mode`**, not a new `scope_*` fn | `scope_command_entry_lock` (`:8688`) and `scope_split_stage` (`:8730`) exist only because they need tab context the mode-scoper cannot see. This action needs none. The one-line form makes `key_action_for_mode` correct for free and needs no `handle_key_event` change. |
| D7 | **No button-bar button** | Neither `Ctrl+L` nor `Ctrl+E` has one; a seventh global chip overflows common widths (the bar already wraps, PRD #144). Keeps every button-bar snapshot and `keybindings/hints/*` test untouched. |

---

## Milestones

### M1 — Keybinding registry (`src/keybindings.rs`)

- `Action::ToggleAgentTypeBadge` on the enum (`:48-95`), inserted **after** `ToggleOrchestrationSplit` (`:63`).
- Matching `ActionSpec` in `ACTIONS` (`:122-344`) at the same position: `name: "toggle_agent_type_badge"`, `default: "Ctrl+m"`, `section: Section::Global`, description `"Toggle agent-type badge on cards"`.
  - `Section::Global` names the **TOML table, not the mode** — `close_pane`, `toggle_orchestration_lock` and `toggle_orchestration_split` are all command-mode-only `[global]` entries already (`docs/keyboard-shortcuts.md:206` says so).
  - **Position is semantically load-bearing.** `resolve_conflicts` (`:744`) is first-defined-wins over `ACTIONS` order; appending at the end would silently lose every user-created collision.
- Bare-`m` alias in `handle_normal_key` (`src/ui.rs:7466`) as the **last** check before the trailing `Action::Continue`, guarded on `key.modifiers.is_empty()`:
  - **last**, so a user who rebinds another dashboard action onto `m` still wins (a hazard the `S` alias at `:7554` does have, and which costs nothing to avoid);
  - **in `handle_normal_key`, never in `global_action`** — `global_action_for_mode` runs before *every* per-mode handler (`src/ui.rs:11453`), so a bare `m` claimed there would make it impossible to type the letter `m` into the filter, rename, dir-picker and new-pane-form fields.

### M2 — Action + scoping (`src/ui.rs`)

- `ui::Action::ToggleAgentTypeBadge` on the enum (`:5419`, beside `CycleSplitStage` `:5463`).
- Resolve in `global_action` (`:8592`) after the `ToggleOrchestrationLock` block (`:8611`).
- Scope by extending the match in `global_action_for_mode` (`:8657-8665`), mirroring the existing `CloseSelected` arm:
  ```rust
  Some(Action::ToggleAgentTypeBadge) if mode != UiMode::Normal => None,
  ```
- Verify no change is needed to `normal_key_claims_without_action` (`:7584`) — it lists only bindings returning `Action::Continue`; ours returns a distinguishable action, so `command_banner_key_signal` (`:7621`) already classifies it as `CommandAction` and the banner collapses correctly.

### M3 — State (`src/ui.rs`)

- `show_agent_type_badge: bool` on `UiState`, declared after `split_stage` (`:2158`), documented in the style of `command_entry_locked` (`:2130-2147`) — stating that it is deck-global, describes how someone is reading the deck right now, and is not persisted.
- Default `false` in `UiState::new` after `:2493`.
- `dispatch_action` arm (`:9209`) after `CycleSplitStage` (ends `:9313`), modelled on `ToggleLayout` (`:9233-9246`): flip the field, set `ui.status_message` to `"Agent badge: shown"` / `"Agent badge: hidden"`. Matches the house pattern (`Layout: …` `:9245`, `Pane entry: …` `:9276`, `Split: …` `:9311`) and gives M6's L2 test a stable, unique needle.

### M4 — Render (`src/ui.rs`)

`render_session_card` (`:19132`) gains a trailing `show_agent_type_badge: bool`. Replace the `identity_text` block (`:19186-19201`) with a conditional restoring exactly what `370b6228` deleted:

- **on** → `(" {sel}{num}", shortcut_style)`, `("{agent_type}", badge_color + BOLD)`, `(" · {name} ", title_bold)`
- **off** → today's two segments, byte-identical
- **skip the badge when `is_placeholder`** (`:19144`) — decision D4

Live call site `:15483` passes `ui.show_agent_type_badge`; `ui.mode` is already read inline at the same site with the same borrow shape, so there is no borrow-checker consequence.

**Rejected:** a `thread_local!` mirror like `ACTIVE_SPLIT_STAGE` (`:2656`). That exists only because `compute_frame_layout` / `resize_panes_to_layout` are pure geometry functions far from `ui`; here it would make every render seam order-dependent and let one L1 test leak into the next on the same thread.

Width and truncation are **unaffected when hidden**: `max_title` (`:19207`) is computed from `area.width` and `status_text` only, never from the segments, and `truncate_styled_segments` (`:6801`) receives a list identical to today's.

**Seams — change exactly one:**

| Seam | Line | Change | Callers |
|---|---|---|---|
| `render_card_to_buffer` | `:19816` | **none** — passes `false` through | 12 sites, untouched |
| `render_card_for_mode_to_buffer` | `:19849` | **add the bool** after `mode` | 5 external sites get a mechanical `false,` |
| `render_dashboard_cards_to_buffer` | `:19903` | **none** — hardcodes `false` at `:19939` | 8 sites, untouched |

`render_card_to_buffer` is documented (`:19809-19813`) as the compatibility baseline, and hidden-by-default **is** the baseline — keep it 8-arg. Both changed fns already carry `#[allow(clippy::too_many_arguments)]`.

### M5 — Consistency across panes (no work, verify only)

There is exactly **one** production `render_session_card` call (`:15483`), inside `render_frame`'s per-card loop, and Orchestration-tab role cards use the same renderer (`:15417` — *"Orchestration tabs use the same dashboard card rendering as the main dashboard"*). So every card on every tab, **including a pane created after the toggle**, reads the one field on its next frame. Nothing per-pane, per-tab or per-session to seed, migrate, persist, or add to the session snapshot.

### M6 — Tests

New catalog sub-area `#### dashboard/agent-badge` under `### Dashboard panes`, after `#### dashboard/layout` (`tests/CATALOG.md:405`). **Not** `dashboard/badge` — that yields fn prefix `badge_001_`, colliding with the allowlisted `status/badge/001` (`xtask/linkage-check/m2.allowlist:107`). `agent-badge` normalizes to the unique `agent_badge_001_…` (`sub_area_prefix`, `xtask/linkage-check/src/main.rs:938`, hyphen→underscore).

| Catalog ID | Tier | Action | Test fn | Asserts |
|---|---|---|---|---|
| `keybindings/safety/005` | L1 | new, RED-first — `tests/render_keybindings.rs` | `safety_005_ctrl_m_is_command_mode_only` | Via `key_action_for_mode`: `Normal`+Ctrl+M → `ToggleAgentTypeBadge`; **`PaneInput`+Ctrl+M → `ForwardToPane([0x0d])`** (agent submit preserved); `Filter`+Ctrl+M → `None`; `PaneInput`+`m` → `ForwardToPane([b'm'])`; `Normal`+Enter never claimed globally. Mirrors `safety_003` (`:199-220`). |
| `dashboard/agent-badge/001` | L1 | new, RED-first — `tests/render_dashboard.rs` | `agent_badge_001_card_shows_registry_badge_only_when_enabled` | Same card off then on via `render_card_for_mode_to_buffer`. **Off:** no type text, no `badge_color` cell (reuse the loops at `:764-820`). **On:** `Codex · wrapped-01` present, a cell carries `badge_color` **and** `Modifier::BOLD`; repeated across all four shipped agent types. One new colour-aware `insta` snapshot of the on-state. Also pins D4 (placeholder card shows no badge even when on). |
| `dashboard/agent-badge/002` | L1 | new, RED-first — `src/ui.rs` `mod tests` | `agent_badge_002_toggle_cycles_hidden_shown_hidden` | **The three-state sequence the request asked for.** Fresh `UiState` → `false`; dispatch once → `true` + `"Agent badge: shown"`; dispatch again → `false` + `"Agent badge: hidden"`. Plus `handle_normal_key('m') == ToggleAgentTypeBadge` and `handle_normal_key(Enter) == Focus` — the alias does not displace FocusPane. Must be in-crate: `handle_normal_key` is private. Models `lock_003_ctrl_e_toggles_the_lock` (`src/ui.rs:36321`). |
| `dashboard/agent-badge/003` | L2 | new, RED-first — new `tests/e2e_agent_badge.rs` | `agent_badge_003_m_toggles_badges_on_every_card_real_binary` | Real binary, two synthetic `SessionStart` hooks (claude_code + codex). Both labels absent at rest — **default-hidden through the real `render_frame`, which no L1 seam covers**; press `m` → `wait_for_string("Agent badge: shown")` then both present; press `m` → `"Agent badge: hidden"` then both absent. Proves deck-global across two cards **and** proves the alias is the working door on a legacy PTY, where `Ctrl+M` decodes as Enter. Satisfies CLAUDE.md rule 4's PTY-attached requirement. Template `tests/e2e_hook_delivery.rs:25-64` (~45 lines, no real agent, no LLM spend). Synthetic → **not** `[reel]`-marked. |
| `dashboard/help/002` | L1 | **extend** — `tests/render_help_overlay.rs:78` | `help_002_overlay_documents_ctrl_d_toggle` | Add an assertion for the new help row; re-accept the snapshot. |

**Deliberately not written** — record in the `Does not assert:` bullets: no `keybindings/hints/004` (the hints bar already omits `Ctrl+L`/`Ctrl+E`); no button-bar test (D7); no `keybindings/help/002` (covered free by `keybindings/help/001`'s remapped-config snapshot re-accept).

### M7 — Existing test impact

**Green unchanged — the load-bearing consequence of hidden-by-default.** `pane_007_pi_card_omits_agent_type_badge` (`tests/render_dashboard.rs:680`) and `pane_008_codex_card_omits_agent_type_badge` (`:764`) both go through the unchanged 8-arg `render_card_to_buffer`; their assertions stay true of the default rendering. **Amend catalog prose and `/// Scenario:` lines only** (`tests/CATALOG.md:65-70`, `:72-76`): *"omits the agent-type badge"* → *"omits the agent-type badge **by default**"*, plus a `Does not assert:` pointer to `dashboard/agent-badge/001`.

Likewise the five e2e files touched by `370b6228` — `e2e_codex_hooks.rs:203`, `e2e_codex_wrapper.rs:96,199`, `e2e_session_restore.rs:245,865`, `e2e_pane_send_result.rs`, `e2e_pi_live.rs:277`. **Every remaining badge mention there is a comment, not an assertion**; they assert on display names and status. No code change; softening the comments is optional.

**Mechanical `false,` argument** at `tests/render_dashboard.rs:529,1214,1419` and `tests/mode_indication.rs:980,991` — rendering identical, **no snapshot re-accept**.

**Snapshots re-accepted: the two help-overlay snaps only** (`render_help_overlay__help_002_…`, `render_keybindings__help_001_…`). Geometry is stable: the left column goes 30 → 31 lines, the right stays 32, and `popup_height = max(left,right) + 4` (`src/ui.rs:17903`), so the box, its `x`/`y` and every right-column row stay put. Every card snapshot is byte-identical.

### M8 — Docs, help overlay, changelog

- **Help overlay** (`render_help_overlay`, `:17749`): one row in the left column after the `ToggleOrchestrationSplit` row (`:17818-17821`) — key `Ctrl+m / m`, description `"Show / hide agent badges"`. Fits: `help_key_line` (`:17585`) pads keys to 18 (10 used), and the 50-wide column leaves ~30 for the description (24 used).
- **`docs/keyboard-shortcuts.md`**:
  1. row in the Global Shortcuts table (lines 16-23), scope column **"Command mode only"**;
  2. new prose subsection after the `Ctrl+W` one (~line 35) stating three things — it is one setting for the whole deck (every tab, and any pane opened afterwards), it resets to hidden on relaunch, and **on tmux and older terminals `Ctrl+M` is indistinguishable from Enter, so press a bare `m` there**; also that inside a pane `Ctrl+M` passes straight through as `^M` (`0x0d`), which is what submits to the agent;
  3. `[global]` actions table row (~198-206), and extend the sentence at :206 to name it alongside `close_pane` / `toggle_orchestration_lock` / `toggle_orchestration_split`;
  4. note the `m` alias is non-remappable, in the paragraph covering the `Down`/`Up`/`Tab` aliases (~215).
- **Changelog**: one `changelog.d/<issue>.feature.md` fragment (directory currently empty; convention `<number>.{feature,bugfix,breaking}.md`, assembled by `scripts/assemble-changelog.sh`).
- **Stale comments written by `370b6228`, now false** — fix in the same commit: `src/ui.rs:~6146` (`truncate_styled_segments` doc), `:19186-19190`, `src/agent_registry.rs:85-89` (`badge_color`'s *"rendering coloured badges on cards is a later PRD #20 milestone"*), `tests/CATALOG.md:98`.

---

## Risks and gotchas

1. **`Ctrl+M` is the agent-submit byte. Never claim it outside `UiMode::Normal`.** If the mode guard in `global_action_for_mode` is dropped or inverted, every user on a kitty-capable terminal silently loses the ability to submit to their agent while typing in a pane. Highest-severity failure mode in this change; `keybindings/safety/005` is the guard.
2. **A bare `m` in `global_action` would break the filter box** — see M1.
3. **`Ctrl+M` will never work under tmux**, by design: `push_keyboard_enhancement` early-returns, the terminal delivers `Enter`, and `FocusPane` (`:7530`) wins *before* the alias check. Enter must keep focusing a pane, so this is correct — it is also why the L2 test must press `m`, not `\x0d`, and why the docs must say so plainly.
4. **`ACTIONS` position is load-bearing** — see M1.
5. **Do not add the parameter to `render_card_to_buffer`** — 12 call sites, and both badge-absence tests depend on it defaulting to hidden.
6. **`dispatch_action`'s match is exhaustive**; every other `match action` (e.g. `close_confirmation_for_action` `:6313-6316`, the mouse/button paths) has a `_` arm, so nothing else breaks and nothing silent appears.
7. **Harness gates**: each `#[spec("…")]` needs a matching `##### <id> — headline` in `tests/CATALOG.md` with all five bullets (Layer / Agent / Asserts / Does not assert / Platform coverage), a `/// Scenario:` doc comment **with a body** (singular — `Scenarios:` does not count), and a fn named `<sub>_<NNN>_…`. `tests/e2e_agent_badge.rs` must open `#![cfg(feature = "e2e")]` and use `wait_for_string` / `wait_for_absence`, never raw sleeps or `for _ in 0..N` polling.
8. **Rule 12 does not apply** — no daemon, protocol, orchestration or hook surface changes, so no `PROTOCOL_VERSION` bump and no cross-version manual test.

---

## Resume checklist

Everything below is unstarted. `main` was clean at `17245088` when this was written; re-verify before branching.

1. **File the fork issue** on `prageethw/dot-agent-deck` titled *"Command-mode toggle (`Ctrl+M` / `m`) to show the agent-type badge on session cards"*. Rule 20 searches were run 2026-08-15 over both trackers, issues **and** PRs — nothing overlapping is open. Re-run them; they are cheap:
   ```bash
   gh issue list --repo prageethw/dot-agent-deck --state open  --search 'badge model pane'
   gh pr    list --repo prageethw/dot-agent-deck --state all   --search 'badge model toggle'
   gh issue list --repo vfarcic/dot-agent-deck   --state open  --search 'badge model pane'
   gh pr    list --repo vfarcic/dot-agent-deck   --state all   --search 'badge model toggle'
   ```
   Then rename this file to `prds/fork-<n>-agent-type-badge-toggle.md` and add the issue link at the top.
2. **Create the worktree** (rule 1) — disk-backed sibling, never the root checkout, never the scratchpad (rule 18):
   ```bash
   git worktree add -b fix/<n>-agent-type-badge-toggle ../dot-agent-deck-agent-badge origin/main
   cd ../dot-agent-deck-agent-badge && git branch --unset-upstream
   ```
   Push explicitly thereafter: `git push origin HEAD:refs/heads/fix/<n>-agent-type-badge-toggle`.
3. **Claim the issue from inside that worktree** (rules 14/23): `worker-agent-deck issue claim <n> --repo prageethw/dot-agent-deck`. Note `worker-agent-deck`, not `dot-agent-deck` — `issue` is a fork-only subcommand.
4. **Delegate M6's four RED tests to `tester`** (M6 rows 1-4 + catalog entries). One push. **Open the draft PR on that first commit** or the push fires no CI and there is nothing to read (rule 5).
5. **Delegate M1-M5 + M8 to `coder`.** One push. Expected to answer: the four RED tests flip GREEN, `clippy`/`fmt`/`linkage-check` clean.
6. **Delegate back to `tester`**: confirm GREEN from CI, extend `dashboard/help/002` (M6 row 5), re-accept the two help-overlay snapshots, amend the M7 catalog prose.
7. **`reviewer` + `auditor` in parallel.** Findings file under the **root checkout's** `.dot-agent-deck/` (rule 15) — derivable as `dirname "$(git rev-parse --path-format=absolute --git-common-dir)"`.
8. **`release` → `/prd-done`**, marking the existing draft ready rather than creating a PR. Pause at the merge gate for the user's go-ahead.
9. **At merge: file the upstream-offer issue** (rule 19) — reframed for upstream, where the badge is currently unconditional. Filing it at merge time is the mechanism; a merge that owed an offer and did not file one is the defect.

**Test-run policy throughout (rule 5 fork addendum): every test run happens in CI.** Workers commit, push, and read RED/GREEN from the PR. Only `cargo fmt --check`, `cargo clippy --workspace --all-targets --features e2e -- -D warnings` and `cd <worktree> && cargo xtask linkage-check` run locally, before every commit. Neither local-run carve-out applies here: this change touches no real-agent spawn/attach path, and `dashboard/agent-badge/003` is synthetic and not reel-eligible, so no `.cast` recording is needed and the demo-reel step is skipped. Per rule 22, one push per delegation, and the orchestrator states what that push must answer.

## Verification

- **CI fast tier** — `keybindings/safety/005`, `dashboard/agent-badge/001` and `/002` flip RED → GREEN; no other test moves; the two help-overlay snapshots are the only re-accepts.
- **CI e2e tier** (informational, `continue-on-error`) — `agent_badge_003` proves the full sequence through the real binary: hidden → `m` → visible → `m` → hidden, across two cards of different agent types. **Read the test summary out of the log, never the run conclusion** (rule 8): `gh run view <id> --repo prageethw/dot-agent-deck --log | sed 's/\x1b\[[0-9;]*m//g' | grep -E 'tests run:|TRY 3 FAIL'`.
- **Highest-severity regression guard** — `safety_005`'s assertion that `PaneInput` + Ctrl+M still yields `ForwardToPane([0x0d])`.
- **Manual, at the merge gate** — launch via the `run-dot-agent-deck` skill: badges hidden on start; `Ctrl+D` then `m` shows them on every card; open a new pane and confirm it inherits the state; toggle off. On an enhanced terminal (Ghostty/kitty/WezTerm) additionally confirm `Ctrl+D` then `Ctrl+M` works, and that typing in a focused pane still submits with Enter/Ctrl+M.
