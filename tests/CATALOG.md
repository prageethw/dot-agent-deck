<!-- Source of truth for the harness Test-Case Catalog. Parsed by
     `cargo xtask linkage-check` and `cargo xtask docs` (PRD #77
     Decision 7 / Decision 30). Relocated here from
     prds/77-tui-testing-harness.md so the tooling no longer depends on a
     PRD's location/lifecycle. Entry format: `##### <area>/<sub>/<NNN> — <headline>`
     followed by `- **Layer:** …` bullets; the `## Test Case Catalog`
     heading is the section the parser keys on — keep it. -->

# Test-Case Catalog

## Test Case Catalog

This is the authoritative list of test cases the harness must cover. IDs are stable per Decision 7; tests reference them via `#[spec("…")]` annotations once the harness exists in M2. Coverage is enumerated from the code as it ships today (Decision 27 — "code is authoritative"); documented behaviors with no catalog entry are listed as deliberate skips at the end of this section.

Platform coverage column shorthand: **mac+linux** = macOS and Linux (Windows once the harness's Windows path is ready per Decision 4); **mac+linux+windows** = portable from day one.

Demo-reel eligibility marker: a trailing ` [reel]` on an entry's `##### <id> — <headline>` line opts that test into the PRD #180 demo reel (`.claude/skills/demo-reel-adapter`). Eligibility is **opt-in** — the default (no marker) is *not* eligible even for a PTY-attached test that records a cast. Mark a test only if it validates the feature **as a user actually runs and sees it** — a real agent genuinely spinning up (spawn → agent → work) — never a synthetic/stand-in test (`cat`, scripted echo, recorder stubs, terminal-probe, or synthesized hook events). The adapter includes a marked test in the reel only when it *also* has a cast and its source changed on the branch.

### Dashboard panes

#### dashboard/pane

##### dashboard/pane/001 — A pane appears in the next free layout region when an agent is started.
- **Layer:** L2 (PTY end-to-end).
- **Agent:** none (synthetic — `StartAgent` over the daemon protocol with a `sleep infinity` stub).
- **Asserts:** rendered card grid shows one new card; the corresponding pane region is visible on the right column.
- **Does not assert:** card text content beyond the display name, color of the status badge, exact pixel coordinates.
- **Platform coverage:** mac+linux.

##### dashboard/pane/002 — Closing a pane via `Ctrl+w` removes its card from the dashboard.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** Ctrl+W opens the close confirmation; navigating from default Cancel to Close and confirming removes the card, and the focused card index stays within bounds.
- **Does not assert:** which card receives focus next (`dashboard/selection/*` covers selection-after-close).
- **Platform coverage:** mac+linux.

##### dashboard/pane/003 — The dashboard pane (tab 0) is never closable.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** `Ctrl+w` from the dashboard tab with no card selected is a no-op: neither the pane-scoped nor tab-scoped confirmation opens, no panic occurs, the dashboard remains rendered, and the tab count is unchanged.
- **Does not assert:** any status-line text.
- **Platform coverage:** mac+linux.

##### dashboard/pane/004 — Card title row carries card number, display name, and a status badge.
- **Layer:** L1 (ratatui `TestBackend` + `insta`).
- **Agent:** none.
- **Asserts:** rendered card buffer matches the committed snapshot for a single Working session in the Normal density.
- **Does not assert:** pane content; this is a card layout snapshot only.
- **Platform coverage:** mac+linux+windows.

##### dashboard/pane/005 — Dashboard card highlight follows the stable `selected_session_id`, not card 0 (PRD #83 M3).
- **Layer:** L1 (ratatui `TestBackend` + `insta`).
- **Agent:** none.
- **Asserts:** with three session cards and a `Tab::Dashboard` whose `selected_session_id` points at the second card (`sess-beta`), `ui::sync_and_derive_selection` derives index 1 (not 0); the rendered snapshot shows the `▸` selection marker and highlighted border on the second card while the first and third stay unselected.
- **Does not assert:** keyboard-driven selection movement (`dashboard/selection/*`); elapsed-time rollover behavior (the fixture uses one current instant for all three cards).
- **Platform coverage:** mac+linux+windows.

##### dashboard/pane/006 — Card row shows `Dir:` (working directory basename), `Last:` (elapsed since last activity), `Tools:` (tool count), `Prmt:` (latest user prompts).
- **Layer:** L1 (ratatui `TestBackend` + `insta`).
- **Agent:** none.
- **Asserts:** an over-long working-directory basename renders with all four fields retained; `Dir:` owns the full inner content width and truncates with an ellipsis immediately before the right border, while `Last:` / `Tools:` live in the bottom border. A second 14-column render proves a newline in `abc\ndef` costs no terminal cell, so all six visible prompt cells render as `abcdef` without an ellipsis.
- **Does not assert:** the card-stats degradation thresholds (covered by `dashboard/card-stats/002` and `/004`); elapsed-time rollovers beyond the fixture's stable one-hour display.
- **Platform coverage:** mac+linux+windows.

##### dashboard/pane/007 — A Pi pane's card omits the agent-type badge by default, same as every other agent type (fork-only rename/hide, never pushed upstream — reverses PRD #201 M2.2 for this fork only).
- **Layer:** L1 (ratatui `TestBackend` + `insta`-style buffer text assertion).
- **Agent:** none (a fixture `SessionState` with `agent_type = AgentType::Pi` and no display name).
- **Asserts:** a live Pi session with no friendly name renders its card title as the bare session id (`orch-01`), with NO `Pi` / `ClaudeCode` / `OpenCode` / `Codex` / `No agent` label text anywhere on the card and no cell carrying Pi's registry `badge_color` — a plain `pi` pane un-badged renders exactly like any other agent type, not falling back to showing its type name.
- **Does not assert:** the status badge color (`status/badge/001`); the toggled-on state, where the badge does render (`dashboard/agent-badge/001`).
- **Platform coverage:** mac+linux+windows.

##### dashboard/pane/008 — Agent cards render with NO agent-type badge by default, even when they have friendly display names (fork-only rename/hide, never pushed upstream — reverses PRD #20 M7 / review finding 9 for this fork only).
- **Layer:** L1 (ratatui `TestBackend` + color-aware `insta` snapshot).
- **Agent:** none (synthetic Claude Code, OpenCode, Pi, and Codex `SessionState` fixtures, including friendly display names).
- **Asserts:** the unnamed Codex card and named cards for all four shipped agents contain NO registry agent-type label text and NO cell anywhere on the card carries that agent's registry `badge_color`; complete color-aware buffers are snapshotted.
- **Does not assert:** wrapper event delivery or real Codex execution (covered by `codex/wrap/001` and `codex/live/001`); the toggled-on state, where the badge does render (`dashboard/agent-badge/001`).
- **Platform coverage:** mac+linux+windows.

##### dashboard/pane/009 — A history-only session is visibly distinct from a live writable session (PRD #20 M4).
- **Layer:** L1 (ratatui `TestBackend` + inline `insta` snapshot).
- **Agent:** synthetic Codex `AgentEvent` fixtures, one live and one history-only.
- **Asserts:** the history-only card visibly contains a history marker and its numeric input shortcut carries `Modifier::DIM`; the live contrast card has neither treatment.
- **Does not assert:** delivery feedback or daemon send results (covered by `prompt/pane-input/004`).
- **Platform coverage:** mac+linux+windows.

##### dashboard/pane/010 — A pane keeps exactly one card when a hook reports on it without an `agent_id` (issue #398).
- **Layer:** L1 (in-process `AppState::apply_event` + ratatui `TestBackend` buffer text assertion).
- **Agent:** none (a tagged spawn placeholder plus one synthetic untagged `WaitingForInput` `AgentEvent`).
- **Asserts:** after an `agent_id: None` event lands on a pane that already carries a tagged session, exactly one session claims that `pane_id`, that session carries the reported `WaitingForInput` status, and the rendered card grid contains exactly one status badge. Before #398 the untagged event minted a second session, so the deck drew two cards for one pane and `build_pane_status` picked between their statuses by `HashMap` iteration order.
- **Does not assert:** that the tagged session keeps its accumulated history (the `pre_f9_hook_with_no_agent_id_*` unit tests in `src/state.rs` pin that half); the `WaitingForInput` command-entry carve-out that reads the collision-hardened join (`orchestration/lock/007`).
- **Platform coverage:** mac+linux+windows.

#### dashboard/stats

##### dashboard/stats/001 — A narrow stats bar keeps the `tools` total and spends no width on a per-agent-type breakdown.
- **Layer:** L1 (in-process `AppState::aggregate_stats` + ratatui `TestBackend` stats render).
- **Agent:** none (22 synthetic sessions: 14 Claude Code + 8 Codex).
- **Asserts:** rendered at 60 columns — the width the bar gets from the left dashboard column when panes are open — the bar still shows `22 active` and the `tools` total, and contains no `ClaudeCode` / `Codex` per-type segments. The breakdown (PRD #20, review finding 10) cost ~30 columns at this width and silently clipped the `tools` total off the right edge; the breakdown was redundant anyway, since each card's status dot and label already summarize agent state (fork #339: cards can carry a registry-colored agent-type badge when the deck-global toggle is on, but that is off by default and this stats bar never renders one regardless).
- **Does not assert:** priority-ordered truncation for bars too narrow even for the status counts, or exact badge colors.
- **Platform coverage:** mac+linux+windows.

#### dashboard/density

##### dashboard/density/001 — Spacious density shows up to 3 prompts and 3 tool calls per card.
- **Layer:** L1.
- **Agent:** none.
- **Asserts:** snapshot rendered with one card in a wide viewport carries the 3+3 capacity.
- **Does not assert:** behavior on Compact / Normal (covered by separate entries).
- **Platform coverage:** mac+linux+windows.

##### dashboard/density/002 — Normal density shows 1 prompt and up to 3 tool calls per card.
- **Layer:** L1.
- **Agent:** none.
- **Asserts:** snapshot rendered with a card count that lands in the Normal-density tier.
- **Does not assert:** the exact boundary card count between tiers — picked by the layout helper.
- **Platform coverage:** mac+linux+windows.

##### dashboard/density/003 — Compact density shows 1 prompt and 1 tool call per card.
- **Layer:** L1.
- **Agent:** none.
- **Asserts:** snapshot rendered with a card count that lands in Compact density.
- **Does not assert:** card visual style beyond the rendered character buffer.
- **Platform coverage:** mac+linux+windows.

##### dashboard/density/004 — A rendered card has no trailing blank rows below its content at any density tier (PRD #147).
- **Layer:** L1 (ratatui `TestBackend`, buffer inspection).
- **Agent:** none.
- **Asserts:** a fully-populated session card (3 prompts + 3 tools) rendered at each tier's own `rendered_height` in an 80-column wide viewport has zero blank inner rows between its last content line and the bottom border on Compact, Normal, and Spacious — reserved card height equals rendered content height.
- **Does not assert:** the exact `card_height` value per tier (covered by `card_height_001_content_derived_values`); the mid-card blank separator line on Normal/Spacious (intentional content, not a trailing row).
- **Platform coverage:** mac+linux+windows.

#### dashboard/card-stats

##### dashboard/card-stats/001 — A wide card renders its full Last/Tools stats at the bottom-right border.
- **Layer:** L1 (ratatui `TestBackend` + `insta`).
- **Agent:** none (synthetic Thinking-session fixture).
- **Asserts:** a comfortably wide live card right-aligns `Last: 1h  Tools: 14` in its bottom border, and neither counter appears on an inner content row; the complete character buffer is snapshotted. A wide placeholder `No agent` card also retains its full Last/Tools counters in the bottom border.
- **Does not assert:** narrow-width degradation (covered by `/002` and `/004`); border title colors.
- **Platform coverage:** mac+linux+windows.

##### dashboard/card-stats/002 — A 20-column card degrades its stats label without damaging border corners.
- **Layer:** L1 (ratatui `TestBackend` + `insta`).
- **Agent:** none (synthetic Thinking-session fixture).
- **Asserts:** with 18 usable bottom-border cells, the card selects `1h · 14 tools`, preserves both bottom corner glyphs, and renders no dedicated stats content row; the complete character buffer is snapshotted.
- **Does not assert:** widths below the shortest form or the complete transition sweep (covered by `/004`).
- **Platform coverage:** mac+linux+windows.

##### dashboard/card-stats/003 — Crossing the former 60-column breakpoint is structurally inert.
- **Layer:** L1 (ratatui `TestBackend`, comparative buffer inspection).
- **Agent:** none (the same synthetic session rendered on both sides of the old breakpoint).
- **Asserts:** real Normal-density card renders with 59 and 61 inner columns expose the same `Dir:` / `Prmt:` / `Last:` / `Tools:` labels and keep those labels on the same rows.
- **Does not assert:** production density selection, because the available L1 render seams require the caller to supply a density; exact horizontal truncation or full-buffer equality, since changing width legitimately changes available text cells.
- **Platform coverage:** mac+linux+windows.

##### dashboard/card-stats/004 — The stats-label degradation ladder transitions at exact display widths.
- **Layer:** L1 pure-data unit test over the hidden-public label selector.
- **Agent:** none.
- **Asserts:** the reference input selects no label below 9 cells, `2m · 14` from 9, `2m · 14 tools` from 15, and `Last: 2m  Tools: 14` from 21 onward, with both sides of the exact transitions pinned. Property sweeps over `1h 5m`/1234, a six-digit tool count, empty elapsed text, and Unicode/combining text prove every result fits its display-column budget and is the first, widest fitting form.
- **Does not assert:** ratatui title placement or styling (covered by `/001` and `/002`).
- **Platform coverage:** mac+linux+windows.

##### dashboard/card-stats/005 — A real interactive Haiku card keeps its height while opening its pane narrows the card and degrades the bottom-border counters. [reel]
- **Layer:** L2 PTY-attached (the real `dot-agent-deck` binary driven through the vt100 `TuiDeck` harness, with recording enabled for a `full-stream.cast`).
- **Agent:** REAL interactive Claude Code on `claude-haiku-4-5-20251001`, with onboarding/project trust seeded and `--allowedTools Bash`; no `-p`. A second real client on the observer's daemon performs the ordinary Ctrl+N flow and types the prefix-only prompt after Claude's native editor becomes ready; the recorded client observes the live card and later attaches that same daemon pane on demand.
- **Asserts:** the sentinel response and native Thinking/Working/Idle plus Bash hook prove the genuine spawn → agent → work path; at one fixed 68×16 recording size, the unattached card shows a nonzero, right-aligned full `Last: … Tools: …` label only in its bottom border, then attaching the real pane narrows the dashboard and selects the shorter `… · … tools` rung while preserving matching-weight intact bottom corners (`└`/`┘` or `┗`/`┛`), the tool count, the `Dir:`/`Prmt:`/`Bash` row offsets, and card height.
- **Does not assert:** exact Claude prose beyond the discovered sentinel filename; exact elapsed-time text; multiple cards or density changes caused by terminal height.
- **Platform coverage:** mac+linux.

#### dashboard/selection

##### dashboard/selection/001 — While the selection is active, `j` / `Down` selects the next card and wraps at the end.
- **Layer:** L1 (in-process `handle_normal_key` dispatch).
- **Agent:** none (synthetic card count).
- **Asserts:** starting active on card 0, `j` advances 0→1, `Down` advances 1→2, and `j` wraps 2→0; the selection stays active (`Some(idx)`) throughout.
- **Does not assert:** how the highlight is drawn (covered by `dashboard/selection/010`); the inactive-start jump-to-first (`dashboard/selection/006`).
- **Platform coverage:** mac+linux+windows.

##### dashboard/selection/002 — While the selection is active, `k` / `Up` selects the previous card and wraps at the start.
- **Layer:** L1 (in-process `handle_normal_key` dispatch).
- **Agent:** none.
- **Asserts:** starting active on card 0, `k` wraps 0→2 and `Up` retreats 2→1; the selection stays active throughout.
- **Does not assert:** the inactive-start jump-to-last (`dashboard/selection/007`).
- **Platform coverage:** mac+linux+windows.

##### dashboard/selection/003 — `1`–`9` jumps to card N, focuses its pane, and activates the highlight — even when the selection was inactive.
- **Layer:** L1 (in-process `focus_deck` dispatch).
- **Agent:** none (3 synthetic sessions with pane ids).
- **Asserts:** starting from an inactive selection, `focus_deck(1, …)` activates the highlight on index 1 (`Some(1)`), focuses that card's pane, and enters PaneInput mode.
- **Does not assert:** what `0` or digits past the card count do (kept open until catalogued).
- **Platform coverage:** mac+linux+windows.

##### dashboard/selection/004 — `Esc` clears an active filter.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** with the filter dialog populated, pressing `Esc` returns the visible cards to the unfiltered set.
- **Does not assert:** filter dialog dismissal animation.
- **Platform coverage:** mac+linux.

##### dashboard/selection/005 — A tab switch away from the Dashboard and back clears the card highlight.
- **Layer:** L1 (in-process `dispatch_action` tab-switch path + renderer).
- **Agent:** none (a real second Mode tab; 3 synthetic dashboard cards).
- **Asserts:** with the highlight active on card 2, driving `Action::CycleTabNext` then `Action::CycleTabPrev` leaves the dashboard selection inactive (`None`), and `render_dashboard_cards_to_buffer` paints no `▸` selection marker on any card.
- **Does not assert:** the cyan focus border on embedded panes (unaffected); Mode/Orchestration tab side-pane focus (out of scope).
- **Platform coverage:** mac+linux+windows.

##### dashboard/selection/006 — With the selection inactive, `j` jumps to the first card and activates the highlight.
- **Layer:** L1 (in-process `handle_normal_key` dispatch).
- **Agent:** none.
- **Asserts:** from an inactive selection (`None`), `j` lands the highlight on the first card (`Some(0)`) and the selection becomes active.
- **Does not assert:** the active-state next/wrap behaviour (`dashboard/selection/001`).
- **Platform coverage:** mac+linux+windows.

##### dashboard/selection/007 — With the selection inactive, `k` jumps to the last card and activates the highlight.
- **Layer:** L1 (in-process `handle_normal_key` dispatch).
- **Agent:** none.
- **Asserts:** from an inactive selection (`None`) with 3 cards, `k` lands the highlight on the last card (`Some(2)`) and the selection becomes active.
- **Does not assert:** the active-state prev/wrap behaviour (`dashboard/selection/002`).
- **Platform coverage:** mac+linux+windows.

##### dashboard/selection/008 — With the selection inactive, Enter restores the previously-selected card (not card 0).
- **Layer:** L1 (in-process `switch_tab_with_focus` round-trip + `handle_normal_key` + `dashboard_focus_target`).
- **Agent:** none (3 synthetic dashboard cards; a Mode tab as the round-trip intermediate).
- **Asserts:** with the highlight armed on a non-first card (index 1), a real Dashboard → Mode → Dashboard round-trip clears the live highlight (`selected_index == None`) but the Enter focus target (`dashboard_focus_target`) is the REMEMBERED card (index 1), not card 0; Enter still maps to `Action::Focus`; the active-selection target is the highlighted card and the no-cards target is `None` (both unchanged). Pins the PRD #113 design revision (2026-06-13) Enter-restores-previous behavior.
- **Does not assert:** the pane-focus side effect of `Action::Focus` itself (exercised by `dashboard/selection/003`).
- **Platform coverage:** mac+linux+windows.

##### dashboard/selection/009 — A focused dashboard pane reactivates the highlight on its card.
- **Layer:** L1 (in-process `reconcile_dashboard_selection`).
- **Agent:** none (3 synthetic `(session_id, pane_id)` pairs).
- **Asserts:** from an inactive selection, reconciling with a focused pane that maps to card 1 activates the highlight on `Some(1)`; reconciling with no matching focused pane leaves the selection inactive.
- **Does not assert:** how the focused pane id is obtained from the embedded controller (the per-frame `pane.focused_pane_id()` read).
- **Platform coverage:** mac+linux+windows.

##### dashboard/selection/010 — Startup default: the dashboard is active on card 0 and paints its highlight.
- **Layer:** L1 (in-process state + renderer).
- **Agent:** none.
- **Asserts:** a freshly-built `UiState` is active on card 0 (`Some(0)`); rendering with that selection paints the `▸` marker on the first card's title row, while rendering with an inactive selection (`None`) paints no marker.
- **Does not assert:** the `Last: … Tools: …` card body (covered by `dashboard/pane/*`).
- **Platform coverage:** mac+linux+windows.

##### dashboard/selection/011 — Switching Dashboard → Orchestration → Dashboard leaves the selection inactive (SC1, any-other-tab path).
- **Layer:** L1 (in-process `switch_tab_with_focus` + per-frame `reconcile_dashboard_selection`).
- **Agent:** none (a real Orchestration tab; 3 synthetic dashboard cards).
- **Asserts:** with the highlight armed on card 2, driving the real switch path to an Orchestration tab and back — running the real per-frame reconcile on each frame — leaves `selected_index == None`. Covers the path `selection/005` cannot (the Orchestration tab shares `selected_index` and its always-active reconcile re-arms `Some(0)` in transit, while deactivation fires only on Dashboard-leave).
- **Does not assert:** Orchestration role-pane selection behaviour itself (covered by `tabs/selection/*`).
- **Platform coverage:** mac+linux+windows.

##### dashboard/selection/012 — An inactive selection makes the close-pane action a no-op (no fall back to card 0).
- **Layer:** L1 (in-process `dispatch_action(Action::CloseSelected)`).
- **Agent:** none (3 synthetic dashboard cards with pane ids).
- **Asserts:** with `selected_index = None` (inactive, nothing armed), dispatching `Action::CloseSelected` opens no confirmation, issues no `close_pane` call, and removes no session — it does NOT arm or close card 0. Encodes the PRD invariant (inactive = nothing armed) alongside `dashboard/pane/003`.
- **Does not assert:** the active-selection close behaviour, or mode/orchestration whole-tab teardown.
- **Platform coverage:** mac+linux+windows.

##### dashboard/selection/013 — A steady-state restored focus must not reactivate the highlight after a tab round-trip.
- **Layer:** L1 (in-process `switch_tab_with_focus` + per-frame `reconcile_dashboard_selection`).
- **Agent:** none (a real Mode tab whose agent pane is also a Dashboard card; 3 synthetic cards).
- **Asserts:** driving the real per-frame reconcile across a Dashboard → Mode → Dashboard round-trip, where the Mode agent pane stays focused on both the mode frame and the return dashboard frame (no focus transition), leaves `selected_index == None` — the blue highlight does not reappear. Regression for PR #151; this is the steady-state-focus path `selection_005`/`selection_011` cannot reach.
- **Does not assert:** the cyan controller focus border (driven separately, unaffected).
- **Platform coverage:** mac+linux+windows.

##### dashboard/selection/014 — A genuine focus transition after a steady-state baseline still reactivates the highlight (M4 not over-suppressed).
- **Layer:** L1 (in-process `reconcile_dashboard_selection`).
- **Agent:** none (3 synthetic `(session_id, pane_id)` pairs).
- **Asserts:** from an inactive selection, holding a non-card pane focused across two frames keeps the selection inactive; then transitioning the focus to a dashboard card reactivates the highlight on that card (`Some(0)`). Guards that the focus-transition fix does not block legitimate M4 reactivation; distinct from `selection_009` (transition from the `None` baseline).
- **Does not assert:** the active-selection derive path (covered by `dashboard/pane/005`).
- **Platform coverage:** mac+linux+windows.

##### dashboard/selection/015 — SC1 against the real binary: the highlight clears on a tab round-trip when the focused pane is a Mode agent pane that is also a dashboard card.
- **Layer:** L2 (real `dot-agent-deck` binary in a PTY; vt100 grid scraping).
- **Agent:** a Mode tab agent (fixture shell script) that self-posts `SessionStart` so its agent pane is also a dashboard card; no LLM tokens.
- **Asserts:** with the highlight armed on the Dashboard (a `▸` marker present), switching away to the Mode tab and back to the Dashboard — where the Mode agent pane stays focused (steady state, no transition) and maps to a card — leaves NO `▸` selection marker on any card. This is the real-binary repro the L1 tests cannot provide (their mocks never restore focus to a Mode agent pane on return); pre-fix the steady-state focus re-armed the highlight.
- **Does not assert:** the cyan controller focus border (driven separately, unaffected); the keyboard nav/wrap semantics (covered by `dashboard/selection/001`–`002`).
- **Platform coverage:** mac+linux.

##### dashboard/selection/016 — The inactive-selection close no-op (012) does NOT suppress closing an active Mode/Orchestration tab via Ctrl+W.
- **Layer:** L1 (in-process `dispatch_action(Action::CloseSelected)` against a recording `PaneController`).
- **Agent:** none (a real Mode tab, then a real Orchestration tab; no dashboard cards armed).
- **Asserts:** with a Mode tab active and `selected_index == None`, dispatching `Action::CloseSelected` opens confirmation and `ConfirmCloseSelected` closes that tab (tab count drops back to the lone Dashboard); the same holds for an active Orchestration tab. Bounds the `dashboard/selection/012` no-op gate: the inactive-selection guard suppresses an unarmed dashboard CARD, but an active Mode/Orchestration TAB remains a valid confirmation target. Regression for the PR #151 e2e failure `e2e_render_contract::layout_002`.
- **Does not assert:** the per-pane PTY teardown / role-pane stop (covered by the L2 `tabs/mode/002`, `tabs/orchestration/002`); the dashboard-card close no-op itself (covered by `dashboard/selection/012`).
- **Platform coverage:** mac+linux+windows.

##### dashboard/selection/017 — Enter (Action::Focus) paints the highlight on BOTH decks by setting `selected_index` to the restored target (unified deck behavior).
- **Layer:** L1 (in-process `dispatch_action(Action::Focus)` against a recording `PaneController`).
- **Agent:** none (a real Orchestration tab with placeholder role-pane sessions; 3 synthetic dashboard cards).
- **Asserts:** with the deck inactive (`selected_index == None`) and a remembered selection (`last_active_selection == Some(1)`), dispatching `Action::Focus` (what Enter maps to) sets `ui.selected_index = Some(1)` — so the highlight paints — for the ORCHESTRATION deck AND the Dashboard. Pins the unified fix for the PR #151 manual-test regression where Enter never painted the highlight on the Orchestration deck (the role pane was already focused on return, so the reconcile focus-transition guard never re-armed it). Pre-fix RED: `Action::Focus` only focuses the pane and leaves `selected_index == None`.
- **Does not assert:** the per-frame reconcile reactivation path (`dashboard/selection/009`/`014`); the focus side effect itself (`dashboard/selection/003`).
- **Platform coverage:** mac+linux+windows.

##### dashboard/selection/018 — On tab return, the previously-selected deck's PANE is re-focused while the highlight stays clear — symmetric across BOTH decks (unified deck behavior).
- **Layer:** L1 (in-process `switch_tab_with_focus` round-trip + recording `PaneController`).
- **Agent:** none (a real Mode tab as the round-trip intermediate; an Orchestration tab; 3 synthetic dashboard cards).
- **Asserts:** after a Dashboard → Mode → Dashboard round-trip with a remembered selection (card index 1 → session `s1` → pane `p1`), the controller's last-focused pane is `p1` (the remembered card's pane is re-focused) AND `selected_index == None` (highlight clear). The Orchestration deck already satisfies this (it re-focuses its remembered role pane on return). Pins the unified fix making the Dashboard leave/return symmetric with Orchestration. Pre-fix RED for the Dashboard: it re-focuses nothing on return (its `selected_session_id` is cleared on leave), so the last-focused pane is the Mode pane, not `p1`. Consistent with `dashboard/selection/013` (focused pane present on return, highlight `None`).
- **Does not assert:** the per-frame reconcile staying `None` under steady focus (covered by `dashboard/selection/013`); the scroll/viewport reveal of the remembered region.
- **Platform coverage:** mac+linux+windows.

##### dashboard/selection/019 — Enter paints the selection highlight on the Orchestration deck after a tab round-trip (real binary).
- **Layer:** L2 (real `dot-agent-deck` binary in a PTY; vt100 grid scraping; `e2e` feature).
- **Agent:** none (an orchestration with two `cat` role panes that stay alive as deck cards; no LLM tokens).
- **Asserts:** open the orchestration, detach to Normal mode, arm a role with `j` (a `▸` marker appears), round-trip Orchestration → Dashboard → Orchestration (the `▸` clears), then press Enter — the `▸` selection marker must reappear on the restored role. This is the real-binary repro of the PR #151 manual-test regression the L1 mocks missed (they never run the real reconcile + focus-restore on an orchestration tab): pre-fix the role pane is already focused on return, so Enter is not a focus transition and the highlight never repaints (the final wait times out).
- **Does not assert:** which role index is restored; the cyan controller focus border; the Dashboard's own Enter-paint (already worked via the reconcile transition and is covered at L1 by `dashboard/selection/017`).
- **Platform coverage:** mac+linux.

##### dashboard/selection/020 — Enter on a live card whose pane is not wired locally attaches it on demand instead of deleting the card.
- **Layer:** L1 (`dispatch_action(Action::Focus, …)` against a mock controller whose `focus_pane` fails until `try_hydrate_pane` attaches the pane).
- **Agent:** none.
- **Asserts:** Enter attempts the on-demand attach exactly once, the session survives, and the deck enters `PaneInput`. Pre-fix the failed `focus_pane` was read as "stale card" and the LIVE session was removed — only the digit-jump path (`dashboard/selection/003`) carried the PRD #127 guard.
- **Does not assert:** the real `list_agents`/attach round-trip behind `EmbeddedPaneController::hydrate_pane` (L2 territory); which tab the card belongs to.
- **Platform coverage:** mac+linux+windows.

##### dashboard/selection/021 — Enter still removes a card whose pane the daemon genuinely does not have.
- **Layer:** L1 (same harness, mock reports the pane is not attachable).
- **Agent:** none.
- **Asserts:** the attach is still attempted, the session is removed, and the deck does not enter `PaneInput` — the fix must not turn a genuinely dead card into an undeletable one.
- **Does not assert:** the status-message wording.
- **Platform coverage:** mac+linux+windows.

#### dashboard/filter

##### dashboard/filter/001 — `/` opens the filter input; typing narrows visible cards by display-name substring.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** after typing two characters that match one of three cards, only that card is rendered.
- **Does not assert:** case-sensitivity flag (covered separately when committed).
- **Platform coverage:** mac+linux.

##### dashboard/filter/002 — `Enter` accepts the filter and leaves the dashboard in the filtered view.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** filter dialog closes; the filtered card list remains; `Esc` then clears it.
- **Does not assert:** subsequent re-open behavior of the filter dialog with prior input restored — not yet specified.
- **Platform coverage:** mac+linux.

##### dashboard/filter/003 — `type:<agent>` filters mixed sessions by registry identity and composes with ordinary text (PRD #20 M9).
- **Layer:** L1 (in-process `filter_sessions` pure-data matrix).
- **Agent:** none (synthetic Claude Code, OpenCode, Pi, and Codex session states).
- **Asserts:** `type:claude`, `type:claudecode`, `type:opencode`, `type:pi`, and `type:codex` each select only that agent; type matching is case-insensitive; a remaining text term is ANDed with the type; conflicting `type:codex type:claude` constraints use true AND semantics and yield no matches; an unknown type yields no matches; plain id/cwd/status/display-name matching is unchanged.
- **Does not assert:** the rendered dashboard result (covered by `dashboard/filter/004`).
- **Platform coverage:** mac+linux+windows.

##### dashboard/filter/004 — Typing `type:codex` in the `/` search visibly narrows the dashboard to Codex cards (PRD #20 M9).
- **Layer:** L1 (in-process keyboard handlers + ratatui `TestBackend` dashboard render).
- **Agent:** none (synthetic Claude Code, OpenCode, Pi, and Codex session states).
- **Asserts:** `/` enters filter mode; typing `type:codex` through the filter input leaves the Codex card visible and hides every non-Codex card in the rendered buffer.
- **Does not assert:** accepting or clearing the filter (covered by `dashboard/filter/002` and `dashboard/selection/004`).
- **Platform coverage:** mac+linux+windows.

#### dashboard/rename

##### dashboard/rename/001 — `r` on the selected card opens a rename input pre-filled with the current name.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** rename input appears with the current display name shown; pressing `Esc` cancels without persisting.
- **Does not assert:** which keystrokes are valid in the input box (covered by `pane/rename/*` validators in the lib pure-data tier).
- **Platform coverage:** mac+linux.

##### dashboard/rename/002 — Confirming a valid new name updates the card title and is mirrored via the daemon `SetAgentLabel` request.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** the card title row shows the new name; a subsequent `list_agents` from a parallel daemon client returns the same `display_name`.
- **Does not assert:** persistence across daemon restart (covered by `session/restore/*`).
- **Platform coverage:** mac+linux.

#### dashboard/help

##### dashboard/help/001 — `?` toggles the help overlay; pressing `?`, `Esc`, or `q` dismisses it.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** the overlay region is rendered on `?` and removed on dismissal.
- **Does not assert:** the exact list of keys shown in the overlay (compared against a snapshot under `dashboard/help/002`).
- **Platform coverage:** mac+linux.

##### dashboard/help/002 — Help overlay content matches the committed snapshot.
- **Layer:** L1.
- **Agent:** none.
- **Asserts:** `insta` file snapshot of the overlay buffer; the Ctrl+D row describes a bidirectional command-mode / pane-input toggle rather than the one-way destination `Command mode (dashboard)`.
- **Does not assert:** dynamic content (none today).
- **Platform coverage:** mac+linux+windows.

#### dashboard/config-gen

##### dashboard/config-gen/001 — `g` on a card opens the Generate Config dialog with options Yes / No / Never.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** dialog region appears; arrow keys move between Yes / No / Never; `Enter` on No dismisses without side effects.
- **Does not assert:** what Yes injects into the agent (covered by `orchestration/delegate/*` for delegate-driven prompt injection, and elsewhere if a non-orchestration path emerges).
- **Platform coverage:** mac+linux.

##### dashboard/config-gen/002 — Picking Never adds the cwd to the suppression list and the prompt does not re-open for that directory.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** after Never, re-opening the new-pane flow for the same cwd does not surface the auto-prompt.
- **Does not assert:** filesystem path of the suppression list (an implementation detail).
- **Platform coverage:** mac+linux.

#### dashboard/title

##### dashboard/title/001 — The dashboard title bar renders the fork's `worker-deck` app name, not upstream's `dot-agent-deck` (fork-only rename, never pushed upstream).
- **Layer:** L2 (PTY end-to-end) — the title bar is painted by the private `render_frame`, which has no public `..._to_buffer` L1 seam.
- **Agent:** none (empty `minimal` fixture — dashboard shows "No active sessions").
- **Asserts:** the rendered grid contains `worker-deck` and does not contain the literal `dot-agent-deck` app-name string.
- **Does not assert:** the trailing `N session(s)` text (unaffected by the rename, covered elsewhere) or any other chrome (tab strip, button bar).
- **Platform coverage:** mac+linux.

#### dashboard/layout

##### dashboard/layout/001 — `Ctrl+l` resolves to the same split-cycle `Action` on a Dashboard tab, and walking the `SplitStage` resolver through the full 3-stage cycle (Default 33/67 -> Narrow 25/75 -> Hidden 0/100 -> Default) pins each stage's geometry, using the Dashboard tab's own default ratio rather than Orchestration's (PRD #361 Item 4; PRD #387 M2/M3 makes the stage itself deck-global).
- **Layer:** L1 (pure-data `compute_frame_layout` geometry; no PTY, no TestBackend render).
- **Agent:** none.
- **Asserts:** a Dashboard tab's default frame geometry is the fixed 33/67 split (`dashboard_area` / `panes_area` widths), distinct from Orchestration's 34/66; walking the pure `next_split_stage` resolver Default -> Narrow -> Hidden -> Default and recomputing the frame geometry at each step (via the single deck-global `ACTIVE_SPLIT_STAGE` thread-local, the SAME mirror `orchestration/layout/002` sets — no longer a Dashboard-only `ACTIVE_DASHBOARD_SPLIT_STAGE`) pins the 25/75 Narrow split, the 0/100 Hidden split (sidebar fully collapsed, pane column full-width), and the wrap back to the original 33/67 Default split.
- **Does not assert:** the visible rendered grid (covered by the PTY-attached `tabs/dashboard/001`); cross-tab or cross-tab-type scoping (covered by `tabs/dashboard/001`); Orchestration-tab geometry (covered by `orchestration/layout/002`).
- **Platform coverage:** mac+linux+windows.

#### dashboard/agent-badge

##### dashboard/agent-badge/001 — A session card shows the agent-type badge only when the deck-global toggle is on (fork #339 — restores `370b6228`'s removal behind an off-by-default toggle).
- **Layer:** L1 (ratatui `TestBackend` + color-aware `insta` snapshot).
- **Agent:** none (a live Codex fixture rendered through `render_card_for_mode_to_buffer`, plus named ClaudeCode / OpenCode / Pi / Codex / Devin fixtures).
- **Asserts:** with the toggle off, no card shows its agent-type label and no cell carries that agent's registry `badge_color`; with the toggle on, the card shows `<Label> · <name>` and a cell carries `badge_color` **and** `Modifier::BOLD`, repeated across all five shipped agent types. Also pins D4: a placeholder (`AgentType::None`) card shows `No agent` exactly once (its unrelated status text) even with the toggle on, never a second occurrence from a restored identity segment.
- **Does not assert:** the toggle's keybinding resolution (`keybindings/safety/005`); the deck-global state transition itself (`dashboard/agent-badge/002`); the real binary's default-hidden render path (`dashboard/agent-badge/003`); no hints-bar test (the bar already omits `Ctrl+L`/`Ctrl+E`); no button-bar test (D7 — the feature has no button).
- **Platform coverage:** mac+linux+windows.

##### dashboard/agent-badge/002 — `Action::ToggleAgentTypeBadge` cycles the deck-global toggle hidden -> shown -> hidden, and a bare `m` resolves to the same action without displacing `Enter`'s `Focus` resolution.
- **Layer:** L1 (in-process `dispatch_action` / `handle_normal_key`, in-crate — `handle_normal_key` is private).
- **Agent:** none.
- **Asserts:** a fresh `UiState` starts with the badge hidden; dispatching the toggle once shows it and sets `ui.status_message` to `"Agent badge: shown"`; dispatching again hides it and sets `"Agent badge: hidden"`; `handle_normal_key` resolves a bare `m` (no modifiers) to `Action::ToggleAgentTypeBadge`; `handle_normal_key` still resolves `Enter` to `Action::Focus`.
- **Does not assert:** the rendered card difference (`dashboard/agent-badge/001`); the real binary's key dispatch (`dashboard/agent-badge/003`); no `keybindings/help/002` (the help overlay's rendering of `toggle_agent_type_badge` is unpinned — `keybindings/help/001`'s remapped-config snapshot remaps `toggle_layout` and `help`, not this binding, so it proves nothing about it).
- **Platform coverage:** mac+linux+windows.

##### dashboard/agent-badge/003 — Pressing `m` toggles the agent-type badge on every card through the real binary, proving both the deck-global reach and the bare-`m` alias as the door that works everywhere.
- **Layer:** L2 (PTY end-to-end).
- **Agent:** none (synthetic Claude Code + Codex `SessionStart` hooks).
- **Asserts:** both cards' agent-type labels are absent at rest (default hidden — the only coverage of default-hidden through the real `render_frame`, which no L1 seam reaches); pressing `m` shows `"Agent badge: shown"` in the status bar and both labels appear; pressing `m` again shows `"Agent badge: hidden"` and both labels disappear. Presses only `m`, never `\x0d` — under a legacy PTY `Ctrl+M` decodes as Enter and `FocusPane` wins first (by design).
- **Does not assert:** the enhanced-terminal `Ctrl+M` chord path (covered by `keybindings/safety/005`'s key-mapper assertion — no PTY harness here emulates the kitty keyboard protocol); real-agent execution.
- **Platform coverage:** mac+linux.

### Statuses

#### status/transition

##### status/transition/001 — Session status transitions to Thinking on `UserPromptSubmit`.
- **Layer:** L2.
- **Agent:** none (synthetic hook event written to the per-test hook socket).
- **Asserts:** card status badge reads Thinking after the hook delivery.
- **Does not assert:** the previous status (covered by predecessor tests).
- **Platform coverage:** mac+linux.

##### status/transition/002 — Session status transitions to Working on `PreToolUse`, carrying the tool name.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** badge reads Working; the card's tool row shows the tool's name (e.g. `Read`).
- **Does not assert:** tool-detail formatting beyond presence of the tool name.
- **Platform coverage:** mac+linux.

##### status/transition/003 — Session status transitions to Idle on `Stop`.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** card status reads Idle.
- **Does not assert:** flashing-dot animation cadence.
- **Platform coverage:** mac+linux.

##### status/transition/004 — Session status transitions to Error on a hook-reported error.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** badge reads Error.
- **Does not assert:** error text content (the hook payload is opaque).
- **Platform coverage:** mac+linux.

##### status/transition/005 — Session status transitions to WaitingForInput on `PermissionRequest`.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** badge reads WaitingForInput; the card surfaces a `y`/`n` affordance.
- **Does not assert:** tool-detail of the permission (covered under `prompt/permission/*`).
- **Platform coverage:** mac+linux.

##### status/transition/006 — Session status transitions to Compacting on `PreCompact`.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** badge reads Compacting.
- **Does not assert:** status reverts on `PostCompact` — covered by a follow-up entry.
- **Platform coverage:** mac+linux.

##### status/transition/007 — A `PreToolUse` arriving while WaitingForInput does not override the WaitingForInput badge.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** WaitingForInput sticks until the matching `PostToolUse` or permission resolution.
- **Does not assert:** other badges' precedence rules — covered separately as each is added.
- **Platform coverage:** mac+linux.

#### status/badge

##### status/badge/001 — Status badge color and label render per palette for each session status.
- **Layer:** L1.
- **Agent:** none.
- **Asserts:** snapshot per status enum value renders the expected label and palette entry.
- **Does not assert:** the dot animation frame.
- **Platform coverage:** mac+linux+windows.

#### status/agent-event

##### status/agent-event/001 — A `dot-agent-deck agent-event --type <state>` frame routes into the existing `AgentEvent` stream and drives the target pane's card status, with NO hook and no `settings.json` mutation (PRD #201 M1.2/M1.3).
- **Layer:** L1 (in-process — resolve the lifecycle state via the production seam `dot_agent_deck::event::agent_event_type_from_state`, build the `AgentEvent` via the agent-agnostic synthetic-agent harness, drive `AppState::apply_event`; no daemon socket, no PTY, no hook).
- **Agent:** none (synthetic — the harness at `AgentType::Pi` identity models the pane's injected `DOT_AGENT_DECK_PANE_ID` / `DOT_AGENT_DECK_AGENT_ID`).
- **Asserts:** `agent-event --type running` maps to an `EventType` via the seam; the built frame carries the pane id, agent id, and the Pi agent type; it serializes as a bare `AgentEvent` with NO `message_type` envelope and does NOT parse as a `DaemonMessage` (it rides the existing raw-event wire, zero new surface); routed through `apply_event` on the registered pane it drives the card to a busy (`Thinking`) status.
- **Does not assert:** the full CLI → daemon-socket → `run_hook_loop` path (real-`pi` e2e, M4); the exact `EventType` chosen for `running` beyond that it yields the `Thinking` badge.
- **Platform coverage:** mac+linux+windows.

##### status/agent-event/002 — The Pi synthetic agent emits `running` → `waiting` → `finished` via `agent-event` and the card badge follows each transition (PRD #201 M1.3).
- **Layer:** L1 (in-process — production state→EventType seam + `AppState::apply_event`, driven by the synthetic-agent harness).
- **Agent:** none (synthetic — the harness at `AgentType::Pi` identity).
- **Asserts:** each lifecycle state resolves through the seam (`running`→`Thinking`, `waiting`→`WaitingForInput`, `finished`→`Idle`) and, routed through `apply_event`, the derived `SessionStatus` (the badge source) moves `Thinking` → `WaitingForInput` → `Idle` in lock-step — with no hook and no `settings.json` mutation.
- **Does not assert:** the TS extension's Pi-event-bus → state mapping (M2.2 TS tests); the rendered badge glyph/color (`status/badge/001`).
- **Platform coverage:** mac+linux+windows.

##### status/agent-event/003 — A Pi pane reports running/waiting/finished HEADLESS/UNATTENDED via `agent-event` against the real `daemon serve`, with NO hook installed and no `~/.claude/settings.json` mutation (PRD #201 M2.2).
- **Layer:** L2 (headless `daemon serve` via the `DaemonProc` harness — no PTY, no attached TUI; spawns the real binary, so the `e2e` tier). The Pi extension is stood in for by the real `dot-agent-deck agent-event --type <state>` CLI subprocess; status is observed via an unattended `SubscribeEvents` consumer and the badge derived locally through `AppState::apply_event` (the same seam the production TUI subscriber uses). Hits no LLM.
- **Agent:** synthetic (the `agent-event` CLI reporting `AgentType::Pi` from a pane carrying the daemon's injected `DOT_AGENT_DECK_PANE_ID` / `DOT_AGENT_DECK_AGENT_ID`).
- **Asserts:** each `agent-event --type running|waiting|finished` exits 0 and is re-broadcast by the daemon as a bare `AgentEvent` carrying the Pi identity + injected ids + the mapped `EventType`; fed through `AppState::apply_event` the unattended badge moves `Thinking` → `WaitingForInput` → `Idle`; and a seeded sentinel `~/.claude/settings.json` (whose presence makes the hook-install guard pass) is byte-for-byte unchanged afterward and never gains a `dot-agent-deck` hook entry — proving the daemon/agent-event path installs no Claude hook.
- **Does not assert:** the real `pi` runtime + bundled extension end to end (real-`pi` e2e, M4.1); the daemon's own internal derived status over the wire (`AgentRecord` carries no status field; the broadcast is the observable).
- **Platform coverage:** linux (headless daemon-serve harness).

##### status/agent-event/004 — A typed synthetic Codex wrapper lifecycle updates one dashboard card through active, error, recovery, and idle states (PRD #20 M7).
- **Layer:** L1 (in-process `SyntheticAgent<AgentType::Codex>` events applied through `AppState::apply_event`).
- **Agent:** synthetic Codex wrapper identity.
- **Asserts:** the same Codex session remains one card and its observable status follows Thinking → Error → Thinking → Idle while retaining `AgentType::Codex`.
- **Does not assert:** stdout classification or socket transport (covered by `codex/wrap/001`).
- **Platform coverage:** mac+linux+windows.

##### status/agent-event/005 — A respawned agent whose first event is NOT a `SessionStart` still retires the previous card, so one pane keeps one card.
- **Layer:** L1 (two `SyntheticAgent` generations on one pane, applied through `AppState::apply_event`).
- **Agent:** synthetic Pi identity (the only shipped agent with no `SessionStart`).
- **Asserts:** after a `clear = true` respawn mints a new `agent_id`, the outgoing generation's card is retired by the incoming generation's first `agent-event` (`Thinking`), leaving exactly one session on the pane, carrying the new `agent_id`.
- **Does not assert:** repeated respawns after the initial spawn-time placeholder → first-respawn transition (`status/supersede/005` covers the stable producer id reused by later generations); the orchestration deck's rendering of the duplicate (the unreachable-highlight consequence is pinned by the `sync_and_derive_selection` unit tests in `src/tab.rs`); the Pi extension's own state mapping (TS unit tests).
- **Platform coverage:** mac+linux+windows.

##### status/agent-event/006 — A delayed event from the OUTGOING agent does not retire the incoming agent's live card.
- **Layer:** L1 (out-of-order `AgentEvent` timestamps applied through `AppState::apply_event`).
- **Agent:** synthetic Pi identity.
- **Asserts:** once the incoming generation has established its card, an older-timestamped event from the previous `agent_id` leaves that live card intact — the monotonicity guard that makes retiring on a non-`SessionStart` event safe.
- **Does not assert:** that the stale event is dropped entirely (it may still surface its own card; what must hold is that the LIVE card survives).
- **Platform coverage:** mac+linux+windows.

#### status/supersede

##### status/supersede/001 — A real scheduler agent supersedes its friendly `No agent` placeholder without creating a duplicate card or losing the task name.
- **Layer:** L1 (in-process scheduler placeholder and real `SessionStart` events applied through `AppState::apply_event`).
- **Agent:** none (synthetic ClaudeCode identity for the real hook).
- **Asserts:** a `Some(agent_id)` real session replaces the same-pane `None` placeholder even when its producer timestamp is older, leaving exactly one live card that inherits the placeholder's friendly display name.
- **Does not assert:** the rendered card grid or daemon hook transport (`scheduler/live/004` covers the PTY-attached surface).
- **Platform coverage:** mac+linux+windows.

##### status/supersede/002 — Replacing a session identity on an armed pane leaves the close-confirm target vanished rather than retargeting the replacement.
- **Layer:** L1 (in-process state replacement through `AppState::apply_event`).
- **Agent:** none (synthetic ClaudeCode generations).
- **Asserts:** after a different-agent `SessionStart` takes over the same pane, the armed session id is absent, the replacement id remains, and only one card owns the pane, which makes stable-id close resolution return vanished.
- **Does not assert:** modal rendering or actual close dispatch (`prompt/close-confirm/005` covers the PTY-attached behavior).
- **Platform coverage:** mac+linux+windows.

##### status/supersede/003 — A delayed outgoing `SessionEnd` cannot erase the live replacement card from its pane.
- **Layer:** L1 (in-process terminal event applied through `AppState::apply_event`).
- **Agent:** none (synthetic ClaudeCode generations).
- **Asserts:** after live agent B establishes a card, a newer-stamped `SessionEnd` from outgoing agent A on the same pane leaves B's card present instead of leaving the live pane with zero cards.
- **Does not assert:** daemon hook transport, placeholder restoration for the ending session, or rendered card layout.
- **Platform coverage:** mac+linux+windows.

##### status/supersede/004 — Reordered same-session activity cannot weaken the outgoing-straggler guard.
- **Layer:** L1 (in-process reordered events applied through `AppState::apply_event`).
- **Agent:** none (synthetic Pi generations).
- **Asserts:** live agent B established at T=30 survives an outgoing agent A straggler at T=20 even after B's delayed same-session event at T=10 is delivered between them.
- **Does not assert:** daemon socket task scheduling or that the outgoing straggler is dropped entirely; only the live card's survival is required.
- **Platform coverage:** mac+linux+windows.

##### status/supersede/005 — A repeated Pi respawn refreshes the card identity carried under the pane-derived stable producer session id.
- **Layer:** L1 (successive in-process Pi generations applied through `AppState::apply_event`).
- **Agent:** none (synthetic Pi generations using the production `{pane_id}-session` construction).
- **Asserts:** after Pi agent 2 establishes the stable card and Pi agent 3 reports through the same producer session id, exactly one card remains and carries agent 3's identity.
- **Does not assert:** close-target retargeting across stable-key generations. Close confirmation arms on the session id alone (`CloseTarget::Session`) and resolves it by direct key lookup; because Pi reuses `{pane_id}-session` across respawns, that target remains resolvable after a generation change and confirmation can act on whichever generation currently occupies the pane. This behavior predates #284 and is neither introduced nor worsened by it: before the fix the key resolved to a stale corpse entry, after it resolves to the live replacement, and in both cases it maps to the pane's current card. The #284 identity refresh is a prerequisite for fixing this properly by arming on the generation (session id plus agent id), because the refreshed `agent_id` can now expose a generation change that the pre-fix stale `pi-agent-2` identity would have concealed. That fix belongs at the arm/resolve seam (`CloseTarget` / `resolve_close_plan`), while `prompt/close-confirm/005` remains the close-flow proof. This test also does not assert the initial spawn-time placeholder → first-respawn transition (`status/agent-event/005`), socket transport, or rendered card history.
- **Platform coverage:** mac+linux+windows.

##### status/supersede/007 — A Pi card that already exists inherits the friendly name when its newer status retires the scheduler placeholder.
- **Layer:** L1 (out-of-order scheduler placeholder and Pi events applied through `AppState::apply_event`).
- **Agent:** none (synthetic scheduler placeholder and Pi identity).
- **Asserts:** an older first Pi frame initially coexists with the friendly scheduler placeholder, then a newer Pi status retires it and leaves one Pi card carrying `morning-digest`.
- **Does not assert:** scheduler dispatch, daemon socket delivery, or rendered card layout.
- **Platform coverage:** mac+linux+windows.

#### status/shell-activity

##### status/shell-activity/001 — The process-table primitive finds a real, detached grandchild process as a descendant and reports its no-controlling-tty / session-leader / argv / session-id facts correctly (PRD #386 M1).
- **Layer:** L1.
- **Agent:** none (a real `sleep` process, spawned and `setsid()`'d by the test itself — no agent involved).
- **Asserts:** `process_table()` enumerates the machine's processes and `descendants()` finds a real grandchild of the test process (spawned on pipes, detached via `setsid()`) as a descendant of the test's own pid; the found entry reports no controlling terminal, session-leader true, its full argv (a uniquely marked command line), and a session id differing from the test process's own (checked independently via `libc::getsid(0)`) — the real-process proof that `getsid` reads what the M2 discriminator's load-bearing condition assumes it reads. On Windows, `process_table()` returns `None` (no process-enumeration backend exists there — same contract as `foreground_pgid`), and — fork issue #160 F9 — `process_table_async()` returns `Err(ProcessTableOutcome::Unsupported)`, the permanent-for-the-process's-life contract distinct from a transient `Failed` sample, that previously had zero coverage anywhere in this suite.
- **Does not assert:** that this primitive is wired into any pane's status, that the discriminator (`descendant_shell_activity`, `status/shell-activity/003`) classifies anything as "busy", or anything about a real agent pane — this is a mechanism test only, included so a later failure localises. PRD #370's failure was exactly a correct mechanism test attached to nothing; this test proves nothing about the shell-activity feature working end to end on its own.
- **Platform coverage:** mac+linux (real-process assertion) + windows (the `None` contract).

##### status/shell-activity/002 — The descendant walk terminates instead of looping forever when a synthetic process table contains a `ppid` cycle (PRD #386 M1).
- **Layer:** L1.
- **Agent:** none (a hand-built synthetic table — no real processes, no `ps` involved).
- **Asserts:** `descendants()` called against a table where a `ppid` cycle loops back to the root pid returns within a bounded timeout (not an infinite loop) and reports each reachable non-root descendant exactly once, correctly excluding the root pid even though the cycle links back to it.
- **Does not assert:** anything about how a real `ps` sample could produce such a cycle, or the discriminator/classification logic — purely a termination/dedup guarantee on the walk.
- **Platform coverage:** mac+linux+windows (pure data, no OS process calls).

##### status/shell-activity/003 — The structural session-id discriminator classifies the measured Bash-tool descendant as busy and every measured confounder as idle, unchanged when every process has no controlling terminal (PRD #386 M2).
- **Layer:** L1.
- **Agent:** none (hand-built fixture tables reproducing the `getsid`/`ps` captures from `.dot-agent-deck/386-argv-notes.md` and the PRD — no real processes, no `ps` involved).
- **Asserts:** `descendant_shell_activity(table, root_pid, shapes)`, called with the argv cross-check disabled (`shapes: &[]`), returns `Some(true)` for a table containing the measured Bash-tool descendant (its own POSIX session, differing from the agent's) alongside the agent's five measured long-lived children (`context7`, `task-master`, `engram`, `pysemgrep`, `caffeinate`, all in the agent's own session), and `Some(false)` for the same table with the Bash-tool descendant removed — pinning the claim that the session-id test alone, without any argv help, already excludes every measured confounder. A third and fourth case rebuild both tables with every row, the agent included, reporting no controlling terminal (the CI/container shape measured in `386-argv-notes.md` §5) and assert classification is unchanged — the direct regression test for a bare no-controlling-terminal fallback collapsing where the agent itself has no terminal either.
- **Does not assert:** the argv cross-check itself (`shapes` is empty throughout — that path is exercised by a real agent in `status/shell-activity/005`, the M6a rot canary), that this primitive is wired into any pane's status, or anything about a real agent pane. One measured field is a documented derivation rather than a direct reading: the fixture's `task-master`/`pysemgrep` session ids are inferred from their measured `ps` `pgid` (which coincides with `sid` throughout this tree), not from an explicit `getsid` line in the notes, which list only three of the five confounders by name.
- **Platform coverage:** mac+linux+windows (pure data, no OS process calls).

##### status/shell-activity/004 — `RunningAgent::shell_foreground_busy` (via the registry's `shell_foreground_busy_snapshot` seam) flips idle → busy → idle for a real, detached, pipes-only descendant of a real PTY pane's shell (PRD #386 M3).
- **Layer:** L1 (real PTY pane spawned through `AgentPtyRegistry`; real `setsid()`'d `ps`-visible child — no daemon, no hooks).
- **Agent:** none (a real `/bin/sh` pane spawned by the test, whose script launches a real `python3`-then-`/bin/sleep` child that `setsid()`s itself — no AI agent involved).
- **Asserts:** the pane is spawned with `agent_type: Some(AgentType::ClaudeCode)` — load-bearing, since `shell_tool_shape_key` selects `CLAUDE_BASH_TOOL_SHAPE` only for that agent kind and `shell_foreground_busy_snapshot` filters the shapes it is handed down to `&[]` for any other kind before the scan ever sees them; without this the test's `&[CLAUDE_BASH_TOOL_SHAPE]` argument would be discarded before reaching the classifier. With that in place, `shell_foreground_busy_snapshot(&[CLAUDE_BASH_TOOL_SHAPE])` reads idle for the pane before the detached child appears, busy while it lives, and idle again once it is killed — the rising *and* falling edge, so an implementation that only sets busy and never clears would still fail here. Independently confirms, via `process_table()` + `descendants()` on the real sample, that the found descendant has no controlling terminal, is its own session leader, and carries a POSIX session id different from the pane's own shell — the exact topology (on pipes, off the PTY entirely, in its own session) PRD #370's `tcgetpgrp`-based test could never produce, because #370 typed its command directly into the pane's PTY, keeping the child in the pane's own foreground process group. The detached child's argv is crafted to carry the measured Bash-tool shape (`shell-snapshots/snapshot-` and `&& eval `), and — because the pane now carries the Claude agent kind — the argv cross-check is genuinely exercised against a real process rather than only the fixture strings in `status/shell-activity/003`.
- **Does not assert:** anything about a real AI agent's Bash tool, the daemon's poll task (`run_shell_activity_monitor`), or the `pane_hook_session_id` gate — this is the pane-primitive layer only. `status/shell-activity/005`–`007` (M6a/b/c) carry the burden of proving the signal fires for a real agent.
- **Platform coverage:** mac+linux (real-process assertion; not run on Windows, where `process_table()` is unconditionally `None`).

##### status/shell-activity/005 — A real interactive Haiku Claude agent's Bash-tool call trips the descendant scan: the daemon's shell-activity monitor synthesizes a `ShellBusy` broadcast event for the pane (PRD #386 M6a, the rot canary).
- **Layer:** L2, PTY-attached, real agent (drives the actual `dot-agent-deck` binary, which lazily spawns its own daemon; no synthetic hook, no fabricated `SessionStart`, no hand-set `pane_id`).
- **Agent:** a real interactive `claude --model claude-haiku-4-5-20251001 --allowedTools Bash` pane, spawned through the normal Ctrl+N new-pane flow with per-folder trust pre-seeded, exactly as a user would drive it.
- **Asserts:** after a directive prompt (naming a uniquely-named sentinel fixture file so the wording survives LLM phrasing variance) drives the agent to make exactly one Bash tool call running `ping -c 20 127.0.0.1 > /dev/null` — real, non-blocked foreground work lasting ~19-20s, since Claude Code blocks long `sleep` at the tool layer and emits no `ToolStart` for it — the test first confirms the native `ToolStart`/`Bash` hook event fired (precondition), then asserts, over a live `SubscribeEvents` connection (never against the rendered grid), that the daemon's `run_shell_activity_monitor` poll synthesizes a `ShellBusy` `AgentEvent` carrying this pane's `pane_id`, within 15s of `ToolStart` — comfortably inside the ~19-20s command window. The badge is never the pass/fail signal: the pane already reads `Working` from `ToolStart` alone regardless of whether this mechanism fires at all, which is exactly how PRD #370's mechanism shipped green while dead. A miss here means either Claude Code stopped `setsid`-detaching its Bash-tool child (a total, silent false negative) or the descendant scan is not reaching this pane in production — never a fixture-only artifact, since nothing here is a fixture.
- **Does not assert:** the falling edge (`ShellIdle` once the command completes — that is `004`'s job against a stand-in, not repeated here against a real agent), the >120s-cap user-visible badge scenario (`status/shell-activity/006`, M6b), or no-false-positive-at-idle (`status/shell-activity/007`, M6c). The soft sentinel-content check on the model's final reply is logged, not gating — matching the model's free-text reply is more phrasing-sensitive than the two typed events the test actually gates on.
- **Platform coverage:** mac+linux (real Claude Code interactive session; not run on Windows, and gated behind the `e2e` feature with no CI credentials — local-only, rule 5 exception (a)).

##### status/shell-activity/006 — A real interactive Haiku Claude agent's Bash call that crosses Claude Code's 120s default timeout keeps the pane's rendered badge on `Working`, with the command genuinely still running — the reported bug, reproduced as the user actually sees it (PRD #386 M6b). [reel]
- **Layer:** L2, PTY-attached, real agent (drives the actual `dot-agent-deck` binary, which lazily spawns its own daemon; no synthetic hook, no fabricated `SessionStart`, no hand-set `pane_id`).
- **Agent:** a real interactive `claude --model claude-haiku-4-5-20251001 --allowedTools Bash` pane, spawned through the normal Ctrl+N new-pane flow with per-folder trust pre-seeded, exactly as a user would drive it.
- **Asserts:** a directive prompt drives the agent to make exactly one Bash tool call running `ping -c 200 127.0.0.1 > <sentinel> 2>&1` — real, non-blocked foreground work lasting ~200s under **default** Bash settings (no `timeout` parameter, no `run_in_background`), reproducing the reported case exactly. After the native `ToolStart`/`Bash` hook event confirms the call actually started, the test waits for the real, native `Idle` event — mapped from Claude Code's own `Stop` hook, never fabricated — for this agent, bounded at 157s (the PRD's own measured 127s `ToolStart`-to-`Idle` gap plus a 30s margin). Only once that `Idle` genuinely lands does the test switch to the dashboard (`Ctrl+D`) and assert the rendered card badge reads `Working` (not `Idle`) — sampled at the instant a broken monitor would have painted it `Idle`. It also independently proves the command is genuinely still running at that moment: `process_table()` + `descendants()`, walked from the test binary's own pid (never a global `ps` scan, so a concurrently running e2e test's own processes can't be mistaken for this one's), finds a live process carrying the sentinel text in its argv (the Bash-tool shell's `eval '<user command>'` segment) with a live `ping` process beneath it. A miss on either half — badge or process — means the fix did not land or a different bug (a stale badge next to a finished command) is passing as one. **A PASS therefore means the bug path was genuinely exercised** — this is the guarantee added for PRD #386's tester follow-up: previously the test sampled on a fixed wall-clock offset regardless of whether Claude Code's `Stop` hook had actually fired, so roughly two runs in three (when Claude ended the capped call with `ToolEnd` and no `Stop`) passed without the card ever having anything to recover from. If the real `Idle` does not land within the bound, the test now **fails loudly** with a `PRECONDITION NOT MET` message distinguishing "this run never exercised the bug path" from "the badge was actually wrong" — an inconclusive run is never reported as a pass.
- **Does not assert:** anything about the discriminator's internals (`descendant_shell_activity`, `003`) or the pane primitive in isolation (`004`) — this test only observes the full pipeline's user-visible output. Does not assert what happens after the sample point (the eventual falling edge once the 200s ping finishes) or what the agent does with the "moved to background" tool result. A `PRECONDITION NOT MET` panic is not a badge assertion at all — it asserts nothing about `Working`/`Idle` and must be read as inconclusive (rerun), not as evidence the fix broke.
- **Platform coverage:** mac+linux (real Claude Code interactive session; not run on Windows, and gated behind the `e2e` feature with no CI credentials — local-only, rule 5 exception (a)).

##### status/shell-activity/007 — A real interactive Haiku Claude agent left at its idle prompt, with its real MCP servers alive as children, keeps the pane's rendered badge on `Idle` — no false positive against a live process table (PRD #386 M6c).
- **Layer:** L2, PTY-attached, real agent (drives the actual `dot-agent-deck` binary, which lazily spawns its own daemon; no synthetic hook, no fabricated `SessionStart`, no hand-set `pane_id`).
- **Agent:** a real interactive `claude --model claude-haiku-4-5-20251001 --allowedTools Bash` pane, spawned through the normal Ctrl+N new-pane flow with per-folder trust pre-seeded — never sent a prompt, left at its own idle prompt exactly as a user who opened a pane and stepped away would leave it.
- **Asserts:** after confirming, by polling the real process table (`process_table()` + `descendants()`, walked from the test binary's own pid so a concurrent test's own processes can't be mistaken for this one's), that the agent genuinely has live children (its MCP servers and whatever else Claude Code keeps alive) — a precondition, since "an agent with no children proves nothing here" — the test waits a margin past the daemon's 500ms shell-activity poll and then asserts the dashboard's rendered card badge reads `Idle`, not `Working`. It re-samples the process table at the same moment to confirm the children are STILL alive (not just before the badge check) and logs their argv as the evidence for what was actually running. It then also runs `descendant_shell_activity()` directly against that live table and asserts it independently agrees (`Some(false)`) — the M2 fixture claim (`003`), proven here against a live process table rather than a captured one.
- **Does not assert:** which specific MCP servers are present — that is whatever the operator's real `~/.claude.json` configures (carried into the seeded test HOME by `seed_claude_trust_in_home`), logged for evidence rather than asserted by name, since a hardcoded expected set would tie the test to one machine's configuration. Does not assert anything about a busy pane (`006`, `005`) or about agent kinds other than Claude.
- **Platform coverage:** mac+linux (real Claude Code interactive session; not run on Windows, and gated behind the `e2e` feature with no CI credentials — local-only, rule 5 exception (a)).

##### status/shell-activity/008 — The daemon's `ingest_event` broadcasts and applies one event under a SINGLE write-lock acquisition, so a second concurrent producer cannot interleave between the two (fork issue #31, the regression test for `efbc31a`).
- **Layer:** L1 (an in-process `AppState` + `broadcast` channel on a current-thread tokio runtime — no daemon socket, no PTY, no processes).
- **Agent:** none (two synthesized `AgentEvent`s standing in for the two real producers, `run_shell_activity_monitor` and `run_hook_loop`).
- **Asserts:** with one managed session in place, a first producer is parked inside `ingest_event`'s body at the exact boundary the fix created — after the broadcast, before the `apply_event` — via a generic hook injected through `ingest_event_with_hook` (production instantiates the same generic with `|| async {}`). While it is parked holding the write guard, a second producer calling the UNMODIFIED production `ingest_event` for the same pane is polled by hand and must return `Pending` on all 5 polls, each separated by a `yield_now()` that hands the runtime every chance to advance it — deterministic, with no sleep and no retry loop, because the guarantee is structural (`tokio::sync::RwLock` admits one writer, and `broadcast::Sender::send` is synchronous, so no yield point remains inside the critical section). Once released, both complete and the two orders must agree: the broadcast stream carries `ShellBusy` then `Idle`, and the applied session status ends `Idle`. A `Working` status is the bug itself — `Idle` applied first (clearing the synthetic marker) then `ShellBusy` promoting the session back to `Working`, while every attached TUI rendered `Idle` from the opposite broadcast order, with nothing afterwards correcting it (the monitor's level-aware re-emit tests the *daemon's* status, already `Working`, so it stays silent). Confirmed non-vacuous by mutation: hand-simulating the pre-fix shape inside the helper (drop the guard after `send`, reacquire it separately for `apply_event`) fails this test deterministically on the pending assertion.
- **Does not assert:** anything about a producer that BYPASSES the helper. The seam covers `ingest_event`'s internals only — a new call site doing `event_tx.send(...)` plus its own `state.write().await`, or a refactor that re-inlines the body, reopens the identical window and this test stays green. That is a visible, small-diff change for review to catch. Also asserts nothing about the descendant scan or the shell-activity classifier (`status/shell-activity/001`–`004`), about a real agent (`005`–`007`), or about what a TUI renders — it stops at the daemon's own broadcast and applied state.
- **Platform coverage:** mac+linux+windows (pure in-process async, no OS process calls).

##### status/shell-activity/009 — `shell_foreground_busy_snapshot_in` names the specific pane that went unconfirmed, not merely a count (fork issue #216).
- **Layer:** L1 (fast unit test, `src/agent_pty.rs`; built directly against `AgentPtyRegistry::shell_foreground_busy_snapshot_in` with a synthetic process table).
- **Agent:** two real spawned panes ("pane-a", "pane-b", both `/bin/sh` via `SpawnOptions::default()`) so `RunningAgent::child.process_id()` is a real, live pid — only the process TABLE handed to the classifier is synthetic, so the two panes' classification outcomes are deterministic instead of depending on what each shell's actual descendants happen to be at the moment of the sample.
- **Asserts:** with pane-a's root present in the table (a readable session id, no descendants — classifies as confirmed idle) and pane-b's root entirely absent from the table (a live candidate the scan attempted but the sample never carried), the snapshot reports `candidates == 2`, `statuses == vec![("pane-a", false)]`, and — the point of this test — `unconfirmed == vec!["pane-b"]`, asserted **by value**, naming the specific pane. `candidates` and `statuses.len()` alone cannot discriminate this from an idle deck with only one live pane; both already existed before this fix and were already insufficient, which is the entirety of #216's premise. Also asserts the invariant `statuses.len() + unconfirmed.len() == candidates`, since the daemon's per-pane `pane_unconfirmed_streaks` streak logic (`run_shell_activity_monitor`, `src/daemon.rs`) depends on it holding.
- **Does not assert:** the daemon's `pane_unconfirmed_streaks` HashMap or its `tracing::warn!` cadence gate — `run_shell_activity_monitor` is private, async, and its only externally-observable effect is that log line, so it gates no event, no status decision, and no wire format; there is no tracing-capture infrastructure in this crate to observe it from a test. This test stops at the `ShellForegroundBusySnapshot` seam the daemon loop consumes, not the loop itself. Also does not assert anything about a real degraded pane (a process actually racing its own exit) — the absent-from-the-table case stands in for that deterministically.
- **Platform coverage:** mac+linux+windows in principle (`descendant_shell_activity` and `ProcessInfo` are pure data per `scan.rs`'s module doc), but this test lives in `agent_pty.rs`'s `spawn_tests` module, which is `#[cfg(all(test, unix))]` because it spawns real PTYs — so it runs mac+linux only, gated by the module, not by anything platform-specific in this test itself.

##### status/shell-activity/010 — A `ps` capture whose output exceeds a size cap reports no sample, the same shape a time-budget overrun already reports (fork issue #212).
- **Layer:** L1 (in-src unit test, `src/platform/proc/unix.rs`; no daemon, no PTY).
- **Agent:** none (`head -c <PS_SAMPLE_BYTE_CAP + 1> /dev/zero` stands in for a process whose `ps` row would be oversized — deterministic and fast, so it finishes well inside `PS_SAMPLE_BUDGET` and cannot be caught by the time bound alone). The fixture size is derived from `PS_SAMPLE_BYTE_CAP` at test run time (fork issue #160's audit) rather than hand-picked, so raising the cap can never again leave this fixture silently under it — a `400000`-byte literal against a cap later raised to 4 MiB would have stopped exercising the size-cap path entirely without failing.
- **Asserts:** both `capture_bounded` (sync) and `capture_bounded_async` (async) report `None` — "no sample" — for a capture one byte over the size cap, exactly like `a_sample_that_outruns_its_budget_reports_no_sample` already asserts for a time-out. Because `process_table`/`process_table_async` already map a `None` capture to `ProcessTableOutcome::Failed` (fork issue #160), and the daemon's existing fail-safe (`last_known` untouched, nothing emitted) already handles `Failed` unchanged, a size cap implemented inside the capture functions needs no new code path anywhere else — this test's whole job is to pin the `None` shape at the capture layer.
- **Does not assert:** the exact byte threshold chosen for the cap (a coder decision, pinned only by reference to the constant, not by a literal), anything about `process_table_async`'s `Failed`/`Unsupported` split (`status/shell-activity` — see fork issue #160's tests in this same file), or the daemon's fail-safe handling of `Failed` itself (already covered by PR #206's tests, unchanged by this fix).
- **Platform coverage:** mac+linux (`head -c`/`/dev/zero` are POSIX; this file is `#[cfg(unix)]` throughout).

##### status/shell-activity/011 — `descendant_shell_activity` reports unknown, not a confident idle, when the only candidate descendant's session id could not be read (fork issue #160's note on #216).
- **Layer:** L1 (in-src unit test, `src/platform/proc/scan.rs`; pure function over a synthetic `&[ProcessInfo]` table, no PTY and no real processes).
- **Agent:** none (synthetic process-table fixtures via the module's `row()` helper).
- **Asserts:** when a root's only candidate descendant has an unreadable session id (`session_id <= 0`), `descendant_shell_activity` returns `None` rather than `Some(false)` — the per-row fail-safe PR #206's `ProcessTableOutcome` split established at the per-sample level (`status/shell-activity/001`–`004`), extended to the per-row level scan.rs's `continue`-then-fall-through currently misses. The discriminating case is asserted in the same test: a table where every descendant's session id was validly read and genuinely matches the root's own must still return `Some(false)` — proving a fix can't satisfy this test by simply never resolving to idle.
- **Does not assert:** anything about a real process table, a real agent, or the daemon's poll loop (`run_shell_activity_monitor`) — this is the pure discriminator only. Does not assert what happens when SOME candidates are unreadable and OTHERS are confirmed busy (a confirmed-busy candidate already short-circuits to `Some(true)` before the unreadable one is reached, per the function's existing candidate-order semantics, untouched by this fix).
- **Platform coverage:** mac+linux+windows (pure data, no OS process calls — `scan.rs` compiles everywhere per its module doc comment).

### Agent protocol

#### protocol/live-target

##### protocol/live-target/001 — `AgentEvent.live_target` preserves every target-kind and writability value while remaining optional for legacy events (PRD #20 M3).
- **Layer:** L1 (pure serde wire contract).
- **Agent:** none (JSON fixtures).
- **Asserts:** every Cartesian combination of `process|pty|tmux|sdk|none` and `live|history-only|none` survives an `AgentEvent` deserialize/serialize round trip; a legacy event without the field still deserializes and reserializes with the optional field omitted.
- **Does not assert:** state propagation or rendering (covered by `dashboard/pane/009`).
- **Platform coverage:** mac+linux+windows.

##### protocol/live-target/002 — A declared non-live capability survives eviction of its declaring event from bounded recent history (PRD #20, blocker 2).
- **Layer:** L1 (in-process `AppState::apply_event` state transition).
- **Agent:** synthetic Codex events.
- **Asserts:** after a history-only `SessionStart` and 51 later events omitting `live_target`, the session remains `Writable::HistoryOnly` rather than falling back to Live when the first event leaves the 50-entry journal.
- **Does not assert:** reconnect serialization (covered by `session/live/010`) or card rendering (`dashboard/pane/009`).
- **Platform coverage:** mac+linux+windows.

#### protocol/send-result

##### protocol/send-result/001 — Every input-delivery result retains its distinct public wire value (PRD #20 M3).
- **Layer:** L1 (pure serde wire contract).
- **Agent:** none (JSON fixtures).
- **Asserts:** `applied`, `queued`, `stale`, `wrong-session`, `history-only`, and `no-live-target` each survive an `AttachResponse` deserialize/serialize round trip.
- **Does not assert:** actual pane delivery or rendered feedback (covered by `prompt/pane-input/004`).
- **Platform coverage:** mac+linux+windows.

#### daemon/status

##### daemon/status/001 — `dot-agent-deck daemon status` names a managed agent and visibly reflects a driven live status, not a placeholder identical to an agent with no session.
- **Layer:** fast synthetic real-binary-subprocess integration (the REAL `dot-agent-deck daemon status` CLI as a subprocess + an in-process daemon attach socket, `common::spawn_inprocess_daemon`, + real `ListAgents`; no PTY attach, no LLM, no `e2e` feature gate).
- **Agent:** none (synthetic — two `cat`-stub worker panes registered as managed; one is driven to `Thinking` over the daemon's hook socket exactly as `agent-event --type running` would, the other never receives an event, as a same-daemon control).
- **Asserts:** the subprocess exits successfully; its stdout names both the driven and the control agent by pane id; and, after normalizing BOTH the pane id and the registry agent id out of each agent's own output lines (the latter differs per spawn regardless of live status), the driven agent's text differs from the control agent's — proving the command actually surfaces the live status rather than an identical placeholder or one that differs only by identity fields. Deliberately does not pin column layout, exact status wording, or row ordering.
- **Does not assert:** `--json` output shape (`daemon/status/002`); the no-daemon path (`daemon/status/003`); prompt/task redaction (`daemon/status/004`).
- **Platform coverage:** mac+linux.

##### daemon/status/002 — `dot-agent-deck daemon status --json` emits a machine-readable document carrying `schema_version` with an entry for the managed agent.
- **Layer:** fast synthetic real-binary-subprocess integration (the REAL `dot-agent-deck daemon status --json` CLI as a subprocess + an in-process daemon attach socket + real `ListAgents`; no PTY attach, no LLM, no `e2e` feature gate).
- **Agent:** none (synthetic — a `cat`-stub worker pane driven to `Thinking` over the daemon's hook socket).
- **Asserts:** the subprocess exits successfully; its stdout parses as a JSON object; the parsed document carries a `schema_version` key; and the raw JSON text names the managed agent by its pane id. Deliberately does not pin any field name beyond `schema_version`, since the rest of the document shape is the coder's to choose (the design rationale).
- **Does not assert:** the human-readable table (`daemon/status/001`); full field-by-field JSON shape.
- **Platform coverage:** mac+linux.

##### daemon/status/003 — `dot-agent-deck daemon status` against an unreachable daemon fails distinguishably from a crash and from clap's own unrecognized-subcommand error, and never brings a daemon into existence at the socket it queried.
- **Layer:** fast synthetic real-binary-subprocess integration (the REAL `dot-agent-deck daemon status` CLI as a subprocess against a scratch attach-socket path with nothing listening; no in-process daemon, no PTY, no LLM, no `e2e` feature gate).
- **Agent:** none.
- **Asserts:** the subprocess does not report success; its stderr carries no Rust panic; its exit code is not clap's own generic usage/parse-error code (`2`) and its stderr carries no clap `Usage:` banner — ruling out "this build's CLI does not understand the `status` subcommand" as the reason for the failure, so it stays distinguishable from a genuinely-handled "no daemon reachable" outcome; and the queried socket path still does not exist on disk afterward, proving the diagnostic never spawned a daemon. Deliberately does not pin the exact exit code value or message wording (the design rationale).
- **Does not assert:** the live-agent path (`daemon/status/001`/`002`); prompt redaction (`daemon/status/004`).
- **Platform coverage:** mac+linux.

##### daemon/status/004 — `dot-agent-deck daemon status` never prints prompt text into its output.
- **Layer:** fast synthetic real-binary-subprocess integration (the REAL `dot-agent-deck daemon status` CLI as a subprocess + an in-process daemon attach socket + real `ListAgents`; no PTY attach, no LLM, no `e2e` feature gate).
- **Agent:** none (synthetic — a `cat`-stub worker pane driven to `Thinking` over the daemon's hook socket with a distinctive sentinel seeded into `user_prompt`, landing in `SessionState::last_user_prompt`/`first_prompts`).
- **Asserts:** the subprocess exits successfully and the seeded sentinel never appears anywhere in its combined stdout/stderr.
- **Does not assert:** `--json` output (the design doc scopes the no-prompt-text requirement to the human view); task-file/delegate text (out of scope for `ListAgents`-derived data).
- **Platform coverage:** mac+linux.

#### worktree/reclaim

##### worktree/reclaim/001 — `dot-agent-deck worktree list` succeeds in a git repo and names the worktree it examined.
- **Layer:** fast synthetic real-binary-subprocess integration (the REAL CLI as a subprocess against real `git` repos in a tempdir, with a synthetic `gh` on `PATH`; no PTY, no daemon, no LLM, no `e2e` feature gate).
- **Agent:** none.
- **Asserts:** the command exits successfully and its output names the examined worktree. Deliberately does not pin the verdict wording or column layout, which are the implementation's to choose.
- **Does not assert:** the removal path (`worktree/reclaim/002`); JSON shape (`006`).
- **Platform coverage:** mac+linux.

##### worktree/reclaim/002 — A deck-owned, MERGED, clean worktree is reclaimed even though its commits are NOT in `main`'s ancestry (the squash-merge case), and its branch survives.
- **Layer:** fast synthetic real-binary-subprocess integration (as `worktree/reclaim/001`).
- **Agent:** none (a real worktree on a branch carrying its own commit, with the stub `gh` reporting `MERGED` — the exact shape a squash-merged branch has locally).
- **Asserts:** first, a **fixture precondition** that `git branch --merged main` does NOT list the branch, so the test provably exercises the ancestry-vs-PR-state divergence rather than passing for the wrong reason; then that the worktree directory is gone after `reclaim --yes`, and that `git branch --list` still shows the branch — committed work stays recoverable.
- **Does not assert:** remote branch state; the ownership-marker file format (only that marking a tree makes it reclaimable).
- **Platform coverage:** mac+linux.

##### worktree/reclaim/003 — A dirty worktree is never removed, even with a MERGED PR and `--yes`, and the report says why it was kept.
- **Layer:** fast synthetic real-binary-subprocess integration (as `worktree/reclaim/001`).
- **Agent:** none (an untracked file placed in an otherwise-reclaimable worktree).
- **Asserts:** first, that the exit code is not clap's own generic `2` and stderr carries no clap `Usage:` banner — ruling out "this build's CLI does not understand `worktree reclaim`" as the reason the worktree survives, so the domain assertion below is not vacuously true; then that the worktree still exists after `reclaim --yes`, and the output names dirtiness/uncommitted/untracked as the reason. The untracked file was never part of the PR, so it is genuinely absent from `main` — the case the "the code is already merged" argument does not cover.
- **Does not assert:** the exact wording of the reason; behaviour for tracked-but-modified files (the same gate, one representative case tested).
- **Platform coverage:** mac+linux.

##### worktree/reclaim/004 — A worktree whose branch IS an ancestor of `main` but has NO pull request is never removed.
- **Layer:** fast synthetic real-binary-subprocess integration (as `worktree/reclaim/001`).
- **Agent:** none (a branch created at `main`'s tip with no canned PR fixture, so the stub `gh` returns `[]`).
- **Asserts:** a **fixture precondition** that `git branch --merged main` DOES list the branch — so the ancestry check's false-positive is genuinely present — then, as in `003`, that the exit code and stderr rule out clap's own unrecognized-subcommand error (without this, "the worktree still exists" would hold vacuously today, since clap never touches the filesystem either) — and finally that the worktree still exists after `reclaim --yes`. This is the destructive direction of the naive check: the same shape as a live scratch worktree that a "git says merged, delete it" rule would destroy.
- **Does not assert:** the reason wording; closed-unmerged or open-PR states (same gate, distinct fixtures).
- **Platform coverage:** mac+linux.

##### worktree/reclaim/005 — A foreign (unmarked) merged clean worktree is asked about, not removed, and the ask names the exact path and the command that would proceed.
- **Layer:** fast synthetic real-binary-subprocess integration (as `worktree/reclaim/001`).
- **Agent:** none (a reclaimable worktree deliberately left without an ownership marker).
- **Asserts:** as in `003`/`004`, first that the exit code/stderr rule out clap's own unrecognized-subcommand error; then that the worktree still exists after a bare `reclaim`; the output contains the worktree's exact path (not a count or a category); and it contains `--yes`, the specific command that would proceed. Pins the "when it asks, it asks specifically" requirement.
- **Does not assert:** interactive confirmation (this is the non-interactive path); the ordering of ask-versus-detail in the output, which is not mechanically checkable here.
- **Platform coverage:** mac+linux.

##### worktree/reclaim/006 — `dot-agent-deck worktree list --json` emits a document carrying `schema_version` and the examined worktree.
- **Layer:** fast synthetic real-binary-subprocess integration (as `worktree/reclaim/001`).
- **Agent:** none.
- **Asserts:** stdout parses as JSON, carries a `schema_version` key, and includes the examined worktree. Deliberately does not pin field names beyond `schema_version`.
- **Does not assert:** the full document shape; per-verdict field naming.
- **Platform coverage:** mac+linux.

##### worktree/reclaim/007 — PR state is resolved against a worktree's OWN `origin` remote, not the caller's cwd — regression coverage for the `resolve_pr_state(repo_dir, ...)` → `resolve_pr_state(&wt.path, ...)` fix.
- **Layer:** fast synthetic real-binary-subprocess integration (as `worktree/reclaim/001`).
- **Agent:** none (the main checkout's `origin` is removed entirely, then a worktree is given its own `origin` via `extensions.worktreeConfig` naming a repo whose branch has a MERGED PR fixture; `remote.<name>.url` is a list-accumulating git config variable — verified directly against git 2.55.0 — so a per-worktree override only actually takes effect when the common config defines no `origin` at all, which is why the main checkout's is removed rather than merely overridden).
- **Asserts:** `worktree list`'s row for the worktree carries PR column `merged`, verdict `remove`, and reason `-` (none) — reachable only by resolving PR state from the worktree's own remote, since the main checkout has no `origin` and resolving against its path (the pre-fix behaviour) can never derive a `--repo`, always failing closed to `keep`/`unresolvable` regardless of the worktree's actual PR.
- **Does not assert:** the `reclaim` (removal) path for this fixture, or JSON output — same gate, already covered elsewhere (`002`, `006`); the "unrelated repo's coincidental MERGED PR" framing from the fix's own doc comment, which this suite could not reproduce (see the test's doc comment and `set_worktree_origin`) because it requires the common config to ALSO carry a resolvable `origin`, which the list-accumulation behavior above rules out.
- **Platform coverage:** mac+linux.

##### worktree/reclaim/008 — A worktree created through the deck's own PRODUCTION creation path (`issue_dispatch_run::create_worktree_sync`) is `Verdict::Remove` and is removed by a BARE `reclaim` (no `--yes`), once that path writes the ownership marker (issue #144 finding 1, corrected).
- **Layer:** fast synthetic direct-call unit test, embedded in `src/worktree_reclaim.rs`'s own `#[cfg(test)] mod tests` — NOT `tests/worktree_reclaim.rs`. `create_worktree_sync` is `pub(crate)`, invisible to an external integration-test crate, so this is the only fast-tier seam that can call it directly rather than through the `mark_owned` test helper. Calls `issue_dispatch_run::create_worktree_sync` and `worktree_reclaim::run_reclaim` in-process against a real git repo and a stub `gh` reached via a `PATH` prepend on the current process, serialized by a test-only `GH_PATH_ENV_LOCK` mutex + RAII restore-on-drop guard (mirroring `config.rs`'s `STATE_DIR_ENV_LOCK`, the only other place in this crate mutates process env for a test). No `dot-agent-deck` binary subprocess, no PTY, no daemon.
- **Agent:** none.
- **Asserts:** `create_worktree_sync` reports `WorktreeCreation::Created` and the worktree directory exists; a subsequent bare `run_reclaim(repo, yes=false)` reports exactly one `removed` entry and the worktree directory is actually gone — reachable only if the production creation path itself wrote the `dot-agent-deck-owner` marker (there is no `mark_owned` call anywhere in this test), since `Verdict::Remove` requires `Ownership::Ours`.
- **Does not assert:** `gh` invocation-shape correctness (`--repo`/`--state` presence, unknown-flag rejection) — the stub here answers unconditionally; that is pinned at the CLI layer by `tests/worktree_reclaim.rs`'s `Fixture`/`GH_STUB_SCRIPT` (`001`–`007`, `009`, `010`). Supersedes this catalog id's prior scenario ("a hand-made worktree survives `--yes`"), which encoded a withdrawn design decision — see `worktree/reclaim/011`, which now pins the corrected, opposite contract for that same hand-made-worktree shape.
- **Platform coverage:** mac+linux (`#[cfg(unix)]`; the fixture shells to `git`/a stub `gh` and reads a Unix `PermissionsExt` mode bit, exactly as `worktree/reclaim/001`).

##### worktree/reclaim/009 — A local unmerged branch survives `worktree reclaim --yes` even when a DIFFERENT fork's already-merged PR shares its exact `headRefName` (issue #144 finding 2), and the reported reason names the real cause rather than falsely claiming no PR exists (issue #144 follow-up, reviewer finding NEW-2).
- **Layer:** fast synthetic real-binary-subprocess integration (as `worktree/reclaim/001`).
- **Agent:** none (a deck-owned, clean worktree on a fresh branch carrying its own unmerged commit; the canned `gh` reply reports a MERGED PR for the same `headRefName` but a `headRepositoryOwner` that does not match this fixture's own `origin` owner — the exact shape a triangular, push-to-fork workflow produces on EVERY branch).
- **Asserts:** `worktree reclaim --yes` exits successfully and the worktree directory still exists afterward — `resolve_pr_state` must not attribute a same-named PR from a different fork's `headRepositoryOwner` to this local branch. Additionally, the report must not contain the literal phrase "no pull request found for this branch" (a genuine `headRefName` match exists; reporting `NoPr` sends the user hunting a PR that is really there) and must name "owner" as the real cause.
- **Does not assert:** the exact resulting `PrState`/verdict label beyond the reason-text checks above; the ambiguity-guard path (`>1` match), which is a distinct, already-covered code path; the exact wording of the reason beyond containing "owner" and excluding the "no pull request" phrase.
- **Platform coverage:** mac+linux.

##### worktree/reclaim/010 — A local unmerged branch survives `worktree reclaim --yes` when the canned `gh` reply carries no `headRepositoryOwner` field at all (issue #144 finding 2, fail-closed case).
- **Layer:** fast synthetic real-binary-subprocess integration (as `worktree/reclaim/001`).
- **Agent:** none (same fixture shape as `009`, but the PR fixture omits `headRepositoryOwner` entirely — the shape real `gh` can return when the head repository is no longer resolvable, e.g. a fork deleted after its PR merged).
- **Asserts:** `worktree reclaim --yes` exits successfully and the worktree directory still exists afterward — an unverifiable head repository owner must fail closed to not-merged, never be treated as a match.
- **Does not assert:** the exact resulting `PrState`/verdict label (only the observable non-removal).
- **Platform coverage:** mac+linux.

##### worktree/reclaim/011 — A FOREIGN worktree (no ownership marker) that is merged and clean is named in the pending list by a bare `reclaim`, then IS removed once the user runs `reclaim --yes` (issue #144 follow-up: corrects a withdrawn design decision).
- **Layer:** fast synthetic real-binary-subprocess integration (as `worktree/reclaim/001`).
- **Agent:** none (a worktree deliberately left without an ownership marker, PR fixture MERGED; `reclaim` is run twice against the same fixture — once bare, once with `--yes`).
- **Asserts:** the bare run succeeds, names the worktree's exact path in its pending list, and leaves the worktree in place; the subsequent `--yes` run then removes it. Pins the corrected contract this suite's original `008` had backwards: `--yes` is the batch confirmation for an `Ask`-verdict (foreign) worktree whose path was already shown to the user, not something that must never touch a foreign worktree — withholding removal here would leave `run_reclaim`'s `"ask" if yes` branch unreachable dead code while `format_reclaim_human` kept telling users to run a flag that no longer did anything.
- **Does not assert:** that a `Remove`-verdict (deck-owned) worktree is also removed under the same conditions — `002` already covers that, unconditionally on the flag; the ask-surface reporting shape in isolation without `--yes` — already covered by `005`.
- **Platform coverage:** mac+linux.

##### worktree/reclaim/012 — Gitignored content demotes an otherwise-`Remove` worktree to `Ask`: a bare `reclaim` must not silently delete it (auditor finding F1, P1).
- **Layer:** fast synthetic real-binary-subprocess integration (as `worktree/reclaim/001`).
- **Agent:** none (a deck-owned, MERGED, `git status --porcelain`-clean worktree that also holds a directory matched by a committed `.gitignore` — the shape of a real, worked-in deck worktree's `target/`).
- **Asserts:** first, a **fixture precondition** that `git status --porcelain` reports nothing despite the ignored content, so the test provably exercises the gate gap rather than an ordinary dirty-tree keep; then that the worktree directory still exists after a bare `reclaim`, and that the output names its exact path (landing on `Ask`, not silently surviving some other way).
- **Does not assert:** the exact verdict label or reason wording; behaviour once `--yes` is passed (same batch-confirmation contract `011` already covers).
- **Platform coverage:** mac+linux.

##### worktree/reclaim/013 — The SAME deck-owned/merged/clean shape as `012`, but with NO ignored content, is still removed by a bare `reclaim`, unprompted — proves the demotion discriminates rather than swallowing every worktree. Expected GREEN from the start.
- **Layer:** fast synthetic real-binary-subprocess integration (as `worktree/reclaim/001`).
- **Agent:** none.
- **Asserts:** the worktree directory is gone after a bare `reclaim`. Pairs with `012`: without this test, a fix could satisfy `012` by demoting every worktree to `Ask` regardless of content, defeating the ownership gate `008`/`011` already establish.
- **Does not assert:** anything about the `012` fixture's ignored-content path itself.
- **Platform coverage:** mac+linux.

##### worktree/reclaim/014 — A forged `.git` file redirect inside a worktree's own directory, pointing at a copied admin dir carrying the ownership marker with `commondir` kept honest, must NOT resolve `Ownership::Ours` (auditor finding F2, P2 — the #152-lineage class of trusting *a* git-dir rather than *this worktree's own*).
- **Layer:** fast synthetic real-binary-subprocess integration (as `worktree/reclaim/001`).
- **Agent:** none (a merged, otherwise-unmarked worktree whose `.git` file — normally `gitdir: <repo>/.git/worktrees/<name>`, one regular file inside the worktree's own directory — is overwritten to redirect to a copied-and-forged admin dir elsewhere, carrying a forged `dot-agent-deck-owner` marker, a `commondir` pointing at the real repo's `.git`, and a `gitdir` back-pointer fixed to match).
- **Asserts:** first, a **fixture precondition** that `git status --porcelain` reports nothing despite the tamper (the cleanliness gate is blind to `.git` itself), so the test provably exercises the forgery; then that the worktree directory still exists after a bare `reclaim`, and that the output names its exact path (landing on `Ask`, not removed unprompted).
- **Does not assert:** the resolution mechanism the fix uses (only the observable verdict/survival) — the coder is free to implement the containment check any way that works; behaviour once `--yes` is passed.
- **Platform coverage:** mac+linux.

##### worktree/reclaim/015 — A legitimate `--separate-git-dir` main-repo layout (a git-native way to relocate metadata, not a forgery) still resolves correctly and is reclaimed — proves a containment fix for `014` discriminates rather than rejecting every worktree whose common dir lives outside `<repo>/.git`. Expected GREEN from the start.
- **Layer:** fast synthetic real-binary-subprocess integration (as `worktree/reclaim/001`, except the main repo is initialized with `git init --separate-git-dir=<store>` rather than a plain `git init`, so linked worktrees' admin dirs resolve under `<store>/worktrees/<name>`).
- **Agent:** none (a deck-owned, merged, clean worktree, legitimately marked via the normal `mark_owned` helper — no forgery).
- **Asserts:** the worktree directory is gone after a bare `reclaim`.
- **Does not assert:** anything about the `014` fixture's forged-redirect path itself.
- **Platform coverage:** mac+linux.

##### worktree/reclaim/016 — A `headRepositoryOwner.login` that differs from the local `origin` owner ONLY in case still resolves `Merged` and is reclaimed (auditor finding F3 / reviewer finding NEW-1, P2 — GitHub logins are ASCII and case-insensitive).
- **Layer:** fast synthetic real-binary-subprocess integration (as `worktree/reclaim/001`).
- **Agent:** none (a deck-owned, clean worktree; the canned `gh` reply's `headRepositoryOwner.login` is `"Test-Org"` against this fixture's own `origin` owner `"test-org"` — the shape GitHub itself returns for a remote a user typed with capitals, since GitHub resolves logins case-insensitively).
- **Asserts:** the worktree directory is gone after a bare `reclaim` — a byte-exact comparison would discard the only matching PR and leave the worktree kept forever with a "no pull request found" reason.
- **Does not assert:** the exact resulting `PrState`/verdict label (only the observable removal); non-ASCII/homoglyph login handling (GitHub logins are ASCII-only, so this is out of scope).
- **Platform coverage:** mac+linux.

##### worktree/reclaim/017 — A worktree marked owned by orchestration `orch-x` via `mark_worktree_owned` reports that exact name back via a new `owner_of` query, and `ownership_of`'s existing `Ours`/`Foreign` bit still agrees it is owned (fork #166 M2.0/M2.1).
- **Layer:** fast synthetic direct-call unit test, embedded in `src/worktree_reclaim.rs`'s own `#[cfg(test)] mod tests`, following `worktree/reclaim/008`'s precedent (a real git repo via `init_repo_with_origin`, a linked worktree via a real `git worktree add`).
- **Agent:** none.
- **Asserts:** `owner_of(repo, worktree)` returns `Some("orch-x")` after `mark_worktree_owned(worktree, "orch-x")`; `ownership_of(repo, worktree)` still returns `Ownership::Ours`.
- **Does not assert:** the marker's on-disk byte format (only that it round-trips through `mark_worktree_owned`/`owner_of`); `WorktreeReport`/JSON surfacing (covered by `021`).
- **Platform coverage:** mac+linux.

##### worktree/reclaim/018 — A pre-#166-legacy marker (the literal `"deck\n"` content `mark_worktree_owned` wrote before this PRD encoded a name) still resolves `Ownership::Ours`, but `owner_of` reports the owner as unknown (`None`) rather than guessing (fork #166 — protects every worktree created before this ships from silently becoming un-reclaimable).
- **Layer:** fast synthetic direct-call unit test, embedded in `src/worktree_reclaim.rs`'s own `#[cfg(test)] mod tests`, as `017`.
- **Agent:** none (a worktree with the marker file written directly as the literal legacy bytes `"deck\n"`, bypassing `mark_worktree_owned` so the fixture controls the exact on-disk content — the same fixture PR #173's own `bare_deck_marker_from_older_build_still_reads_as_ours` test uses).
- **Asserts:** `ownership_of` still returns `Ownership::Ours` (the presence-only check `reclaim` depends on is unchanged, already pinned by #173's own test — asserted again here only as the precondition for the next line) and `owner_of` returns `None`.
- **Does not assert:** the presence/`Ours` half in isolation — that is #173's `bare_deck_marker_from_older_build_still_reads_as_ours`, not duplicated here; any other unparseable-content shape (empty marker, etc.); `WorktreeReport`/JSON surfacing (covered by `021`).
- **Platform coverage:** mac+linux.

##### worktree/reclaim/019 — `main` (the enumerating repo's own checkout, not a linked worktree) is never owned, even when its own directory is named to match the `<name>-<change>` convention and even with a marker planted directly in its own git-dir. Expected GREEN from the start — fork #144's existing containment check already guarantees this; no new ownership-identity code is needed to satisfy it.
- **Layer:** fast synthetic direct-call unit test, embedded in `src/worktree_reclaim.rs`'s own `#[cfg(test)] mod tests`, as `017`.
- **Agent:** none (a repo directory literally named `myorch-feature`, with `dot-agent-deck-owner` written directly into its own resolved git-dir).
- **Asserts:** `ownership_of(repo, repo)` returns `Ownership::Foreign`.
- **Does not assert:** the containment mechanism itself (already pinned by `014`/`015`); anything about `owner_of` (this test exercises only the pre-existing `Ownership`/`ownership_of` surface, deliberately unchanged by fork #166).
- **Platform coverage:** mac+linux.

##### worktree/reclaim/020 — A worktree owned by orchestration `Y` reports `Y`, never a different name `X`; a directory carrying NO marker at all is never owned, whatever it is named (fork #166 — ownership is decided by the marker, never by a directory's name).
- **Layer:** fast synthetic direct-call unit test, embedded in `src/worktree_reclaim.rs`'s own `#[cfg(test)] mod tests`, as `017`.
- **Agent:** none (one worktree marked owned by `"Y"`; one unmarked worktree deliberately named `X-decoy` to look like it belongs to a different orchestration's naming convention).
- **Asserts:** `owner_of` on the first returns `Some("Y")` and is asserted `!= Some("X")`; on the second, `ownership_of` returns `Ownership::Foreign` and `owner_of` returns `None`, despite the `X`-matching name.
- **Does not assert:** cross-repo collisions; more than two orchestration names at once.
- **Platform coverage:** mac+linux.

##### worktree/reclaim/021 — `dot-agent-deck worktree list --json` carries the recorded owner name in each `WorktreeReport` entry (fork #166 M2.2).
- **Layer:** fast synthetic direct-call unit test, embedded in `src/worktree_reclaim.rs`'s own `#[cfg(test)] mod tests`, following `worktree/reclaim/008`'s precedent for a stubbed `gh` reached via a `PATH` prepend (here answering unconditionally with an empty PR list — the reclaim verdict itself is not this test's concern).
- **Agent:** none.
- **Asserts:** `examine_worktrees` returns a report whose `owner` field is `Some("orch-x")` for a worktree marked owned by that name; the report's serialized JSON (via `WorktreeListDocument`) contains `"owner":"orch-x"`.
- **Does not assert:** the human-table (`format_list_human`) rendering, which this fork does not require to surface the owner; the `schema_version` bump question (the field is additive).
- **Platform coverage:** mac+linux (`#[cfg(unix)]`, as `008`).

##### worktree/reclaim/022 — Two live orchestrations of the SAME config type (`review`) but with DISTINCT typed names each record a DISTINCT owner in their own worktree's marker, not the identical `orchestration:review` string (fork #166 — instance identity, not just provenance).
- **Layer:** fast synthetic real-dispatch integration, embedded in `src/ui.rs`'s own `#[cfg(test)] mod tests`, following `orchestration/worktree/004`'s precedent (a real git repo, the real `Action::SpawnPane` dispatch, a fresh `TabManager`/`AppState` per spawn) — placed in `ui.rs` rather than `src/worktree_reclaim.rs` because the property under test is produced by `ui.rs`'s own `SpawnPane` handler (the `format!("orchestration:{}", orch_config.name)` creator-identity line), which no helper outside that file's private test module (`CapturingPaneController`, `default_ui`) can drive.
- **Agent:** none.
- **Asserts:** `crate::worktree_reclaim::owner_of` on the two independently-spawned worktrees returns two different values, both spawned from `make_orchestration("review")` but given the distinct typed names `review-orchestrator-1` and `review-orchestrator-2` — the precondition M1.0 makes required (Name is required and unique), so an empty or shared name is not a reachable fixture state.
- **Does not assert:** the exact string either owner resolves to (the interactive path is expected to move from `orch_config.name` to the typed unique name, and this test must survive that spelling change); role-pane cwd threading (already covered by `orchestration/worktree/003`/`004`).
- **Platform coverage:** mac+linux.

##### worktree/reclaim/023 — A reclaimable worktree whose DIRECTORY NAME contains a non-UTF-8 byte is still removed by `reclaim --yes`.
- **Layer:** fast synthetic real-binary-subprocess integration (as `worktree/reclaim/001`).
- **Agent:** none (a worktree directory built from raw bytes via `OsStr::from_bytes`/`Command::arg`, never through a `&str`/`to_string_lossy` conversion that would corrupt the byte before git ever saw it).
- **Asserts:** first, a **fixture precondition** that the scratch dir genuinely contains an entry whose raw bytes exactly match the intended non-UTF-8 name — ruling out "the filesystem silently normalised or rejected it" as the reason later assertions pass; then, as in `003`/`004`/`005`, that the exit code/stderr rule out clap's own unrecognized-subcommand error; then that the human report actually carries a non-empty `Removed:` section (not `Removed: none`) — ruling out "the directory was simply never created" as the reason it's absent; and finally that the worktree directory is gone. Pins Greptile P1 (upstream PR #427, `src/worktree_reclaim.rs:482`): `examine_worktrees` lossy-converts the parsed `PathBuf` into a `String`, and `run_reclaim` feeds that mangled string to `git worktree remove`, so a worktree whose path contains non-UTF-8 bytes is never reclaimed even though it is otherwise fully eligible.
- **Does not assert:** behaviour on non-Linux filesystems (APFS/HFS+ reject non-UTF-8 filenames outright, so this scenario cannot exist there); which specific byte is preserved, only that the exact bytes round-trip.
- **Platform coverage:** linux.

##### worktree/reclaim/024 — Two markers written directly via `mark_worktree_owned` for two DIFFERENT worktrees of the SAME repo, using the exact owner strings the interactive path records for two live orchestrations of the SAME config type (`review`) in the SAME directory, report DISTINCT owners back via `owner_of` (fork#192 — the unit-level complement to `worktree/reclaim/022`'s real-dispatch pin of the same success criterion).
- **Layer:** fast synthetic direct-call unit test, embedded in `src/worktree_reclaim.rs`'s own `#[cfg(test)] mod tests`, following `worktree/reclaim/017`'s precedent (a real git repo via `init_repo_with_origin`, two linked worktrees via real `git worktree add`).
- **Agent:** none.
- **Asserts:** `mark_worktree_owned(wt_a, "orchestration:review-orchestrator-1")` and `mark_worktree_owned(wt_b, "orchestration:review-orchestrator-2")` on two worktrees of the same repo each round-trip through `owner_of` to their own exact string, and the two owners are `assert_ne!`.
- **Does not assert:** the interactive `SpawnPane` handler that derives these owner strings from the typed Name (covered end-to-end by `worktree/reclaim/022`, `src/ui.rs`); `mark_worktree_owned`/`owner_of` themselves, which are unchanged by fork#192 and already covered by `017`/`020`.
- **Platform coverage:** mac+linux.

##### worktree/reclaim/030 — `format_list_human` renders an OWNER column: the marker identity for an owned worktree, and the existing `DASH` placeholder for one whose marker carries no `created-by:` line (fork #166 M2.3, the human-table half).
- **Layer:** fast synthetic direct-call unit test, embedded in `src/worktree_reclaim.rs`'s own `#[cfg(test)] mod tests` — no fixture repo needed, two `WorktreeReport` literals constructed directly.
- **Agent:** none.
- **Asserts:** the header row of `format_list_human`'s output carries a field literally equal to `"OWNER"`; at that same column index, a report with `owner: Some("orchestration:owner-x")` renders that exact string, and a report with `owner: None` renders the existing `DASH` (`"-"`) placeholder. Both reports carry a non-`None` `reason`, so the only unexplained dash in either row is the owner column under test.
- **Does not assert:** the OWNER column's position relative to the other columns (only that one exists and both rows' entries can be read from it); `worktree list --json`'s owner field (already covered by `021`); the `--mine` filter (covered by `031`–`034`).
- **Platform coverage:** mac+linux.

##### worktree/reclaim/031 — With `DOT_AGENT_DECK_WORKTREE_OWNER` set, `worktree list --mine` keeps a worktree whose owner equals it and excludes a same-repo worktree owned by a different orchestration (fork #166 M3.0 — the happy path).
- **Layer:** fast synthetic real-binary-subprocess integration (as `worktree/reclaim/001`), with `DOT_AGENT_DECK_WORKTREE_OWNER` set explicitly on the spawned subprocess's environment rather than left to the ambient one.
- **Agent:** none (two worktrees of the same repo, each marked owned by a distinct `orchestration:<name>` creator via the full fork #166 marker format).
- **Asserts:** `--mine`, run with the env var set to one worktree's exact owner string, succeeds and names that worktree; it does not name the other, same-repo worktree owned by a different orchestration.
- **Does not assert:** behaviour when the env var is absent or set to the `orchestration:unknown` sentinel (covered by `033`/`034`); restart/process-independence (covered by `032`); the OWNER column's rendering (covered by `030`).
- **Platform coverage:** mac+linux.

##### worktree/reclaim/032 — A marker written directly to disk, standing in for a prior orchestration process, is matched by `worktree list --mine` run in a brand-new subprocess with no shared in-memory state — the milestone's actual "correct after a restart" claim (fork #166 M3.0).
- **Layer:** fast synthetic real-binary-subprocess integration (as `worktree/reclaim/001`), with the ownership marker written via a plain filesystem call rather than through any `dot-agent-deck` invocation, and `--mine` run via a fresh `Command::new` subprocess spawn.
- **Agent:** none.
- **Asserts:** `--mine`, run in a fresh subprocess against a marker written earlier and independently, with the env var set to the exact owner string in that marker, succeeds and names the worktree.
- **Does not assert:** the exclusion half (covered by `031`); any daemon or in-process cache (`--mine` has no daemon dependency at all, by design — see the PRD's M2.4 "why an env var" rationale).
- **Platform coverage:** mac+linux.

##### worktree/reclaim/033 — With `DOT_AGENT_DECK_WORKTREE_OWNER` entirely absent, `--mine` fails loudly: non-zero exit and a message naming what is missing — never falling back to "everything" or silently printing "nothing" (fork #166 M3.0).
- **Layer:** fast synthetic real-binary-subprocess integration (as `worktree/reclaim/001`), with the env var explicitly removed from the spawned subprocess's environment regardless of the ambient one.
- **Agent:** none (one owned worktree present, so a silent-empty-list failure mode and a silent-list-everything failure mode are both distinguishable from the correct refusal).
- **Asserts:** the process exits non-zero; the combined output names `DOT_AGENT_DECK_WORKTREE_OWNER`; the output does not name the worktree present in the fixture (rules out the "list everything" failure mode).
- **Does not assert:** the exact wording of the failure message beyond naming the missing variable; the `orchestration:unknown` sentinel case (covered by `034`, deliberately a separate spec since a wrong answer here hands one orchestration another's worktrees per fork #74).
- **Platform coverage:** mac+linux.

##### worktree/reclaim/034 — With `DOT_AGENT_DECK_WORKTREE_OWNER` set to the literal `orchestration:unknown` sentinel, `--mine` refuses exactly as it does when the variable is absent — two nameless orchestrations must never match each other's worktrees (fork #166 M2.4/M3.0).
- **Layer:** fast synthetic real-binary-subprocess integration (as `worktree/reclaim/001`), with the env var explicitly set to the sentinel string.
- **Agent:** none (the fixture worktree is marked owned by the SAME sentinel string, so a wrong sentinel-matches-sentinel implementation would wrongly hand it over).
- **Asserts:** the process exits non-zero, exactly as `033`'s absent-variable case; the output names the problem (the variable or the word "unknown"); the output does not name the sentinel-owned worktree present in the fixture.
- **Does not assert:** the absent-variable case itself (covered by `033`); any handling of a non-sentinel, genuinely-set owner (covered by `031`/`032`); an exported-but-empty value (covered by `035`).
- **Platform coverage:** mac+linux.

##### worktree/reclaim/035 — With `DOT_AGENT_DECK_WORKTREE_OWNER` exported but literal-empty (`Some("")`) or whitespace-only, `--mine` refuses exactly as it does when the variable is absent — an empty identity is exactly as meaningless as an absent one; conversely a legitimate identity carrying stray whitespace still matches once both sides are sanitized identically, and the sentinel is still refused with a trailing control character (fork #166 M3.0, PR #215 round-3 reviewer F4, round-4 R4-1).
- **Layer:** fast synthetic real-binary-subprocess integration (as `worktree/reclaim/001`), with the env var explicitly set to a literal-empty string, a whitespace-only string, a whitespace-padded legitimate identity, and the sentinel plus a trailing control character, in turn.
- **Agent:** none (one owned worktree present, so a silent "use the value as the filter" failure mode is distinguishable from the correct refusal, and a genuine match is distinguishable from a silent non-match).
- **Asserts:** for `""` and `"   "`, the process exits non-zero, the combined output names `DOT_AGENT_DECK_WORKTREE_OWNER`, and the output does not name the worktree present in the fixture (rules out the "list everything" failure mode); for `" orchestration:someone "` against a marker of `orchestration:someone`, the process exits zero and names the worktree (rules out the raw-vs-sanitized filter mismatch, round-4 R4-1); for the sentinel plus a trailing control character, the process exits non-zero and does not name the fixture's worktree (rules out `trim` alone being mistaken for full sanitization).
- **Does not assert:** the absent-variable case itself (covered by `033`); the plain sentinel case with no control character (covered by `034`).
- **Platform coverage:** mac+linux.

##### worktree/reclaim/036 — `owner_disagreements` finds a row whose marker names the owner but whose independent `owned` resolution disagrees, and the shared `is_mine` predicate `--mine`'s retain also calls still excludes that row afterward (fork issue #221 — a definitive empty `--mine` answer must not silently swallow evidence of a disagreement).
- **Layer:** fast synthetic direct-call unit test, embedded in `src/worktree_reclaim.rs`'s own `#[cfg(test)] mod tests`, following `worktree/reclaim/030`'s precedent — no fixture repo needed, `WorktreeReport` literals constructed directly.
- **Agent:** none.
- **Asserts:** given five `WorktreeReport`s — (a) `owner: Some("orch-x")` with `owned: false`, (b) a genuinely-owned row for the same owner, (c) a row owned by a different name, (d) `owned: false` under that different name, and (e) `owned: false` with `owner: None` — `owner_disagreements(&reports, "orch-x")` returns exactly (a)'s path, not (d) or (e), which discriminates against an owner-blind implementation that would otherwise still pass with only one `owned: false` row in the fixture. `is_mine` — the same predicate `run_worktree_list_cli`'s retain calls, in `src/worktree_reclaim.rs` — still excludes (a) when filtering for `"orch-x"`, pinning that surfacing the disagreement never relaxes the fail-closed filter against the actual production predicate rather than a hand-written copy of it. `format_disagreement_warning`'s output for (a) is also asserted verbatim, including the likely-cause/remedy clause.
- **Does not assert:** the CLI's stderr text as printed through `run_worktree_list_cli` itself (only the `format_disagreement_warning` function it calls). The divergent `owned=false` + `owner=Some(..)` state is produced by a race between two independent `owned_git_dir` resolutions (`ownership_of` and `owner_of` each spawning their own `git rev-parse`s), so it cannot be staged deterministically through the real-binary `Fixture` that `001`–`007`/`009`/`010` use — constructing `WorktreeReport` values directly is the only deterministic seam.
- **Platform coverage:** mac+linux.

##### worktree/reclaim/037 — A worktree marked owned via the fork #166 marker format, carrying `created-by: orchestration:foo`, resolves `owner_kind: "agent"` in the `worktree list --json` document, alongside the existing `owner` string (PRD fork#298 M1.0's `WorktreeOwner::Agent`).
- **Layer:** fast synthetic real-binary-subprocess integration (as `worktree/reclaim/001`), reading the JSON row back via `serde_json::Value` rather than a `WorktreeReport` struct field — `owner_kind` does not exist in the struct yet, so a missing key fails the assertion, not the build.
- **Agent:** none.
- **Asserts:** the examined worktree's JSON entry carries `owner_kind: "agent"` and `owner: "orchestration:foo"` (unchanged from today).
- **Does not assert:** the human or unknown kinds (covered by `038`/`041`); the human-table OWNER column (covered by `030`); removal authority (covered by `039`).
- **Platform coverage:** mac+linux.

##### worktree/reclaim/038 — An UNMARKED, hand-made worktree (CLAUDE.md rule 1's dominant real path) resolves `owner_kind: "human"` with a populated `owner` naming the resolved login, and `owned: false` (PRD fork#298 M1.0's `WorktreeOwner::Human`).
- **Layer:** fast synthetic real-binary-subprocess integration (as `worktree/reclaim/001`), with the stub `gh` extended to answer `gh api user --jq .login` from `$GHSTUB_DIR/login` via the new `Fixture::set_login` (mirroring `tests/issue_claim.rs`'s own stub byte-for-byte) — the seam a `WorktreeOwner::Human` resolution is expected to reuse from `issue_claim.rs`'s `resolve_gh_login`/`gh_current_login_argv`.
- **Agent:** none.
- **Asserts:** the examined worktree's JSON entry carries `owner_kind: "human"`; `owner` contains the stubbed login (`"alice"`); `owned` is `false` — the last is the safety-property half: reporting a human owner must never look like proof of deck-creation.
- **Does not assert:** removal authority under `reclaim` (covered by `039`, the safety pin); the agent or unknown kinds (covered by `037`/`041`).
- **Platform coverage:** mac+linux.

##### worktree/reclaim/039 — THE SAFETY PIN. A MERGED, clean, human-owned (unmarked) worktree still resolves `Verdict::Ask`, never `Verdict::Remove`, under a bare `reclaim` (no `--yes`) — reopening fork #144's P1 if removal authority is ever derived from the NEW `WorktreeOwner::Human` reporting instead of staying keyed strictly on the existing marker-presence `Ownership` bit, which fork #166 explicitly refused to let happen.
- **Layer:** fast synthetic real-binary-subprocess integration (as `worktree/reclaim/001`), combining a real `worktree reclaim` invocation (checking the worktree directory survives on disk) with a `worktree list --json` call on the same fixture (checking the JSON row).
- **Agent:** none.
- **Asserts:** after a bare `reclaim`, the worktree directory still exists; the SAME row in `worktree list --json` carries `verdict: "ask"`, `owner_kind: "human"` (a positive resolution, not merely an absent owner), and `owned: false` — all three together, in one row, so a future change that starts deriving removability from `WorktreeOwner` fails this test loudly.
- **Does not assert:** the `--yes` removal path itself (covered by `011`, a foreign worktree — the mechanism is identical since `owned` stays keyed on the marker); the human-owner reporting fields in isolation (covered by `038`).
- **Platform coverage:** mac+linux.

##### worktree/reclaim/040 — A single `worktree list --json` document examining two worktrees (one deck-created and marked, one hand-made and unmarked) carries `owner_kind` AND a populated `owner` for BOTH rows (PRD fork#298 M2.0) — `owner: Option<String>` has carried a per-row identity string since fork #166 and is only omitted from JSON via `skip_serializing_if = "Option::is_none"` when `None`, which is why the document previously read as carrying no owner string anywhere even though the field already existed.
- **Layer:** fast synthetic real-binary-subprocess integration (as `worktree/reclaim/001`), one fixture repo with two linked worktrees examined in a single `worktree list --json` call.
- **Agent:** none.
- **Asserts:** the agent row carries `owner_kind: "agent"` and `owner: "orchestration:doc-carrier"`; the human row carries `owner_kind: "human"` and an `owner` containing the stubbed login (`"dana"`) — in the same JSON document.
- **Does not assert:** the legacy/unknown kind (covered by `041`); the human-table OWNER column (covered by `030`).
- **Platform coverage:** mac+linux.

##### worktree/reclaim/041 — A pre-fork#166 LEGACY marker (the bare `"deck\n"` content `mark_worktree_owned` wrote before identity tracking existed, no `created-by:` line) resolves `owner_kind: "unknown"` — never `"agent"` (no identity to attribute it to) and never `"human"` (the marker DOES prove deck creation) — while `owned` stays `true` and `owner` stays absent, both unchanged (fork issue #231's still-silent mirror of #221's disagreement warning, now resolved to something stated rather than a blank).
- **Layer:** fast synthetic real-binary-subprocess integration (as `worktree/reclaim/001`), the worktree marked via the existing `Fixture::mark_owned` (bare `"deck\n"`, no creator).
- **Agent:** none.
- **Asserts:** `owned: true` and `owner` absent/`None`, both unchanged from today's behaviour; `owner_kind: "unknown"` in the same row.
- **Does not assert:** the agent or human kinds (covered by `037`/`038`); `--mine`'s handling of a legacy-marked worktree (unaffected — `is_mine` reads `owned`/`owner`, neither of which changes here).
- **Platform coverage:** mac+linux.

##### worktree/reclaim/042 — `remove_worktree` returns an outcome distinguishing WHY a tree was kept, instead of the `()` it used to return, which discarded the reason entirely (PRD 236 M1.1, GREEN).
- **Layer:** pure-data unit (`tokio::test`, `dot_agent_deck::issue_dispatch_run::remove_worktree` called directly against a real local git clone + worktree, no CLI subprocess, no `gh`).
- **Agent:** none.
- **Asserts:** a dirty tree under `RemovalPolicy::KeepIfDirty` survives the call; AND the call's own return value equals `RemoveOutcome::Kept(KeptReason::Dirty)` exactly (not merely "not `()`"), since the production function now returns a typed outcome and the daemon's close handler has something specific to hand back to the TUI.
- **Does not assert:** the removed-clean and probe-error branches (covered by `043`); the detached-spawn boundary (covered by `044`); the `RemoveFailed` branch (removal itself failing — not exercised by these local-fixture tests, since the fixtures always produce a git worktree `git worktree remove` can actually remove).
- **Platform coverage:** mac+linux.

##### worktree/reclaim/043 — `remove_worktree`'s outcome also distinguishes "removed" (clean tree) from "kept, unknown" (the `git status` probe itself failed) — not just "kept, dirty" (PRD 236 M1.1, GREEN).
- **Layer:** pure-data unit (as `042`), one clean worktree removed by the call and one non-git directory that makes the internal `git status --porcelain` probe fail outright.
- **Agent:** none.
- **Asserts:** a CLEAN tree under `KeepIfDirty` is actually removed from disk, and the call's return value equals `RemoveOutcome::Removed` exactly; a worktree path whose status probe errors is left in place — the fail-safe — and its return value equals `RemoveOutcome::Kept(KeptReason::ProbeError)` exactly.
- **Does not assert:** the kept-because-dirty branch (covered by `042`); the detached-spawn boundary (covered by `044`).
- **Platform coverage:** mac+linux.

##### worktree/reclaim/044 — The close path's detached `tokio::spawn` (mirroring `daemon_protocol.rs`'s shape) has a typed outcome to propagate: joining a task built the same way `remove_worktree(...).await` ends on now yields `RemoveOutcome::Kept(KeptReason::Dirty)`, not the `()` it used to (PRD 236 M1.1, GREEN).
- **Layer:** pure-data unit (as `042`), a `tokio::spawn` mirroring the close handler's bare `remove_worktree(...).await` shape, joined via its own `JoinHandle`.
- **Agent:** none.
- **Asserts:** the dirty tree survives the detached task; the joined result equals `RemoveOutcome::Kept(KeptReason::Dirty)` exactly.
- **Does not assert:** the daemon's real socket/attach-response wiring, or that `daemon_protocol.rs` itself actually broadcasts the outcome (out of reach for a pure-data unit test — this test only mirrors the spawn shape); the removed/kept-unknown branches (covered by `042`/`043`).
- **Platform coverage:** mac+linux.

##### worktree/reclaim/045 — The #120 issue-dispatch producer (`run_issue_dispatch`) records `RemovalPolicy::KeepIfDirty` for its per-issue worktree, like PRD #220's own dispatch does — the policies unified, no longer `RemovalPolicy::Force` — proven both as the recorded enum and as actual close-time survival (PRD 236 M2, GREEN).
- **Layer:** pure-data unit — the real `run_issue_dispatch` called in-process (no daemon, no attach socket) against a local git clone/origin and a minimal stub `gh` on `PATH`, with `cat` as the dispatched agent (`detach_delivery = true`, so this returns promptly rather than paying the ~30s readiness-wait cost `dispatch.rs`'s own e2e-gated spawn test documents).
- **Agent:** one real `cat` PTY as the dispatched single agent (alive on stdin, no LLM tokens) — required because `record_worktree`'s call site sits after a real spawn in `dispatch_one_issue`.
- **Asserts:** after a successful dispatch of issue #7, the `WorktreeRegistry` holds an entry for its worktree; dirtying that worktree and calling the real `remove_worktree` under the entry's OWN recorded policy leaves the directory in place (the previous, pre-unification `Force` policy would instead have destroyed it); the recorded policy equals `RemovalPolicy::KeepIfDirty`.
- **Does not assert:** the claim/label/comment writes `dispatch_one_issue` makes afterward (best-effort, irrelevant to the policy recorded); the daemon-hosted end-to-end flow (covered by the `scheduler/dispatch/*` e2e family).
- **Platform coverage:** mac+linux.

##### worktree/reclaim/046 — THE REGRESSION GUARD. A kept, dirty #120 worktree must still read as "already claimed" to `dispatch_decision` (the exact safety property the shipped code used to cite as its reason for forcing), AND the slot must not be a permanent dead end: the `worktree reclaim` path must see it, and once the operator resolves the dirtiness and its PR merges, must be able to remove it and free the slot (PRD 236, GREEN — M2 has unified the policy).
- **Layer:** fast synthetic real-binary-subprocess integration (as `worktree/reclaim/001`) for the reclaim-path half, plus a direct pure call into `dot_agent_deck::issue_dispatch::dispatch_decision` for the idempotency half — one fixture, two assertions that must BOTH hold.
- **Agent:** none.
- **Asserts:** `dispatch_decision(true, false, false)` is `Skip` (a present worktree is always already-claimed, regardless of which policy created/kept it); a dirty, deck-marked, #120-style worktree is visible to `worktree list --json` (`owned: true`); after the operator cleans the dirty content and the PR merges, a bare `worktree reclaim` genuinely removes it, freeing the slot for a future fire.
- **Does not assert:** that reclaim can remove a STILL-dirty tree (it never can, by `003`'s own regression guard — recovery requires resolving the dirtiness first, same as any other kept worktree).
- **Platform coverage:** mac+linux.

##### worktree/reclaim/047 — `BroadcastMsg::WorktreeKept` round-trips through the exact `KIND_EVENT` wire the daemon forwards, tagged `worktree_kept` so it's distinguishable from `event`/`orchestration_surface` (the reason `PROTOCOL_VERSION` bumped), and `WorktreeKeptNotice.error` (populated for `KeptReason::RemovalFailed`, the field the error text rides on since `KeptReason` itself must stay field-less for its `#[serde(other)]` catch-all to compile) survives the round trip too (PRD 236 review — the wire path had no test on this branch until now).
- **Layer:** pure-data unit (`src/event.rs`'s own `#[cfg(test)] mod tests`), serializing/deserializing `BroadcastMsg` directly via `serde_json` — no daemon, no socket, mirrors `orchestration_surface_broadcast_round_trips` immediately above it in the same file.
- **Agent:** none.
- **Asserts:** a `WorktreeKept(WorktreeKeptNotice { reason: KeptReason::Dirty, error: None })` serializes with `"kind":"worktree_kept"` and the expected `path`/`reason` fields, omits `error` from the wire entirely (`skip_serializing_if`) rather than sending `null`, and deserializes back to an equal value; a second message with `reason: KeptReason::RemovalFailed, error: Some(..)` also round-trips, including the error string.
- **Does not assert:** that the daemon's close handler (`daemon_protocol.rs`) actually sends this broadcast, or that `apply_broadcast`/`queue_kept_worktree` (`reconnect.rs`/`state.rs`) route it into `AppState` — those remain covered only by `dispatch/close/002`'s last-hop test and by reading the source, not by an automated test on this branch.
- **Platform coverage:** mac+linux.

#### issue/claim

Round 3 (PRD fork#235, re-scoped TWICE after review): identity is the caller's WORKTREE — its absolute path plus its git branch (CLAUDE.md rule 23) — never a `DOT_AGENT_DECK_PANE_ID` value (round 2, dropped: those ids recycle across a daemon restart, fork #160/#163/#166) and never the worktree ownership marker (round 1, dropped: the marker is almost never present under CLAUDE.md rule 1's mandated hand-made `git worktree add`). Both the path and the branch are derivable straight from `git`, so no marker is required at all — the marker, when present, supplies human-readable DECORATION only and is never part of the compared identity. A human claiming outside any worktree still resolves as `human:<login>@<host>` — that half is unchanged since round 1. `issue claim` is a real, already-wired subcommand (`src/issue_claim.rs`); what these tests pin is round 3's identity, which `src/issue_claim.rs`'s `resolve_caller_identity` does not yet implement (still pane-id-based), so a failure here is a genuine behavioral mismatch, not a missing-subcommand error.

##### issue/claim/001 — `dot-agent-deck issue claim` refuses when the issue is already held by a DIFFERENT agent-worktree identity, exits non-zero, writes nothing, and names the holder's worktree absolute path and branch (PRD fork#235 — the centrepiece lock).
- **Layer:** fast synthetic real-binary-subprocess integration (the REAL `dot-agent-deck issue claim` CLI as a subprocess against real git repos in a tempdir, with a synthetic STATEFUL `gh` on `PATH` — `issue comment`/`issue edit --add-label`/`--add-assignee`/`--remove-assignee` persist into per-issue files and `issue view --json ...` reads them back, so a sequential claim-then-claim exercises the same read-your-own-writes loop as real GitHub; no PTY, no daemon, no LLM, no `e2e` feature gate).
- **Agent:** none (two agent-shaped identities, each running from its own real linked worktree/branch pair carrying a `dot-agent-deck-owner` marker that is now fully inert).
- **Asserts:** pane A claims the issue; pane B's later claim on the same issue (from B's own, DIFFERENT worktree) exits non-zero; no `gh` call attributable to B's run adds a label, assignee, or comment; B's stderr names A's worktree absolute path and A's branch.
- **Does not assert:** the `--takeover` override path (`002`/`003`); the labelled-with-no-comment case (`004`); human claimants (`005`).
- **Platform coverage:** mac+linux.

##### issue/claim/002 — `--takeover` alone still refuses: nothing written, the message instructs `--confirm-stopped` (PRD fork#235 — the two-step override is deliberate friction).
- **Layer:** fast synthetic real-binary-subprocess integration (as `issue/claim/001`).
- **Agent:** none (as `001`; B's second claim adds `--takeover` with no `--confirm-stopped`).
- **Asserts:** B's `--takeover`-only claim still exits non-zero, writes nothing (no label/assignee/comment call), and its output instructs the caller to re-run with `--confirm-stopped` — an agent must not be able to satisfy the override in the same breath it discovers the conflict.
- **Does not assert:** the successful takeover path once `--confirm-stopped` is added (`003`).
- **Platform coverage:** mac+linux.

##### issue/claim/003 — `--takeover --confirm-stopped` succeeds: the comment log holds both claims in order, the newest still starts with `Claimed by ` and names who it took over from, and the assignee list ends up holding ONLY B's human — A's IS removed (PRD fork#235 FINAL round 5, reverted from round 4: the removal target is now `current GitHub assignees − {claimant}`, read from `gh issue view`'s own `assignees` field, never from any claim comment's content or authorship. `alice` genuinely IS a current assignee — A's own earlier claim added her — so B's takeover removes her and adds `bob`; replace-to-one is restored to "always exactly one". The round-4 author gate this test previously pinned is deleted: the round-5 audit found it did not narrow the removal, it DISABLED it — a deck-authored comment's `, for @X` clause always names the authenticated account, so `X == author` always, and the gate's `author == login_now` check made the removal drop on every legitimate run).
- **Layer:** fast synthetic real-binary-subprocess integration (as `issue/claim/001`).
- **Agent:** none (as `001`; B's second claim adds both `--takeover` and `--confirm-stopped`).
- **Asserts:** B's takeover claim succeeds; the recorded `gh` comment calls for the issue number at least two (A's original plus B's); the LATEST comment call still contains the literal `Claimed by ` prefix (`parse_claim_comment` finds claims via `.rfind` on it, so any other wording would be invisible and the system would still believe A holds the issue) and names A's worktree absolute path and branch in its tail; the final persisted assignee list is exactly `["bob"]` — B's human added, A's human removed.
- **Does not assert:** the exact identity string formatting beyond path+branch; the `--takeover`-alone refusal (`002`).
- **Platform coverage:** mac+linux.

##### issue/claim/004 — An issue labelled `in-progress` with NO discoverable claim comment (the hand-typed CLAUDE.md rule 14 claim) refuses — identity unknown (PRD fork#235).
- **Layer:** fast synthetic real-binary-subprocess integration (as `issue/claim/001`; the label is seeded directly into the stub's files, bypassing `gh` entirely, standing in for a label a human or external tool applied by hand).
- **Agent:** none.
- **Asserts:** the claim exits non-zero and no `gh` call adds a label, assignee, or comment.
- **Does not assert:** the exact refusal wording; the read-back mechanism (per-issue `gh issue view` vs. a list-embedded field) — either shape the coder chooses is served identically by the stub.
- **Platform coverage:** mac+linux.

##### issue/claim/005 — With no `DOT_AGENT_DECK_PANE_ID`, a claim resolves as `human:<login>@<host>`; a later agent claim on the same issue is refused, naming the human (PRD fork#235).
- **Layer:** fast synthetic real-binary-subprocess integration (as `issue/claim/001`; the first claim runs with `DOT_AGENT_DECK_PANE_ID` absent from the environment).
- **Agent:** none.
- **Asserts:** the human's claim is followed by an agent's claim on the same issue, which exits non-zero and whose message names the human's login.
- **Does not assert:** the exact `human:<login>@<host>` string formatting; the blank-pane-env case (`006`), which is the inverse direction (a pane whose id somehow resolved blank, which must NOT be read as human either).
- **Platform coverage:** mac+linux.

##### issue/claim/006 — `DOT_AGENT_DECK_PANE_ID` set but BLANK (empty, and separately whitespace-only) refuses, and specifically does NOT fall back to `human:<login>` (PRD fork#235 — round 2 dropped the marker requirement entirely and round 3 keeps that; see `009` for the marker-less-but-present-pane-id case, which succeeds).
- **Layer:** fast synthetic real-binary-subprocess integration (as `issue/claim/001`; run twice, once with the pane env set to `""` and once to `"   "`).
- **Agent:** none.
- **Asserts:** both blank-pane-env claims exit non-zero; neither output contains the `human:<login>` form for the login the stub would have resolved; no `gh` call adds a label, assignee, or comment across either run. Every agent on one deck whose pane id resolved blank would otherwise collapse to the SAME `human:<login>` identity, and the lock would read "held by me" and wave them all through while appearing to work.
- **Does not assert:** the exact refusal wording.
- **Platform coverage:** mac+linux.

##### issue/claim/007 — Two agents sharing the exact SAME decorative orchestration name (in their now-inert owner markers) but running from TWO DIFFERENT worktrees: the second is REFUSED, never treated as an idempotent self-refresh (PRD fork#235 — the regression guard against anyone later "simplifying" the comparison back onto the decorative name OR the pane id; fork #201 records name uniqueness as only advisory and states "this is the case #74 is actually about").
- **Layer:** fast synthetic real-binary-subprocess integration (as `issue/claim/001`; two DISTINCT linked worktrees, each marked owned by the identical decorative orchestration name, run under two DISTINCT pane ids).
- **Agent:** none.
- **Asserts:** the first same-named pane's claim is followed by the second's, from a different worktree under a different pane id; the second exits non-zero; exactly one comment call is ever recorded for the issue (the first's) — a self-refresh would post, or attempt, a second.
- **Does not assert:** fork #201 itself (name-collision UI advisory) — this test proves the LOCK stays correct despite it (now trivially, since round 3 doesn't compare on the name at all), not that #201 is fixed.
- **Platform coverage:** mac+linux.

##### issue/claim/009 — A deck-spawned pane in a worktree carrying NO owner marker at all claims SUCCESSFULLY (PRD fork#235 — reviewer F1: round 1 refused this unconditionally, but it is the orchestrator's own dominant real path under CLAUDE.md rule 1's mandated hand-made `git worktree add`, which writes no marker; round 3 makes this even more foundational, since the worktree's path/branch are derivable straight from `git` and no marker is EVER consulted).
- **Layer:** fast synthetic real-binary-subprocess integration (as `issue/claim/001`; the worktree is deliberately never marked owned).
- **Agent:** none.
- **Asserts:** the claim succeeds; at least one `gh` call writes the label/assignee/comment.
- **Does not assert:** the decorative rendering when no marker exists (whether the comment omits decoration entirely or falls back to some other label) — left to the coder.
- **Platform coverage:** mac+linux.

##### issue/claim/010 — The SAME pane id claiming from TWO DIFFERENT worktrees (its own, then a DIFFERENT orchestration's own worktree) is REFUSED, never treated as an idempotent self-refresh or an impersonation (PRD fork#235 round 3 — flipped from round 2's own regression: round 2 keyed identity on `DOT_AGENT_DECK_PANE_ID` alone, so the SAME pane id `cd`-ing into another orchestration's worktree was wrongly waved through as a self-refresh; round 3 makes the worktree the unit of identity, so entering someone else's worktree — the rule 1 violation itself — is now correctly caught).
- **Layer:** fast synthetic real-binary-subprocess integration (as `issue/claim/001`; two distinct linked worktrees, each marked owned by a DIFFERENT decorative name, both runs sharing one pane id).
- **Agent:** none.
- **Asserts:** the first claim (from the pane's own worktree) succeeds; the second claim (SAME pane id, a DIFFERENT worktree) exits non-zero, names the FIRST worktree's absolute path and branch as the holder, and writes nothing.
- **Does not assert:** N/A.
- **Platform coverage:** mac+linux.

##### issue/claim/011 — An idempotent refresh (the SAME worktree claiming twice) leaves the assignee INTACT rather than unassigning it (PRD fork#235 — reviewer F3: today's refresh path emits a self-cancelling `--add-assignee X --remove-assignee X`, which nets UNASSIGNED under real `gh`'s ordering; this file's stub is fixed to match that ordering so the defect is observable at all). **Passes for a different reason under round 5** (FINAL round 5, checked per that round's own instruction to verify `011`/`019` still exercise what they claim): the originally-pinned fix was an EXPLICIT same-login skip guard in the writer; round 5 deletes that guard along with the whole prior-login-from-a-comment mechanism it special-cased — the removal target is now `current assignees − {claimant}`, a set difference that STRUCTURALLY excludes the claimant from their own removal set, no special-casing required. The property (a same-identity refresh never unassigns) is unchanged; only the mechanism moved from an explicit guard to a structural set-difference.
- **Layer:** fast synthetic real-binary-subprocess integration (as `issue/claim/001`).
- **Agent:** none.
- **Asserts:** the first claim assigns the claiming human; a second claim by the SAME worktree succeeds; the assignee after the second claim is STILL exactly the same human — not unassigned.
- **Does not assert:** which of the two documented fixes (skip the redundant remove; skip the assignee write entirely on a same-identity refresh) the coder chooses; the ROUND-5 coder now instead computes removal from GitHub's own `assignees` field, which this test's stub already round-trips correctly.
- **Note:** this test's RED-ness (round 3/4) also depended on a companion fix IN THIS FILE — the synthetic `gh` stub's assignee-edit handling was changed from remove-then-add to add-then-remove ordering (matching real `gh`), since the prior ordering made the self-cancelling pair net ASSIGNED and hid the defect from CI entirely; this stub already reports `assignees` correctly in its `gh issue view` reply, so no round-5 fixture change was needed here (contrast `019`, whose minimal stub had to be updated).
- **Platform coverage:** mac+linux.

##### issue/claim/012 — A hostile claim comment, REWRITTEN in genuinely round-3-parseable form (the ORIGINAL version wrote round-2 shape text the round-3 parser rejects outright, so the comment was never actually parsed and every assertion held TRIVIALLY — "it passes with the parser deleted"): a well-formed first line, a raw embedded newline, then a forged second `Claimed by ` line carrying a `, for @victim` clause and cc mentions. Asserts directly on the PARSED fields that the recognised identity/timestamp/login come from the FIRST line only, never the forged second one reachable by scanning past the newline; then that no `gh` call the takeover makes carries the forged clause as an argv value, and the deck's own new comment never carries a LIVE (non-code-spanned) mention — including one embedded in the LEGITIMATE (not forged) branch text, confirming the wrapping code-span mechanism holds for recognised data too (PRD fork#235 — auditor F2/F3/F5, reviewer F8/F9).
- **Layer:** fast synthetic real-binary-subprocess integration (as `issue/claim/001`; the issue's sole claim comment is seeded DIRECTLY as adversarial JSON via `serde_json`, bypassing `gh` entirely — standing in for a comment `dot-agent-deck issue claim`'s own `gh issue view` will read back, since this PRD's Threat model places forgery of a comment's AUTHOR out of scope but the PARSER of its BODY must still not be corrupted).
- **Agent:** none.
- **Asserts:** `parse_claim_fields` returns `Some` for the hostile body (a hard guard so this test can never again silently defang itself the way the original did); the parsed `identity` equals exactly the first line's `worktree:<path>@<branch>`, never containing the forged second line's marker; the parsed `timestamp` is scoped to the first line (no embedded newline / forged-line content); the parsed `login` is never the forged `@forgedvictim`; a legitimate pane's `--takeover --confirm-stopped` against the hostile comment succeeds; no `gh` call (raw, unparsed log) contains the forged clause's distinguishing marker, and none contains a `--remove-assignee` value equal to the forged victim; the bytes the deck itself appends to `comments.jsonl` never carry a LIVE `@`-mention for any of the four forged/embedded handles fed in (three from the forged region, one embedded directly in the legitimate branch text).
- **Does not assert:** anything about the ORIGINAL hostile comment's own content (it is expected to and does contain the forged markers/mentions verbatim — that data was never authored by the deck); a full parser grammar for every possible malformed input; a raw backtick surviving into `held.identity` (structurally impossible under the current parser's first-backtick-terminates rule for both `path` and `branch` — covered as a regression guard by the legitimate-branch mention check instead).
- **Platform coverage:** mac+linux.

##### issue/claim/013 — `RefuseNoIdentity` (labelled, no discoverable claim comment) is ESCAPABLE via `--takeover --confirm-stopped` (PRD fork#235 — reviewer/auditor F4: today this state has no override path, and `do_claim`'s label-then-comment write ordering can CREATE the state itself when the comment write fails after the label write lands, permanently wedging the issue for both `issue claim` and `issue_dispatch`).
- **Layer:** fast synthetic real-binary-subprocess integration (as `issue/claim/001`; the label is seeded directly, as in `004`).
- **Agent:** none.
- **Asserts:** a bare claim against the labelled-with-no-comment issue still refuses (unchanged from `004`); `--takeover --confirm-stopped` against the SAME state succeeds and writes the label/assignee/comment.
- **Does not assert:** how the state was reached in production (a failed comment write vs. a hand-typed label) — the test seeds it directly, since both routes converge on the identical `RefuseNoIdentity` decision.
- **Platform coverage:** mac+linux.

##### issue/claim/014 — Identity SURVIVES a `DOT_AGENT_DECK_PANE_ID` change: the SAME worktree re-claiming under a DIFFERENT pane id (simulating a daemon restart that recycled the pane-id counter) is recognized as the SAME identity and succeeds as an idempotent refresh (PRD fork#235 — the round-2 regression guard: CLAUDE.md rule 23, verified 2026-08-10, records that pane ids are small daemon-scoped integers that recycle across a restart, the exact mechanism behind fork #160/#163/#166).
- **Layer:** fast synthetic real-binary-subprocess integration (as `issue/claim/001`; one worktree, claimed twice under two DIFFERENT pane ids).
- **Agent:** none.
- **Asserts:** the first claim succeeds; the second claim (same worktree, different pane id) also succeeds, and its output contains neither "held by" nor "refus" — i.e. is not rendered as a refusal.
- **Does not assert:** what (if anything) the refresh writes to `gh` — only that it is not treated as a conflict.
- **Platform coverage:** mac+linux.

##### issue/claim/015 — `--repo` omitted derives the repo from `origin`, and the DERIVED repo is named explicitly in BOTH a success and a refusal (PRD fork#235 — reviewer F11: this fork's `origin` is the fork itself while plenty of issues live upstream, so a silently-derived repo could target the wrong tracker).
- **Layer:** fast synthetic real-binary-subprocess integration (as `issue/claim/001`; the fixture repo's `origin` remote is set to a real GitHub-shaped URL, `git@github.com:acme/widgets.git`, so `derive_repo_slug` has something real to parse).
- **Agent:** none.
- **Asserts:** a `--repo`-omitted claim succeeds and its output names `acme/widgets` explicitly; a second, different identity's `--repo`-omitted claim on the SAME issue is refused, and its output ALSO names `acme/widgets`.
- **Does not assert:** non-GitHub or malformed `origin` URLs (covered, if at all, by `derive_repo_slug`'s own unit tests in `src/worktree_reclaim.rs`); the explicit-`--repo` path (covered by every other test in this family).
- **Platform coverage:** mac+linux.

##### issue/claim/016 — The host is NOT part of the identity: a claim comment is seeded naming the EXACT worktree path and branch a legitimate pane will itself resolve, standing in for a second deck on a DIFFERENT physical host whose worktree happens to share this path (ordinary under Codespaces/devcontainers, not exotic). Asserts the claim is REFUSED (PRD fork#235 — auditor A1, a blocker: `worktree:{path}@{branch}` carries no host component, so two decks on two machines with identical worktree paths compare EQUAL and both take the idempotent-refresh row — #74 verbatim).
- **Layer:** fast synthetic real-binary-subprocess integration (as `issue/claim/001`; the held claim is seeded directly, as in `012`, since no real second host is available to the test process).
- **Agent:** none.
- **Asserts:** a claim from the pane's own worktree, against an issue already held by a comment naming that SAME path+branch (standing in for a different host), is refused.
- **Does not assert:** how the coder's fix distinguishes hosts (a new comment field, a different identity shape, etc.) — only that SOME distinction exists once fixed; this test's own seeded comment intentionally carries no host field, since the round-3 format has none to seed.
- **Platform coverage:** mac+linux.

##### issue/claim/017 — A subdirectory does not split one actor: the same worktree is claimed from its ROOT, then again from a SUBDIRECTORY of it. Asserts the second claim is recognized as the SAME identity (idempotent refresh), never refused (PRD fork#235 — reviewer R1 / auditor A5: identity anchors on `cwd` verbatim rather than the worktree root, contradicting the PRD's own "every pane in one worktree shares that worktree's identity"; the case most likely to be hit in practice).
- **Layer:** fast synthetic real-binary-subprocess integration (as `issue/claim/001`; one worktree, claimed once from its root and once from a freshly-created `src` subdirectory inside it).
- **Agent:** none.
- **Asserts:** the root claim succeeds; the subdirectory claim also succeeds, and its output contains neither "held by" nor "refus".
- **Does not assert:** how deep a subdirectory can go, or symlinked subdirectories (covered separately by `018`'s whole-worktree symlink case).
- **Platform coverage:** mac+linux.

##### issue/claim/018 — One normalisation on both sides: a claim comment is seeded from an `Identity` built with the worktree's SYMLINKED (lexical, unresolved) path — the shape the dispatch path's `derive_issue_paths` produces from configured workspace text with no canonicalization. A legitimate CLI claim is then run with its OWN `cwd` set through that SAME symlink. Asserts the CLI claim is recognized as an idempotent refresh, never refused (PRD fork#235 — reviewer R2 / auditor A6: the CLI resolves a physical `getcwd` via `std::env::current_dir()` while the dispatch path builds a lexical path from configured text, so a symlinked or `/tmp` workspace can never match its own dispatch's claim).
- **Layer:** fast synthetic real-binary-subprocess integration (as `issue/claim/001`; a real symlink is constructed with `std::os::unix::fs::symlink`, portable across CI runners rather than relying on a platform's own `/tmp` symlink quirk).
- **Agent:** none.
- **Asserts:** a claim seeded via the SYMLINKED path's identity, then claimed from the CLI with `cwd` set through that SAME symlink (which resolves to the physical path inside the process), succeeds and is not reported as a refusal.
- **Does not assert:** Windows junction/symlink behavior (test is `#[cfg(unix)]`); the exact mechanism the coder's fix uses to normalize (canonicalizing one side, both sides, or comparing inode identity).
- **Platform coverage:** mac+linux.

##### issue/claim/019 — The dispatch path gets the SAME assignee fix as the CLI: `claim_issue` (the unattended `issue_dispatch` claim path) is called directly with a login matching a claim comment already on record, that comment's `author` field ALSO set to the same login, AND that login ALSO reported as a REAL current GitHub assignee in `gh issue view`'s `assignees` field — a same-identity refresh. Asserts the assignee ends up STILL SET afterward. **Fixture updated for round 5** (PRD fork#235 FINAL round 5, checked per that round's own instruction to verify `011`/`019` still exercise what they claim): originally (rounds 3/4) the stub's `gh issue view` reply carried no `assignees` field at all and the comment's matching `author` was what let the then-existing author gate resolve `prior_login`. Round 5 deletes that gate and the whole comment-parsing-for-writes mechanism — the removal target is now `current assignees − {claimant}`, read from the `assignees` field — so the stub was updated to report the claimant as a real current assignee too, specifically so this test still exercises a genuine same-identity refresh under round 5 rather than passing vacuously because there was nothing to remove regardless of any refresh logic (the fate this same task instructed checking `011`/`019` against; this file's `CLAIM_019_GH_STUB` constant records the full history).
- **Layer:** async unit test, in-process — a direct call to the private `claim_issue` function (same crate, `#[cfg(test)] mod tests` in `src/issue_dispatch_run.rs`) with a minimal synthetic `gh` stub on `PATH`; no CLI subprocess, no full `issue_dispatch` scheduler orchestration, no repo clone.
- **Agent:** none.
- **Asserts:** after `claim_issue` runs with `login` equal to the login already named by the (stubbed) prior claim comment, that comment's `author`, AND a real current assignee in the stub's `assignees` field, the stub's `assignees.txt` still contains that login.
- **Does not assert:** the label or comment writes `claim_issue` also makes (unexercised by this stub beyond no-op success); the full `run_issue_dispatch` orchestration around it.
- **Platform coverage:** mac+linux (`#[cfg(unix)]` — the stub `gh` is a shell script needing `chmod +x`).

##### issue/claim/020 — An adversarial task name cannot inject: a scheduled issue-dispatch task NAME contains a raw backtick immediately followed by an `@mention`. `sanitize_clone_segment` (deriving the task's worktree PATH component) strips only `/ \ \0 ..`, so both survive intact into the real worktree directory name that becomes `Identity::Worktree.path`. Asserts the FIRST claim comment the deck itself posts never lets that mention go LIVE — no forged/hostile comment is involved at all, the task name alone is the attack surface. Also covers the branch component (PRD fork#235 — auditor A3).
- **Layer:** L1 (pure — direct calls to `derive_issue_paths`/`Identity::issue_dispatch`/`Identity::worktree`/`claim_comment_body`; no subprocess, no `gh`, no git).
- **Agent:** none.
- **Asserts:** a sanity check that `sanitize_clone_segment` really did leave the backtick/mention intact in the derived path (so the test is exercising something real); the deck's own rendered claim-comment body never carries the mention LIVE via the path component; a second, independent construction proves the same holds for the branch component.
- **Does not assert:** the coder's chosen fix (sanitizing `path`/`branch` before interpolation, escaping backticks, or rejecting task names carrying them at config-load time).
- **Platform coverage:** mac+linux+windows.

##### issue/claim/021 — No raw control characters reach the terminal: a claim comment is seeded whose timestamp field carries raw ESC and CR control characters (reachable because `extract_timestamp` bounds its capture only at the next comma, with no character restriction). A different identity's bare claim against it is refused. Asserts the refusal's combined output carries no raw ESC/CR (PRD fork#235 — auditor A4: `RefuseHeldByOther` sanitizes `holder` but interpolates `held.timestamp` RAW into the refusal message printed to the operator's terminal — the earlier sanitizer fix cleaned only its sibling field).
- **Layer:** fast synthetic real-binary-subprocess integration (as `issue/claim/001`; the held claim is seeded directly, as in `012`/`016`, with a control-character-laden timestamp).
- **Agent:** none.
- **Asserts:** the refused claim's combined stdout+stderr contains neither a raw ESC (`\u{1b}`) nor a raw CR (`\r`) byte.
- **Does not assert:** every conceivable control character (covers ESC and CR specifically, the auditor's named example); sanitizing `held.timestamp` at the parse layer vs. at the display layer (either satisfies this test).
- **Platform coverage:** mac+linux.

##### issue/claim/022 — A stranger's well-formed claim comment cannot drive a removal: an UNLABELLED issue already carries a `Claimed by … , for @victim.` comment authored by `eve`, unrelated to the claiming agent. A bare `issue claim` (which always succeeds on an unlabelled issue) is asserted to succeed with NO `--remove-assignee victim` ever reaching `gh`. **Re-pointed for round 5** (PRD fork#235 FINAL round 5 — the removal target comes from GitHub, not comment text): this test originally pinned the round-4 author gate (`held.author == login_now`), which round 5 deletes along with the whole comment-parsing-for-writes mechanism it guarded — `do_claim` no longer parses ANY login out of a comment for a write at all, so "the author gate blocks a stranger's comment" is no longer a mechanism that exists to pin. The property that still matters, and that this test still genuinely exercises: `victim` was never added as a REAL GitHub assignee (only named in a comment), so the round-5 removal target — `current assignees − {claimant}`, read from `gh issue view`'s own `assignees` field — never contains `victim` regardless of who authored the comment naming him or whether any author-gate exists. A future reader must not read this as still guarding a deleted gate.
- **Layer:** fast synthetic real-binary-subprocess integration (as `issue/claim/001`; the comment is seeded directly with an explicit `author` field via a new `Fixture::seed_claim_comment_unlabelled_by`, leaving the issue UNLABELLED — unlike `Fixture::seed_claim_comment`, which always labels it).
- **Agent:** none.
- **Asserts:** the claim exits zero; no `gh` call in the log contains both `--remove-assignee` and `victim`.
- **Does not assert:** the coder's chosen implementation shape for computing `current assignees − {claimant}`; `023`/`027`/`028` cover the complementary cases (a REAL non-claimant assignee IS removed; self-authorship alone still doesn't drive a removal of a non-assignee).
- **Platform coverage:** mac+linux.

##### issue/claim/023 — The SAME shape as `022`, but the seeded comment's author IS the currently-authenticated account (`legit`), naming `priorholder` in its `, for @priorholder.` clause, AND `priorholder` is ALSO seeded directly as a REAL current GitHub assignee (standing in for a human who assigned the issue by hand before any deck ever claimed it). Asserts the removal still happens. **Repurposed for round 5** (PRD fork#235 FINAL round 5): originally (round 4) this test pinned the author gate opening for a self-authored comment — self-authorship was WHY the removal was allowed. Round 5 deletes the author gate entirely, so authorship no longer has any bearing on a removal; `priorholder`'s self-authored `, for @priorholder.` clause is now pure decoration. Repurposed to pin the property that DOES still drive this removal: replace-to-one applies uniformly, even to an issue's FIRST claim, against whatever is ALREADY in GitHub's own `assignees` field — including an assignee a human set by hand, never through this deck at all. This is the round-5 PRD's own stated accepted cost ("the deck can overwrite an assignee a human set by hand, because GitHub's assignee list does not record who set it") made concrete. Still a regression guard, not RED-first: the removal already happens under round-4 code too (the self-authored gate was open), so it stays GREEN before AND after the coder's round-5 fix — only the REASON it passes changes.
- **Layer:** fast synthetic real-binary-subprocess integration (as `issue/claim/022`, plus a direct `Fixture::seed_assignee` seeding `priorholder` as a real current assignee).
- **Agent:** none.
- **Asserts:** the claim exits zero; some `gh` call in the log contains both `--remove-assignee` and `priorholder`.
- **Does not assert:** that this test is RED before the fix — it is a guard, expected to already pass, for a different (now correct) reason after the fix.
- **Platform coverage:** mac+linux.

##### issue/claim/024 — A scheduled issue-dispatch task named `nightly, for @torvalds,` — plain hand-edited config, no forged/hostile comment involved. `sanitize_clone_segment` leaves the substring intact in the derived worktree path (and the task-name-decorated label), so it lands in the deck's OWN claim-comment body when rendered. `claim_issue` reading that SAME comment back does NOT mis-parse it: `parse_worktree_claim`'s `, for @` search is bounded to start AFTER the timestamp clause (fix 3), so `parse_claim_fields` correctly returns `login: None` for this body — pinned as one of two independent sanity preconditions. Asserts no `gh` call ever carries `--remove-assignee torvalds`. **Re-pointed for round 5** (PRD fork#235 FINAL round 5): originally (round 4) this test's refusal rested on two independent grounds — the parse-boundary fix, and the author gate (the fixture's `gh issue view` JSON carries no `author` field). Round 5 deletes the author gate and stops parsing ANY login out of a comment for a write at all, collapsing the two-cause defence into a single STRUCTURAL one: `torvalds` was never added as a real GitHub assignee (this fixture's `gh issue view` reports no `assignees` field at all), so the round-5 removal target — `current assignees − {claimant}` — never contains it regardless of what any comment says or who wrote it. The two sanity preconditions remain worth keeping (they still pin `parse_claim_fields`'s own correctness, a pure function unrelated to whether its output is used for a write), but the final assertion no longer guards the deleted author gate.
- **Layer:** async unit test, in-process — a direct call to the private `claim_issue` function (same crate, `#[cfg(test)] mod tests` in `src/issue_dispatch_run.rs`, as `issue/claim/019`) with a minimal synthetic `gh` stub on `PATH` that replies to `gh issue view` with a REAL `claim_comment_body` rendering built from the malicious task name; no CLI subprocess, no full `issue_dispatch` scheduler orchestration, no repo clone.
- **Agent:** none.
- **Asserts:** two independent sanity preconditions — (1) `sanitize_clone_segment` really did leave the substring intact in the derived path (so the test cannot pass vacuously the way the ORIGINAL `issue/claim/012` once did), and (2) `parse_claim_fields` correctly returns `login: None` for the rendered body (fix-3 behaviour, not the pre-fix-3 mis-parse); after `claim_issue` runs, no `gh` call in the log contains `--remove-assignee torvalds` — now because comment content is never consulted for a write, and `torvalds` was never a real assignee either.
- **Does not assert:** the coder's chosen fix shape (bounding the `, for @` search to start after the timestamp clause, vs. some other parser fix); that this test is still about the (deleted) author gate.
- **Platform coverage:** mac+linux (`#[cfg(unix)]` — the stub `gh` is a shell script needing `chmod +x`).

##### issue/claim/025 — An UNLABELLED issue carries a well-formed, SELF-authored claim comment (author `legit`, matching the claiming agent's own login) whose `, for @<login>` clause names a MALFORMED login, `-baduser025` (leading `-`, failing `validate_gh_login`'s `^[A-Za-z0-9][A-Za-z0-9-]*$` shape). Asserts the claim succeeds but `-baduser025` never reaches a `gh` argv at all. **Re-pointed for round 5** (PRD fork#235 FINAL round 5): this test originally pinned `parse_worktree_claim`'s parser-boundary `validate_gh_login` check — cause 1 of the round-4 PRD's two independent causes, independent of the (then still-open) author gate. Round 5 removes that mechanism's relevance entirely: `do_claim` no longer parses ANY login out of a comment for a write, valid or not, so there is no longer a "validated, parsed value" step to slip past. The property that still matters: `-baduser025` was never added as a REAL GitHub assignee (only named, malformed, in a comment), so the round-5 removal target — `current assignees − {claimant}` — never contains it regardless; a malformed string like this could never BE a real GitHub login to begin with either, making the round-5 guarantee more robust than the round-4 parser check it replaces (it never reads comment content for this purpose at all, rather than merely rejecting bad shapes).
- **Layer:** fast synthetic real-binary-subprocess integration (as `issue/claim/022`).
- **Agent:** none.
- **Asserts:** the claim exits zero; the raw (unsplit) gh-calls log never contains the literal substring `-baduser025`.
- **Does not assert:** every conceivable malformed shape (covers a leading `-`, the PRD's own named example; a space or an empty string share the same root cause but are harder to pin through this log's space-joined format); that this test is still about `validate_gh_login`'s parser-boundary call sites (deleted along with the author gate).
- **Platform coverage:** mac+linux.

##### issue/claim/026 — The mirror image of `024`: the SAME task-name-derived body carries an EARLIER, coincidental `, for @` clause (inside the decorative label/path, BEFORE the timestamp) AND a GENUINE trailing `, for @genuineuser.` clause AFTER it — unlike `024`, whose body carries no genuine clause at all (`login: None`). The comment is authored as the deck's own currently-authenticated account — the BEST-CASE authorship for a comment-driven removal — and the login clause parses out precisely (`genuineuser`, proven by a sanity precondition, not the earlier fake match). Asserts the genuine login's removal is NEVER attempted regardless. **Assertion flipped for round 5** (PRD fork#235 FINAL round 5): under round 4 this test proved the OPPOSITE — that a well-formed, self-authored, precisely-parsed trailing login clause DID drive a removal, showing fix 3's timestamp-bound search was precise rather than merely suppressive (the round-4 author gate did not itself mask the result, since authorship matched). Round 5 deletes the whole mechanism that made that removal happen at all: `claim_issue` no longer parses ANY login out of a comment for a write, so even this best-favourable case for a comment-driven removal must now do NOTHING. Deliberately the STRONGEST form of `022`/`024`/`025`'s "comment content never reaches a removal argv" property: those each have an independent reason the OLD mechanism would already have refused (stranger authorship, invalid shape, an unparseable clause) that could mask a still-existing removal mechanism; this one removes every such excuse, so it alone proves the removal-from-a-comment mechanism is gone, not merely blocked on this particular input. Function renamed from `issue_claim_026_genuine_trailing_login_wins_over_earlier_fake_match` to `issue_claim_026_genuine_trailing_login_still_never_drives_a_removal` to match.
- **Layer:** async unit test, in-process — a direct call to the private `claim_issue` function (same crate, `#[cfg(test)] mod tests` in `src/issue_dispatch_run.rs`, as `issue/claim/019`/`024`), reusing `024`'s minimal synthetic `gh` stub.
- **Agent:** none.
- **Asserts:** a sanity precondition that `parse_claim_fields` resolves `login: Some("genuineuser")` (not `torvalds`) from the rendered body; after `claim_issue` runs, NO `gh` call in the log contains `--remove-assignee genuineuser`.
- **Does not assert:** the coder's chosen implementation shape (as `024`); that this test is still about the (deleted) author gate — it never was primarily about the gate, and is even less so now.
- **Platform coverage:** mac+linux (`#[cfg(unix)]` — the stub `gh` is a shell script needing `chmod +x`).

##### issue/claim/027 — The removal target comes from GitHub's own `assignees` field, not comment text: an issue already holds a REAL current assignee, `priorholder` (seeded directly into the stub's assignee list, standing in for an earlier claim). Its held claim comment — required for the takeover's lock decision — carries a `, for @wronguser.` clause that DISAGREES with the real assignees list (`wronguser` is not, and never was, an actual assignee; the comment's author is left unset so a round-4-shaped gate could never treat it as self-authored either). A new agent pane takes over (`--takeover --confirm-stopped`, required because the issue is held by a different identity). Asserts the takeover succeeds, `priorholder` — the REAL prior assignee — is removed and the claimant added, and no `gh` call ever carries `--remove-assignee wronguser` (PRD fork#235 FINAL round 5 — new test for the round-5 design: `remove = current GitHub assignees − {the claimant}`, read from `gh issue view`'s own `assignees` field).
- **Layer:** fast synthetic real-binary-subprocess integration (as `issue/claim/001`; the held claim comment is seeded via `Fixture::seed_claim_comment`, and `priorholder` is seeded as a real assignee via a new `Fixture::seed_assignee`, independently of anything the comment says).
- **Agent:** none.
- **Asserts:** the takeover claim exits zero; the final persisted assignee list is exactly `["claimant027"]` (`priorholder` removed, the claimant added); no `gh` call (raw, unparsed log) contains `--remove-assignee wronguser`; some `gh` call contains both `--remove-assignee` and `priorholder`.
- **Does not assert:** the coder's chosen implementation shape for computing `current assignees − {claimant}` (a single combined `--add-assignee`/`--remove-assignee` call, vs. separate calls); RED-first status of the OLD author-gate mechanism (already deleted by round 5 — this test is RED against round-4 code because that code never reads the real `assignees` field at all, not because of anything to do with authorship).
- **Platform coverage:** mac+linux.

##### issue/claim/028 — A comment naming a non-assignee removes nobody, even when self-authored: an UNLABELLED issue carries a well-formed claim comment, self-authored by `claimant028` — the SAME account about to run the claim, so a round-4-shaped author gate would have OPENED for it — naming `victim` in its `, for @victim.` clause. `victim` is never seeded as a real GitHub assignee. A bare `issue claim` (which always succeeds on an unlabelled issue) is asserted to succeed with NO `--remove-assignee victim` ever reaching `gh` (PRD fork#235 FINAL round 5 — new test, B1's original exploit re-pointed at the round-5 design: "a stranger posts a well-formed single-line claim comment ending `, for @maintainer.`; the next claim removes `maintainer`"). Deliberately the ONE quadrant `022`/`023`/`025` don't already cover: self-authored (so the now-deleted round-4 gate would have let it through) AND naming a login that is not a real assignee (so round 5's own removal target excludes it regardless).
- **Layer:** fast synthetic real-binary-subprocess integration (as `issue/claim/022`; the comment is self-authored via `Fixture::seed_claim_comment_unlabelled_by`, and — unlike `023`/`027` — deliberately NO `Fixture::seed_assignee` call is made).
- **Agent:** none.
- **Asserts:** the claim exits zero; no `gh` call (raw, unparsed log) contains `--remove-assignee victim`.
- **Does not assert:** the coder's chosen implementation shape (as `022`/`027`). RED-first: under round-4 code this test genuinely FAILS — the self-authored gate opens and nothing yet checks whether `victim` is a real assignee, so `--remove-assignee victim` IS issued — making this test RED before the coder's round-5 fix and GREEN after, unlike `022`/`023`/`025`, which already pass under round-4 code for the (soon-superseded) author-gate reason.
- **Platform coverage:** mac+linux.

#### daemon/protocol

##### daemon/protocol/001 — A `SubscribeEvents` receiver that falls behind the broadcast capacity is torn down with `KIND_STREAM_END` carrying exactly the documented `"lagged"` reason (`handle_subscribe_events`'s doc comment, src/daemon_protocol.rs).
- **Layer:** L1 client/wire integration (real `serve_attach_with_counter`/`handle_subscribe_events` over a real Unix socket; a raw client reads frames directly rather than through `DaemonClient`, since `EventSubscription::next_event` collapses every `KIND_STREAM_END` reason to `Ok(None)`).
- **Agent:** none (a flood of inert filler `AgentEvent`s; no PTY, no LLM).
- **Asserts:** a client that subscribes and never drains, once the daemon-wide broadcast is flooded well past its capacity in a tight loop with no `.await` between sends (deterministic by construction — the connection task cannot poll `rx.recv()` even once until every send has landed, so its first poll is guaranteed to observe `RecvError::Lagged`, not a race against the flood), receives `KIND_STREAM_END` with payload exactly `b"lagged"` — not a timeout reason, not more `KIND_EVENT` frames. Added because no test in the suite exercised this path before: part of why issues #49/#28 could drift the TUI's reconnect behavior away from what this handler already documents.
- **Does not assert:** anything about the TUI's reconnect/re-hydration response to this tear-down (`session/live/013`, which pins that separately and does not depend on this daemon-side reason string being exactly right); the `timeout`/`Closed` tear-down reasons on the same handler; wire-format serde round-trips (`protocol/live-target`, `protocol/send-result`).
- **Platform coverage:** mac+linux.

### Prompts

#### prompt/permission

##### prompt/permission/001 — `y` approves the pending permission request and clears the WaitingForInput status.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** badge transitions away from WaitingForInput; the daemon receives the approval over its protocol channel.
- **Does not assert:** how the daemon routes the approval to the agent process (out-of-scope at the TUI layer).
- **Platform coverage:** mac+linux.

##### prompt/permission/002 — `n` denies the pending permission request.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** badge transitions away from WaitingForInput; daemon receives a denial.
- **Does not assert:** retry behavior.
- **Platform coverage:** mac+linux.

##### prompt/permission/003 — `y`/`n` are no-ops when no session is waiting for input.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** keystroke produces no protocol traffic and leaves card status unchanged.
- **Does not assert:** any beep or visual ack.
- **Platform coverage:** mac+linux.

#### prompt/close-confirm

##### prompt/close-confirm/001 — Command-mode Ctrl+W opens a Cancel-default close confirmation.
- **Layer:** L1 (in-process key mapper + close-confirm state + ratatui `TestBackend`).
- **Agent:** none.
- **Asserts:** Ctrl+W resolves `CloseSelected`, an available target opens the confirmation, both pane- and tab-scoped states render their exact blast-radius sentence/description without copy leakage, and `Cancel` remains selected by default.
- **Does not assert:** daemon teardown after confirmation (covered by `lifecycle/stop/*` and `dashboard/pane/002`).
- **Platform coverage:** mac+linux+windows.

##### prompt/close-confirm/002 — Cancel preserves the target while explicit confirmation authorizes one close.
- **Layer:** L2 (real-binary PTY plus real daemon registry).
- **Agent:** none (continued `cat` pane).
- **Asserts:** production Ctrl+W on a plain dashboard pane opens the pane-scoped `Close selected pane?` Cancel-default modal; Enter on Cancel preserves the rendered card and daemon agent record; a fresh Ctrl+W followed by Down+Enter removes both.
- **Does not assert:** StopAgent error classification (covered by `lifecycle/stop/005`–`008`).
- **Platform coverage:** mac+linux.

##### prompt/close-confirm/003 — The `[Close]` button and Ctrl+W share the same confirmation action path.
- **Layer:** L2 (real-binary PTY; production button render, SGR mouse decoding, and keyboard dispatch).
- **Agent:** none (continued `cat` pane).
- **Asserts:** clicking the live `[Close Ctrl+W]` button opens the same rendered pane-scoped Cancel-default modal as Ctrl+W, and neither path tears down the daemon agent before explicit confirmation.
- **Does not assert:** tab-strip `×` dispatch (covered by `mouse/tabstrip/002`–`003`).
- **Platform coverage:** mac+linux.

##### prompt/close-confirm/004 — Input queued behind the arming mouse event cannot confirm an unseen modal.
- **Layer:** L2 (real-binary PTY; one raw burst through production SGR mouse + keyboard event decoding).
- **Agent:** none (continued `cat` pane).
- **Asserts:** a single burst containing the real Close-button click followed by Down+Enter opens the modal with Cancel still selected and leaves the daemon agent alive; only a fresh post-render Down+Enter closes it.
- **Does not assert:** terminal-driver event chunking beyond the one-write burst used by the regression.
- **Platform coverage:** mac+linux.

##### prompt/close-confirm/005 — A vanished armed session closes nothing and never retargets its replacement.
- **Layer:** L2 (real-binary PTY plus a synthetic replacement SessionStart delivered through the real daemon hook socket).
- **Agent:** none (continued `cat` pane; the hook gives the same pane a distinct replacement agent/session identity in rendered state).
- **Asserts:** after Ctrl+W arms the original session identity, a different-agent SessionStart replaces it on the same pane; confirming surfaces `Nothing closed`, retains the card, and leaves the daemon agent alive rather than closing the replacement.
- **Does not assert:** tab identity binding (covered independently by `mouse/tabstrip/003`).
- **Platform coverage:** mac+linux.

##### prompt/close-confirm/006 — A dashboard Session target that belongs to a Mode tab uses whole-tab copy and teardown.
- **Layer:** L2 (real-binary PTY against a protocol-faithful scripted daemon).
- **Agent:** none (a hydrated Mode agent pane rendered as a dashboard card plus one persistent side pane).
- **Asserts:** arming Ctrl+W from the selected dashboard card renders `Close this tab and all its panes?`, never the pane sentence; confirming sends stops for both daemon panes and removes the tab only after the registry is empty.
- **Does not assert:** internal `CloseTarget`/`ClosePlan` variants; the rendered promise and observable blast radius are the contract.
- **Platform coverage:** mac+linux.

#### prompt/pane-input

##### prompt/pane-input/001 — `Enter` on a focused side pane enters PaneInput mode.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** the mode line / focus indicator updates to indicate PaneInput mode; a subsequent letter keystroke is forwarded to the side pane's PTY.
- **Does not assert:** the side pane's command output (depends on the fixture shell).
- **Platform coverage:** mac+linux.

##### prompt/pane-input/002 — `Ctrl+d` from PaneInput returns to Normal mode without writing the keystroke to the PTY.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** mode flips back to Normal; the PTY's parsed grid does not gain a stray `^D`.
- **Does not assert:** any toast / status-line message.
- **Platform coverage:** mac+linux.

##### prompt/pane-input/003 — `Ctrl+c` in PaneInput delivers SIGINT (0x03) to the pane's process.
- **Layer:** L2.
- **Agent:** none (fixture: `sh -c 'trap "echo INT" INT; sleep 5'`).
- **Asserts:** the pane PTY shows `INT` after the keystroke, confirming the signal was delivered.
- **Does not assert:** signal handling in the dashboard tab itself (covered by `dashboard/quit/*`).
- **Platform coverage:** mac+linux.

##### prompt/pane-input/004 — A history-only send returns an honest result and surfaces feedback instead of silently dropping input (PRD #20 M3/M4).
- **Layer:** L2 (real spawned TUI + daemon in the PTY/vt100 harness; synthetic pane and hook event, no LLM).
- **Agent:** synthetic wrapped Codex session backed by `cat`, declared `writable = history-only` through `AgentEvent.live_target`.
- **Asserts:** `WriteAndSubmit` returns `send_result = history-only`; attempting to enter the card renders `History-only session cannot accept live input`; the rejected send does not remove the Codex card.
- **Does not assert:** real Codex execution or wrapper stdout classification (covered by `codex/live/001` and `codex/wrap/001`).
- **Platform coverage:** mac+linux.

##### prompt/pane-input/005 — An open attach stream rejects key and paste input after its focused session becomes non-live (PRD #20, blocker 6).
- **Layer:** L1 protocol integration (in-process daemon attach server + real PTY-backed shell; fast tier).
- **Agent:** synthetic Codex session bound to the shell pane.
- **Asserts:** a baseline live key reaches the child; after the same session declares history-only, subsequent key and bracketed-paste `KIND_STREAM_IN` frames produce no child output.
- **Does not assert:** UI mode exit or card feedback; this pins the authoritative daemon stream-input gate.
- **Platform coverage:** mac+linux.

##### prompt/pane-input/006 — Seed-prompt delivery is retained safely and abandoned after its deadline (PRD #20 findings #3/#4/#13).
- **Layer:** L1 (in-process seed-prompt readiness consumer with a controllable `PaneController`).
- **Agent:** none.
- **Asserts:** injected transport error and non-applied outcomes retain the seed with feedback and backoff; two fresh TUI states generate distinct IDs; delivery captures its logical session; an expired permanent failure is abandoned without another RPC.
- **Does not assert:** daemon production of stale/wrong-session or orchestration-role status; those require identity-bearing daemon requests and the orchestration render loop.
- **Platform coverage:** mac+linux+windows.

##### prompt/pane-input/007 — An orchestrator prompt is retained and the role stays non-working after a non-applied result (PRD #20, blocker 5 / finding 17).
- **Layer:** L2 PTY-attached real orchestration flow with a synthetic role that changes from history-only to live.
- **Agent:** synthetic Codex role emitting raw `AgentEvent` liveness transitions; no LLM.
- **Asserts:** the real spawn-time orchestrator-prompt action surfaces HistoryOnly feedback, does not mark the role Working, retains the prompt, and retries it successfully once the same role declares a live PTY target.
- **Does not assert:** the other result variants (covered at the seed consumer by `prompt/pane-input/006`).
- **Platform coverage:** mac+linux.

##### prompt/pane-input/008 — Stream-input rejection visibly exits PaneInput for both keys and paste (PRD #20 R20-007).
- **Layer:** L2 PTY-attached real dashboard with a synthetic pane and hook liveness transition.
- **Agent:** synthetic Codex session backed by `cat`; no LLM.
- **Asserts:** after a focused live pane becomes history-only, a rejected key and rejected bracketed paste each render feedback and leave PaneInput mode.
- **Does not assert:** the daemon's byte-level stream gate (covered by `prompt/pane-input/005`).
- **Platform coverage:** mac+linux.

##### prompt/pane-input/009 — A queued prompt cannot cross an agent or logical-session generation (PRD #20 finding #4).
- **Layer:** L1 protocol integration with an in-process daemon and real PTY-backed shells.
- **Agent:** synthetic Codex identities bound sequentially to the same pane.
- **Asserts:** requests queued for an original agent, a same-agent pre-`/clear` session, or a session missing on the target return `wrong-session`/`stale` and write no marker.
- **Does not assert:** UI feedback for the returned result (covered by `prompt/pane-input/006`).
- **Platform coverage:** mac+linux.

##### prompt/pane-input/010 — Delivery IDs are atomic and bound to a request fingerprint (PRD #20 finding #3).
- **Layer:** L1 protocol integration with an in-process daemon and real PTY-backed shell.
- **Agent:** synthetic Codex identity backed by `/bin/sh`.
- **Asserts:** sequential and writer-barrier concurrent duplicates produce one append; reusing an ID with a different payload or target cannot replay a false successful result.
- **Does not assert:** retry scheduling or visible feedback.
- **Platform coverage:** mac+linux.

##### prompt/pane-input/011 — Unknown send-result variants decode as safe non-delivery (PRD #20 R20-011).
- **Layer:** L1 fast wire-decoding unit test.
- **Agent:** none.
- **Asserts:** a future `send_result` value does not reject the whole response and is not classified as delivered.
- **Does not assert:** live daemon version skew.
- **Platform coverage:** mac+linux+windows.

##### prompt/pane-input/012 — `ok=false` overrides a contradictory applied send result (PRD #20 R20-011).
- **Layer:** L1 fast client test with a synthetic Unix-socket daemon.
- **Agent:** none.
- **Asserts:** the client does not report delivery for `{ok:false, send_result:"applied"}`.
- **Does not assert:** server-side response construction.
- **Platform coverage:** mac+linux.

##### prompt/pane-input/013 — Liveness is revalidated after acquiring the exact target writer (PRD #20 R20-006).
- **Layer:** L1 protocol integration with an in-process daemon, held writer mutex, and real PTY-backed shell.
- **Agent:** synthetic Codex identity backed by `/bin/sh`.
- **Asserts:** a request authorized while live but blocked on the writer writes no bytes after the session becomes history-only.
- **Does not assert:** the attach-handle removal race in R20-008, which has no deterministic harness barrier.
- **Platform coverage:** mac+linux.

##### prompt/pane-input/014 — Post-snapshot stream rejection returns a typed reason (PRD #20 finding #10).
- **Layer:** L1 protocol integration with an in-process daemon and live state handle.
- **Agent:** synthetic Codex identity backed by `/bin/sh`.
- **Asserts:** after the client observes `Live` but daemon state changes before `KIND_STREAM_IN`, both key and paste frames receive a non-empty typed rejection frame.
- **Does not assert:** the TUI's visible feedback/mode exit after consuming that frame; no injectable UI/server barrier currently spans those processes.
- **Platform coverage:** mac+linux.

##### prompt/pane-input/015 — Guarded send fails safe against a daemon without the capability (PRD #20 finding #6).
- **Layer:** L1 client protocol test with a synthetic previous-shape Unix-socket daemon.
- **Agent:** none.
- **Asserts:** an identity-bearing send returns an error and submits zero requests when the daemon handshake lacks guarded-send capability.
- **Does not assert:** the release-step manual test against the actual previous-release daemon.
- **Platform coverage:** mac+linux.

##### prompt/pane-input/016 — Orchestrator prompt identity is captured at tab creation (PRD #20 finding #5).
- **Layer:** L1 orchestration action with a controllable pane-controller rebind.
- **Agent:** none.
- **Asserts:** replacing the start pane's agent after tab creation cannot change the queued prompt's captured target identity.
- **Does not assert:** daemon-side stale rejection, covered by `prompt/pane-input/009`.
- **Platform coverage:** mac+linux+windows.

##### prompt/pane-input/017 — Malformed guarded-send identity fails closed (PRD #20 Greptile finding #1).
- **Layer:** L1 protocol integration with an in-process daemon and real PTY-backed shells.
- **Agent:** synthetic pane targets backed by `/bin/sh`.
- **Asserts:** a wrong JSON type for `expected_agent_id`, `expected_session_id`, or `delivery_id` is rejected and submits no marker bytes.
- **Does not assert:** malformed base `WriteAndSubmit` fields, covered by the protocol's general malformed-request tests.
- **Platform coverage:** mac+linux.

##### prompt/pane-input/018 — A pane-less history-only target rejects stream input (PRD #20 Greptile finding #2).
- **Layer:** L1 protocol integration with an in-process daemon and a real PTY-backed shell carrying no pane environment ID.
- **Agent:** synthetic Codex history-only event attached to a pane-less `/bin/sh` target.
- **Asserts:** `KIND_STREAM_IN` returns a typed non-empty rejection and writes no marker bytes when the attach handle resolves to the no-pane sentinel.
- **Does not assert:** visible TUI feedback after consuming the rejection, covered by `prompt/pane-input/008`.
- **Platform coverage:** mac+linux.

##### prompt/pane-input/019 — Guarded-send generation remains monotonic under delayed prior-session and same-session events (PRD #20 Greptile findings #3/P1).
- **Layer:** L1 protocol integration with an in-process daemon and real PTY-backed shells.
- **Agent:** synthetic Codex lifecycle generations sharing one pane and agent identity.
- **Asserts:** delayed activity cannot restore an old generation, a delayed `SessionEnd` from either a prior session or an older timestamp cannot clear the current generation, a current `SessionEnd` does clear it, stale prompts remain rejected, and current prompts remain deliverable.
- **Does not assert:** transport-level event reordering before `AppState::apply_event`.
- **Platform coverage:** mac+linux.

##### prompt/pane-input/020 — Guarded sends resolve pane-less writability and routing by agent identity (PRD #20 Greptile P1).
- **Layer:** L1 protocol integration with an in-process Unix-domain socket daemon, held writer mutex, and real PTY-backed shells.
- **Agent:** synthetic Codex live and history-only events bound by agent identity to pane-less `/bin/sh` targets.
- **Asserts:** pre-lock history-only sends return `history-only` without bytes, a live-to-history transition while waiting for the writer is rejected after the lock, and a live pane-less target still receives its guarded prompt.
- **Does not assert:** visible TUI feedback for the returned result.
- **Platform coverage:** mac+linux.

##### prompt/pane-input/021 — Ctrl+W performs real shell word deletion without closing the pane.
- **Layer:** L2 (PTY-attached real binary and a real interactive Bash/readline pane).
- **Agent:** none (the shell is the genuine user surface under test, not an agent stand-in).
- **Asserts:** after typing two words, Ctrl+W deletes the previous word, the replacement word is what the submitted command visibly prints, and both the rendered pane and daemon agent record still exist.
- **Does not assert:** close confirmation from command mode (covered by `prompt/close-confirm/*`).
- **Platform coverage:** mac+linux.

##### prompt/pane-input/022 — Ctrl+W while editing a real interactive Claude Haiku prompt does not tear down the pane.
- **Layer:** L2 (PTY-attached real binary, runtime-skipped when Claude CLI/credentials are unavailable; flaky-tolerant pre-PR tier).
- **Agent:** REAL interactive Claude Code on `claude-haiku-4-5-20251001`, with onboarding/project trust seeded and `--allowedTools Bash Read`; no `-p`.
- **Asserts:** after the real Claude pane registers under its temp-directory-prefixed display name and the genuine interactive prompt renders, typing two sentinel words and pressing Ctrl+W visibly deletes the final word, proving the keystroke reached Claude; returning to command mode leaves the pane visible and the same daemon-side agent record present.
- **Does not assert:** an LLM response (the safety invariant and native prompt-edit behavior are proven without submitting a model turn).
- **Platform coverage:** mac+linux.

##### prompt/pane-input/023 — Orchestrator prompt writes remain provisional until the matching submission is observed.
- **Layer:** L1 (in-process orchestrator prompt consumer with a controllable `PaneController` and hook-derived state snapshot).
- **Agent:** none.
- **Asserts:** both `Applied` and `Queued` retain the prompt text, delivery identity, retry backoff, non-Working role, and unprompted tab; a matching `UserPromptSubmit`-derived event for that pane clears all provisional state and alone finalizes the role as Working without another write.
- **Does not assert:** how confirmation is correlated internally or the daemon's PTY behavior; only the consumer's observable delivery state contract.
- **Platform coverage:** mac+linux+windows.

##### prompt/pane-input/024 — Seed delivery distinguishes confirmable panes, unconfirmable panes, and both swallowed-CR duplicate shapes.
- **Layer:** L1 (in-process `process_pending_seed_prompts` consumer with a controllable `PaneController` and hook-derived state snapshot).
- **Agent:** none.
- **Asserts:** `Applied`/`Queued` reporting panes remain provisional until matching submission; one Pi status event and a pane with no identity each write exactly once without arming retries; short and >200-byte doubled submissions joined by either a newline or no separator clear retry state before an immediately eligible third write; repetition is bounded to 16 newline-separated copies and is not a wildcard.
- **Does not assert:** orchestration-role status (covered by `prompt/pane-input/023`) or whether the seed came from dispatch versus a configured mode.
- **Platform coverage:** mac+linux+windows.

##### prompt/pane-input/025 — Unconfirmed prompts retry to deadline, including a producer identified only after the readiness fallback.
- **Layer:** L1 (clock-controlled orchestrator prompt consumer and controllable `PaneController`).
- **Agent:** none.
- **Asserts:** an `Applied` write with no matching submission stays pending, retries only after its armed backoff, never marks the role Working, and is abandoned without a final write after `AUTOMATIC_PROMPT_DEADLINE`; an unidentified fallback write stays provisional without retyping and arms a real retry when a late reporting `SessionStart` arrives.
- **Does not assert:** wall-clock scheduling in the render loop or exact tracing-log wording.
- **Platform coverage:** mac+linux+windows.

##### prompt/pane-input/026 — Only fresh matching prompt evidence carrying the target identity confirms provisional delivery.
- **Layer:** L1 (in-process seed-prompt consumer with two pane identities and synthetic hook-derived snapshots).
- **Agent:** none.
- **Asserts:** matching text with no agent id, matching text from another pane, unrelated target-pane text, and matching text already present before the write all leave delivery identity and retry armed; only fresh matching pane/text/identity evidence finalizes the seed.
- **Does not assert:** a particular reconciliation key or algorithm beyond rejecting these observable false matches.
- **Platform coverage:** mac+linux+windows.

##### prompt/pane-input/027 — Attempt-ID rotation crosses a caching delivery ledger without weakening same-attempt idempotency.
- **Layer:** L1 (in-process seed consumer backed by a faithful per-delivery-id caching controller).
- **Agent:** none.
- **Asserts:** a lost response retries the same `#a1` id and replays cached `Applied` without a second physical write; the later unconfirmed retry rotates to `#a2`, reaches the writer physically, and a returned `Ambiguous` terminally clears all delivery state with no further attempt.
- **Does not assert:** daemon socket framing or the registry's ledger implementation internals; the controller reproduces its observable caching contract.
- **Platform coverage:** mac+linux+windows.

##### prompt/pane-input/028 — A provisional retry never reaches a replacement agent or a same-agent conversation ended by clear.
- **Layer:** L1 (in-process seed consumer with an identity-guarding, rebindable `PaneController` and hook-derived generation state).
- **Agent:** none.
- **Asserts:** after the first write, a different registry agent appearing on the pane gets zero bytes and terminally disarms the old delivery; a `SessionEnd` for the bound generation likewise prevents any same-agent retry and clears provisional state.
- **Does not assert:** the detached scheduler/dispatch confirmation task or a real agent's `/clear` command; it pins the same observable identity/generation contract at the TUI controller seam.
- **Platform coverage:** mac+linux+windows.

##### prompt/pane-input/030 — An unbound launcher delivery may bind a live generation before retry, but cannot follow a generation through its end into a successor, even when the first applied write's response was lost.
- **Layer:** L1 (in-process seed and orchestrator consumers with a payload/identity-recording `PaneController`, a first-response-loss mode, and hook-derived generation state).
- **Agent:** none.
- **Asserts:** the first write into a pane with no announced hook session declares no generation; once the real agent's `SessionStart` arrives and remains current, the next retry binds it and — fork #194: `MAX_PAYLOAD_SUBMISSIONS = 1` leaves no bounded replacement payload — already probes submission with an empty payload, and a further retry probes again under its own distinct wire delivery id, proving the binding persists across more than one retry; separately, both seed and orchestrator TUI write sites send no bytes into a successor when a generation is observed and then ends, when its complete start/end plus the successor start burst between two render passes, or when that burst follows a physically applied first write whose RPC response was lost.
- **Does not assert:** the daemon-side confirmation task's own latch (covered by `scheduler/dispatch/016`) or the PTY bytes an empty payload produces (covered by the registry's submit path).
- **Platform coverage:** mac+linux+windows.

##### prompt/pane-input/031 — Daemon-synthetic events do not prove a usable prompt-reporting channel.
- **Layer:** L1 (in-process seed consumer with a payload-recording `PaneController` and synthetic hook-derived snapshots).
- **Agent:** none.
- **Asserts:** identified daemon-authored shell-activity and delivery-notice events landing after the write, alongside a real but untagged legacy hook frame, leave the delivery held.
- **Does not assert:** an unauthenticated unmarked producer claim on the TUI path — deliberately not pinned because it is indistinguishable from `prompt/pane-input/025`'s accepted slow-launcher recovery and blocking it would re-open #424 for launchers with no bootstrap event; the detached path is pinned by `scheduler/dispatch/016`. It also does not assert authentication of the delivery-notice metadata key, which grants no privilege.
- **Platform coverage:** mac+linux+windows.

##### prompt/pane-input/032 — User typing after a TUI automatic payload disarms the next retry.
- **Layer:** L1 (in-process seed consumer driving the production registry guard and a real `/bin/cat` byte-observation PTY).
- **Agent:** none.
- **Asserts:** attempt 1 physically reaches the pane before any automatic-write timestamp exists; a draft typed after attempt 1 prevents attempt 2's submit-only probe, proven by an unchanged PTY byte snapshot.
- **Does not assert:** the fix's internal clock-comparison location or the detached spawn watcher (covered by `scheduler/dispatch/018`). **Retired 2026-08-15 (fork #194/#341):** this test used to carry a second "replacement pane" sub-scenario asserting that a draft typed between attempt 1 and attempt 2 prevented attempt 2 from appending its replacement payload. `MAX_PAYLOAD_SUBMISSIONS = 1` (fork #194, `src/prompt_delivery.rs`) makes `attempt_writes_payload(2)` `false`, so attempt 2 now takes the same empty-payload probe branch (`user_typed_since_automatic_write`) this entry's retained scenario already exercises — the retired sub-scenario had become a duplicate of the retained one, not a vacuous pass, and was removed rather than kept for appearances. Recovering a launcher that genuinely consumes attempt 1 — what would make the replacement-payload branch reachable again — is deferred to fork issue #343.
- **Platform coverage:** mac+linux.

##### prompt/pane-input/033 — A confirmation retry writes the orchestrator prompt payload exactly once; every later attempt probes submission only.
- **Layer:** L1 (in-process orchestrator consumer with a payload-recording `PaneController` and a controlled clock).
- **Agent:** none.
- **Asserts:** across five delivery attempts on a pane that never reports the prompt submitted, only the first recorded write carries the prompt text; every attempt after it carries an empty payload, proving the retry probes submission rather than retyping the prompt (fork #194).
- **Does not assert:** the TUI-owned seed delivery path (covered by `prompt/pane-input/030`/`032`) or the daemon-owned spawn delivery in `src/spawn.rs` (out of this layer's reach).
- **Platform coverage:** mac+linux+windows.

#### prompt/quit

##### prompt/quit/001 — `Ctrl+c` from command mode opens the quit confirmation dialog with three options: **Detach** (default), **Stop**, **Cancel**.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** dialog appears; option list reads `Detach / Stop / Cancel` in that order; the selection cursor starts on Detach (index 0).
- **Does not assert:** local-vs-remote rendering — the dialog is identical (`Detach` is the daemon-attach-aware option in both cases since every pane is daemon-backed).
- **Platform coverage:** mac+linux.

##### prompt/quit/002 — `Ctrl+c` again while the quit dialog is open exits the TUI without sending an explicit `KIND_DETACH` frame.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** the harness's spawned binary exits; daemon and managed agents stay alive; no detach frame was observed on the daemon socket.
- **Does not assert:** daemon's eventual idle exit (covered by `lifecycle/daemon-idle/*`).
- **Platform coverage:** mac+linux.

##### prompt/quit/003 — Selecting **Detach** from the quit dialog sends an explicit `KIND_DETACH` frame to the daemon, then exits.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** dialog yields a `KIND_DETACH` frame on the daemon's attach socket before the TUI exits; managed agents stay alive afterwards.
- **Does not assert:** any difference between local and remote daemons — the frame and exit behavior are identical; the observable difference (daemon-side log line) is daemon-side, not deck-side.
- **Platform coverage:** mac+linux.

##### prompt/quit/004 — Selecting **Stop** with managed agents alive opens a secondary confirm dialog (`No` / `Yes`, `No` default) naming the agent count.
- **Layer:** L2.
- **Agent:** none (synthetic — one running stub agent).
- **Asserts:** the secondary dialog appears with header containing `1 managed agent will be terminated`; options read `No / Yes` in that order with `No` selected; pressing `No` returns to the primary `Detach / Stop / Cancel` dialog; pressing `Yes` performs StopAndQuit (daemon and agents terminate).
- **Does not assert:** the singular/plural agent-count wording (loose substring match on the count).
- **Platform coverage:** mac+linux.

##### prompt/quit/005 — Selecting **Stop** with zero managed agents skips the secondary confirm and terminates the daemon directly.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** no secondary dialog appears; the TUI exits and the daemon socket disappears within the grace window.
- **Does not assert:** SIGTERM vs SIGKILL escalation (covered by `lifecycle/stop/003`).
- **Platform coverage:** mac+linux.

#### prompt/dir-picker

##### prompt/dir-picker/001 — `Ctrl+n` opens the new-pane flow; the directory picker is the first step and lists the start directory's entries.
- **Layer:** L2.
- **Agent:** none (fixture with a small directory tree at the harness's redirected `HOME`).
- **Asserts:** the picker appears with the fixture's root entries rendered; the selection cursor starts on the first entry (`..` parent is visible but not selected).
- **Does not assert:** sort order beyond "directories before files" (covered if needed).
- **Platform coverage:** mac+linux.

##### prompt/dir-picker/002 — `j` / `Down` / `k` / `Up` cycle the selected directory; selection wraps end-to-end.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** selection cursor advances through entries; pressing `Up` on the first entry jumps to the last (and vice versa).
- **Does not assert:** rendering of inactive entries beyond presence.
- **Platform coverage:** mac+linux.

##### prompt/dir-picker/003 — `l` / `Right` / `Enter` descend into the selected directory; `h` / `Left` / `Backspace` ascend.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** after descending, the picker shows the child directory's contents; after ascending, it shows the parent's contents again.
- **Does not assert:** any breadcrumb / path rendering beyond directory contents.
- **Platform coverage:** mac+linux.

##### prompt/dir-picker/004 — `Space` confirms the current directory and advances to the new-pane form.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** the directory picker closes; the new-pane form appears with the chosen directory pre-filled.
- **Does not assert:** the form's default field values (covered by `prompt/new-pane/*`).
- **Platform coverage:** mac+linux.

##### prompt/dir-picker/005 — `/` opens filter mode; typing narrows directories case-insensitively; the `..` parent stays visible.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** filter accepts a substring; only matching directories remain; `..` is rendered regardless of filter.
- **Does not assert:** filter regex syntax (it is plain substring matching).
- **Platform coverage:** mac+linux.

##### prompt/dir-picker/006 — `Esc` clears the active filter; pressing `Esc` again closes the picker.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** first `Esc` empties the filter and restores the full directory list; second `Esc` returns control to the dashboard.
- **Does not assert:** filter input box visibility between key presses.
- **Platform coverage:** mac+linux.

##### prompt/dir-picker/007 — `q` cancels the picker and returns to the dashboard without spawning a pane.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** the picker closes; no new pane appears; daemon `list_agents` is unchanged.
- **Does not assert:** rendering of any toast / status-line message.
- **Platform coverage:** mac+linux.

#### prompt/new-pane

##### prompt/new-pane/001 — The new-pane form opens after the directory picker with three fields visible (Name, Command, Mode) and the initial focus on Name.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** the form renders all three field labels; the focus indicator is on the Name field; Mode is set to the default.
- **Does not assert:** the default command string (a configurable `default_command`).
- **Platform coverage:** mac+linux.

##### prompt/new-pane/002 — `Tab` and `Shift+Tab` cycle focus forward and backward between fields.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** `Tab` from Name moves focus to Command; another `Tab` moves to Mode; `Shift+Tab` from Mode moves back to Command; cycling wraps at both ends.
- **Does not assert:** which field accepts which input (text vs cycle).
- **Platform coverage:** mac+linux.

##### prompt/new-pane/003 — On the Mode field, `Left` / `Right` / `h` / `l` cycle through the available modes including the default and any project-defined modes / orchestrations.
- **Layer:** L2.
- **Agent:** none (fixture `.dot-agent-deck.toml` defines one mode and one orchestration).
- **Asserts:** cycling from the default shows the mode name, then the orchestration name, then wraps back; the rendered Mode field text follows the cycle.
- **Does not assert:** what happens to other fields while the Mode cycles (Command may be hidden when an orchestration is selected — covered by `prompt/new-pane/004`).
- **Platform coverage:** mac+linux.

##### prompt/new-pane/004 — Selecting an orchestration hides the Command field (each role's command is supplied by the config).
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** with the Mode cycled to an orchestration, the Command label is not rendered; cycling back to a non-orchestration Mode re-renders Command.
- **Does not assert:** what content `Command` had before being hidden (no data loss expected, but not pinned here).
- **Platform coverage:** mac+linux.

##### prompt/new-pane/005 — `Enter` submits the form; the resulting pane (or mode / orchestration tab) is created.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** after submit, a card / tab appears that matches the form inputs.
- **Does not assert:** post-submit focus location (covered by `lifecycle/start/*`).
- **Platform coverage:** mac+linux.

##### prompt/new-pane/006 — `Esc` cancels the form and returns to the dashboard.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** form closes; no new pane appears; daemon `list_agents` is unchanged.
- **Does not assert:** the dashboard's selection cursor location on return.
- **Platform coverage:** mac+linux.

##### prompt/new-pane/007 — The new-deck dialog surfaces a built-in `schedule` authoring option, visually separated from the workload modes (PRD #127 M3.2).
- **Layer:** L2 (re-sequenced from L1: the dialog renderer + `NewPaneFormState` are private and there is no public L1 render seam, so the real dialog is driven via PTY keystrokes and asserted on the rendered vt100 grid).
- **Agent:** none (drives Ctrl+n → dir-picker → new-pane form, then cycles the Mode field).
- **Asserts:** after cycling the Mode field to the end, the dialog's authoring-session affordance — the `↳`-marked hint that separates `schedule` from the workload modes — renders its FULL text (normalized for grid padding) as exactly `↳ authoring (one-off)` AND stays fully contained within the new-pane modal border (its tail is followed by padding before the right `│`, not clipped by it).
- **Does not assert:** the authoring seed-prompt delivery (covered by `tabs/mode/005`); the manager dialog's add/edit path (Phase 3B-ii); the leading-pad width that aligns the hint under the mode chips.
- **Platform coverage:** mac+linux.

##### prompt/new-pane/008 — Submitting the built-in `schedule` authoring option opens a single-agent dashboard card, not a 50/50 mode tab (PRD #127 bug fix).
- **Layer:** L2 (no public L1 render seam for the dialog or the post-submit layout — same constraint as `prompt/new-pane/007`; the real TUI is driven via PTY keystrokes and asserted on the rendered vt100 grid).
- **Agent:** none (the schedule option's Command field is empty, so the spawn falls back to `$SHELL`; the card-vs-mode-tab layout renders independent of the agent).
- **Asserts:** after cycling the Mode field to the `schedule` option and submitting, the rendered grid shows the dashboard-with-card layout — the dashboard's `dot-agent-deck — N session(s)` title is present (it renders only on the Dashboard tab) AND no `×` tab-close glyph appears — proving the authoring session stayed a single-agent card rather than opening as a separate 50/50 mode tab.
- **Does not assert:** the authoring seed-prompt delivery (covered by `tabs/mode/005`); the exact mode-tab split geometry; the spawned agent's command behavior.
- **Platform coverage:** mac+linux.

##### prompt/new-pane/009 — The built-in `[schedule]` Mode chip stays fully visible inside the modal even when the chip row is wider than the modal (overflow regression guard).
- **Layer:** L2 (no public L1 render seam for the dialog — same constraint as `prompt/new-pane/007`; the real TUI is driven via PTY keystrokes and asserted on the rendered vt100 grid).
- **Agent:** none (drives Ctrl+n → dir-picker → new-pane form, then cycles the Mode field to the `schedule` option).
- **Asserts:** with a fixture defining a workload mode (`build`) plus an orchestration (`ci-deployment`) — so the Mode chip row `  Mode: [No mode] [build] [Orch: ci-deployment] [schedule]` is wider than the capped modal — cycling to and selecting the trailing built-in `[schedule]` option leaves that `[schedule]` chip rendered FULLY between some row's modal borders (`│ … │`), not clipped at the right edge. Approach-agnostic: passes whether the renderer wraps the chip row or windows/scrolls the cycler, as long as the selected chip ends up visible inside the modal.
- **Does not assert:** the exact layout used to keep the chip visible (wrap vs. window/scroll); the visibility of the non-selected chips when the row overflows; the authoring hint text (covered by `prompt/new-pane/007`).
- **Platform coverage:** mac+linux.

##### prompt/new-pane/010 — The new-pane Mode cycler offers an experimental `schedule: issues` issue-dispatch authoring option only when the experimental flag is ON; it is hidden when OFF while the plain `[schedule]` option still shows (PRD #120).
- **Layer:** L2 (no public L1 render seam for the dialog — same constraint as `prompt/new-pane/007`; the real TUI is driven via PTY keystrokes and asserted on the rendered vt100 grid).
- **Agent:** none (drives Ctrl+n → dir-picker → new-pane form in two flag states).
- **Asserts:** launched with `DOT_AGENT_DECK_EXPERIMENTAL=1`, opening the new-pane form shows a `schedule: issues` option on the Mode cycler alongside the existing `[schedule]` option; a control launch with no env var (flag OFF) renders the plain `[schedule]` option but NOT `schedule: issues`. RED until the option exists: today no flag state carries `schedule: issues`, so the experimental-ON grid never contains it.
- **Does not assert:** the authoring seed delivered when the option is selected (covered by `scheduler/form/007`); the post-submit layout; the chip's exact position in the cycler.
- **Platform coverage:** mac+linux.

##### prompt/new-pane/011 — The new-agent Command field seeds from the last command you spawned when no `default_command` is configured (PRD #196).
- **Layer:** L2 (no public L1 render seam for the new-pane form — same constraint as `prompt/new-pane/007`; the real TUI is driven via PTY keystrokes and asserted on the rendered vt100 grid).
- **Agent:** none (`cat` is a real runnable stand-in command — the spawn succeeds and records a last command, and `cat` blocks on stdin so the pane stays alive; no LLM tokens).
- **Asserts:** with an empty `default_command`, opening the new-pane form the first time leaves the Command field BLANK; after typing `cat` and submitting (spawning a pane), reopening the form pre-fills the Command field with `cat`, seeded from the recorded last command. RED until the feature lands: nothing reads the recorded last command back, so the reopened field renders blank.
- **Does not assert:** persistence of the last command across a full deck restart (the read-back here is in-process); per-directory last commands (the value is global); the exclusion of authoring-mode fallback commands from the recorded last command.
- **Platform coverage:** mac+linux.

##### prompt/new-pane/012 — An explicit `default_command` still wins over the recorded last command in the new-agent form — precedence guard (PRD #196).
- **Layer:** L2 (no public L1 render seam for the new-pane form — same constraint as `prompt/new-pane/007`; the real TUI is driven via PTY keystrokes and asserted on the rendered vt100 grid).
- **Agent:** none (`cat` is a real runnable stand-in command — the spawn succeeds and records a last command; no LLM tokens).
- **Asserts:** with `default_command = "configured-default-cmd"`, the new-pane Command field pre-fills from it; after clearing the field, typing `cat`, and submitting (recording `cat` as the last command), reopening the form STILL pre-fills `configured-default-cmd` — the explicit config value wins over the recorded last command. GREEN today and after the feature lands.
- **Does not assert:** the empty-`default_command` fallback to the last command (covered by `prompt/new-pane/011`); persistence across a restart.
- **Platform coverage:** mac+linux.

##### prompt/new-pane/013 — An authoring-mode spawn's command IS recorded and seeds a later regular form — the exclusion was dropped so all form-launched spawns record their command (PRD #196).
- **Layer:** L2 (no public L1 render seam for the new-pane form — same constraint as `prompt/new-pane/007`; the real TUI is driven via PTY keystrokes and asserted on the rendered vt100 grid).
- **Agent:** none (`cat` is a real runnable stand-in command — the authoring spawn succeeds and `cat` blocks on stdin so the card stays alive; no LLM tokens).
- **Asserts:** with an empty `default_command`, cycling the Mode field to the built-in `schedule` AUTHORING option, clearing the Command field, typing `cat`, and submitting dispatches an authoring-mode spawn; reopening a FRESH regular form (no Mode cycle) then PRE-FILLS the Command field with `cat` — an authoring-mode spawn now records a last command like any other form-launched spawn (the exclusion was dropped for consistency), so the regular form seeds from it. RED until the coder removes the authoring gate.
- **Does not assert:** the plain-spawn seed-from-last-command path (covered by `prompt/new-pane/011`); the `default_command` precedence (covered by `prompt/new-pane/012`); persistence across a restart (covered by `prompt/new-pane/014`).
- **Platform coverage:** mac+linux.

##### prompt/new-pane/014 — The recorded last command survives a full deck restart and pre-fills the new-agent form on the next launch (PRD #196).
- **Layer:** L2 (no public L1 render seam for the new-pane form — same constraint as `prompt/new-pane/007`; the real TUI is driven via PTY keystrokes and asserted on the rendered vt100 grid; two launches share one isolated HOME so the persisted session carries over).
- **Agent:** none (`cat` is a real runnable stand-in command — the spawn succeeds and records a last command, and `cat` blocks on stdin so the pane stays alive; no LLM tokens).
- **Asserts:** with an empty `default_command`, launch 1 spawns `cat` and quits cleanly so the session flushes to disk; launch 2 (sharing the same HOME) then PRE-FILLS the new-pane Command field with `cat`, read back from the persisted `session.toml` launch 1 wrote — proving the recorded last command round-trips through persist → reload → seed, not just in-process state. GREEN against the current implementation — a regression guard.
- **Does not assert:** the in-process read-back within one launch (covered by `prompt/new-pane/011`); the `default_command` precedence (covered by `prompt/new-pane/012`); the authoring-mode recording (covered by `prompt/new-pane/013`).
- **Platform coverage:** mac+linux.

##### prompt/new-pane/015 — Selecting an agent in the real new-pane form seeds its registry default without a global-config copy (PRD #20, finding 8).
- **Layer:** L2 PTY-attached (the private new-pane form is driven through the real binary and its visible selector is clicked/cycled).
- **Agent:** none (selection rows for Claude Code, OpenCode, Pi, and Codex; no agent process is submitted).
- **Asserts:** with no global `default_command`, the form exposes an `Agent:` selector; selecting each shipped type visibly updates Command to exactly that type's `AgentSpec.default_command`.
- **Does not assert:** launch wrapping (covered by `codex/spawn/*` and `codex/live/001`) or custom command arguments.
- **Platform coverage:** mac+linux.

##### prompt/new-pane/016 — Selecting the "dispatcher" option in the new-pane form opens a live dispatcher dashboard card whose real Claude agent, given a goal, invokes `dot-agent-deck dispatch` itself and the daemon creates the promised sibling git worktree (PRD #220). [reel]
- **Layer:** L2 PTY-attached (the REAL `dot-agent-deck` binary driven through the vt100 `TuiDeck` harness with imported Claude credentials — records a `full-stream.cast`). The freshly-built binary's dir is prepended to the PATH the deck → daemon → agents inherit, so the agent's `dot-agent-deck dispatch` resolves to the build under test rather than a host-installed binary that predates the verb.
- **Agent:** Claude Code (interactive `claude`, real Anthropic API — receives the dispatcher seed prompt via gated delivery, acts on the typed goal, and runs the `dispatch` verb itself; no stand-in).
- **Asserts:** the dispatcher surfaces LIVE as a dashboard CARD within 60s of form submission (`1 session(s)`, no tab strip — a mode tab would instead route through `render_mode_tab`'s 50/50 split and render the agent at half width beside an empty column, which is the shape this pins against); that the seed actually reached the pane (a distinctive `DISPATCHER_SEED_PROMPT` phrase, so it cannot pass on an unseeded agent); then, after a directive one-unit goal is typed into the pane, the sibling worktree `../<repo>-dispatch-probe-unit` appears on disk within 180s — proving agent → `dispatch` CLI → daemon → `git worktree add` end to end, at the sibling (never nested) path.
- **Also asserts (added after real use found three defects underneath the original green run):** that the unit comes up as a real AGENT — a second live session whose card carries an agent type — because `SpawnRequest.command: None` reads as `$SHELL` in the spawn path, so the previous assertions passed while the unit was a bash prompt with the task text typed into it. Verified to be capable of failing by reintroducing `command: None`. The typed goal also names `--single`, so the shape selector is exercised end to end rather than steering the agent back onto the legacy config-derived path.
- **Does not assert:** the dispatched unit's own OUTPUT; an `--orchestration` dispatch (covered deterministically by `dispatch::tests::an_orchestration_dispatch_writes_the_delegation_protocol_and_the_task`, which spawns `cat` roles and asserts the orchestrator-context file — no LLM tokens); the return edge (#220's own deferred Phase 2 — NOT #174, which depends on this PRD rather than tracking it); cleanup on tab close (covered by `src/dispatch.rs` unit tests).
- **Platform coverage:** mac+linux.
- **Note:** the fixture repo is given an initial commit by the test — the harness `git init`s fixtures but never commits, and `git worktree add` cannot branch from an unborn HEAD.

### Focus / navigation

#### focus/dashboard

##### focus/dashboard/001 — From command mode, `j` / `k` cycle the selected card; `Enter` is a no-op on the dashboard tab (selection is the source of truth).
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** selection moves; pressing `Enter` does not switch tabs or open any dialog from a selected card.
- **Does not assert:** the broken `Enter`-to-jump behavior tracked in [#68](https://github.com/vfarcic/dot-agent-deck/issues/68); see deliberate skips.
- **Platform coverage:** mac+linux.

#### focus/mode-tab

##### focus/mode-tab/001 — `j` / `k` cycle focus through agent → side panes → agent on a mode tab.
- **Layer:** L2.
- **Agent:** none (two persistent side panes from a fixture mode).
- **Asserts:** the cyan focus border moves through panes in order and wraps.
- **Does not assert:** focus during PaneInput mode (PaneInput pins focus on the active pane).
- **Platform coverage:** mac+linux.

##### focus/mode-tab/002 — `Esc` from a focused side pane returns focus to the agent pane.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** focus indicator jumps to the agent pane region.
- **Does not assert:** focus persistence across tab switches.
- **Platform coverage:** mac+linux.

#### focus/orchestration

##### focus/orchestration/001 — `1`–`9` on an orchestration tab jumps to role pane N and focuses it.
- **Layer:** L2.
- **Agent:** none (orchestration fixture with stub role commands).
- **Asserts:** focused pane index matches the keystroke; the sidebar role-card highlight follows.
- **Does not assert:** what happens beyond the available role count.
- **Platform coverage:** mac+linux.

##### focus/orchestration/002 — Sidebar role cards reflect each role's live status (Thinking / Working / WaitingForInput / Idle / Error).
- **Layer:** L2.
- **Agent:** none (synthetic events targeting two roles).
- **Asserts:** distinct sidebar entries show distinct statuses after distinct hook deliveries.
- **Does not assert:** sidebar layout pixel dimensions.
- **Platform coverage:** mac+linux.

### Modes / tabs

#### tabs/navigation

##### tabs/navigation/001 — `Ctrl+PageDown` / `Ctrl+PageUp` switch tabs from any mode (including from inside a focused pane).
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** active tab index advances / retreats; the keystroke is not delivered to the focused pane's PTY.
- **Does not assert:** the tab bar's exact label widths under truncation (covered by `tab_layout` pure-data tests in the lib tier).
- **Platform coverage:** mac+linux.

##### tabs/navigation/002 — `Tab` / `Shift+Tab` switch tabs only in command mode; in PaneInput mode the keystroke reaches the agent PTY.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** with PaneInput active, `Tab` is delivered to the pane (parsed grid grows); with command mode active, the tab index advances.
- **Does not assert:** `Left` / `Right` / `h` / `l` aliases — covered by `tabs/navigation/003`.
- **Platform coverage:** mac+linux.

##### tabs/navigation/003 — `Left` / `Right` / `h` / `l` alias `Shift+Tab` / `Tab` in command mode.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** each alias keystroke moves the active tab one step in the documented direction.
- **Does not assert:** any aliases under PaneInput mode (those go to the pane).
- **Platform coverage:** mac+linux.

#### tabs/mode

##### tabs/mode/001 — Selecting a mode on the new-pane form opens a mode tab with the agent pane on the left and persistent side panes stacked on the right; both side panes render SIMULTANEOUSLY under the deck's default (Stacked) global `pane_layout` (PRD #311 regression guard).
- **Layer:** L2 (PTY-attached, `tests/e2e_mode_tab_layout.rs`).
- **Agent:** none (fixture `tests/fixtures/mode-two-side-panes` with TWO persistent side panes, each printing a unique sentinel and idling).
- **Asserts:** the new-pane form's Mode selection opens a Mode tab (tab strip appears); with the deck's default `PaneLayout::Stacked` global, BOTH side panes' sentinels are visible in the grid at the same time — proving the Mode tab's side-pane column (hardcoded `PaneLayout::Tiled` in `render_mode_tab`, `src/ui.rs`) does not read the shared global `pane_layout` field (PRD #311's Open Question 2 risk) and so never collapses a side pane to a titled 1-row frame regardless of the global's value.
- **Does not assert:** the side pane's command output content beyond the sentinel line; the agent pane's exact left-half geometry (covered by `compute_frame_layout_mode_geometry`, a plain unit test in `src/ui.rs`); orchestration/dashboard pane-column geometry (covered by `orchestration/layout/002`).
- **Platform coverage:** mac+linux.

##### tabs/mode/002 — `Ctrl+w` on a mode tab tears down the entire workspace (agent + all side panes).
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** tab disappears; the daemon's `list_agents` no longer returns the agent that lived in the tab.
- **Does not assert:** side panes' shells receive SIGTERM vs SIGKILL (an implementation detail).
- **Platform coverage:** mac+linux.

##### tabs/mode/003 — Reactive rule routes a matching agent bash command to a reactive side pane.
- **Layer:** L2.
- **Agent:** none (synthetic `PostToolUse` event for a `Bash` tool whose command matches a rule's pattern).
- **Asserts:** the reactive side pane is populated; its title reflects the matched command.
- **Does not assert:** the rule's regex internals (covered by `config_validation` pure-data tests).
- **Platform coverage:** mac+linux.

##### tabs/mode/004 — Once all reactive slots are full, the next match reuses the oldest slot (circular pool).
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** three distinct matches against a 2-slot pool leave the second and third matches visible; the first is gone.
- **Does not assert:** slot reuse ordering beyond "oldest first".
- **Platform coverage:** mac+linux.

##### tabs/mode/005 — A `[[modes]]` mode carrying a `seed_prompt` auto-delivers it to the agent pane once the agent is ready (gated, like orchestrations); a mode without one delivers nothing (PRD #127 M3.1).
- **Layer:** L2.
- **Agent:** none — a fixture "recorder" agent that self-posts `SessionStart` (the readiness signal) via the real `dot-agent-deck hook` path, then records every prompt written into its PTY stdin.
- **Asserts:** spawning the seeded mode via the new-pane dialog delivers the configured `seed_prompt` into the agent pane after the agent signals readiness (the marker is recorded); spawning a mode without a `seed_prompt` starts the agent but records no auto-delivered prompt.
- **Does not assert:** which gate path fires (SessionStart fast path vs the slow-path fallback) — only that delivery is gated on readiness, not ungated/immediate; the serde round-trip of `seed_prompt` (covered by a coder unit test).
- **Platform coverage:** mac+linux.

##### tabs/mode/006 — A persistent side pane keeping the default `watch = true` shows its command's output while the command is still running (issue #367).
- **Layer:** L2.
- **Agent:** none (fixture whose single mode has one persistent pane running `printf …; sleep 600` under the default watch wrapper).
- **Asserts:** a sentinel assembled at runtime by the command — so it cannot appear in the command line the pane's shell echoes — is visible in the side pane although the command never exits; the echoed wrapper invocation is gone from the pane, proving the watcher cleared the screen ahead of its first output rather than after process exit.
- **Does not assert:** the 10s re-run interval; the ordering of interleaved stdout/stderr; the buffer-then-clear internals (covered by `watch::tests` unit tests).
- **Platform coverage:** mac+linux.

#### tabs/orchestration

##### tabs/orchestration/001 — Selecting an orchestration on the new-pane form opens one pane per role with the orchestrator's pane in focus.
- **Layer:** L2.
- **Agent:** none (orchestration fixture with three stub-command roles, one with `start = true`).
- **Asserts:** the new tab contains three panes; the focused pane is the `start = true` role.
- **Does not assert:** what command is rendered in each pane (the stub fixture is opaque to the harness).
- **Platform coverage:** mac+linux.

##### tabs/orchestration/002 — `Ctrl+w` on an orchestration tab closes the tab and stops every role pane.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** tab disappears; the daemon no longer carries the role agents.
- **Does not assert:** the order in which roles are closed.
- **Platform coverage:** mac+linux.

##### tabs/orchestration/003 — Switching tabs clears the Orchestration deck highlight across ALL tab switches, including orchestration-to-orchestration.
- **Layer:** L1 (in-process `switch_tab_with_focus` + per-frame `reconcile_dashboard_selection`).
- **Agent:** none (two real Orchestration tabs, two roles each).
- **Asserts:** with the orchestration highlight armed on role 1 and the focus baseline established, the highlight is inactive (`selected_index == None`) on the destination after a real round-trip plus the real per-frame reconcile, in BOTH cases: (Part 1) Orchestration → Dashboard → Orchestration — the destination restores the SAME role pane (steady-state focus, no transition); and (Part 2, PR #151 follow-up) Orchestration A → Orchestration B — the destination restores a DIFFERENT role pane than the source, which the first reconcile frame would otherwise read as a focus transition and re-arm. Pins the PRD #113 design revision (2026-06-13) Change 1 (symmetric clearing); analog of `dashboard/selection/011`/`013`.
- **Does not assert:** the cyan controller focus border (driven separately, unaffected); the orchestrator's spawn-time role prompt.
- **Platform coverage:** mac+linux+windows.

##### tabs/orchestration/004 — Enter restores the previously-selected role on the Orchestration deck (not role 0).
- **Layer:** L1 (in-process `switch_tab_with_focus` round-trip + `dashboard_focus_target`).
- **Agent:** none (a real Orchestration tab with two roles; a Mode tab as the round-trip intermediate).
- **Asserts:** with the orchestration highlight armed on role 1, a real Orchestration → Mode → Orchestration round-trip clears the live highlight (`selected_index == None`) but the Enter focus target (`dashboard_focus_target`, the same SSOT the Dashboard uses) is the REMEMBERED role (index 1), not role 0. Pins the PRD #113 design revision (2026-06-13) Change 2 (Enter restores previous) for the Orchestration deck.
- **Does not assert:** the pane-focus side effect of activating the role; the active-selection target.
- **Platform coverage:** mac+linux+windows.

##### tabs/orchestration/005 — Enter restore is per-deck: the Orchestration deck restores ITS OWN previous role, not a Dashboard selection leaked through shared state.
- **Layer:** L1 (in-process `switch_tab_with_focus` round-trip + `dashboard_focus_target`).
- **Agent:** none (a real Orchestration tab with three roles; the Dashboard as the round-trip intermediate).
- **Asserts:** arm the Orchestration deck on role 1, leave to the Dashboard, arm the Dashboard on card 2, then return to the (now inactive) Orchestration deck — Enter restores the Orchestration's OWN remembered role (index 1), NOT the Dashboard's leaked index 2. Pins per-deck independence of the Enter-restore state (the remembered selection must be stored per deck, not in a single shared field). Complements `tabs/orchestration/004` (which restores via a non-deck Mode-tab intermediate that can't clobber the shared field).
- **Does not assert:** the pane-focus side effect of activating the role; the Dashboard's own restore (covered by `dashboard/selection/008`).
- **Platform coverage:** mac+linux+windows.

##### tabs/orchestration/006 — `Ctrl+l` cycles the deck-global sidebar/pane-column split through the three PRD #361 stages — default 34/66, narrower-sidebar 25/75, and Hidden (sidebar collapsed, 0/100) — looping back to default, and a second open orchestration tab ADOPTS and SHARES the one deck-global stage with the first tab (PRD #336, extended to three stages by PRD #361 Item 4; PRD #387 M1 scopes the chord to command mode; PRD #387 M2/M3 collapses what was per-tab isolation into one deck-global `split_stage`, inverting this test's cross-tab assertions from isolation to sharing).
- **Layer:** L2 (real-binary PTY via the vt100 `TuiDeck` harness).
- **Agent:** none (`orch-deck` fixture — two stub `cat` roles, no LLM tokens spent).
- **Asserts:** opening a real orchestration tab A renders the pane column's left edge at the default 34%-width boundary; since opening a tab leaves the deck in PaneInput mode focused on its start-role pane and PRD #387 M1 claims `Ctrl+l` only in command mode, each toggle press is preceded by `Ctrl+D` to enter Normal mode; `Ctrl+l` then moves that boundary to the narrower-sidebar 25%-width position; opening a SECOND real orchestration tab B in the same directory renders it ALREADY at the deck-global Narrow boundary tab A was just toggled to, not its own untoggled default (PRD #387 decision 2 — a new tab adopts the shared stage); cycling tab B through Narrow then Hidden (edge at column 0, sidebar fully collapsed — and directly asserts neither role's sidebar card marker is present on the grid at all, proving Hidden genuinely renders no sidebar content rather than merely that the pane column's edge reached column 0) and switching back to tab A (Shift+Tab, which needs no extra `Ctrl+D` since the deck is already in Normal mode from the toggle above) confirms tab A is now ALSO Hidden — toggling tab B moved the SAME shared stage tab A reads; finishing tab A's own cycle (Hidden -> Default) restores the boundary and, switching to tab B, confirms it is ALSO back at Default — proving the 3-stage loop and deck-global sharing, in both directions, together.
- **Does not assert:** persistence of the toggled state across restart (explicitly out of scope per the PRD); remapping the chord via config; Dashboard-tab coverage (the Dashboard/Orchestration cross-tab-type sharing case lives in `tabs/dashboard/001`).
- **Platform coverage:** mac+linux.

##### tabs/orchestration/007 — `Ctrl+l` must still forward to a live pane's PTY when the active tab is NOT an orchestration tab.
- **Layer:** L2 (real-binary PTY via the vt100 `TuiDeck` harness).
- **Agent:** none (a real interactive `bash --noprofile --norc -i` pane on the Dashboard, no LLM tokens spent).
- **Asserts:** on a Dashboard (non-orchestration) tab with a live shell pane in PaneInput mode, printing a unique sentinel line then pressing `Ctrl+l` must trigger readline's `clear-screen` binding and remove the sentinel from the rendered grid, proving the raw byte reached the PTY. Pins the PRD #336 scope note that `Action::ToggleOrchestrationSplit` only claims `Ctrl+l` on an orchestration tab — regression coverage for the Greptile P1 finding on PR #342 (the global resolver claimed `Ctrl+l` unconditionally, swallowing it on every other tab).
- **Does not assert:** the orchestration-tab toggle behavior itself (covered by `tabs/orchestration/006`); Mode-tab or other non-Dashboard tab types (Dashboard is sufficient to prove the missing tab-context check).
- **Platform coverage:** mac+linux.

##### tabs/orchestration/008 — In a real multi-role orchestration tab under `PaneLayout::Stacked`, non-focused roles render no collapsed title-bar frame, a non-focused role's agent keeps running with its sidebar status transitioning live, and switching focus between roles preserves each role's rendered content with no lost scrollback (PRD #311).
- **Layer:** L2 (PTY-attached, `tests/e2e_orchestration_pane_column.rs`).
- **Agent:** none (fixture `tests/fixtures/orch-focus-lifecycle`: a 3-role orchestration — `orchestrator` (start), `alpha`, `beta` — each printing a unique sentinel; `beta`'s script additionally self-posts real `SessionStart`/`PreToolUse` hook events via `dot-agent-deck hook --agent claude-code`, resolved to the freshly built test binary, so its sidebar status transitions Idle -> Working while its pane is not the focused/expanded slot).
- **Asserts:** (a) with `orchestrator` focused/expanded, the settled grid carries no collapsed `Borders::TOP` title-bar frame for either non-focused role (`alpha`, `beta`) — matched by a row that, after trimming only leading blank columns, begins with the bare role name directly followed by border-fill dashes, a pattern only the collapsed-pane block itself can produce; (b) `beta`'s sidebar status card visibly transitions to `Working` purely from its own self-posted hook events while never becoming the focused pane, proving a non-focused role's agent lifecycle (PTY, hook delivery, status) is untouched by the rendering change; (c) driving `j`/`k` (Normal mode) round-trips focus orchestrator -> alpha -> beta -> alpha -> orchestrator, and each role's own sentinel text is visible again once it becomes the expanded pane, proving no lost scrollback or stale fragment across a focus switch.
- **Does not assert:** PTY resizing of the reclaimed area (`resize_panes_to_layout`); the L1 geometry math (covered by `orchestration/layout/002`); a real LLM agent (all three roles are shell stand-ins); dashboard-tab (non-orchestration) collapsed frames.
- **Platform coverage:** mac+linux.

##### tabs/orchestration/009 — An orchestration tab's tab-bar label renders in the color of the single highest-priority state among its panes (PRD #333).
- **Layer:** L1 (in-process `TestBackend` render via `render_tab_bar_to_buffer`, `tests/render_tab_strip.rs`).
- **Agent:** none (synthetic `SessionStatus` values, no panes/PTYs).
- **Asserts:** given an orchestration tab whose panes carry a mix of `SessionStatus` values, the rendered tab-bar label's foreground color is `palette::status_color()` of the SINGLE highest-priority status among them, in the fixed order Error(Red) > WaitingForInput(Yellow) > Working(Green) > Thinking/Compacting(Blue) > Idle/Unknown(`palette::STATUS_IDLE`) — covering (a) one `Error` among several `Idle` panes -> Red; (b) one `WaitingForInput` among `Working`/`Idle` (no `Error`) -> Yellow; (c) all `Idle` -> `palette::STATUS_IDLE` (fork issue #351, maintainer decision 2026-08-15: colour is a total function of status, including Idle, reversing PRD #333 defect B); (d) a mix of `Thinking` and `Working` (no higher-priority state) -> Green, since Working outranks Thinking. Also asserts a tab with no status data (the Dashboard case) is unaffected (same base color as any other unaffected tab).
- **Does not assert:** the aggregate-priority resolver as a standalone pure-function unit test (PRD #333 M1, may land separately); per-pane sidebar status rendering (covered by `focus/orchestration/002`); pane-column geometry (covered by `orchestration/layout/002`/`004`).
- **Platform coverage:** mac+linux+windows.

##### tabs/orchestration/010 — An ACTIVE orchestration tab renders its status tint as ordinary foreground text, cued as active via UNDERLINED | BOLD instead of REVERSED, and an inactive Idle orchestration tab renders palette::STATUS_IDLE (PRD #333; issue #306 reverses the earlier no-tint-on-active carve-out; fork issue #351, maintainer decision 2026-08-15, reverses defect B so Idle is coloured too).
- **Layer:** L1 (in-process `TestBackend` render via `render_tab_bar_to_buffer`, `tests/render_tab_strip.rs`).
- **Agent:** none (synthetic `SessionStatus` values, no panes/PTYs).
- **Asserts:** an orchestration tab made the ACTIVE tab with a non-idle (`Error`) pane renders its status `fg` tint (Red) as ordinary foreground text, cued as active via `UNDERLINED | BOLD` with no `REVERSED` — since stacking a status `fg` on `Modifier::REVERSED` would invert the color into a background at display time (issue #306, reversing PRD #333 defect A's no-tint carve-out). Also asserts an INACTIVE orchestration tab whose aggregate status is `Idle` renders `palette::STATUS_IDLE` (`Color::DarkGray`) — fork issue #351, maintainer decision 2026-08-15: colour is now a total function of status, reversing defect B — and that an INACTIVE orchestration tab with a non-idle (`Error`) aggregate status still colors its label text with neither `REVERSED` nor `BOLD` nor `UNDERLINED` — pinning the `UNDERLINED` active cue from both sides so an inactive tab can never become indistinguishable from an active one (reviewer finding F3 on PR #307).
- **Does not assert:** the aggregate-priority resolver (covered by `tabs/orchestration/009`); per-pane sidebar status rendering (covered by `focus/orchestration/002`); pane-column geometry (covered by `orchestration/layout/002`/`004`).
- **Platform coverage:** mac+linux+windows.

##### tabs/orchestration/011 — The render loop actually APPLIES `TabManager::auto_focus_waiting_pane`'s result via `pane.focus_pane`, and the resulting focus change is visible in the rendered layout (PR #5 Greptile gap — the resolver alone was pinned, not its wiring into the real per-frame call site).
- **Layer:** L1 (in-process `TestBackend`; drives `TabManager::auto_focus_waiting_pane` + `pane.focus_pane` + `compute_frame_layout` + `render_frame` in the same sequence `run_tui`'s render loop uses, rather than asserting on `TabManager`'s internal field the way `tabs/orchestration/013` does).
- **Agent:** none (synthetic `SessionStatus` map, no panes/PTYs).
- **Asserts:** with the higher-order `coder` role manually focused and the lower-order `orchestrator` role marked `WaitingForInput`, applying the resolver's result through `focus_pane` and reading focus back off the SAME pane controller — the value the render loop actually feeds `compute_frame_layout` — shows the auto-focused `orchestrator` role reclaiming the full `PaneLayout::Stacked` pane-column height while the manually-focused-but-superseded `coder` role cedes its slot to zero height.
- **Does not assert:** the resolver's own selection logic in isolation (`tabs/orchestration/013` and the `orchestration/focus/*` suite); any other layout mode.
- **Platform coverage:** mac+linux+windows.

##### tabs/orchestration/012 — An ACTIVE Dashboard tab — the tab that carries no status data — still carries the UNDERLINED | BOLD active cue, no REVERSED, and no absolute foreground color (issue #306).
- **Layer:** L1 (in-process `TestBackend` render via `render_tab_bar_to_buffer`, `tests/render_tab_strip.rs`).
- **Agent:** none (synthetic; no `tab_statuses` entry, no panes/PTYs).
- **Asserts:** a single active Dashboard tab with `tab_statuses = [None]` renders `UNDERLINED | BOLD` as its active cue, contains no `REVERSED`, and carries `Color::Reset` (no absolute foreground color) — proving the new active cue applies uniformly to the Dashboard, the one tab kind that carries no status data (fork issue #351 narrows this from "tabs PRD #333's status-tint feature does not touch" now that Mode tabs also carry status data via `tab_status_data`; see `tabs/label/001`).
- **Does not assert:** any status-tinted tab (covered by `tabs/orchestration/009`/`010`); the Idle colouring on an active orchestration tab (covered by `tabs/orchestration/014`).
- **Platform coverage:** mac+linux+windows.

##### tabs/orchestration/014 — An ACTIVE orchestration tab whose aggregate status resolves to Idle renders palette::STATUS_IDLE, while still carrying the UNDERLINED | BOLD active cue (issue #306 extended PRD #333 defect B to the active tab as a no-tint carve-out; fork issue #351, maintainer decision 2026-08-15, reverses that carve-out so Idle is coloured here too).
- **Layer:** L1 (in-process `TestBackend` render via `render_tab_bar_to_buffer`, `tests/render_tab_strip.rs`).
- **Agent:** none (synthetic `SessionStatus` values, no panes/PTYs).
- **Asserts:** an orchestration tab made the ACTIVE tab with an all-`Idle` aggregate renders its label in `palette::STATUS_IDLE`, while still carrying `UNDERLINED | BOLD` and no `REVERSED` as its active cue — the active cue is unchanged, only the colour changes.
- **Does not assert:** the non-Idle active-tint case (covered by `tabs/orchestration/010`); the inactive Idle case (also covered by `tabs/orchestration/010`).
- **Platform coverage:** mac+linux+windows.

##### tabs/orchestration/024 — `Ctrl+l` must forward to a focused orchestration role pane's PTY, not be claimed as the split-cycle action, while the deck is in `PaneInput` mode (PRD #387 Defect 1 / M1b — the reported bug, in a real pane).
- **Layer:** L2 (real-binary PTY via the vt100 `TuiDeck` harness).
- **Agent:** none (`orch-bash-role` fixture — a single orchestrator role running a real interactive `bash --noprofile --norc -i`, no LLM tokens spent).
- **Asserts:** opening a real orchestration tab from the `orch-bash-role` fixture lands the deck in `PaneInput` mode with the orchestrator role pane already focused; printing a unique sentinel line then pressing `Ctrl+l` must trigger readline's `clear-screen` binding and remove the sentinel from the rendered grid, proving the raw `0x0c` byte reached the role pane's PTY rather than being claimed as `Action::CycleSplitStage`. Regression coverage for PRD #387 Defect 1: orchestration tabs claimed `Ctrl+l` mode-independently, so a focused role pane's agent never received its own clear-screen.
- **Does not assert:** the split-cycle toggle behavior itself when NOT a role pane is focused (covered by `tabs/orchestration/006`); the already-guarded Dashboard-tab case (covered by `tabs/orchestration/007`); Mode-tab coverage (Mode tabs never claim `Ctrl+l` and have no sidebar split to toggle).
- **Platform coverage:** mac+linux.

#### tabs/label

##### tabs/label/001 — `tab_status_data`, the pure-data extraction behind the `run_tui` tab-bar builder, keys a Mode tab by its OWN agent pane's status instead of `None` (fork issue #351: clicking a worker tab highlighted it with a neutral underline instead of the worker's status colour, because the inline builder mapped every non-Orchestration tab — Mode included — to `None`).
- **Layer:** L1 (in-process unit test; `src/ui.rs`).
- **Agent:** none (a `TabManager`-independent `&[Tab]` fixture plus a synthetic `pane_status` map; the Mode tab's `ModeManager` is backed by a no-op `PaneController` stub that is never called).
- **Asserts:** given one `Tab::Mode` whose agent pane is `Working`, one `Tab::Mode` whose agent pane has no entry in the map (not live yet), one `Tab::Orchestration` with two role panes carrying distinct statuses, and the `Tab::Dashboard`, `tab_status_data` returns positionally: `Some(vec![Working])` for the live Mode tab; `Some(vec![])` (not `None`) for the not-yet-live Mode tab; `Some(<the two role statuses>)` for the Orchestration tab, unchanged from before the extraction; and `None` for the Dashboard. The Mode and Orchestration panes carry deliberately distinct statuses so a wrong lookup key can't accidentally produce a passing result.
- **Does not assert:** the render half that turns this data into a colored label (covered by `tabs/label/002`/`003` and `tabs/orchestration/009`/`010`/`014`, which pass `tab_statuses` as a parameter and so cannot exercise the builder itself).
- **Platform coverage:** mac+linux+windows.

##### tabs/label/002 — An active tab carrying `Some(&[Working])` status data — the shape `tab_status_data` produces for a Mode tab whose agent is Working — renders its label in `palette::STATUS_WORKING`, still cued active via UNDERLINED | BOLD with no REVERSED (fork issue #351 regression guard).
- **Layer:** L1 (in-process `TestBackend` render via `render_tab_bar_to_buffer`, `tests/render_tab_strip.rs`).
- **Agent:** none (synthetic `SessionStatus` values, no panes/PTYs).
- **Asserts:** a tab made the ACTIVE tab and carrying `Some(&[Working])` renders its label foreground in `palette::STATUS_WORKING` (Green), while still carrying `UNDERLINED | BOLD` as its active cue and no `REVERSED`. GREEN from the start — the render half (`render_tab_strip`) already colors any tab whose `tab_statuses` slot is `Some(..)` regardless of tab kind; this guards that half against regression, exercised directly via `render_tab_bar_to_buffer`.
- **Does not assert:** that `run_tui`'s call site actually wires `tab_status_data`'s output through to this renderer — covered by no test (see `tabs/label/001`); the Idle/empty colouring case (covered by `tabs/label/003`).
- **Platform coverage:** mac+linux+windows.

##### tabs/label/003 — A tab carrying `Some(&[Idle])` or `Some(&[])` status data — the shapes `tab_status_data` produces for a Mode tab whose agent is Idle, or whose agent pane isn't live yet — renders `palette::STATUS_IDLE` (fork issue #351, maintainer decision 2026-08-15: colour is a total function of status, including Idle).
- **Layer:** L1 (in-process `TestBackend` render via `render_tab_bar_to_buffer`, `tests/render_tab_strip.rs`).
- **Agent:** none (synthetic `SessionStatus` values, no panes/PTYs).
- **Asserts:** a tab carrying `Some(&[Idle])` and, separately, a tab carrying `Some(&[])` (the empty-slice shape `tab_status_data` yields for a Mode tab whose pane hasn't spawned yet) each render `palette::STATUS_IDLE`. Follows the colour rule pinned in `orchestration_014_active_idle_coloured_underlined`. `palette::highest_priority_status(&[])` resolves the empty slice to Idle too, so it is intended to paint the same as an explicit Idle.
- **Does not assert:** the active-tab Idle colouring (covered by `tabs/orchestration/014`, same underlying render rule); that `run_tui`'s call site actually wires `tab_status_data`'s output through to this renderer — covered by no test (see `tabs/label/001`).
- **Platform coverage:** mac+linux+windows.

#### tabs/dashboard

##### tabs/dashboard/001 — `Ctrl+l` cycles a Dashboard tab's sidebar/pane-column split through the three PRD #361 stages — default 33/67, narrower-sidebar 25/75, and Hidden (sidebar collapsed, 0/100) — looping back to default, and an open Orchestration tab SHARES the ONE deck-global stage with the Dashboard tab in both directions (PRD #387 M1 scopes the chord to command mode; decision 2 makes the stage itself deck-global, inverting this entry's former cross-tab-type isolation).
- **Layer:** L2 (real-binary PTY via the vt100 `TuiDeck` harness).
- **Agent:** none (`orch-deck` fixture; a `with_continue_session` stub `cat` pane for the Dashboard tab plus the fixture's `demo-orch` orchestration, no LLM tokens spent).
- **Asserts:** the deck launches in PaneInput mode on the live Dashboard pane, so a `Ctrl+D` enters Normal (command) mode before any toggle; a Dashboard tab with one live pane then renders the pane column's left edge at the default 33%-width boundary; `Ctrl+l` cycles it through the narrower-sidebar 25%-width position and the Hidden 0%-width (sidebar fully collapsed) position and back to 33%, then narrows it again; opening a SECOND, real Orchestration tab in the same directory (which re-lands the deck in PaneInput mode on its own start-role pane) renders it AT the deck-global Narrow stage the Dashboard tab was just toggled to, NOT its own untoggled 34%-width Default; another `Ctrl+D` plus `Ctrl+l` toggles the Orchestration tab to Hidden, and switching back to the Dashboard tab (Shift+Tab, no extra `Ctrl+D` needed since the deck is already in Normal mode from that toggle) confirms the Dashboard tab is ALSO NOW Hidden — proving the Orchestration tab's toggle moved the Dashboard tab too; cycling the Dashboard tab Hidden -> Default and switching forward to the Orchestration tab (Right -> `CycleTabNext`) confirms IT is ALSO back at its own 34%-width Default, proving sharing in both directions.
- **Does not assert:** persistence of the toggled state across restart; remapping the chord via config; the reverse toggle order (toggling the Dashboard tab first vs. the Orchestration tab first — `tabs/orchestration/006` covers the same-type case, and both entries together exercise the sharing direction from each tab type without duplicating the full matrix).
- **Platform coverage:** mac+linux.

##### tabs/dashboard/002 — Opening a new Orchestration tab (Ctrl+n) while a Dashboard pane is already live and attached switches to and STAYS on the new tab, without resetting the deck-global split stage, including once further input is sent to the new tab (positive coverage for the precondition behind fork issue #224, which investigation showed was a test defect, not a product regression).
- **Layer:** L2 (real-binary PTY via the vt100 `TuiDeck` harness).
- **Agent:** none (`orch-deck` fixture; a `with_continue_session` stub `cat` pane for the Dashboard tab plus the fixture's `demo-orch` orchestration, no LLM tokens spent).
- **Asserts:** positive regression coverage — with a Dashboard pane already live and attached (the distinguishing precondition — `orchestration_006`/`identity_004` open an orchestration from an EMPTY Dashboard and both pass), running the SAME four-press `Ctrl+l` pre-open cycle `tabs/dashboard/001` uses (Default 33/67 -> Narrow 25/75 -> Hidden 0/100 -> Default -> Narrow, ending Narrow) and then opening a new Orchestration tab via `Ctrl+n` renders the new tab's role pane and KEEPS it as the active view — checked with `wait_until_grid_then_hold`, which re-asserts the predicate across a 3s hold window rather than returning on the first grid read that finds it; and the pane-column edge stays at the Narrow 25-column boundary rather than resetting to the Orchestration tab's own untoggled 34-column Default. Round 3: mirrors `dashboard_001`'s post-open `Ctrl+D` + `Ctrl+l` by sending that same input to the freshly-opened tab, then repeats both checks — still the active view, held again across a 3s window, and the split stage now advanced to Hidden (0/100) rather than reset to Default. Fork issue #224's originally reported revert never reproduced across four investigation rounds; the apparent positive, `dashboard_001`, turned out to owe it to two defects of its own (a panicking helper called directly inside a grid-predicate closure, and an inert wait-guard that matched the wrong tab's PTY session name) — both now fixed there. This test stands as coverage for the behaviour itself, independent of that investigation.
- **Does not assert:** the reverse precondition (`tabs/dashboard/001`'s Dashboard-first-then-Orchestration cross-tab-type sharing after both tabs are already open — covered there); `dashboard_001`'s later shared-stage assertions or its Shift+Tab tail (covered there); the reverse toggle order or Mode-tab coverage; any claim that a revert was ever reproduced (it was not — see above).
- **Platform coverage:** mac+linux.

#### tabs/selection

##### tabs/selection/001 — Each tab remembers its own selection by stable id across switch-away/switch-back (PRD #83 M1).
- **Layer:** L1 (in-process unit test; `src/tab.rs`).
- **Agent:** none (mock `PaneController`).
- **Asserts:** stamping a distinct stable id on the Dashboard (`selected_session_id`), a Mode tab (`focused_pane_id`), and an Orchestration tab (`focused_role_pane_id`), then switching through every tab and back, leaves each tab holding its own id unchanged — selection is per-tab, not a single global value.
- **Does not assert:** rendering of the selection; focus restore (covered by `tabs/selection/002`).
- **Platform coverage:** mac+linux+windows.

##### tabs/selection/002 — `switch_to` focus restore + capture round-trips a Mode tab's focused pane (PRD #83 M2).
- **Layer:** L1 (in-process unit test; `src/tab.rs`).
- **Agent:** none (mock `PaneController` records `focus_pane` calls).
- **Asserts:** focusing side pane #2 then switching out captures that pane id into the Mode tab; switching back calls `focus_pane` with the stored id; with the field cleared to `None`, switch-in instead focuses the agent pane.
- **Does not assert:** Dashboard focus restore (keyed by session id, handled in the UI loop, not `TabManager`).
- **Platform coverage:** mac+linux+windows.

##### tabs/selection/003 — Dashboard `selected_index` is derived from `selected_session_id`; the sync is gated to the active tab (PRD #83 M3).
- **Layer:** L1 (in-process unit test; `src/tab.rs`).
- **Agent:** none.
- **Asserts:** `ui::sync_and_derive_selection` resolves a Dashboard `selected_session_id` to its card index, and adopts a focused pane that maps to a visible card; running the same sync against a Mode tab returns `None` and never rewrites the Dashboard's stored id (no cross-tab leak).
- **Does not assert:** the per-frame call site in `run_tui` (exercised by the L1 render test `dashboard/pane/005`).
- **Platform coverage:** mac+linux+windows.

##### tabs/selection/004 — Stale-id fallback clears the field and defaults; reactive-pane recreation remaps focus (PRD #83 M4).
- **Layer:** L1 (in-process unit test; `src/tab.rs`).
- **Agent:** none (mock `PaneController`).
- **Asserts:** a remembered session/role id no longer in the filtered list is cleared and the selection falls back to index 0; `remap_focus_after_reactive_change` follows a `(closed_id, new_id)` pair to the successor pane on BOTH the active tab (returning its new id for re-focus) and a background (non-active) Mode/Orchestration tab, and clears the field on either when a focused pane vanished with no successor.
- **Does not assert:** the controller-level resize that follows a reactive swap.
- **Platform coverage:** mac+linux+windows.

##### tabs/selection/005 — Multi-tab walkthrough: each switch-in restores that tab's own deck/pane (PRD #83 M2/M6).
- **Layer:** L1 (in-process integration test; `src/tab.rs`).
- **Agent:** none (mock `PaneController` records `focus_pane` calls).
- **Asserts:** across a Dashboard, two Mode tabs, and one Orchestration tab, focusing a side pane on each Mode tab and switching between tabs restores each destination tab's own remembered pane (or its default agent / start-role pane) via a `focus_pane` call.
- **Does not assert:** rendering; this drives the `TabManager` capture/restore path directly.
- **Platform coverage:** mac+linux+windows.

#### tabs/spawn

##### tabs/spawn/001 — Creating a single-agent card while an Orchestration tab is active switches the active tab back to the Dashboard with the new card selected and focused (PRD #154).
- **Layer:** L1 (in-process — open a REAL Orchestration tab via `TabManager::open_orchestration_tab`, then dispatch the real `Action::SpawnPane` for a plain single-agent card through `dispatch_action` against a recording `OpenTabPC`; no daemon, no PTY).
- **Agent:** none (mock `PaneController` hands out `mock-pane-N` ids and records `focus_pane` calls).
- **Asserts:** with the orchestration tab active (the non-Dashboard launch precondition), dispatching the no-mode/no-orchestration `SpawnPane` leaves `tab_manager.active_index() == 0` (the Dashboard), sets `ui.selected_index` to the new card's index (`filtered.len()`), and focuses the freshly-created card pane (last `focus_pane` target). A single-agent card belongs to the Dashboard (tab 0), so it must not be stranded on the orchestration tab.
- **Does not assert:** how the highlight is drawn (covered by `dashboard/selection/010`); orchestration/mode tab creation switching to their OWN tab (`open_*_tab` paths, unchanged by PRD #154).
- **Platform coverage:** mac+linux+windows.

##### tabs/spawn/002 — Creating a single-agent card while a Mode tab is active switches the active tab back to the Dashboard with the new card selected and focused (PRD #154).
- **Layer:** L1 (in-process — open a REAL Mode tab via `TabManager::open_mode_tab`, then dispatch the real plain-card `Action::SpawnPane` through `dispatch_action` against a recording `OpenTabPC`; no daemon, no PTY).
- **Agent:** none (mock `PaneController`).
- **Asserts:** with the mode tab active, dispatching the no-mode/no-orchestration `SpawnPane` leaves `tab_manager.active_index() == 0` (the Dashboard), sets `ui.selected_index` to the new card's index, and focuses the new card pane — same "a card always lands on the Dashboard" rule as the orchestration case.
- **Does not assert:** mode-tab geometry / side-pane layout (covered by `tabs/mode/001`); the spawned agent's command behavior.
- **Platform coverage:** mac+linux+windows.

##### tabs/spawn/003 — Creating a single-agent card while already on the Dashboard leaves the Dashboard active with the new card selected and focused (no-regression guard, PRD #154).
- **Layer:** L1 (in-process — dispatch the real plain-card `Action::SpawnPane` through `dispatch_action` against a recording `OpenTabPC` with only the Dashboard tab present).
- **Agent:** none (mock `PaneController`).
- **Asserts:** with the Dashboard already active, dispatching the plain-card `SpawnPane` keeps `tab_manager.active_index() == 0`, sets `ui.selected_index` to the new card's index, and focuses the new card pane. Bounds the `tabs/spawn/001`/`002` switch-to-Dashboard fix so it never moves the active tab off the Dashboard in the common case (Ctrl+N from the Dashboard).
- **Does not assert:** the non-Dashboard launch paths (covered by `tabs/spawn/001`/`002`).
- **Platform coverage:** mac+linux+windows.

##### tabs/spawn/004 — Creating a single-agent card from a Mode tab captures that tab's focused side pane, so it is restored when the user returns to it (PRD #154 follow-up).
- **Layer:** L1 (in-process — open a REAL Mode tab via `TabManager::open_mode_tab`, focus a side pane, dispatch the real plain-card `Action::SpawnPane` through `dispatch_action`, then `switch_to` the Mode tab and `restore_focus_on_switch_in` against a focus-echoing mock; no daemon, no PTY).
- **Agent:** none (mock `PaneController` that, unlike `OpenTabPC`, reports the last `focus_pane` target back through `focused_pane_id()` so the switch-out capture has a live focus to read).
- **Asserts:** after focusing side pane #2 on a Mode tab and creating a single-agent card (which switches to the Dashboard), returning to the Mode tab restores that exact side pane via `focus_pane`. Pins that the plain-card spawn calls `capture_focus_on_switch_out()` before leaving the Mode tab; without it the Mode tab's `focused_pane_id` is never captured and restore falls back to the agent pane (`agent-m`), losing the user's prior focus. (Mode is the genuine regression surface: `sync_and_derive_selection` returns `None` for Mode tabs and never refreshes `focused_pane_id`, unlike the Orchestration branch whose per-frame derive keeps `focused_role_pane_id` fresh regardless of the capture.)
- **Does not assert:** the Orchestration-tab variant (masked by the per-frame `focused_role_pane_id` derive — not a faithful regression surface); the new card's own selection/focus on the Dashboard (covered by `tabs/spawn/002`).
- **Platform coverage:** mac+linux+windows.

### Embedded pane attach

#### embed/attach

##### embed/attach/001 — Starting an agent attaches a live PTY stream to the embedded pane region; its output renders into the parsed grid.
- **Layer:** L2.
- **Agent:** none (fixture stub command writes a fixed banner).
- **Asserts:** the banner string appears in the parsed grid for the agent pane region within a `wait_until_quiescent` window.
- **Does not assert:** byte-level timing of the stream.
- **Platform coverage:** mac+linux.

##### embed/attach/002 — Reattach replays the daemon's per-agent scrollback snapshot.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** after detaching and reattaching, a banner that was emitted before the detach is still in the parsed grid.
- **Does not assert:** the full scrollback length (the snapshot is bounded).
- **Platform coverage:** mac+linux.

##### embed/attach/003 — Mouse scroll forwards to the focused embedded pane when the pane reports mouse-mode support.
- **Layer:** L2.
- **Agent:** none (fixture: a pane that enables mouse tracking and echoes wheel events).
- **Asserts:** the parsed grid shows the wheel-event echo after a simulated scroll.
- **Does not assert:** scroll velocity / acceleration.
- **Platform coverage:** mac+linux.

##### embed/attach/004 — Scrollback navigation (Page Up / Down) does not corrupt the live region.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** after scrolling back and returning to the bottom, the parsed grid still tracks new bytes.
- **Does not assert:** the exact scroll keymap on every platform.
- **Platform coverage:** mac+linux.

##### embed/attach/005 — `AgentRecord.tab_membership` returned by the daemon's `list_agents` is sanitized on hydration; hostile fields (ANSI escapes, NUL bytes, control chars, oversized cwd/role_name) do not corrupt the rebuilt tab bar.
- **Layer:** L2.
- **Agent:** none (fixture forces a daemon to advertise an `AgentRecord` whose `tab_membership` carries `\x1b[31m`, an embedded NUL, and an over-cap role name; harness exposes a helper to override the daemon's outgoing record).
- **Asserts:** after reattach, the rebuilt tab bar contains no raw ANSI / control bytes in any rendered cell; the offending agent either appears under a sanitized label or is bucketed back to the dashboard (per `validate_tab_membership`'s policy).
- **Does not assert:** the exact sanitization output beyond "no raw control bytes survive into the rendered grid" (the pure-data `validate_tab_membership_*` tests pin the per-field policy).
- **Platform coverage:** mac+linux.

#### embed/key-forwarding

##### embed/key-forwarding/001 — Shift+Enter typed into a focused embedded agent pane inserts a NEWLINE into the agent's draft instead of SUBMITTING it (PRD #227).
- **Layer:** L2 PTY-attached (the REAL `dot-agent-deck` binary driven through the vt100 `TuiDeck` harness). Mirrors the `scheduler/dispatch/013` reference harness: imported Claude credentials, project-trust pre-seeded into the per-test HOME (`with_claude_trust_workdir`) so the first-run onboarding/trust gates clear with no keystroke, and `--allowedTools Bash` so no permission prompt can block the pane.
- **Agent:** REAL interactive `claude` pinned to Haiku (`claude --model claude-haiku-4-5-20251001 --allowedTools Bash`, NO `-p`). A stand-in cannot cover this case: `cat` has no draft, so it cannot distinguish "inserted a newline" from "submitted" — which is the entire behavior under test.
- **Asserts:** that the deck pushed the enhanced keyboard protocol at startup (`ESC[>1u` in its output stream), so the forwarding behavior below is measured with M2 actually in effect. Then: with the restored pane auto-focused, typing a first draft line, injecting `ESC[13;2u` (the CSI-u encoding of Shift+Enter a kitty-capable terminal emits) into the DECK's PTY, and typing a second line leaves the draft as TWO lines of ONE input box — the second marker renders on the row IMMEDIATELY BELOW the first, and both rows are bracketed by the prompt editor's own horizontal rules. Adjacency is simultaneously the newline proof and the no-submission proof (a submitted first line would have been repainted into the transcript far above the box before the second line was typed); the rule bracketing is what scopes the two markers to the input box, so a submitted draft the agent repainted into the transcript as two consecutive rows cannot satisfy it vacuously. Independently: the uniquely-named sentinel `shiftnl-7f3c.txt` that the first line's directive would create if submitted does NOT exist in the pane cwd, and after a deliberate plain Enter it DOES appear — a gating positive control, without which the absence could hold for the wrong reason (a slow agent, or one that declined the tool call).
- **Does not assert:** which encoding the user's outer terminal emits for a physical Shift+Enter (the keypress is injected already CSI-u-encoded); the push/pop lifecycle itself, which is `embed/key-forwarding/002`.
- **Cost:** the draft assertions submit nothing (zero LLM tokens); only the positive control spends one short Haiku turn.
- **Platform coverage:** mac+linux (pre-PR e2e tier; flaky-tolerant, run once, not looped).

##### embed/key-forwarding/002 — The deck pushes the enhanced (kitty) keyboard protocol at TUI startup and pops it on clean exit, so Shift+Enter reaches the deck with no user-side terminal configuration and no keyboard mode leaks into the user's shell (PRD #227 M2).
- **Layer:** L2 PTY-attached (the REAL `dot-agent-deck` binary driven through the vt100 `TuiDeck` harness). Asserts on the deck's raw OUTPUT byte stream (`stream_text`) rather than the rendered grid, because the escape sequences under test are consumed by the vt100 parser and never paint a cell.
- **Agent:** none — the behavior is the deck's own terminal negotiation, so this is fully deterministic and spends zero LLM tokens. The harness's `answer_terminal_queries` replies to the `ESC[?u` / `ESC[c` capability probe, which is what makes `supports_keyboard_enhancement()` return true and the gated push fire, modelling the kitty-capable terminal the fix targets.
- **Asserts:** `ESC[>1u` (crossterm's `PushKeyboardEnhancementFlags(DISAMBIGUATE_ESCAPE_CODES)`) appears once the dashboard is up; after a clean exit via Ctrl+C twice, the matching `ESC[<1u` pop appears, after the push, exactly once. The multiplicity check pins the pop's idempotence — both the normal teardown and the RAII guard's `Drop` run on a clean exit, and a second pop would discard a flag set another program on the terminal's stack owns.
- **Does not assert:** the pop on a `?`-error return or a panic unwind from inside the event loop (both need a real terminal whose I/O fails, so the guard mechanism is covered by the L1 `ui::tests::keyboard_enhancement_*` tests instead); that a real terminal honors the pushed mode.
- **Platform coverage:** mac+linux.

### Hook delivery

#### hooks/delivery

##### hooks/delivery/001 — A Claude Code `SessionStart` hook arriving at the daemon's hook socket creates a session entry on the dashboard.
- **Layer:** L2.
- **Agent:** none (write JSON directly to the per-test hook socket).
- **Asserts:** a card appears for the new `session_id`; status is the post-`SessionStart` resting state per the `state` module.
- **Does not assert:** card position in the grid (covered by `dashboard/pane/001`).
- **Platform coverage:** mac+linux.

##### hooks/delivery/002 — A `PreToolUse` hook updates the right session's card by `pane_id`/`session_id` correlation.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** with two synthetic sessions present, only the targeted card transitions to Working.
- **Does not assert:** how `pane_id` is propagated through the env var (a hooks-install concern covered by `hooks/install/*`).
- **Platform coverage:** mac+linux.

##### hooks/delivery/003 — An OpenCode `tool.execute.before` hook updates the right session's card.
- **Layer:** L2.
- **Agent:** none (synthetic OpenCode-format payload).
- **Asserts:** correct OpenCode session transitions to Working with the right tool name.
- **Does not assert:** Claude-vs-OpenCode card visual differentiation.
- **Platform coverage:** mac+linux.

##### hooks/delivery/004 — A malformed hook payload is dropped without disrupting the deck.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** sending invalid JSON to the hook socket leaves all cards and statuses unchanged; the deck does not exit.
- **Does not assert:** error logging content (best-effort logging path).
- **Platform coverage:** mac+linux.

##### hooks/delivery/005 — Hook events survive a TUI detach/reattach cycle (daemon buffers).
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** an event sent while the TUI is detached is reflected in the card status on reattach.
- **Does not assert:** how the daemon buffers (snapshot vs queue).
- **Platform coverage:** mac+linux.

##### hooks/delivery/006 — `DOT_AGENT_DECK_PANE_ID` is scrubbed and re-set per-agent so hooks from agent A never carry agent B's `pane_id`.
- **Layer:** L2.
- **Agent:** none (two synthetic agents started under the same daemon; each invokes the bundled `hook` subcommand and the daemon's env-scrub is what isolates them).
- **Asserts:** with two cards alive, a hook emitted from agent A updates only A's card; a subsequent hook from agent B updates only B's card; neither hook's payload arrives carrying the other agent's `pane_id`.
- **Does not assert:** the absolute env-scrub call sites (covered by `agent_pty` pure-data tests `spawn_scrubs_via_daemon_env_from_child`, `spawn_scrubs_pane_id_env_from_child`, `spawn_opts_env_overrides_pane_id_scrub` — moved to `tmp/legacy-tests/`; this catalog entry replaces that lost end-to-end signal).
- **Platform coverage:** mac+linux.

##### hooks/delivery/007 — A hook event teaches the daemon an agent's type, so `list_agents` reports it on a fresh reconnect instead of "No agent".
- **Layer:** L2.
- **Agent:** none (synthetic — `StartAgent` over the daemon protocol with a shell command whose `from_command` type is `None`, then a JSON `SessionStart` written directly to the per-test hook socket).
- **Asserts:** an agent started with no inferable type registers with `agent_type == None`; after a `SessionStart` hook carrying `agent_type = claude_code` for that pane's id, a subsequent `ListAgents` (the same call `hydrate_from_daemon` issues on reconnect) reports `agent_type == ClaudeCode`.
- **Does not assert:** the rendered card label (the `AgentRecord`→placeholder→render mapping is covered by `rehydration` + L1 dashboard tests); the live-stream upgrade path while a TUI is already attached.
- **Platform coverage:** mac+linux.

#### hooks/install

##### hooks/install/001 — Launching the deck with `~/.claude/` present writes hook entries into `~/.claude/settings.json` idempotently.
- **Layer:** L2.
- **Agent:** none (fixture redirects `HOME`).
- **Asserts:** after first launch, `settings.json` contains the expected hook list; a second launch leaves it byte-identical.
- **Does not assert:** other unrelated keys in `settings.json` (must be preserved verbatim).
- **Platform coverage:** mac+linux.

##### hooks/install/002 — Launching the deck with `~/.opencode/` present writes the JS plugin to `~/.opencode/plugin/dot-agent-deck/index.js`.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** plugin file exists; its content equals the bundled template with `BINARY_PATH` interpolated.
- **Does not assert:** the plugin runs (verified end-to-end by `hooks/delivery/003`).
- **Platform coverage:** mac+linux.

##### hooks/install/003 — Missing agent directories result in a silent skip — no error path.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** launching with neither `~/.claude/` nor `~/.opencode/` does not write any settings file and the TUI starts normally.
- **Does not assert:** the (absence of a) tracing log line.
- **Platform coverage:** mac+linux.

#### hooks/permission

##### hooks/permission/001 — A `PreToolUse`/`ToolStart` for the SAME tool a permission prompt was just approved for clears the WaitingForInput badge (PRD #361 Item 1).
- **Layer:** L1 (in-process `AppState::apply_event` sequence — `SessionStart` → `PermissionRequest` → `ToolStart`, no PTY, no daemon socket).
- **Agent:** none (synthetic ClaudeCode session).
- **Asserts:** after a `PermissionRequest` for `Bash` arms WaitingForInput, a `ToolStart` carrying the SAME tool name (`Bash`) moves the status to `Working` instead of leaving it stuck on WaitingForInput for the tool's whole run.
- **Does not assert:** the TUI-side `y`/`n` keystroke or `Action::SendPermissionResponse` PTY write (covered by `prompt/permission/*`); which status a non-matching-name `ToolStart` produces (`hooks/permission/002`).
- **Platform coverage:** mac+linux+windows.

##### hooks/permission/002 — A `PreToolUse`/`ToolStart` for an UNRELATED tool while a different tool's permission prompt is pending does NOT clear WaitingForInput (PRD #361 Item 1 regression pin).
- **Layer:** L1 (in-process `AppState::apply_event` sequence — `SessionStart` → `PermissionRequest` → `ToolStart` for a different tool name, no PTY, no daemon socket).
- **Agent:** none (synthetic ClaudeCode session).
- **Asserts:** after a `PermissionRequest` for `Bash` arms WaitingForInput, a `ToolStart` carrying a DIFFERENT tool name (`Read`) — modeling a concurrent subagent's own tool starting — leaves the status at WaitingForInput.
- **Does not assert:** the eventual clearing of that pending `Bash` prompt (out of scope for this pin); `hooks/permission/001` covers the matching-name clear.
- **Platform coverage:** mac+linux+windows.

##### hooks/permission/003 — A `PermissionRequest` with no `tool_name` (OpenCode's real `permission.asked` payload shape) does NOT let an unrelated `ToolStart` clear WaitingForInput (Greptile finding on PRD #361 Item 1's marker logic).
- **Layer:** L1 (in-process `AppState::apply_event` sequence — `SessionStart` → `PermissionRequest` with `tool_name: None` → `ToolStart` for an unrelated tool, no PTY, no daemon socket).
- **Agent:** none (synthetic ClaudeCode session; the nameless-`tool_name` shape mirrors OpenCode's real `permission.asked` payload, which `src/opencode_manage.rs`'s `permissionPayload` never populates with a `tool_name` field).
- **Asserts:** a `PermissionRequest` carrying `tool_name: None` still arms WaitingForInput, and a subsequent `ToolStart` for an unrelated tool (`Read`) leaves the status at WaitingForInput rather than being treated as a plain notification-wait clear — `pending_permission_tool = None` from a nameless prompt must not be confused with "no permission pending at all".
- **Does not assert:** which status a `ToolStart` produces once the SAME nameless-prompt's approval genuinely lands (no tool name is available to match against, so this case is inherently a "leave it pending" one, not a "clear on match" one).
- **Platform coverage:** mac+linux+windows.

### Pane / agent lifecycle

#### lifecycle/start

##### lifecycle/start/001 — Starting an agent via the new-pane form creates one card and one PTY in the daemon registry.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** the daemon's `list_agents` returns one entry whose `pane_id_env` matches what the TUI assigned.
- **Does not assert:** PTY size at spawn (covered by `resize/sigwinch/*`).
- **Platform coverage:** mac+linux.

##### lifecycle/start/002 — An invalid command field shows an inline form error and does not spawn an agent.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** the form gains an error message; no new agent appears in `list_agents`.
- **Does not assert:** the error message wording (loose substring match).
- **Platform coverage:** mac+linux.

#### lifecycle/stop

##### lifecycle/stop/001 — `Ctrl+w` on a focused dashboard card stops the agent and removes the card.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** daemon-side `list_agents` shrinks; the card disappears.
- **Does not assert:** filesystem cleanup of the agent's scratch dir.
- **Platform coverage:** mac+linux.

##### lifecycle/stop/002 — `dot-agent-deck daemon stop` with managed agents alive exits non-zero without killing them (data-loss guard).
- **Layer:** L2.
- **Agent:** none (the harness runs the `daemon stop` subcommand).
- **Asserts:** subprocess exits non-zero; the daemon and managed agents are still alive afterwards.
- **Does not assert:** stderr content beyond mentioning `--force`.
- **Platform coverage:** mac+linux.

##### lifecycle/stop/003 — `daemon stop --force` kills the daemon and any managed agents, then exits zero.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** the daemon socket disappears within the grace window; managed agents are reaped.
- **Does not assert:** SIGTERM-vs-SIGKILL escalation timing (covered indirectly by the lib's terminate tests now living in `tmp/legacy-tests/`).
- **Platform coverage:** mac+linux.

##### lifecycle/stop/004 — `daemon stop` with no daemon running is idempotent (exit 0).
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** subprocess exits 0; no daemon spawned by the call.
- **Does not assert:** stdout content (loose contains-check).
- **Platform coverage:** mac+linux.

##### lifecycle/stop/005 — Closing an already-stopped daemon agent completes local teardown.
- **Layer:** L1 (real `EmbeddedPaneController` against a synthetic Unix-socket daemon).
- **Agent:** none (synthetic StartAgent / AttachStream; both StopAgent attempts return exact `Agent agent-1 not found`, and ListAgents reports the stable pane slot empty).
- **Asserts:** `close_pane` performs both stale-id attempts, enters the real ListAgents slot-resolution path, returns success for the proven-empty slot, removes the pane, does not re-insert the ghost card, and emits no unverified-close warning.
- **Does not assert:** the dashboard confirmation UI (`prompt/close-confirm/*`); daemon process termination (the synthetic daemon reports the agent already absent).
- **Platform coverage:** mac+linux.

##### lifecycle/stop/006 — A genuine StopAgent failure still retains the pane and surfaces the error.
- **Layer:** L1 (real `EmbeddedPaneController` against a synthetic Unix-socket daemon).
- **Agent:** none (synthetic StartAgent / AttachStream; StopAgent returns a non-NotFound server error).
- **Asserts:** `close_pane` returns the daemon error, re-inserts the pane for retry, and does not apply the NotFound-only retry/classification to other failures.
- **Does not assert:** the timeout arm (the existing retain-and-surface implementation remains unchanged); dashboard status-message layout.
- **Platform coverage:** mac+linux.

##### lifecycle/stop/007 — Unrelated errors containing `not found` retain the live pane and surface the error.
- **Layer:** L1 (real `EmbeddedPaneController` against a synthetic Unix-socket daemon).
- **Agent:** none (synthetic StartAgent / AttachStream; StopAgent returns `pane not found`, `session not found`, another agent's exact NotFound, or a wrapped requested-agent NotFound).
- **Asserts:** every non-exact/non-id-scoped message returns an error containing the daemon reason, re-inserts the pane, sends only one StopAgent request, and never enters ListAgents slot resolution.
- **Does not assert:** presentation of the surfaced message in the TUI status row.
- **Platform coverage:** mac+linux.

##### lifecycle/stop/008 — A replacement agent occupying the pane slot is stopped before local teardown.
- **Layer:** L1 (real `EmbeddedPaneController` against a synthetic Unix-socket daemon).
- **Agent:** none (both stale `agent-1` StopAgent attempts return exact NotFound; ListAgents reports `agent-2` with the same `pane_id_env`; stopping `agent-2` succeeds).
- **Asserts:** the request sequence is `agent-1`, `agent-1`, `agent-2`; replacement discovery uses ListAgents; only then does `close_pane` succeed and remove the pane; no unverified-close warning is emitted.
- **Does not assert:** the asynchronous real-agent respawn mechanism that creates the replacement.
- **Platform coverage:** mac+linux.

##### lifecycle/stop/009 — A replacement appearing near the respawn worst case is stopped before local teardown.
- **Layer:** L1 (real `EmbeddedPaneController` against a timing-controlled synthetic Unix-socket daemon).
- **Agent:** none (the initial AttachStream ends to put pane I/O into reattachment; both stale-id StopAgent attempts return exact NotFound; ListAgents reports the stable slot empty for 4.8 seconds before exposing `agent-2`).
- **Asserts:** close keeps polling through the documented slow-respawn window, sends StopAgent to the late replacement, removes the pane only after that stop succeeds, and emits no unverified-close warning.
- **Does not assert:** a real agent process's SIGTERM/startup timing; the synthetic delay deterministically represents that handover gap.
- **Platform coverage:** mac+linux.

##### lifecycle/stop/010 — A ListAgents error completes close with one unattended-agent warning.
- **Layer:** L1 (real `EmbeddedPaneController` against a synthetic Unix-socket daemon).
- **Agent:** none (both StopAgent attempts return exact NotFound; ListAgents returns `registry unavailable`).
- **Asserts:** close returns success and removes the pane instead of restoring the ghost card; exactly one drainable warning says the pane was closed, daemon verification failed, and an agent may still be running unattended.
- **Does not assert:** rendering the queued warning on the TUI status line.
- **Platform coverage:** mac+linux.

##### lifecycle/stop/011 — A ListAgents timeout completes close with one unattended-agent warning.
- **Layer:** L1 (real `EmbeddedPaneController` against a synthetic Unix-socket daemon).
- **Agent:** none (both StopAgent attempts return exact NotFound; ListAgents accepts the request but never replies).
- **Asserts:** close returns success after the bounded lookup timeout and removes the pane instead of restoring the ghost card; exactly one drainable warning says the pane was closed, verification timed out, and an agent may still be running unattended.
- **Does not assert:** rendering the queued warning on the TUI status line.
- **Platform coverage:** mac+linux.

##### lifecycle/stop/012 — A chained pane-slot handover stops the last owner before teardown.
- **Layer:** L1 (real `EmbeddedPaneController` against a stateful synthetic Unix-socket daemon).
- **Agent:** none (both stale `agent-1` StopAgent attempts return exact NotFound; ListAgents reports replacement B; stopping B returns exact NotFound after replacement C takes the slot; stopping C succeeds).
- **Asserts:** close sends StopAgent to C, returns success, removes the pane only after the final owner is stopped, and emits no unverified-close warning.
- **Does not assert:** an exact number of stop requests; the guard pins the last owner being stopped so alternative depth-handling implementations remain valid.
- **Platform coverage:** mac+linux.

##### lifecycle/stop/013 — Immediate unresolvable pane-slot churn is round-bounded and announced.
- **Layer:** L1 (real `EmbeddedPaneController` against a stateful synthetic Unix-socket daemon, with a 13-second test-side hang ceiling).
- **Agent:** none (every replacement StopAgent returns exact NotFound after handing the stable pane slot to a fresh synthetic agent).
- **Asserts:** immediate churn returns well before the total budget through the three-replacement round cap, removes the pane, and queues exactly one drainable warning saying the slot kept changing owners, the close could not be verified, and an agent may still be running unattended.
- **Does not assert:** rendering the queued warning on the TUI status line; the wall-clock budget path (covered by `lifecycle/stop/014`).
- **Platform coverage:** mac+linux.

##### lifecycle/stop/014 — Slow unresolvable pane-slot churn is wall-clock-bounded and announced.
- **Layer:** L1 (real `EmbeddedPaneController` against a stateful timing-controlled synthetic Unix-socket daemon, with a 13-second test-side hang ceiling).
- **Agent:** none (each replacement StopAgent takes four seconds before returning exact NotFound and handing the stable pane slot to another synthetic agent).
- **Asserts:** the total budget ends resolution after two delayed replacement stops and before the three-round cap, close returns success and removes the pane, and exactly one drainable slot-churn/unattended-agent warning is queued.
- **Does not assert:** rendering the queued warning on the TUI status line; real process stop latency.
- **Platform coverage:** mac+linux.

##### lifecycle/stop/015 — A genuine replacement-agent stop failure retains the pane and surfaces the error.
- **Layer:** L1 (real `EmbeddedPaneController` against a stateful synthetic Unix-socket daemon).
- **Agent:** none (the stale original agent returns exact NotFound, ListAgents reports replacement B, and B's StopAgent returns a permission-denied server error).
- **Asserts:** close reaches B, surfaces its daemon error, retains the pane for retry, and emits no unverified-close warning instead of absorbing the failure into slot churn.
- **Does not assert:** presentation of the surfaced error in the TUI status row; the replacement timeout arm (covered by `lifecycle/stop/016`).
- **Platform coverage:** mac+linux.

##### lifecycle/stop/016 — A replacement-agent stop timeout retains the pane and surfaces the timeout.
- **Layer:** L1 (real `EmbeddedPaneController` against a stateful synthetic Unix-socket daemon, with a seven-second test-side hang ceiling).
- **Agent:** none (the stale original agent returns exact NotFound, ListAgents reports replacement B, and B's StopAgent never replies).
- **Asserts:** close reaches B, exercises the real five-second stop timeout, surfaces the timeout, retains the pane for retry, and emits no unverified-close warning instead of absorbing the timeout into slot churn.
- **Does not assert:** presentation of the surfaced error in the TUI status row; OS-level process termination.
- **Platform coverage:** mac+linux.

##### lifecycle/stop/017 — A partially failed tab close stays visible and succeeds on retry.
- **Layer:** L2 (real-binary PTY against a protocol-faithful scripted daemon).
- **Agent:** none (a hydrated Mode tab with one agent pane and one persistent side pane; the side pane's first StopAgent is denied and its retry succeeds).
- **Asserts:** the first confirmed whole-tab close removes the successful pane, retains the failed pane and its tab/`×`, and renders that the tab was kept; after switching into the retained tab, a second confirmed close removes the failed pane, daemon record, and tab.
- **Does not assert:** an exact count of `close_pane` calls; the observable retry outcome is the contract.
- **Platform coverage:** mac+linux.

##### lifecycle/stop/018 — Already-gone and unverified-success panes do not block whole-tab removal.
- **Layer:** L2 (real-binary PTY against a protocol-faithful scripted daemon).
- **Agent:** none (two hydrated one-pane Mode tabs: exact id-scoped NotFound with an empty slot, then exact NotFound whose ListAgents verification fails).
- **Asserts:** both outcomes remove the tab; the proven-gone close renders no unattended-agent warning, while DoneUnverified renders exactly one such warning on the live status line.
- **Does not assert:** warning expiry timing or terminal styling.
- **Platform coverage:** mac+linux.

##### lifecycle/stop/019 — A six-pane tab closes concurrently while preserving pane order in its outcome.
- **Layer:** L1 (in-process `TabManager` with a delay-scripted `PaneController`).
- **Agent:** none (six hydrated orchestration role panes with staggered 150–400 ms synthetic close delays).
- **Asserts:** the close completes below a 1.0-second wall-clock ceiling versus a 1.65-second sequential sum, reports closed pane ids in original role order rather than completion order, and removes the clean tab.
- **Does not assert:** production daemon/RPC latency; the synthetic delays isolate fan-out semantics.
- **Platform coverage:** mac+linux.

#### lifecycle/shutdown-graceful

##### lifecycle/shutdown-graceful/001 — `AgentPtyRegistry::shutdown_all_graceful`'s phase 2 observes an agent's exit without reaping it, and phase 3 signals every agent's group unconditionally, then reaps (fork #163 reworked: the shipped fix — tracking which agents phase 2 reaped and skipping exactly those in phase 3 — was issue #163's option 1, which the issue explicitly warned should not be chosen on cost grounds; PR #207's review and audit independently converged that it forfeits fork #133's descendant kill in precisely the case phase 3 was doing real work, and shipped unconditionally across platforms although its safety justification is Unix-only. The replacement keeps the pgid reserved for the whole grace window instead of skipping phase 3: a non-reaping peek, `libc::waitid(P_PID, pid, WEXITED | WNOHANG | WNOWAIT)` on Unix).
- **Layer:** L1 (pure — three registries seeded with real, `setsid`'d OS processes spawned via `crate::platform::proc::spawn_in_new_process_group` — the same helper `orchestration/worktree/008-010`/`013` use — wrapped in a logging `portable_pty::Child` adapter over a real `std::process::Child`; no daemon, no PTY-attached child. A real pid is required here (unlike the sibling `orchestration/worktree/013`): the peek this contract requires is a raw `libc::waitid` call on the bare pid, entirely outside the `Child` trait, so a fake with no pid has nothing for it to act on).
- **Agent:** none.
- **Asserts:** a promptly-exiting agent (`true`) produces a call log of exactly `["wait"]` — no `try_wait` (the reaping call must never be used) and phase 3 still reaches and reaps it (`wait` present, which is exactly what the rejected `be1fde4` design would *not* have produced, since that design's phase 2 already reaped it and phase 3 skipped it) — and the real process is confirmed gone; the same all-exited registry does not burn the full grace window (early break still load-bearing); a control agent that ignores SIGTERM — a shell that installs `trap '' TERM`, then writes a readiness marker, then `exec`s into `sleep 300`, with the test blocking on that marker before starting the timer or calling `shutdown_all_graceful` (closing a setup race where phase 1's SIGTERM could otherwise reach `/bin/sh` before the `trap` builtin, killing the stand-in for real and silently making this case not test SIGTERM-resistance at all) — is confirmed to burn the full grace window (`elapsed >= grace`, the lower-bound half of the peek's `Ok(false)` contract: a peek that wrongly reports it as exited collapses `elapsed` and fails this deterministically) and can only be reaped by phase 3's SIGKILL, producing the same `["wait"]` log, with its own death confirmed via `kill(pid, 0)` rather than trusting `wait()` returning at all (a broken SIGKILL would hang the call instead of failing cleanly) — proves the rework discriminates on real exit rather than silencing phase 3 universally, which would reintroduce fork #133's gap; a third, shared two-agent registry (one of each) confirms both agents are independently signalled, reaped, and gone regardless of `HashMap` iteration order.
- **Does not assert:** the real pid-recycling race itself (not deterministically forceable — the kernel recycling a freed pid within the grace window needs pid-space wraparound); confirmed SIGTERM-then-SIGKILL signal *identity* for the promptly-exiting agent (it is already dead before either signal is sent, so both are no-ops against it — the control agent's forced reliance on SIGKILL is what stands in for signal identity here, same reasoning as `orchestration/worktree/010` needing a SIGTERM-resistant descendant to distinguish the two signals); descendant/grandchild coverage (fork #133's own mechanism is `orchestration/worktree/009`/`010`'s remit — this test is about phase 2/3 ordering for the direct child, not process-group membership). CORRECTED CONTRACT: this entry's prose, last written at `73b6b09`, went stale behind two later commits on the same branch — `6d3829c` added the `elapsed >= grace` lower-bound assertion without this entry recording it, and `4cadb1c` added the trap-readiness handshake (the marker-wait described above) without this entry describing the control command as it now is; the entry also still contrasted against `be1fde4` as "today's shipped fix" after this PR replaced it with the rework described in the id's own title. All three are corrected above (PR #207 review r2, finding P2).
- **Platform coverage:** mac+linux (`#[cfg(unix)]` — real POSIX `setsid`/signal semantics; the Windows equivalent needs no peek at all per the PR's contract and is covered separately).

#### lifecycle/restart

##### lifecycle/restart/001 — `daemon restart` reuses the next-launch lazy-spawn — a subsequent `dot-agent-deck` launch comes up against a fresh daemon process.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** the daemon PID before and after a restart cycle differ; the deck still attaches.
- **Does not assert:** any timing characteristics of the restart.
- **Platform coverage:** mac+linux.

#### lifecycle/daemon-idle

##### lifecycle/daemon-idle/001 — The daemon exits after the idle window elapses with no TUI and no managed agents.
- **Layer:** L2.
- **Agent:** none (tunable idle window via `DOT_AGENT_DECK_IDLE_SHUTDOWN_SECS`).
- **Asserts:** the daemon socket disappears within the window plus a small jitter budget.
- **Does not assert:** behavior with the env var set to `0` (covered by `lifecycle/daemon-idle/002`).
- **Platform coverage:** mac+linux.

##### lifecycle/daemon-idle/002 — Setting `DOT_AGENT_DECK_IDLE_SHUTDOWN_SECS=0` disables the idle shutdown.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** after a window comfortably longer than the default, the daemon still answers.
- **Does not assert:** indefinite lifetime (capped by the test timeout).
- **Platform coverage:** mac+linux.

##### lifecycle/daemon-idle/003 — A registered enabled schedule keeps the daemon alive past the idle window (PRD #127 M1.4 carve-out); removing it lets the daemon idle-exit.
- **Layer:** L2.
- **Agent:** none (a global `schedules.toml` with one enabled task; fast `DOT_AGENT_DECK_IDLE_SHUTDOWN_SECS`).
- **Asserts:** with zero clients and zero live agents the daemon survives well past the idle window while an enabled schedule is registered (covers the before-first-fire and after-agent-exit gaps); after the schedule is cleared and reloaded the daemon exits within the window plus margin.
- **Does not assert:** any fire behavior of the schedule, nor reuse-tab semantics.
- **Platform coverage:** mac+linux.

#### lifecycle/orphan-exit

##### lifecycle/orphan-exit/001 — An idle-disabled daemon with `DOT_AGENT_DECK_EXIT_WHEN_ORPHANED=1` self-exits gracefully once its parent dies (orphaned to init), instead of leaking to PID 1.
- **Layer:** L2.
- **Agent:** none (the daemon runs under a short-lived intermediate `sh` parent the test can kill without killing itself).
- **Asserts:** after SIGKILLing the intermediate parent, the daemon process terminates within a few seconds, even though idle shutdown is disabled so only the orphan watchdog can end it.
- **Does not assert:** the max-lifetime backstop (`DOT_AGENT_DECK_TEST_MAX_LIFETIME_SECS`, covered by the daemon pure-data unit tests) or production daemons (the watchdog is OFF unless the env var is set).
- **Platform coverage:** mac+linux.

#### lifecycle/sigterm

##### lifecycle/sigterm/001 — A daemon sent SIGTERM (what `daemon stop` / `daemon restart` deliver) exits through its graceful shutdown path AND logs the signal, instead of dying silently under the default disposition.
- **Layer:** L2.
- **Agent:** none (a bare `daemon serve` with idle shutdown disabled, so only the signal handler can end it).
- **Asserts:** after a plain `kill(pid, SIGTERM)` the daemon process terminates within a few seconds, and its `DOT_AGENT_DECK_LOG` file contains a termination line naming `SIGTERM`.
- **Does not assert:** agent teardown ordering under signal shutdown, or `SIGINT` (the handler treats both identically and the CLI only ever sends `SIGTERM`); `--force`'s SIGKILL escalation stays with `lifecycle/stop/003`.
- **Regression origin:** the daemon installed no signal handler at all, so a stopped daemon left no log line — a real session lost seven live agent panes and the daemon's own log said nothing about why.
- **Platform coverage:** linux+mac (Unix signals; the Windows build watches Ctrl-C instead).

##### lifecycle/sigterm/002 — A second SIGTERM during shutdown forces an immediate exit instead of being swallowed, so a wedged daemon is still killable with `pkill`.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** after the daemon logs the first termination signal, a second SIGTERM leaves the process gone within a few seconds.
- **Does not assert:** the exact exit status (`143`), since a shutdown fast enough to finish before the second signal exits `0` and both outcomes satisfy "the daemon does not linger".
- **Regression origin:** installing a handler replaces the default disposition process-wide, so once the first signal is consumed every later SIGTERM would be absorbed by a stream nobody reads — removing the `pkill` escape hatch that the pre-handler behaviour always provided.
- **Platform coverage:** linux+mac (Unix signals).

#### lifecycle/version

##### lifecycle/version/001 — A build environment that pre-sets `DAD_VERSION` / `DAD_BUILD_ID` produces a binary that reports those values, and changing either one invalidates the cached build (issue #250).
- **Layer:** L2 (three real `cargo build`s into one shared scratch `CARGO_TARGET_DIR`, pinned to the rustc host target and capped at half the machine's cores, then plain subprocess runs of each produced binary — no PTY).
- **Agent:** none.
- **Asserts:** with `DAD_VERSION=42.7.13` / `DAD_BUILD_ID=42.7.13-ginjected0` pre-set only in the *build* environment, the produced binary's `--version` reports `42.7.13` (not the `0.1.0` `CARGO_PKG_VERSION` placeholder, and not the checkout's git tag) and `daemon hello` advertises both injected values as `daemon_version` / `build_version`; then that changing **only** `DAD_VERSION` (to `58.1.2`) and afterwards **only** `DAD_BUILD_ID` (to `58.1.2-ginjected1`) is each picked up by the next build in the same target dir — the one-at-a-time change is what pins each `cargo:rerun-if-env-changed` directive individually.
- **Does not assert:** the full fallback order *below* an injection — an absent or invalid `DAD_VERSION` falling through to git and then to the `CARGO_PKG_VERSION` placeholder — nor the build-script directive-injection rejection (both are pure-data unit tests in `tests/build_version.rs`); the `cargo:warning` text on the placeholder path; that a git-less checkout degrades correctly (would need a second cold build).
- **Platform coverage:** mac+linux.

#### lifecycle/handshake

##### lifecycle/handshake/001 — Build-version match on attach proceeds silently into the dashboard.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** no mismatch prompt is rendered; the dashboard appears.
- **Does not assert:** any tracing log line.
- **Platform coverage:** mac+linux.

##### lifecycle/handshake/002 — Build-version mismatch with NO running agents restarts the daemon silently and proceeds into the dashboard (PRD #161 Part A).
- **Layer:** L2.
- **Agent:** none (an older external daemon at `DOT_AGENT_DECK_BUILD_ID_OVERRIDE` is reused by a newer TUI to simulate skew).
- **Asserts:** with no agents running, no prompt is shown and no keypress is sent — the dashboard's empty state (`No active sessions`) appears, and the original (older) daemon process exits (the silent restart terminated it; a fresh daemon was lazy-spawned at the new build).
- **Does not assert:** the new daemon's exact build id (covered by the protocol round-trip tests).
- **Platform coverage:** mac+linux.

##### lifecycle/handshake/003 — Build-version mismatch with live agents in a TTY renders a consent prompt that names the live agents and states restarting stops them (PRD #161 Part A / M1.1).
- **Layer:** L2.
- **Agent:** one synthetic `sleep`-style agent with a distinctive display name, started over the daemon's attach socket before the TUI attaches.
- **Asserts:** the rendered prompt surfaces the live agent's **display name** (from the handshake reply's `running_agents.names`) together with the stop/restart intent.
- **Does not assert:** exact prompt wording (loose substring match on the agent name + stop/restart intent); the agent's generated id.
- **Platform coverage:** mac+linux.

##### lifecycle/handshake/004 — Build-version mismatch with live agents on a non-TTY (mandatory-restart path) exits non-zero with a stderr recovery hint and no prompt (PRD #161 Part A).
- **Layer:** L2.
- **Agent:** one synthetic `sleep`-style agent (the binary is run directly with stdout redirected to a pipe, so `is_terminal()` is false).
- **Asserts:** exit code is non-zero; stderr carries a clear daemon recovery hint (mentions the daemon and stop/restart) and no prompt is rendered.
- **Does not assert:** exact stderr wording (pinned in lib pure-data tests); the no-agents non-TTY path (which silently restarts).
- **Platform coverage:** mac+linux.

##### lifecycle/handshake/005 — Build-version mismatch with live agents in a TTY: a single consent keystroke restarts the daemon (agents stopped) and the dashboard appears (PRD #161 Part A — replaces #103's two-`S` double-confirm).
- **Layer:** L2.
- **Agent:** one synthetic `sleep`-style agent.
- **Asserts:** after the prompt appears, a single `s` consent restarts the daemon — the original daemon process exits and the fresh (now empty) dashboard's `No active sessions` appears.
- **Does not assert:** exact prompt wording; the recovered daemon's build id.
- **Platform coverage:** mac+linux.

##### lifecycle/handshake/006 — Build-version mismatch with live agents in a TTY: declining keeps the EXISTING daemon and lands in a working dashboard with the agents still reachable (PRD #161 D4 never-strand).
- **Layer:** L2.
- **Agent:** one synthetic `sleep`-style agent with a distinctive display name.
- **Asserts:** after the prompt appears, pressing `Esc` does NOT exit — a working dashboard appears against the still-running older daemon (the session is listed), the original daemon process is still alive, and the live agent remains reachable on it (never-strand). This is the key change from #103, where declining exited.
- **Does not assert:** the other decline keystrokes individually (`q` / `Ctrl+C` / `Ctrl+D` — covered by the same decline path); exact prompt wording.
- **Platform coverage:** mac+linux.

##### lifecycle/handshake/007 — Build-version mismatch with a live agent where the daemon OMITS `running_agents` (a pre-#161 daemon predating M1.1): the handshake falls back to `list_agents()` and shows the consent prompt instead of silently restarting over the unseen agent (PRD #161 FIX 1 / D2 / D4 never-strand).
- **Layer:** L2.
- **Agent:** one synthetic `sleep`-style agent, started over the daemon's attach socket; the daemon runs with `DOT_AGENT_DECK_TEST_OMIT_RUNNING_AGENTS` so its `Hello` reply leaves `running_agents = None`, simulating a daemon that predates the M1.1 summary field.
- **Asserts:** the agents-PRESENT consent prompt appears (the TUI did NOT silently restart into the dashboard) — proving the handshake fell back to `list_agents()` rather than treating the absent field as "no agents" and SIGTERM'ing the live agent unseen; then pressing `Esc` declines and a working dashboard appears against the still-running old daemon with the agent still reachable (never-strand).
- **Does not assert:** that the prompt names the agent by its *display* name specifically (loose match — with `running_agents` omitted the label comes from `list_agents()`, so the display name OR a non-zero "(N agent(s) running)" header is accepted); exact prompt wording.
- **Platform coverage:** mac+linux.

##### lifecycle/handshake/008 — The local (same-machine) daemon↔TUI attach refuses cleanly when the daemon's `server_version` does not match the TUI's compiled-in `PROTOCOL_VERSION`, instead of proceeding into a dashboard that silently drops every event it cannot decode (fork issue #17).
- **Layer:** L1 client/wire integration — the real production `build_version_handshake::ensure_compatible_daemon_or_die` driven against a mock daemon over a real Unix socket.
- **Agent:** none (a mock daemon answering exactly one `Hello`; no PTY, no LLM).
- **Asserts:** with the mock daemon's `Hello` reply carrying the SAME `build_version` the test process reports for itself (so today's build-id comparison matches) but a `server_version` one higher than the compiled-in `PROTOCOL_VERSION` — mirroring the issue's verified newer-daemon/older-TUI direction — `ensure_compatible_daemon_or_die` must return an `Err` rather than `Ok(HandshakeOutcome::Match)`. RED today: the local-attach handshake compares only `build_version`; `server_version` is never inspected, so the call returns `Ok(Match)` regardless of protocol skew (unlike `connect::probe_remote_protocol`, which refuses the equivalent remote pairing with `ProtocolMismatch`).
- **Does not assert:** the interactive-TTY `ProceedOnExisting` decline path from a build-id mismatch (`lifecycle/handshake/006`) — a real keypress cannot be driven headlessly, and this test isolates the missing protocol check via the build-id-matching fast path instead, which reaches the same unchecked `server_version` field; the exact refusal error type or user-facing message (not yet designed); the remote/SSH path (already covered by `connect::probe_remote_protocol`'s own tests).
- **Platform coverage:** mac+linux.

#### lifecycle/login-path

##### lifecycle/login-path/001 — A dashboard new-pane whose command is a bare binary living only in the user's login-shell PATH spawns successfully when the daemon was launched without that dir on PATH (PRD #170 M1.3).
- **Layer:** L2 (real `dot-agent-deck` binary in a PTY; the deck lazy-spawns its daemon, which inherits the deck's env).
- **Agent:** none (a synthetic stub binary placed only in a temp dir that is NOT on the inherited PATH; the deck's `$SHELL` is a fake login shell whose `-lc` output adds that dir to PATH, mirroring how `~/.profile` adds `~/.local/bin`). `default_command` is set to the bare stub so the new-pane form pre-fills it.
- **Asserts:** opening the new-pane form (Ctrl+n → confirm dir → Submit) with the bare stub as the command spawns it successfully — the stub writes an on-disk marker that appears within the wait window. RED today: nothing captures the login-shell PATH, so the daemon's PATH lacks the stub dir, the bare command is not found, the spawn fails, and the marker never appears.
- **Does not assert:** the exact spawn-failure error text in the pane; the non-PATH login environment (out of scope per PRD #170).
- **Platform coverage:** mac+linux.

##### lifecycle/login-path/002 — A scheduled-task fire whose command is a bare binary living only in the user's login-shell PATH spawns successfully when the daemon was launched without that dir on PATH (PRD #170 M1.3).
- **Layer:** L2 (headless `dot-agent-deck daemon serve` driven via the `RunNow` control message — no PTY/grid, same shape as `scheduler/spawn/*`).
- **Agent:** none (a synthetic stub binary placed only in a temp dir absent from the daemon's PATH; the daemon's `$SHELL` is a fake login shell whose `-lc` output adds that dir to PATH). The scheduled task's `command` is the bare stub.
- **Asserts:** firing the task via `RunNow` spawns the bare stub successfully — the stub writes an on-disk marker that appears within the wait window. RED today: with no login-shell PATH capture the daemon's PATH lacks the stub dir, the bare command is not found, and the marker never appears.
- **Does not assert:** prompt delivery to the spawned agent (covered by `scheduler/spawn/004`); the orchestration-vs-card branch (covered by `scheduler/spawn/002`).
- **Platform coverage:** mac+linux.

##### lifecycle/login-path/003 — The schedule-authoring helper's bare authoring command (living only in the user's login-shell PATH) resolves and spawns when the daemon was launched without that dir on PATH (PRD #170 M1.3 + M2.1, the originally-motivating bug path).
- **Layer:** L2 (real `dot-agent-deck` binary in a PTY; the deck lazy-spawns its daemon, which inherits the deck's env). Reuses the `login_path_fixture` mechanics (stripped PATH + fake login shell) from `lifecycle/login-path/001`/`002` and the unified dir-picker + mode-locked form Edit flow from `scheduler/manager/002`.
- **Agent:** none (a synthetic stub binary placed only in a temp dir absent from the inherited PATH; the deck's `$SHELL` is a fake login shell whose `-lc` output adds that dir to PATH). `default_command` is the bare stub, so the mode-locked form's pre-filled Command defaults to it. A fixture `schedules.toml` supplies one task to edit (its own `cat` run command is irrelevant — the authoring command comes from `default_command`).
- **Asserts:** opening the Scheduled-Tasks manager (`S`), pressing `e` to edit the auto-selected row opens the directory picker (` Select Directory `); confirming the dir with Space opens the mode-locked ` Edit Schedule ` form (Command pre-filled with the bare authoring command); submitting via `[Submit]` spawns it through the daemon spawn primitive, and the bare command resolves under the daemon's login-shell-enriched PATH — the stub writes an on-disk marker that appears within the wait window. GREEN once M1.3 + M2.1 + the unified flow are merged: pins PRD #170's third spawn path (the schedule-authoring helper), which routes through the same daemon spawn primitive as `001`/`002` plus the configurable-command change of `scheduler/manager/002`.
- **Does not assert:** the authoring seed/prompt delivery to the spawned agent (covered by `scheduler/manager/002`); the dir-picker/form interaction details (covered by `scheduler/form/001`–`003`); the non-PATH login environment (out of scope per PRD #170).
- **Platform coverage:** mac+linux.

### Resize

#### resize/sigwinch

##### resize/sigwinch/001 — Resizing the outer terminal mid-run propagates a SIGWINCH and the dashboard re-renders to the new dimensions.
- **Layer:** L2.
- **Agent:** none (Decision 20 requires at least one catalog test here).
- **Asserts:** after `deck.resize(80, 24)`, the rendered grid is 80 columns wide; cards reflow accordingly.
- **Does not assert:** font-related metrics.
- **Platform coverage:** mac+linux.

##### resize/sigwinch/002 — Resize of the outer terminal also resizes every managed agent PTY.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** the daemon reports each agent's PTY at the new size; agent processes that print `tput cols` see the new column count.
- **Does not assert:** any visual reflow inside the agent (subprocess-dependent).
- **Platform coverage:** mac+linux.

##### resize/sigwinch/003 — Resize coalescing — a rapid sequence of resize events results in one final reflow, not N.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** observed reflow count under a burst of resize events is bounded; final size matches the last input.
- **Does not assert:** the exact debounce window (a harness constant).
- **Platform coverage:** mac+linux.

#### resize/layout

##### resize/layout/001 — `Ctrl+t` toggles stacked / tiled dashboard layout without dropping any agents.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** after toggling, all cards are still present; the layout differs across snapshots.
- **Does not assert:** which layout is the "default" (already a settled product call).
- **Platform coverage:** mac+linux.

#### resize/render

##### resize/render/001 — Enlarging the outer terminal fills the new width across an embedded pane — no empty band on the right edge.
- **Layer:** L2.
- **Agent:** none (a long-lived `sleep` pane gives a focusable embedded PTY without LLM credentials).
- **Asserts:** with an embedded pane present, after `deck.resize(W+10, H)` and the deck quiescent, the rendered frame spans the full new width and the pane's bordered region reaches the new right edge — no unfilled column band between the deck's chrome and the new edge.
- **Does not assert:** the pane *program's* own reflow (a non-redrawing `sleep` pane never repaints newly exposed columns — expected terminal behaviour, not the deck bug); exact per-cell colours; the transient single-frame band itself.
- **Platform coverage:** mac+linux.
- **M1 status (PRD #84):** RED side of the M4 chain (`Event::Resize` → recompute layout → resize PTYs → render). The empty-band symptom is a one-frame race the current code self-heals once the resize handler fires, so this is written as an **invariant guard**: it pins "the post-resize frame fills the new width" and currently passes after quiescence. It *flags* (does not hard-fail) because the transient band is not deterministically observable through the PTY+vt100 harness. The widget-level half of the same defect (the `min(area, screen)` col clamp) is covered deterministically by `render/widget/001` and `render/widget/002`. Goes/stays GREEN at M4.
- **Post-M5 resolution (PRD #84):** **GREEN.** After M4 (layout-driven PTY resize) and M5 (1:1 widget render with the contract `debug_assert!` live in debug builds), the enlarge path drives recompute-layout → resize-PTYs-to-match → render, and the settled frame fills the new width. The guard now exercises that contract chain with the col clamp gone, rather than masking a self-healing race. Confirmed green post-M5.

### Render contract (PRD #84)

The rendering-contract reproducers for the PRD #84 (`prds/done/84-rendering-layer-rework.md`)
rework: one reproducer per known render-path defect, each the RED side of a TDD chain that
goes GREEN at M4 (layout-driven PTY resize) or M5 (1:1 `TerminalWidget`). They target the
`src/terminal_widget.rs` `min(area, screen)` col clamp + cursor-anchored row window (removed
in M5) and the scattered, per-path layout/resize math (unified in M3/M4). `render/widget/*`
are deterministic L1/unit tests over `TerminalWidget` rendered against a `ratatui` buffer;
`render/layout/*` drive the real spawned-binary layout-change pipelines and are invariant
guards where the underlying glitch is transient/race-y (per the PRD's "race-y resize timing"
note).

#### render/widget

##### render/widget/001 — `TerminalWidget` renders the PTY screen 1:1 from row 0 — no cursor-anchored row window that drops or shifts the top rows.
- **Layer:** L1 (in-process `TerminalWidget` rendered into a `ratatui::buffer::Buffer`; no PTY, no subprocess).
- **Agent:** none.
- **Asserts:** given a vt100 screen taller than the widget's inner area with the cursor parked on the bottom row, the widget maps screen cell (r, c) → inner cell (r, c) so the inner top row shows screen row 0 — i.e. the top-of-screen marker is rendered at the top of the pane.
- **Does not assert:** behaviour when the screen fits the area exactly (already correct today); colours / cursor-highlight styling; scrollback.
- **Platform coverage:** mac+linux.
- **M1 status (PRD #84):** **RED.** Current `src/terminal_widget.rs:96-117` anchors a row window on the cursor (`start_row = effective_rows - rows`), so with the cursor low it shows the *bottom* rows and the row-0 marker is absent → assertion fails today. Core gate for M5 (the 1:1 widget maps screen row 0 → area row 0). Deterministic at the widget level — the fixture intentionally violates the (future) upstream size contract to exercise the windowing heuristic M5 removes.
- **Post-M5 resolution (PRD #84):** **GREEN.** M5 removed the cursor-anchored row window (and the `min(area, screen)` col clamp) from `src/terminal_widget.rs`, so the widget now maps screen cell (r, c) → inner cell (r, c) and renders 1:1 from row 0: the inner top row shows screen row 0 (`TOP_ROW_0`) and the assertion passes. Confirmed RED→GREEN post-M5 — the core M5 gate is met.

##### render/widget/002 — `TerminalWidget` tolerates an inner area larger than the PTY screen — falls back to drawing the available cells at the top-left, no panic, no out-of-bounds read.
- **Layer:** unit (in-process `TerminalWidget` rendered into a `ratatui::buffer::Buffer`).
- **Agent:** none.
- **Asserts:** rendering a small (e.g. 3×6) PTY screen into a larger (e.g. 6×12) inner area completes without panicking; the PTY content lands at the top-left and the excess rows/columns stay blank (the `min(area, pty)` fallback).
- **Does not assert:** the debug-build `debug_assert!(pty == inner)` invariant M5 adds (a dev guard, not a runtime assertion — see PRD #84 M5); the single release-mode log line on mismatch.
- **Platform coverage:** mac+linux.
- **M1 status (PRD #84):** **Flag / guard (passes today).** Pins the release-path contract M5 must preserve: area > PTY must fall back to `min` and never panic. Current code already does `min` and does not panic, so this is GREEN now and stays GREEN through M5's release fallback. (M5's debug-only `debug_assert!` is explicitly out of scope here — orchestrator brief: "test the release fallback path".)
- **Post-M5 resolution (PRD #84):** **GREEN (unchanged throughout M1→M5).** M5 preserved the release `min(area, pty)` no-panic fallback (log-once on mismatch) alongside the new debug-build contract `debug_assert!`, so this release-path guard stays green and now pins the fallback the M5 contract intentionally keeps.

#### render/layout

##### render/layout/001 — After a tab/layout switch with N panes the embedded pane's bottom rows show correct (non-stale) content — no off-by-one row shift.
- **Layer:** L2.
- **Agent:** none (long-lived `sleep` panes).
- **Asserts:** with ≥1 embedded pane carrying a known bottom-row marker, after a layout change (`Ctrl+t` toggle) and quiescence, the pane's bottom row still shows its marker — not a stale fragment of the pre-switch layout, and not shifted by a row.
- **Does not assert:** which layout is default; the pane program's own redraw; that the defect reproduces every run.
- **Platform coverage:** mac+linux.
- **M1 status (PRD #84):** **Flag / invariant-check (riskiest entry).** The PRD risk row flags this symptom as possibly a vt100/parser issue below scope. The current code resizes panes on every layout-change path (`Action::ToggleLayout` routes through `resize_*_panes`), so the area/PTY mismatch that would scramble the bottom rows self-heals and is not deterministically observable through the harness. Written as an invariant guard on bottom-row content (PTY size == inner area, observed via rendered content). If it reproduces deterministically after M4+M5, that's follow-up signal — NOT a reason to re-add the clamp.
- **Post-M5 resolution (PRD #84):** **GREEN.** Stays green after M4+M5 and now runs with the M5 contract `debug_assert!` live in debug builds: a layout toggle that left a pane's PTY out of step with its rect would trip the debug assert instead of self-healing, so the guard exercises the layout-driven resize + 1:1 render contract rather than masking the race. No deterministic bottom-row scramble survived M4+M5 — no below-scope (vt100/parser) follow-up signal, and the clamp stays removed.

##### render/layout/002 — Reactive pane recreation/replace leaves no scrambled fragments — the replacement pane renders cleanly.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** after a pane is recreated/replaced in place (open a Mode tab, return to command mode, request its close with Ctrl+W, observe the tab-scoped confirmation, then choose Close with Down+Enter), the rendered grid contains the surviving Dashboard and no leftover fragment of the removed pane at a stale position.
- **Does not assert:** the exact recreation trigger internals; per-cell colours.
- **Platform coverage:** mac+linux.
- **M1 status (PRD #84):** **Flag / invariant-check.** Pane open/close and reactive recreation (`src/ui.rs:1510`, `:2147` areas) currently resize the affected PTYs on the spot, so any scramble is transient. Invariant guard on "no stale fragment after replace". GREEN target at M4/M5.
- **Post-M5 resolution (PRD #84):** **GREEN.** Stays green after M4+M5 and now exercises the pane open/close replace through layout-driven resize + 1:1 widget render with the M5 contract `debug_assert!` live in debug builds — asserting the replace contract rather than masking a self-healing race.

##### render/layout/003 — A mode switch (the `render_mode_tab` path) leaves no short-lived render artefacts after the transition settles.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** after switching into a mode tab and quiescence, the rendered grid shows the destination layout cleanly with no leftover fragment from the dashboard/source layout.
- **Does not assert:** mode-tab content semantics; the transient mid-transition frame.
- **Platform coverage:** mac+linux.
- **M1 status (PRD #84):** **Flag / invariant-check.** Mode switch (`src/ui.rs:2828` area) resizes panes through `resize_mode_tab_panes`, so artefacts are transient. Invariant guard on post-transition cleanliness. GREEN target at M4/M5.
- **Post-M5 resolution (PRD #84):** **GREEN.** Stays green after M4+M5 and now exercises the `render_mode_tab` switch through layout-driven resize + 1:1 widget render with the M5 contract `debug_assert!` live in debug builds — asserting the mode-switch contract rather than masking a self-healing race.

##### render/layout/004 — A wrapped button bar costs the dashboard exactly one extra row of its height budget (PRD #144).
- **Layer:** L1 (in-process `TestBackend` via `render_button_bar_with_bindings_to_buffer`; no PTY, no subprocess).
- **Agent:** none (renders the full global + dashboard context bar into a tall area at two widths).
- **Asserts:** at the 120-col reference width the full button set (~133 cells) does not fit one row, so the bar wraps to EXACTLY two rendered rows — meaning the dashboard/pane region above must cede exactly that one extra row (the PRD #144 height-budget contract that keeps a 2-row bar from overlapping / clipping the cards); at a roomy 200-col width the same set fits one row, so the bar occupies exactly one row and the dashboard cedes nothing extra. Complements `mouse/buttonbar/006` (which pins the wrapped bar's label content) by pinning its height.
- **Does not assert:** the card/pane rects themselves (no public full-frame layout seam at L1 — the post-transition card cleanliness is guarded at L2 by `render/layout/001`–`003`); which button lands on which row; the exact column widths.
- **Platform coverage:** mac+linux+windows.

##### render/layout/005 — The new-pane form modal renders without panicking on a wide-but-very-short terminal (PRD #144 bounds-safety guard).
- **Layer:** L1 (in-process `TestBackend` via `render_new_pane_form_to_buffer`; no PTY, no subprocess).
- **Agent:** none (renders the new-pane form with two mode options into an 80×3 buffer).
- **Asserts:** rendering the content-sized new-pane form modal at a wide-but-very-short 80×3 terminal — where the modal is clamped to ~2 rows, far fewer than the form's reserved field rows — completes WITHOUT panicking, and returns a buffer of exactly the requested size so every overlay cell (mode chips, `[Submit]`/`[Cancel]` row, cursor) stayed within the clamped modal/buffer bounds instead of being placed by an absolute line index that runs past the buffer bottom. A TUI must not panic on a small-but-valid terminal.
- **Does not assert:** the exact rows the overlays land on; which overlays are skipped when they don't fit; the modal's content/labels at this degenerate size; behaviour at roomy sizes (covered by `mouse/form/001`).
- **Platform coverage:** mac+linux+windows.

### Keybindings (PRD #40)

Keybindings resolve **client-side**: the config file lives on the machine
running the TUI (`$HOME/.config/dot-agent-deck/keybindings.toml`, mirroring
the `config.toml` path), the TUI event loop reads it and matches each
keypress to a semantic action, and the daemon never sees raw command-mode
keystrokes — it stays binding-agnostic. The L2 tests below are
interface-agnostic: each stages a `keybindings.toml` under the per-test
HOME (harness `TuiDeckBuilder::with_keybindings_toml`) and asserts on the
rendered grid, so they exercise the full client-side resolution path
without depending on the config struct API.

#### keybindings/remap

##### keybindings/remap/001 — A config remap of a **global** action (`toggle_layout` → `Alt+Shift+l`) takes effect on the new combo and the old default stops toggling.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** with a `keybindings.toml` rebinding `[global] toggle_layout = "Alt+Shift+l"`, pressing `Alt+Shift+l` toggles the dashboard layout (the `Layout: …` status message appears in the bottom bar); the old default toggle key (`Ctrl+t`) no longer toggles. The remap is resolved **client-side** — the file is read on the TUI side, the TUI matches the keypress to the action, and the daemon stays binding-agnostic.
- **Does not assert:** which layout (stacked vs tiled) is the default, exact status-message wording beyond the `Layout:` prefix, daemon-side behaviour (there is none — binding resolution is entirely client-side).
- **Platform coverage:** mac+linux.

##### keybindings/remap/002 — A config remap of a **dashboard** action (`help` `?` → `F1`) opens the help overlay on the new key.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** with a `keybindings.toml` rebinding `[dashboard] help = "F1"`, pressing `F1` opens the help overlay (the "Create new pane" line is rendered).
- **Does not assert:** that the old `?` still opens help (the action was remapped, not added), help-overlay content beyond one anchor line.
- **Platform coverage:** mac+linux.

##### keybindings/remap/003 — Existing `[global] close_pane` remaps survive mode-gated dispatch.
- **Layer:** L1 (TOML parse + in-process production key mapper).
- **Agent:** none.
- **Asserts:** `[global] close_pane = "Ctrl+x"` parses without warnings, the custom chord requests close in command mode, and the same chord remains ordinary `0x18` PTY input in PaneInput.
- **Does not assert:** filesystem loading of `keybindings.toml` (covered by `keybindings/remap/001`); arbitrary per-mode config syntax (out of scope).
- **Platform coverage:** mac+linux+windows.

#### keybindings/safety

##### keybindings/safety/001 — `Ctrl+C` always opens the quit modal, even when another action is bound to `Ctrl+C`.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** with a `keybindings.toml` that tries to hijack `Ctrl+C` for another action (`[global] new_pane = "Ctrl+C"`), pressing `Ctrl+C` still opens the quit/detach modal ("Quit dot-agent-deck?"). `Ctrl+C` is a non-overridable safety net — quit is not a configurable action (it is hardcoded in the event loop), so no action bound to `Ctrl+C` can hijack it. Exercises the GLOBAL-block `Ctrl+C` exclusion path. Guard test — must stay green so config can never disable emergency quit.
- **Does not assert:** which quit option is selected by default, the dialog layout.
- **Platform coverage:** mac+linux.

##### keybindings/safety/002 — `Ctrl+C` always opens the quit modal, even when a tab-navigation action is bound to `Ctrl+C`.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** with a `keybindings.toml` that binds both `[dashboard] move_left = "Ctrl+C"` and `move_right = "Ctrl+C"`, pressing `Ctrl+C` still opens the quit/detach modal ("Quit dot-agent-deck?"). Complements safety/001 by covering the Normal-mode tab-cycle dispatch path: `Ctrl+C` is never routed through the configurable `move_left`/`move_right` matching, so it can't be turned into a tab switch. `Ctrl+C` is non-overridable. Regression guard for the `!is_ctrl_c` gate on that dispatch path.
- **Does not assert:** tab-switch behaviour for non-`Ctrl+C` `move_left`/`move_right` bindings, conflict-resolution warning wording.
- **Platform coverage:** mac+linux.

##### keybindings/safety/003 — Ctrl+W is PTY input in PaneInput and a close request in command mode.
- **Layer:** L1 (in-process production key mapper).
- **Agent:** none.
- **Asserts:** the same default Ctrl+W chord yields `ForwardToPane([0x17])` in `UiMode::PaneInput` and `CloseSelected` in `UiMode::Normal`; both halves live in one regression test.
- **Does not assert:** readline's visible editing result or pane survival through the real binary (covered by `prompt/pane-input/021`).
- **Platform coverage:** mac+linux+windows.

##### keybindings/safety/004 — Mode-gating Close does not scope the other global commands.
- **Layer:** L1 (in-process production key mapper).
- **Agent:** none.
- **Asserts:** Dashboard, NewPane, and ToggleLayout still resolve from PaneInput; only ClosePane falls through to PTY input.
- **Does not assert:** each action's downstream UI mutation (covered by its feature-specific tests).
- **Platform coverage:** mac+linux+windows.

##### keybindings/safety/005 — `Ctrl+M` resolves to the agent-badge toggle only in command mode; PaneInput always forwards it as the CR submit byte, never the toggle (fork #339 — the highest-severity guard in the change).
- **Layer:** L1 (in-process production key mapper).
- **Agent:** none.
- **Asserts:** looping over every `UiMode` variant, `Ctrl+M` resolves to `Action::ToggleAgentTypeBadge` if and only if the mode is `Normal`; `PaneInput` + `Ctrl+M` forwards the exact byte `0x0d` to the PTY and is never claimed as the toggle; `PaneInput` + a bare `m` forwards `b'm'` as plain input; `Normal` + `Enter` is never claimed by the global command layer.
- **Does not assert:** the bare-`m` alias in command mode (`dashboard/agent-badge/002`); the real PTY's decoding of `Ctrl+M` as `Enter` under a legacy terminal (`dashboard/agent-badge/003` presses `m`, never `\x0d`, for exactly this reason).
- **Platform coverage:** mac+linux+windows.

#### keybindings/unbind

##### keybindings/unbind/001 — An empty-string binding (`new_pane = ""`) makes the default key a no-op.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** with a `keybindings.toml` setting `[global] new_pane = ""`, pressing the default `Ctrl+n` does nothing — the directory picker / new-pane flow ("Select Directory") never opens. The deck stays in Normal mode (a following `?` still opens help).
- **Does not assert:** behaviour of other unbound actions, that the new-pane flow can be re-bound to a different key (separate concern).
- **Platform coverage:** mac+linux.

#### keybindings/fallback

##### keybindings/fallback/001 — A malformed `keybindings.toml` falls back to defaults and warns on stderr.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** with an unparseable `keybindings.toml`, the deck still launches to its empty dashboard, default bindings still work (`?` opens help), and a warning mentioning "keybindings" is emitted on stderr (observed in the merged PTY byte stream, which retains it after the TUI clears the screen).
- **Does not assert:** the exact warning wording beyond the "keybindings" substring, per-entry vs whole-file fallback granularity.
- **Platform coverage:** mac+linux.

#### keybindings/help

##### keybindings/help/001 — The help overlay is generated from the active keybinding config and shows remapped keys.
- **Layer:** L1 (ratatui `TestBackend` + `insta` file snapshot).
- **Agent:** none.
- **Asserts:** rendered against a `KeybindingConfig` that remaps `toggle_layout` → `Alt+Shift+l` and `help` → `F1`, the help-overlay buffer shows those custom notations and describes Ctrl+D as a command-mode / pane-input toggle, proving the overlay is generated from the active config while retaining the corrected semantics. The default-config content guard lives at `dashboard/help/002`.
- **Does not assert:** the overlay's exact column layout or footer wording beyond what the committed snapshot pins; behaviour with the *default* config (that is `dashboard/help/002`'s job).
- **Platform coverage:** mac+linux+windows.

#### keybindings/hints

##### keybindings/hints/001 — The hints bar is generated from the active keybinding config and shows remapped keys.
- **Layer:** L1 (ratatui `TestBackend` + `insta`, through the same production `render_bottom_bar` path the app draws).
- **Agent:** none.
- **Asserts:** rendered in command mode against a config that remaps `toggle_layout` → `Alt+Shift+l`, the live button bar shows `[Toggle Layout Alt+Shift+L]` and `[Back to Pane Ctrl+D]`; the snapshot pins the complete production bar.
- **Does not assert:** truncation behaviour at narrow widths.
- **Platform coverage:** mac+linux+windows.

##### keybindings/hints/002 — An unbound action is rendered as `(unbound)` in the hints bar, never as a bare `: <label>`.
- **Layer:** L1 (ratatui `TestBackend`, through the production button-bar renderer; asserts on buffer text, no snapshot).
- **Agent:** none.
- **Asserts:** with `new_pane` unbound, the live bar renders `[New Pane (unbound)]` and never `[New Pane ]` with a blank shortcut.
- **Does not assert:** the exact placeholder wording beyond `(unbound)`, behaviour of other simultaneously-unbound actions, snapshot of the full bar.
- **Platform coverage:** mac+linux+windows.

##### keybindings/hints/003 — The hints bar reflects Close's mode scope and makes command-mode exit discoverable.
- **Layer:** L1 (ratatui `TestBackend`, rendered through `render_button_bar_for_mode_to_buffer`, which calls the live `render_bottom_bar` path).
- **Agent:** none.
- **Asserts:** command mode shows enabled `[Back to Pane Ctrl+D]` and `[Close Ctrl+W]`; Help shows `[Command Mode Ctrl+D]` and a DIM Close whose Ctrl+W mapping is inert; PaneInput shows only `[Command Mode Ctrl+D]` and no Close button.
- **Does not assert:** narrow-width wrapping or mouse hit-testing of the disabled button.
- **Platform coverage:** mac+linux+windows.

#### keybindings/buttons

##### keybindings/buttons/001 — The prd-80 button bar labels are derived from the active keybinding config.
- **Layer:** L1 (ratatui `TestBackend`; asserts on buffer text, no `insta` snapshot).
- **Agent:** none.
- **Asserts:** rendered against a `KeybindingConfig` that remaps `new_pane` → `Alt+P` and `help` → `F1`, the button bar shows the remapped New-pane key `Alt+P` and Help key `F1`, and does NOT show the default New-pane key `Ctrl+N` — proving the button labels are generated from the active config, not hardcoded. Guards against a future refactor silently re-hardcoding the labels.
- **Does not assert:** button positions/ordering, the non-remappable `Quit` button label (fixed `Ctrl+C`), truncation behaviour at narrow widths.
- **Platform coverage:** mac+linux+windows.

#### keybindings/scheduler

##### keybindings/scheduler/001 — The "Scheduled Tasks" dialog open-shortcut is registry-routed: the default lowercase `s` opens it, not uppercase-only `Shift+S` (PRD #127 finding #4).
- **Layer:** L2.
- **Agent:** none (fixture global `schedules.toml` via `DOT_AGENT_DECK_SCHEDULES`).
- **Asserts:** with no `keybindings.toml`, pressing the DEFAULT lowercase `s` from the empty dashboard opens the "Scheduled Tasks" manager dialog (confirmed by the seeded task name appearing in the dialog list) — proving the open-shortcut is routed through the KbAction registry with a case-insensitive default (lowercase `s` as well as `S`, like the registry's `t`/`T` and `l`/`L` pairs) rather than the hardcoded uppercase-only `KeyCode::Char('S')`.
- **Does not assert:** that `S` still works (covered by `scheduler/manager/*`); remappability of the open-shortcut to an arbitrary key; the dialog's list/action contents beyond the seeded task name.
- **Platform coverage:** mac+linux.

### Error paths

#### error/socket

##### error/socket/001 — The deck refuses to attach to a Unix socket owned by another uid.
- **Layer:** L2.
- **Agent:** none (fixture builds a socket whose mode/owner mimic a foreign daemon).
- **Asserts:** the deck exits non-zero with a stderr message; the foreign socket is left intact.
- **Does not assert:** the message wording beyond mentioning the trust failure.
- **Platform coverage:** mac+linux.

##### error/socket/002 — Stale socket file (inode without a listener) is recovered transparently — the next launch unlinks it and lazy-spawns a fresh daemon.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** the dashboard appears on second launch; the socket is now a live daemon's.
- **Does not assert:** the time spent in the recovery path.
- **Platform coverage:** mac+linux.

##### error/socket/003 — `request_from_socket` returns `None` within a bounded wait against a daemon that reads the request and then never replies and never closes, instead of hanging forever.
- **Layer:** L1 (`src/hook.rs`'s `#[cfg(test)] mod tests`; a synthetic stub daemon over a real temp Unix socket, no PTY, no daemon binary, no real agent).
- **Agent:** none (a `std::thread` stub daemon that accepts one connection, reads the request line, then sleeps well past the bound without replying or closing).
- **Asserts:** `request_from_socket`, driven on a worker thread and awaited via `mpsc::recv_timeout` at 15s (comfortably above the production 5s bound the fix adds), returns before the 15s bound and the returned value is exactly `None` — a timed-out/unbounded daemon must fold into the same "no seed" bucket as a daemon that closes without replying, not a distinct outcome. A `RecvTimeoutError::Timeout` is treated as the RED failure (`request_from_socket` is unbounded) and fails the test with an explicit panic message rather than hanging until nextest's own timeout.
- **Does not assert:** the exact timeout duration chosen by the fix (only that it is comfortably under 15s); `SocketReply`'s three-way outcome (only `request_from_socket`'s two-way `None` collapse is exercised here — the richer outcome exists for a not-yet-submitted caller); real daemon behavior; Windows named-pipe timeout semantics.
- **Platform coverage:** mac+linux (Unix-domain socket).

##### error/socket/004 — `request_from_socket` still returns the reply from a daemon that is merely slow, not absent — a bound that fires too eagerly must not be mistaken for "no seed".
- **Layer:** L1 (`src/hook.rs`'s `#[cfg(test)] mod tests`; same synthetic stub-daemon setup as `error/socket/003`).
- **Agent:** none (a `std::thread` stub daemon that accepts one connection, reads the request line, sleeps 300ms — comfortably inside the production 5s bound — then writes one JSON reply line).
- **Asserts:** `request_from_socket` returns `Some("{\"seed\":\"abc123\"}")` — the exact reply line, unmodified — proving the timeout bound added for `error/socket/003` does not fire against a daemon that is merely slow. Passes both before and after the fix; it is a correctness control, not a timing measurement, and the delay is deliberately far from the 5s bound to avoid flaking under scheduler jitter.
- **Does not assert:** the timeout duration itself (`error/socket/003` pins the unbounded-hang failure mode; this test never reaches the bound); daemon behavior beyond a single reply line; real daemon timing.
- **Platform coverage:** mac+linux (Unix-domain socket).

##### error/socket/005 — `request_from_socket` still hangs against a peer that dribbles one non-newline byte just before each per-read timeout, because every byte resets it.
- **Layer:** L1 (`src/hook.rs`'s `#[cfg(test)] mod tests`; same synthetic stub-daemon setup as `error/socket/003`/`004`).
- **Agent:** none (a `std::thread` stub daemon that accepts one connection, reads the request line, then writes a single non-newline byte every 200ms for 20s without ever sending a newline).
- **Asserts:** `request_from_socket`, driven on a worker thread and awaited via `mpsc::recv_timeout` at a 15s ceiling — comfortably above whatever operation-level deadline the fix adds, and comfortably inside the 20s the drip keeps running — returns before the ceiling. A `RecvTimeoutError::Timeout` is the RED failure (the per-read timeout keeps getting reset and never fires) and fails the test with an explicit panic rather than hanging until nextest's own timeout. Deliberately does not pin the exact deadline value so it keeps passing once any sane operation-level bound exists.
- **Does not assert:** the exact operation-level deadline chosen by the fix; the reply-length cap (a separate, deliberately out-of-scope follow-up); any other caller of the shared, vulnerable `request_from_socket_inner` code path with a different timeout value — this test exercises the choke point itself, so any future caller inherits the same coverage.
- **Platform coverage:** mac+linux (Unix-domain socket).

##### error/socket/006 — A daemon that closes without writing any bytes back folds into `SocketReply::NoReply`, not `SocketReply::Line("")` (Greptile finding on upstream PR #419, against code already merged here via #106/#110).
- **Layer:** L1 (`src/hook.rs`'s `#[cfg(test)] mod tests`; same synthetic stub-daemon setup as `error/socket/003`/`004`/`005`).
- **Agent:** none (a `std::thread` stub daemon that accepts one connection, reads the request line, then closes without writing a single byte).
- **Asserts:** `request_from_socket_at` returns `SocketReply::NoReply`. Before the fix, an EOF with an empty in-progress buffer returned `Some(String::new())` (`SocketReply::Line("")`), contradicting `SocketReply::NoReply`'s own doc comment, which already names "the daemon closed without answering" as a `NoReply` case.
- **Does not assert:** the *partial*-line-then-EOF case (some bytes written, then closed before the newline) — that is deliberately left returning `Line(partial)`, unchanged by this fix; `SocketReply::Unreachable`; timing.
- **Platform coverage:** mac+linux (Unix-domain socket).

##### error/socket/007 — `request_from_socket` abandons a reply line that exceeds the maximum length, instead of buffering it until the total-operation deadline expires (fork issue #101 item 2, left open by #120).
- **Layer:** L1 (`src/hook.rs`'s `#[cfg(test)] mod tests`; same synthetic stub-daemon setup as `error/socket/003`–`006`).
- **Agent:** none (a `std::thread` stub daemon that reads the request line, writes a bounded 4 MiB of non-newline bytes as fast as it can, then holds the connection open and silent past the deadline).
- **Asserts:** two things together — the outcome is `SocketReply::NoReply` (an over-long line folds into the same "no seed" bucket as any other non-answer, per fork issue #89's contract, rather than becoming a new error path), **and** the call returns in under 2500ms. The timing assertion is what carries the RED signal: the outcome is `NoReply` with or without a cap, so only the elapsed time distinguishes "the cap fired" (milliseconds, once the buffer crosses the limit) from "only the 5s deadline stopped it" (the uncapped behavior #120 shipped). The peer's 4 MiB is deliberately bounded so the RED run has a fixed allocation ceiling — an open-ended flood would exercise the unbounded growth under test by allocating without limit inside CI.
- **Does not assert:** the exact cap value (only that one exists and fires well before the deadline); the daemon-side ingestion bound (upstream #319, the other half of this problem); that memory is actually released; Windows named-pipe semantics.
- **Platform coverage:** mac+linux (Unix-domain socket).

##### error/socket/008 — A large but legitimate reply line (256 KiB seed) is returned whole, so the cap added for `error/socket/007` cannot silently truncate a real seed.
- **Layer:** L1 (`src/hook.rs`'s `#[cfg(test)] mod tests`; same synthetic stub-daemon setup as `error/socket/003`–`007`).
- **Agent:** none (a `std::thread` stub daemon that replies with one well-formed line carrying a 256 KiB seed).
- **Asserts:** `request_from_socket_at` returns `SocketReply::Line` whose length equals the sent line's exactly. 256 KiB is far above the 64 KiB `MAX_FIRST_PROMPT_BYTES` the daemon clamps stored prompts to, so this pins real headroom rather than a value that merely happens to fit. This is the control fork issue #101 explicitly asks for: "a cap that is too tight would silently truncate a legitimate prompt, which is worse than the DoS it prevents". Passes both before and after the fix — a correctness control, not a timing measurement.
- **Harness note:** the stub holds the connection open until the client signals it has finished reading, and propagates its write result rather than swallowing it. This reply is the only one in the file larger than the socket buffer; closing with unread data pending resets the peer and discards it on macOS (Linux leaves it readable), which failed this test on macOS for a harness reason unrelated to truncation.
- **Does not assert:** the cap value; any seed larger than 256 KiB; that the daemon would ever actually produce a seed this large.
- **Platform coverage:** mac+linux (Unix-domain socket).

#### error/config

##### error/config/001 — `.dot-agent-deck.toml` with an invalid regex makes the new-pane form refuse the mode and surface a status-line message.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** the mode is missing from the **Mode** cycle; a status-line message names the invalid pattern.
- **Does not assert:** message wording exact match.
- **Platform coverage:** mac+linux.

##### error/config/002 — Missing `.dot-agent-deck.toml` results in the **Mode** field showing only the default; the new-pane form still launches a plain pane.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** the form opens with the default mode selectable; submitting creates a dashboard pane (not a mode tab).
- **Does not assert:** the absence-of-config tip rendering (covered by `dashboard/config-gen/001`).
- **Platform coverage:** mac+linux.

#### error/agent-spawn

##### error/agent-spawn/001 — Submitting the new-pane form with a non-existent command produces a card whose status is Error and whose card body names the missing binary.
- **Layer:** L2.
- **Agent:** none (fixture command: `nonexistent-binary-78f3c`).
- **Asserts:** card appears; badge reads Error; card text contains the binary name.
- **Does not assert:** how long the failure takes to surface.
- **Platform coverage:** mac+linux.

### Orchestration delegation

#### orchestration/delegate

##### orchestration/delegate/001 — `dot-agent-deck delegate --to coder --task <text>` from the orchestrator pane writes the task into the target role's pane.
- **Layer:** L2.
- **Agent:** none (synthetic — invoke the delegate subcommand from inside the orchestrator pane via a scripted keystroke).
- **Asserts:** the target role's parsed grid contains the task text; the orchestrator's pane stays clean.
- **Does not assert:** the target agent's response (no real agent in the loop).
- **Platform coverage:** mac+linux.

##### orchestration/delegate/002 — Delegating to a role missing from the config produces a clear error on the orchestrator pane and no other side effects.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** the orchestrator pane's parsed grid carries an error mentioning the unknown role; no card statuses change.
- **Does not assert:** the error message text exactly.
- **Platform coverage:** mac+linux.

##### orchestration/delegate/003 — `dot-agent-deck work-done --task <summary>` from a worker pane writes the summary to the orchestrator and to `.dot-agent-deck/work-done-<role>-<pane digest>.md`.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** orchestrator pane shows the summary; the file exists with the expected contents.
- **Does not assert:** the orchestrator's reply (no real LLM in this synthetic test).
- **Platform coverage:** mac+linux.

##### orchestration/delegate/004 — A worker calling `delegate` is rejected (only the `start = true` role may delegate).
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** worker's pane gains an error line; no task is delivered to any role.
- **Does not assert:** the daemon-side log entry.
- **Platform coverage:** mac+linux.

##### orchestration/delegate/005 — A Pi-identity orchestrator's `delegate` routes into the worker pane (the synthetic-agent harness proves the delegate contract holds for a Pi identity) (PRD #201 M1.3).
- **Layer:** L1/fast (in-process — the daemon's real `handle_delegate` against a `cat`-stub worker pane; mirrors the fast-tier precedent `delegate_prompt_injection`, no daemon socket, no LLM).
- **Agent:** none (synthetic — the harness at `AgentType::Pi` identity is the orchestrator; the `coder` worker is a `cat` stub whose PTY echoes injected bytes).
- **Asserts:** with a Pi orchestrator (the `start = true` role) and a `coder` worker registered in the same orchestration, calling the harness's `delegate --to coder` routes the single-line task pointer into the worker pane's PTY. Additive Pi coverage of the `orchestration/delegate/001` contract; expected green-on-write because routing keys on pane role, not agent type.
- **Does not assert:** the worker task-file footer / single-line-prompt shape (covered by `delegate_prompt_injection`); the real-agent response (no LLM).
- **Platform coverage:** mac+linux.

##### orchestration/delegate/006 — A Pi-identity WORKER calling `delegate` is rejected by the pane-role guard; no task is delivered (PRD #201 M1.3).
- **Layer:** L1/fast (in-process — the daemon's real `handle_delegate` against a `cat`-stub worker pane; no daemon socket, no LLM).
- **Agent:** none (synthetic — the harness at `AgentType::Pi` identity is a non-orchestrator worker; a `coder` worker `cat` stub shares the orchestration so an orchestrator's delegate WOULD deliver).
- **Asserts:** a Pi worker (registered in `pane_role_map` but deliberately absent from `orchestrator_pane_ids`) calling the harness's `delegate --to coder` is rejected — the `coder` stub's PTY never receives the task pointer within a bounded grace window (rejection is a synchronous early return before any dispatch task spawns). Additive Pi coverage of the `orchestration/delegate/004` guard; expected green-on-write.
- **Does not assert:** the orchestrator pane's error-line rendering (L2 `orchestration/delegate/004`); the daemon-side log entry.
- **Platform coverage:** mac+linux.

##### orchestration/delegate/007 — A wrapped native-hook agent ignores its fork-time card-surfacing `SessionStart` for delegate readiness (PRD #225 M1).
- **Layer:** fast synthetic real-binary-subprocess integration (a real `dot-agent-deck wrap` child + in-process daemon hook socket + real `handle_delegate` + managed PTY; no vt100 attach, no LLM, no `e2e` feature gate).
- **Agent:** synthetic Codex executable backed by `cat`; the real `dot-agent-deck wrap` emits the early wrapper event and the test later injects the genuine native Codex event.
- **Asserts:** after a `clear = true` respawn, the task pointer is absent from the replacement PTY while only the wrapper's fork-time `SessionStart` has arrived; after the matching native `SessionStart`, the pointer is delivered promptly.
- **Does not assert:** real Codex boot timing or task execution (covered by the real-agent `orchestration/delegate/009`).
- **Platform coverage:** mac+linux.

##### orchestration/delegate/008 — A hookless wrapper-like agent still treats its sole fork-time `SessionStart` as ready (PRD #225 M1 guard).
- **Layer:** fast synthetic PTY integration (real `handle_delegate` + managed PTY + daemon broadcast; no socket or LLM).
- **Agent:** hookless future-wrapper stand-in represented by the neutral registry identity because no shipped hookless Wrapper agent exists yet.
- **Asserts:** a marked wrapper-fork `SessionStart` releases prompt delivery within two seconds — well inside the 30 s `SESSION_START_WAIT_TIMEOUT` fallback, so a pass cannot be the fallback firing — when the agent has no native hook installer and will emit no later readiness event.
- **Does not assert:** a concrete Gemini registry entry or wrapper classifier; those do not exist yet.
- **Platform coverage:** mac+linux.

##### orchestration/delegate/009 — A `clear = true` delegate to a REAL wrapped Codex worker delivers the prompt and the worker acts on it — the user-visible end of PRD #225 (M5). [reel]
- **Layer:** L2 PTY-attached REAL-agent (the real `dot-agent-deck` binary driven through the vt100 `TuiDeck` harness — records a `full-stream.cast`; pre-PR e2e tier per CLAUDE.md rule 5, flaky-tolerant, never in CI).
- **Agent:** a REAL interactive cheap-model Codex (`common::codex_test_model()`, no `-p`, no stand-in) as the `clear = true` `coder` role, wrapped from its first spawn because the role command's basename resolves to Codex; the `orchestrator` role is a deterministic script that invokes the genuine `dot-agent-deck delegate --to coder` CLI over the same hook socket a real orchestrator agent uses (the defect is entirely on the worker side, so a second LLM would add a flaky link without covering another line of the fix).
- **Asserts:** opening the orchestration through the normal Ctrl+N new-pane form surfaces the `coder` role card live; jumping into the worker's role pane shows the REAL Codex TUI up (its header names the pinned model) BEFORE anything is delegated — the readiness precondition, taken on the user-visible surface because codex-cli 0.145.0 posts its native `SessionStart` only when the first turn starts, so gating on that event would deadlock on the delegate that causes it; after the delegate the worker's card visibly enters `Thinking`, the daemon broadcasts the worker's GENUINE native Codex `SessionStart` (no wrapper-fork origin marker, so it is Codex itself and not the wrapper's fork-time card-surfacing event) plus a `Thinking` whose `user_prompt` is the injected `worker-task-coder.md` pointer — a field only Codex's native `UserPromptSubmit` hook sets, the wrapper's line classifier always leaves it `None` — so the pointer was submitted INSIDE the agent rather than echoed away by the launcher's line discipline; and the respawned worker creates the uniquely named sentinel `prd225-codex-delegate-6f21ba.txt` with the requested contents. Pre-fix the wrapper's fork-time event released the readiness gate seconds before the Codex TUI existed, the prompt was lost, and no sentinel ever appeared.
- **Does not assert:** the work-done leg (logged as a soft observation; hard-covered by `codex/worker/001`); the launch-shape half of PRD #225 (`codex/spawn/007` for the hook-learned badge, `codex/spawn/008` for the respawn wrap decision); the hookless-wrapper guard (`orchestration/delegate/008`).
- **Platform coverage:** mac+linux (unix-only — writes an executable role script).

##### orchestration/delegate/010 — An observed replacement `SessionStart` starts, but does not bypass, the delegate readiness buffer (PRD #249 M1).
- **Layer:** fast synthetic PTY integration (real `handle_delegate` + `clear = true` respawn + in-process daemon hook socket; no LLM and no `e2e` feature gate).
- **Agent:** synthetic hook-emitting worker backed by `cat`.
- **Asserts:** with `DOT_AGENT_DECK_DELEGATE_READINESS_BUFFER_MS=1000`, the task pointer is absent 350 ms after the replacement agent's matching `SessionStart` and appears after the configured buffer elapses.
- **Does not assert:** real-agent startup timing or timeout-fallback behavior (covered by `orchestration/delegate/011` and `/012`).
- **Platform coverage:** mac+linux.

##### orchestration/delegate/011 — The timeout fallback waits the delegate readiness buffer even when no `SessionStart` arrives (PRD #249 M1).
- **Layer:** fast synthetic PTY integration (real `handle_delegate` + `clear = true` respawn + daemon broadcast, with Tokio's clock paused to cross the production timeout instantly; no socket, LLM, or `e2e` feature gate).
- **Agent:** hookless `cat` stand-in that never emits `SessionStart`.
- **Asserts:** after the 30-second fallback expires in virtual time, the pointer remains absent both immediately and 998 ms into the additional 1000 ms readiness buffer, then is delivered after the clock advances to 1001 ms; `1` and whitespace-padded `1` both perform a real wait instead of collapsing to `sleep(0)`; and an integer above `u64::MAX` stays held past the 1000 ms default and releases at the 30 s cap.
- **Does not assert:** the observed-`SessionStart` branch or whether a real hookless agent is interactive at fallback time.
- **Platform coverage:** mac+linux.

##### orchestration/delegate/012 — A slow-readiness toggle proves the delegate buffer prevents lost payload and submit bytes (PRD #249 M4).
- **Layer:** fast synthetic real-binary-subprocess integration (real `handle_delegate`, respawn, hook socket, managed PTY, and Python raw-mode readiness stub; no LLM and no `e2e` feature gate).
- **Agent:** deterministic slow-readiness stand-in that discards PTY input for 650 ms after `SessionStart`, then echoes accepted bytes in raw mode.
- **Asserts:** changing only `DOT_AGENT_DECK_DELEGATE_READINESS_BUFFER_MS` loses the pointer at `0`, while `1000` delivers the pointer and its trailing submit CR after the measured input-readiness window.
- **Does not assert:** a real Claude or OpenCode timing distribution; the deterministic stub pins the race that real-agent timing cannot reproduce reliably.
- **Platform coverage:** mac+linux (unix-only — Python `termios` raw-mode stub).

##### orchestration/delegate/013 — A worker that receives a delegate and then emits no event produces a visible orchestrator notice (PRD #249 M3).
- **Layer:** fast synthetic PTY integration (real `handle_delegate`, managed worker and orchestrator PTYs, and shortened worker-response window; no LLM and no `e2e` feature gate).
- **Agent:** silent `cat` worker plus a raw no-echo orchestrator observer.
- **Asserts:** the worker first receives the task pointer, then its lack of any agent event produces an LF-terminated fixed daemon-authored notice in the orchestrator pane describing the missing event. The pane notice deliberately carries no role name or other project-controlled interpolation.
- **Does not assert:** tracing output from the companion `warn!`, an actual agent response, whether every supported agent treats bare LF as inert, or recovery after the notice.
- **Platform coverage:** mac+linux (unix-only — raw-mode shell observer).

##### orchestration/delegate/014 — A `clear = true` delegate reaches a REAL interactive Claude worker and the worker visibly acts on it (PRD #249 M4 real-agent happy path). [reel]
- **Layer:** L2 PTY-attached (the REAL `dot-agent-deck` binary driven through the vt100 `TuiDeck` harness; flaky-tolerant pre-PR e2e tier, runtime-skipped when the Claude CLI or credentials are unavailable). Imported Claude credentials plus project trust clear onboarding without a keystroke, and the production delegate CLI drives the daemon through its real socket.
- **Agent:** REAL interactive Claude Code pinned to Haiku (`claude-haiku-4-5-20251001`, `--allowedTools Bash Read Write`, no `-p`) as the `clear = true` `coder` role; the deterministic orchestrator role only invokes the same `dot-agent-deck delegate` CLI a real orchestrator uses. `Write` is allowed so the task file's `## When done` footer (#303) does not park the worker on an approval prompt after the sentinel is created — the sentinel itself is written with Bash.
- **Asserts:** the worker's real prompt editor is visibly ready before delegation; after the delegate respawns it, the role card visibly traverses Thinking → Working with Bash, its native `UserPromptSubmit` hook carries the injected `worker-task-coder.md` pointer (submission rather than PTY echo), and it creates `prd249-claude-respawn-4d37c1.txt` with exact known contents. This proves the happy path against a current real agent; the deterministic `/012` stand-in pins the race itself.
- **Does not assert:** the exact agent response, the measured readiness threshold (covered by `/012`), the timeout-fallback branch (covered by `/011`), or work-done delivery.
- **Platform coverage:** mac+linux (unix-only PTY/UDS; local real-agent tier).
- **Cost note:** one short Haiku worker turn.

##### orchestration/delegate/015 — Post-fix `clear = true` delivery reaches a REAL interactive OpenCode worker and the worker visibly acts on it. [reel]
- **Layer:** L2 PTY-attached (the REAL `dot-agent-deck` binary driven through the vt100 `TuiDeck` harness; flaky-tolerant pre-PR e2e tier, runtime-skipped when the OpenCode CLI or credentials are unavailable). Imported OpenCode credentials and `--auto` prevent a permission prompt from blocking the pane; a test-only forwarding env can set the production readiness buffer to zero for the explicit pre-fix observation run.
- **Agent:** REAL interactive OpenCode pinned to the cheap mini model `openrouter/openai/gpt-4o-mini` (no `opencode run`, no stand-in) as the `clear = true` `coder` role; the deterministic orchestrator role invokes the genuine delegate CLI.
- **Asserts:** the OpenCode TUI is visibly ready before delegation; after the delegate respawns it, the role card visibly traverses Thinking → Working with its shell tool, the OpenCode plugin's native `session.prompt` event carries the injected `worker-task-coder.md` pointer, and it creates `prd249-opencode-respawn-8a62f4.txt` with exact known contents.
- **Does not assert:** exact model phrasing, a universal OpenCode startup-time distribution from one host, the deterministic race (covered by `/012`), or work-done delivery.
- **Platform coverage:** mac+linux (unix-only PTY/UDS; local real-agent tier).
- **Cost note:** one short GPT-4o-mini worker turn per observation.

##### orchestration/delegate/016 — The generated orchestrator context names what `binary_name()` resolves for the running process, not a baked-in literal (issue prageethw/dot-agent-deck#253).
- **Layer:** pure-data (in-crate `#[cfg(test)]` unit test on `orchestrator_context::build_orchestrator_context`; no TUI harness, no daemon).
- **Agent:** none.
- **Asserts:** with a synthetic role config, the composed context's `delegate` and `work-done` command examples both contain `platform::paths::binary_name()`'s resolution for the running process — under `cargo test` the throwaway test binary is never on `$PATH`, so this is its own absolute `current_exe()` path, never literally `dot-agent-deck` — proving the text is generated from `current_exe()` rather than a hardcoded string.
- **Does not assert:** the symlink-resolution behavior of `current_exe()` itself (a property of the platform, not this crate); the malformed-`current_exe()` fallback branch (`orchestration/delegate/018`).
- **Platform coverage:** mac+linux+windows.

##### orchestration/delegate/017 — The generated worker task file's `work-done` instruction names what `binary_name()` resolves for the running process, not a baked-in literal (issue prageethw/dot-agent-deck#253).
- **Layer:** pure-data (in-crate `#[cfg(test)]` unit test on `state::compose_worker_task_file`; no TUI harness, no daemon).
- **Agent:** none.
- **Asserts:** the composed worker task file's `## When done` footer's `--task-file` and inline `--task` command examples both contain `platform::paths::binary_name()`'s resolution for the running process (the `cargo test` test binary's own absolute `current_exe()` path, which is never literally `dot-agent-deck`).
- **Does not assert:** the malformed-`current_exe()` fallback branch (`orchestration/delegate/018`); the rest of the footer's shell-safety content (covered by the pre-existing `compose_worker_task_file_appends_work_done_footer`).
- **Platform coverage:** mac+linux+windows.

##### orchestration/delegate/018 — The command-name resolver falls back to the crate's default literal only when `current_exe()` itself is unavailable or unusable (issue prageethw/dot-agent-deck#253).
- **Layer:** pure-data (in-crate `#[cfg(test)]` unit test on `platform::paths::resolve_binary_name`, the pure seam behind `binary_name`; no TUI harness, no daemon).
- **Agent:** none.
- **Asserts:** an `Err` result, a path with no file name (`/`), and (Unix-only) a non-UTF-8 file name all resolve to `DEFAULT_BINARY_NAME` (`env!("CARGO_PKG_NAME")`) rather than panicking or producing an empty string. A well-formed `current_exe()` whose bare file name is merely shell-unsafe or absent from `$PATH` does NOT fall back to this literal — it falls back to the absolute `current_exe()` path instead (`platform::paths::resolve_binary_name_falls_back_to_the_absolute_path_when_the_name_is_shell_unsafe`/`_not_on_path`, plain `#[test]`s alongside this one, not separately cataloged).
- **Does not assert:** a real `current_exe()` failure (not reproducible on demand); the happy path (`orchestration/delegate/016`–`017`).
- **Platform coverage:** mac+linux+windows.

##### orchestration/delegate/019 — A same-named binary shadowing the running executable earlier on `$PATH` is rejected by identity, not merely resolved (issue prageethw/dot-agent-deck#253 `$PATH`-identity tightening).
- **Layer:** pure-data (in-crate `#[cfg(test)]` unit test on `platform::paths::path_identity_match` and `platform::paths::resolve_binary_name`; no TUI harness, no daemon).
- **Agent:** none.
- **Asserts:** with a synthetic `$PATH` value (never the real process-global `PATH`) listing two directories that each hold an executable file sharing one basename — a "shadow" file first, the "real" (`current_exe()`-standing-in) file second — `path_identity_match` reports no match for the shadow-first ordering and a match once the roles are reversed (proving the rejection is genuinely about file identity, not mere absence), and `resolve_binary_name` driven through that shadow-first `$PATH` falls back to the shell-quoted absolute `current_exe()` path rather than emitting the bare name a consuming shell would resolve to the shadowing binary.
- **Does not assert:** the real process-global `$PATH` (a synthetic value is used throughout); the empty/relative-`$PATH`-entry branch (`platform::paths::is_untrustworthy_path_entry_rejects_empty_and_relative_but_accepts_absolute`, a plain `#[test]` alongside this one, not separately cataloged).
- **Platform coverage:** mac+linux+windows.

##### orchestration/delegate/020 — The bare-name success branch is reached against a REAL `current_exe()` on a REAL `$PATH` — PR #520's whole motivating scenario, previously untested (prageethw/dot-agent-deck#253 round-4 verification, finding 1).
- **Layer:** L2 (in-process daemon whose `handle_delegate` fan-out composes the worker task file; a `cat`-stub worker PTY via `AgentPtyRegistry::spawn_agent`, no real agent — the `e2e` tier, no LLM call). Entry point is a sync `#[test]` that `block_on`s an async body (the linkage-check scanner links `#[spec]` to the next PLAIN `fn`, so a `#[tokio::test] async fn` would misbind — same pattern as `chain-smoke/pi/002`).
- **Agent:** none (`cat` stub; only the generated file is under test).
- **Asserts:** with the built deck binary's own directory prepended to this process's `$PATH` (the deck's normal on-`PATH` install shape) and `spawn_inprocess_daemon`'s test-current-exe override injecting the real built `dot-agent-deck` binary as `binary_name()`'s effective `current_exe()`, delegating a task writes `.dot-agent-deck/worker-task-coder.md` whose `work-done` instruction names the BARE binary (`dot-agent-deck work-done --task-file …`) — not the quoted absolute-path fallback every other `binary_name()` test in this repo exercises, and not the running libtest binary's own path (the regression this issue's round-4 verification found: without the override, an in-process daemon's `handle_delegate` runs in the TEST process, so `binary_name()` correctly-for-that-process named the libtest binary, and a real worker following the generated command hit libtest's CLI parser instead of the deck's).
- **Does not assert:** a real agent following the generated command (covered, for the two real-agent arms this regression broke, by `delegate_work_done_chain_claude` and `chain-smoke/pi/002`, both now fixed by the same override); the malformed-`current_exe()` fallback (`orchestration/delegate/018`); the `$PATH`-identity-shadowing rejection (`orchestration/delegate/019`).
- **Platform coverage:** mac+linux (unix-only PTY/UDS; `spawn_inprocess_daemon` is `#[cfg(unix)]`).

##### orchestration/delegate/021 — Work-done completion does not make the next same-pointer delegate disappear after the user types an unsent draft.
- **Layer:** fast synthetic PTY integration (real `handle_delegate` and `handle_work_done`, managed worker and orchestrator PTYs, and production silence-watch accounting; no socket or LLM).
- **Agent:** none (`cat` worker stand-in plus a raw no-echo orchestrator observer).
- **Asserts:** delegation A's fixed `worker-task-coder.md` pointer physically reaches the worker, real work-done handling retires A, an unsent user draft then physically reaches the same pane, and delegation B produces another observable copy of the same pointer; independently, a late completion for an older of two live delegations leaves the newer delivery's no-event notice armed.
- **Does not assert:** the payload guard's records or refusal reason, exact task-file contents, or which safe mechanism admits B; the outcome is solely that B is physically delivered after A completed.
- **Platform coverage:** mac+linux.

##### orchestration/delegate/022 — Two panes reporting `work-done` for the same role name and the same cwd (fork #76's two-orchestrations collision) must not clobber each other's report.
- **Layer:** fast/pure-ish state (real `AppState::handle_work_done` against two hand-registered panes sharing a role and a tempdir cwd; no PTY spawn — the file write happens before the function's orchestrator lookup, so no daemon socket or LLM is needed either).
- **Agent:** none.
- **Asserts:** after `pane-a` and `pane-b` (both role `coder`, same cwd) each call `handle_work_done` with distinct marked report content, BOTH markers are discoverable somewhere under `.dot-agent-deck/` — a recursive scan of the directory's file contents, not a check on a specific filename, so the assertion survives whatever naming scheme a fix picks. Fixed: the daemon's output path is now keyed on the reporting pane's `pane_id` (`work_done_file_name` in `src/state.rs`), not on role name + cwd alone, so `pane-a` and `pane-b` write to two distinct files and both markers survive.
- **Does not assert:** which literal filename(s) a fix produces (one file vs. two, `pane_id`-digest-suffixed or otherwise); orchestrator-side feedback delivery (out of scope for this pane pair, which is not wired to any orchestrator).
- **Platform coverage:** mac+linux.

##### orchestration/delegate/023 — A report already sitting at the daemon's own work-done output path must survive a subsequent, differently-sourced `work-done` call, and the collision must be announced rather than silent (upstream #331).
- **Layer:** fast synthetic PTY integration (real `handle_work_done` calls against a real orchestrator PTY stub, so the orchestrator's feedback text is directly observable; no daemon socket or LLM).
- **Agent:** none (a raw no-echo `cat` orchestrator observer per scenario).
- **Asserts:** two scenarios ("baseline": two `work-done` calls from one worker pane with nothing injected between them; "collision": the same two calls, but between them a marker is written directly to the path the daemon's OWN first-call feedback names — parsed out of that feedback text, never assumed — standing in for #331's actual mechanism, a worker's unrelated generic file write landing on the daemon's path before a differently-sourced `work-done` arrives). Required: (1) the injected marker is still discoverable somewhere under `.dot-agent-deck/` after the second call (a recursive content scan, not a filename check); (2) the collision scenario's cumulative orchestrator feedback is NOT byte-identical to the baseline's — some observable signal must distinguish "something was already there" from "nothing was," rather than the fixed template the daemon composes today, which does not vary on that fact at all. Fixed: `handle_work_done` archives an existing file aside (`<name>.prev.md`) instead of overwriting it, and appends a sentence to the orchestrator feedback noting the archive when it happens.
- **Does not assert:** the wording of any announcement (only that the feedback differs at all); a daemon-log-only announcement invisible to the orchestrator pane (not observable from this harness); the exact archived-file naming scheme, if a fix chooses one.
- **Platform coverage:** mac+linux (unix-only — raw-mode shell observer).

##### orchestration/delegate/024 — `dot-agent-deck work-done --task-file <path>` must be refused, client-side, when `<path>` resolves inside the daemon's own `.dot-agent-deck/work-done-*.md` output namespace (upstream #331's own proposed fix).
- **Layer:** fast real-binary-subprocess integration (a real spawned daemon reachable over its hook socket, and the actual `dot-agent-deck` binary run as a subprocess for the CLI call under test — this behavior lives in `src/main.rs`'s CLI argument handling, not in daemon state, so an in-process call cannot observe it).
- **Agent:** none.
- **Asserts:** with a real, reachable daemon, `work-done --task-file .dot-agent-deck/work-done-coder.md` (a file that already exists in cwd) exits NON-ZERO — told apart from "daemon unreachable" by the daemon genuinely being up, so a non-refusing CLI would forward the message and exit 0 instead. A second, otherwise-identical call whose `--task-file` points OUTSIDE that namespace still exits 0, proving the refusal is scoped to the collision path rather than a blanket rejection of `--task-file`. Fixed: `src/main.rs`'s `work-done` arm refuses client-side, before `resolve_task`, whenever `--task-file` RESOLVES (via [`resolve_work_done_candidate`], following every symlink the filesystem allows) into a directory literally named `.dot-agent-deck` with a filename matching `work-done-*.md` — a glob, not an exact match, so it stays correct now that the daemon's own filename carries a per-pane digest suffix (fork #76). The rule is anchored to nothing but that resolved shape — not to the CLI process's own cwd or to the argument's own lexical parent — so it also covers this case's plain, no-symlink lexical match; the symlink-defeated (`/026`/`/027`) and cwd-drift (`/028`) variants of the same rule are pinned separately.
- **Does not assert:** the daemon-side archive/no-clobber behavior for a NON-`--task-file`-sourced collision (covered by `orchestration/delegate/023`); the exact refusal wording (stderr is not inspected — only the exit code).
- **Platform coverage:** mac+linux (unix-only — spawns a real daemon subprocess).

##### orchestration/delegate/025 — A THIRD `work-done` collision from the same pane must not destroy the report a SECOND collision already archived (PR #90 pre-merge review P1).
- **Layer:** fast/pure-ish state (real `AppState::handle_work_done` against one hand-registered pane, three calls in a row; no PTY spawn — same technique as `orchestration/delegate/022`).
- **Agent:** none.
- **Asserts:** one worker pane calls `handle_work_done` three times back to back with three distinctly marked reports (A, B, C). All three markers must remain discoverable somewhere under `.dot-agent-deck/` — a recursive scan of the directory's file contents, not a check on a specific filename. Fixed: `archive_existing_report` (`src/state.rs`) now allocates a UNIQUE archive slot per collision — trying `<file_name>.prev.md`, then `<file_name>.2.prev.md`, `<file_name>.3.prev.md`, … via `create_new` until one doesn't already exist — instead of reusing the fixed `<file_name>.prev.md` name every time, so the third call's archive no longer overwrites the second call's.
- **Does not assert:** the archive path/naming scheme a real fix picks (a uniquely-allocated path is one direction the review suggests, not a contract); the platform-specific case where `rename` refuses (rather than replaces) an existing destination — not expressible on Unix without inducing a categorically broader failure (see the tester's `work-done` report for why that was intentionally left unwritten rather than approximated).
- **Platform coverage:** mac+linux.

##### orchestration/delegate/029 — A Pi start role declared BEFORE a worker role in an orchestration's config can delegate before the worker's daemon-side registration exists, losing its first task with no retry (fork #92 P1, PR #93 pre-merge review).
- **Layer:** fast synthetic real-daemon integration through the actual production spawn path — a real `TabManager` + `EmbeddedPaneController` against a real M1.2 attach-protocol daemon, plus a real `pi` shell-script shim and a `cat` stand-in worker on `$PATH`; no vt100 attach, no LLM, no `e2e` feature gate. Uses the fork #92 `StartAgentRegistrationHook` test seam (`src/daemon.rs`/`src/daemon_protocol.rs`) to deterministically hold the worker's `StartAgent` registration open, rather than relying on real OS scheduling to probabilistically reproduce the race.
- **Agent:** none (synthetic — a real `pi`-named shell script that calls `get-seed` then `delegate --to coder` on boot, and a `cat` stand-in worker whose PTY echoes whatever the daemon injects).
- **Asserts:** while a registration gate holds the worker's `AppState` maps unpublished (armed by role name, not spawn order), the pi shim's seed-consumption marker must NOT appear; after releasing the gate, the marker must hold the exact seed text and the worker's PTY scrollback must contain the delegate pointer EXACTLY once — 0 is this defect's loss, 2+ would reproduce a duplicate-arming harm. Deliberately does not assert which role's `StartAgent` is issued first — see `orchestration/delegate/030` for the spawn-order-agnostic pane-id-indexing guard.
- **Does not assert:** the reordering mechanism itself (spawn-index plan, which role's `StartAgent` fires first); the exact wording of any daemon log line; the PTY-injection seed-fallback safety net's timing.
- **Platform coverage:** mac+linux.

##### orchestration/delegate/030 — Reordering `open_orchestration_tab`'s spawn loop (a Pi start role spawned last, per PR #93's approved fix) must not disturb `role_pane_ids`/`TabMembership.role_index`, which stay keyed by the config's DECLARED role position, not by spawn order (fork #92 P1 follow-up).
- **Layer:** fast in-process unit test — a real `TabManager` against a `MockPaneController` that records the command each `create_pane` call actually received, so the test can distinguish "declared index" from "spawn call order" even though today's code doesn't yet reorder them.
- **Agent:** none (synthetic — `MockPaneController`, no real process, no daemon).
- **Asserts:** `role_pane_ids[0]`/`role_pane_ids[1]` hold the pane ids actually minted for the start role's and the worker's commands respectively (declared order: start role first), and the stored `Tab::Orchestration.start_role_index`/`role_pane_ids` match. Not RED before PR #93's fix exists — today's spawn loop already iterates in declaration order, so this holds trivially; it is a regression guard for the coming reorder, not a pin on current behavior.
- **Does not assert:** the reorder mechanism itself or actual spawn call order (that's `orchestration/delegate/029`'s territory, exercised against a real daemon); layout, focus, or persisted-status ordering beyond `role_pane_ids`/`start_role_index`.
- **Platform coverage:** mac+linux.

##### orchestration/delegate/026 — `work-done --task-file` must be refused when the argument is a SYMLINK whose target resolves inside the daemon's own output namespace, even though the argument's own name does not match `work-done-*.md` (PR #90 pre-merge review P1).
- **Layer:** fast real-binary-subprocess integration (same technique as `orchestration/delegate/024` — a real spawned daemon reachable over its hook socket, and the actual `dot-agent-deck` binary run as a subprocess).
- **Agent:** none.
- **Asserts:** with a real, reachable daemon, a real file `work-done-coder.md` sits inside `.dot-agent-deck`, and a symlink at `external-report.md` (a name that does NOT match `work-done-*.md`, and does not live in `.dot-agent-deck`) points at it. `work-done --task-file external-report.md` must exit NON-ZERO — told apart from "daemon unreachable" the same way `024` is. A second, otherwise-identical call with no symlink at all must still exit 0. Fixed: `src/main.rs`'s `is_work_done_output_path` now resolves `task_file` via `resolve_work_done_candidate` (which canonicalizes as much of the path as the filesystem allows, following symlinks) BEFORE classifying it, so a symlink whose own name/location sit outside the namespace is still caught by what it resolves to.
- **Does not assert:** the exact refusal wording (stderr is not inspected — only the exit code); the intermediate-directory-symlink variant (covered by `orchestration/delegate/027`).
- **Platform coverage:** mac+linux (unix-only — spawns a real daemon subprocess; symlinks).

##### orchestration/delegate/027 — `work-done --task-file` must be refused when reached through an INTERMEDIATE DIRECTORY SYMLINK that aliases into `.dot-agent-deck`, even though the argument's own literal parent segment is not named `.dot-agent-deck` (PR #90 pre-merge review P1).
- **Layer:** fast real-binary-subprocess integration (same technique as `orchestration/delegate/024`/`026`).
- **Agent:** none.
- **Asserts:** with a real, reachable daemon, a real file `work-done-coder.md` sits inside `.dot-agent-deck`, and a directory symlink `alias` points AT `.dot-agent-deck` itself. `work-done --task-file alias/work-done-coder.md` must exit NON-ZERO, told apart from "daemon unreachable" the same way `024`/`026` are. A second, otherwise-identical call with no symlink at all must still exit 0. Fixed by the same resolution change as `026`: `resolve_work_done_candidate` canonicalizes the intermediate directory too, so what the literal parent segment ("alias") resolves to is what gets classified, not its own name.
- **Does not assert:** the exact refusal wording; the file-symlink variant (covered by `orchestration/delegate/026`); the cwd-independence of the resolved-parent comparison itself (covered by `orchestration/delegate/028`).
- **Platform coverage:** mac+linux (unix-only — spawns a real daemon subprocess; symlinks).

##### orchestration/delegate/028 — `work-done --task-file` naming the daemon's real work-done output must be refused even when the CLI process's OWN cwd is not the pane's cwd the daemon actually writes under — a regression guard for PR #90's re-review (upstream #331, fork #76).
- **Layer:** fast real-binary-subprocess integration (same technique as `orchestration/delegate/024`/`026`/`027`).
- **Agent:** none.
- **Asserts:** with a real, reachable daemon, a pane cwd holds the real `.dot-agent-deck/work-done-coder.md`, and a CHILD cwd of it (standing in for a worker that `cd`s into a subdirectory before invoking the CLI) carries a second, unrelated `.dot-agent-deck` directory of its own. Invoked FROM the child cwd, `work-done --task-file` naming the pane cwd's real output file — first as an ABSOLUTE path, then as a RELATIVE `../.dot-agent-deck/work-done-coder.md` path — must both exit NON-ZERO, told apart from "daemon unreachable" the same way `024` is. A harmless `work-done-*.md` inside the CHILD's own decoy `.dot-agent-deck` (not the pane's real output at all) is refused too — an accepted false positive, pinned deliberately because a decoy refusal is harmless while a missed real file destroys a report. A plain file outside any `.dot-agent-deck` is still accepted. This is a regression guard, not a RED pin: `024`/`026`/`027` all set the CLI's cwd equal to the namespace under test and so could not expose a cwd-anchored check; a pre-`545df7a` implementation that compared the resolved parent against `current_dir().join(".dot-agent-deck")` would pass the absolute- and relative-path cases here (the child cwd's OWN `.dot-agent-deck` resolves successfully and differs from the pane's), which this test would have caught red — the fixed check in place today compares the resolved parent's directory NAME only, anchored to no cwd at all, so it passes green.
- **Does not assert:** the symlink-defeated variants of the same rule (covered by `orchestration/delegate/026`/`027`); the exact refusal wording (stderr is not inspected — only the exit code).
- **Platform coverage:** mac+linux (unix-only — spawns a real daemon subprocess).

#### orchestration/identity

##### orchestration/identity/001 — Opening an orchestration whose form/display name (worktree dir basename) differs from the TOML config orchestration name stamps the CANONICAL config name as the daemon IDENTITY, not the basename (PRD #107 regression).
- **Layer:** L1 (in-process — dispatch the real `Action::SpawnPane` through `dispatch_action` against a recording `PaneController`; no daemon, no PTY).
- **Agent:** none (stub role commands; orchestration_config carries `name = "dot-agent-deck"` with a `coder` role at `clear = true`).
- **Asserts:** when the new-pane form's Name field defaults to the worktree basename (`dot-agent-deck-prd-113-foo`) while the config name is `dot-agent-deck`, every role pane's `TabMembership::Orchestration.name` (the IDENTITY the daemon's `lookup_orchestration_role` compares) equals the canonical config name `dot-agent-deck` — so the role resolves and `clear = true` respawn fires — while the tab TITLE (`Tab::Orchestration.name`) still shows the basename. Pre-fix the PRD #107 SpawnPane override copies the basename into `orch_config.name`, so the identity is the basename and the lookup misses.
- **Does not assert:** the daemon-side `pane_orchestration_map` recording or the live delegate respawn (L2 path); the on-disk config reload inside `lookup_orchestration_role`.
- **Platform coverage:** mac+linux+windows.

##### orchestration/identity/002 — Selecting the form's orchestration (Right arrow) suggests `<folder>-orchestrator-1` in the Name field in place of the bare directory basename it was pre-filled with, when no orchestration is live yet; a single further keystroke (Enter, no character typed) accepts it as-is at submit (fork#192 M1.0).
- **Layer:** L1 (`src/ui.rs`'s own `#[cfg(test)] mod tests` — the real `handle_new_pane_form_key` path against a `NewPaneFormState` built with the bare-basename pre-fill `transition_after_dir_pick` produces today; no daemon, no PTY).
- **Agent:** none.
- **Asserts:** a form built with Name `"myproj"` (the basename pre-fill) and one orchestration, after a Right-arrow selects that orchestration, has `form.name == "myproj-orchestrator-1"`, not `"myproj"`; submitting from there (Enter Mode→Name, Enter to submit) with no further edit yields `Action::SpawnPane` carrying `req.name == "myproj-orchestrator-1"` unchanged.
- **Does not assert:** the daemon round-trip `live_orchestration_cwds_and_titles()`/`transition_after_dir_pick` performs to learn live names (not unit-testable without a live daemon — covered informally by `orchestration/identity/004`'s real-binary path); rendering of the suggestion (no L1 render seam asserts the Name field's literal text here).
- **Platform coverage:** mac+linux+windows.

##### orchestration/identity/003 — With `<folder>-orchestrator-1` already live (injected via a new test-only `NewPaneFormState::with_live_orchestration_names` builder), selecting the orchestration suggests `<folder>-orchestrator-2` next, skipping the taken slot; submitting a name a live orchestration already holds is REFUSED — no `Action::SpawnPane`, form stays open (fork#192 M1.0).
- **Layer:** L1 (`src/ui.rs`'s own `#[cfg(test)] mod tests`, as `orchestration/identity/002`).
- **Agent:** none.
- **Asserts:** a form built with Name `"myproj"`, one orchestration, and `with_live_orchestration_names(vec!["myproj-orchestrator-1".into()])`, after a Right-arrow selects the orchestration, has `form.name == "myproj-orchestrator-2"`; overwriting the Name field back to the taken `"myproj-orchestrator-1"` and submitting via `handle_new_pane_form_key` does NOT yield `Action::SpawnPane`, and `ui.mode` stays `UiMode::NewPaneForm`.
- **Does not assert:** the exact refusal UI copy/rendering (covered by `orchestration/guard/002`); what N is counted over across multiple cwds (PRD's own open question, scoped global-over-live).
- **Platform coverage:** mac+linux+windows.

##### orchestration/identity/004 — Driving the real binary through TWO orchestration opens in the SAME directory, each accepting the form's suggested Name with a single Enter (no character typed), lands both as visible tabs with DISTINCT labels (`<basename>-orchestrator-1`, `<basename>-orchestrator-2`) — never the identical basename-derived title recorded twice, the fork #74 collision fork#192 exists to stop (fork#192 M1.0).
- **Layer:** L2 (PTY-attached real binary via `TuiDeck`; stand-in `cat` agent, no real LLM tokens spent, no credentials required).
- **Agent:** none (both roles of the `orch-deck` fixture run `cat`).
- **Asserts:** after opening the fixture's one orchestration twice from the Dashboard (`Ctrl+n` → confirm dir → select orchestration → accept the suggested Name unedited → submit), the rendered tab strip contains both `" <basename>-orchestrator-1 "` and `" <basename>-orchestrator-2 "` as distinct substrings, where `<basename>` is the real launch directory's basename read from `TuiDeck::workdir()`. The second wait barrier is `wait_for_string(&second_label)` itself (fork#192 review round 2 F7 — the prior `wait_for_string(" worker ")` was vacuous there: the Dashboard already shows " worker " from the FIRST orchestration's panes before the second open even starts, since the fixture's second role is literally named "worker").
- **Does not assert:** the suggestion/refusal MECHANISMS in isolation (covered by `orchestration/identity/002`/`003`); worktree creation (no worktree slug is typed on this path); ownership marker content (covered by `worktree/reclaim/022`/`023`).
- **Platform coverage:** mac+linux (PTY-attached, `#[cfg(feature = "e2e")]`, as the other `e2e_orchestration_*.rs` files).

##### orchestration/identity/005 — The Name field stops accepting input at the daemon's `DISPLAY_NAME_MAX_LEN` (128-byte) cap, so a name the form lets the user type always survives `is_valid_display_name`; a name at the cap still round-trips into the uniqueness check against an identical live name (fork#192 review round 2, F1).
- **Layer:** L1 (`src/ui.rs`'s own `#[cfg(test)] mod tests`, driving `handle_new_pane_form_key` directly; no daemon, no PTY).
- **Agent:** none.
- **Asserts:** typing (via repeated `KeyCode::Char` events, simulating a paste) `DISPLAY_NAME_MAX_LEN + 40` bytes into a focused, empty Name field yields `form.name.len() <= DISPLAY_NAME_MAX_LEN`; the resulting name satisfies `crate::agent_pty::is_valid_display_name`; and with that exact string pushed into `live_orchestration_names` and the form's one orchestration selected directly, `form.name_collision()` is true.
- **Does not assert:** WHERE the cap is enforced (keystroke-time rejection vs. submit-time truncation are both consistent with the assertions here); the daemon-side `spawn_agent` null-and-keep path itself (`src/agent_pty.rs`, unit-covered separately); multi-byte/Unicode boundary truncation (the paste here is single-byte ASCII, matching the reviewer's own GitHub-issue-title example).
- **Platform coverage:** mac+linux+windows.

##### orchestration/identity/006 — The uniqueness gate (`name_collision`) and the suggestion loop (`suggest_orchestration_name`) normalize on the SAME trim the sink (`build_new_pane_request`) applies — not a looser, untrimmed comparison (fork#192 audit F1).
- **Layer:** L1 (`src/ui.rs`'s own `#[cfg(test)] mod tests`, driving `handle_new_pane_form_key` and the form's builder methods directly; no daemon, no PTY).
- **Agent:** none.
- **Asserts:** (a) with `myproj-orchestrator-1` live, a form whose typed Name is `"myproj-orchestrator-1 "` (that exact string plus one trailing space) reports `name_collision() == true`, and submitting it via `handle_new_pane_form_key` (Enter, Enter) does NOT yield `Action::SpawnPane`; (b) a form built for directory `/tmp/ myproj` (basename carries a leading space) with `myproj-orchestrator-1` already live (the TRIMMED identity a previous open recorded) has `suggest_orchestration_name() == "myproj-orchestrator-2"`, not a candidate built from the untrimmed basename.
- **Does not assert:** the click-door (`Action::FormSubmit`) refusal for the same untrimmed-name case (covered generically by `orchestration/guard/003`, using a plain taken name rather than a whitespace variant); marker-truncation collisions past 200 chars (audit F8, out of scope — subsumed by `orchestration/identity/005`'s 128-byte cap).
- **Platform coverage:** mac+linux+windows.

##### orchestration/identity/007 — Landing on an orchestration must not silently destroy a Name the user actually typed; a still-untouched suggestion must keep being replaced on a later landing (fork#192 review F4 / audit F7).
- **Layer:** L1 (`src/ui.rs`'s own `#[cfg(test)] mod tests`, driving `handle_new_pane_form_key` directly through the Right/Tab/Backspace/Char/BackTab/Left key sequence a real user's round-trip produces; no daemon, no PTY).
- **Agent:** none.
- **Asserts:** selecting the form's one orchestration (Right), tabbing to Name, backspacing out the suggestion and typing `"hotfix-triage"`, then Shift+Tabbing back to Mode and pressing Left (lands on "No mode", a no-op landing) then Right again (lands back on the orchestration) leaves `form.name == "hotfix-triage"` — unchanged by either landing. A SEPARATE case in the same test: with `myproj-orchestrator-1` live, selecting the orchestration leaves the field holding the untouched suggestion `myproj-orchestrator-2`; widening `live_orchestration_names` to also include `myproj-orchestrator-2` (simulating what a later daemon read would show) and repeating the Left/Right landing updates the field to `myproj-orchestrator-3` — an untouched suggestion still gets replaced.
- **Does not assert:** the chip-click arm (`Action::FormSelectMode`, `src/ui.rs:9744`) that calls the same `suggest_name_if_orchestration_selected` — same production function as the keyboard path, not independently pinned; persistence of the distinction across a form rebuild (there is none — the state lives only in the live `NewPaneFormState`).
- **Platform coverage:** mac+linux+windows.

##### orchestration/identity/008 — For a given live orchestration, the string `mark_worktree_owned` writes into the worktree's `created-by:` marker and the string every role pane's `AgentSpawnOptions::owner` carries are the LITERAL SAME computed string, from one source — not two derivations of one input (fork #166 M2.4).
- **Layer:** fast synthetic direct-call unit test, embedded in `src/ui.rs`'s own `#[cfg(test)] mod tests`, driving the real `Action::SpawnPane` path (as `orchestration/identity/001`/`022`) against a real git repo + worktree via `CapturingPaneController`, extended to record `AgentSpawnOptions::owner` per spawned pane.
- **Agent:** none.
- **Asserts:** after a live orchestration spawn, `owner_of(repo, worktree)` (the marker read-back) is `Some`; every role pane's recorded `owner` equals that exact value; at least one role pane was spawned to compare.
- **Does not assert:** anything about whether the recorded `owner` value reaches a real spawned process's environment — `CapturingPaneController` is a test double whose `create_pane_with_options` records `opts.owner` and returns, so this test stops exactly one hop short of `DOT_AGENT_DECK_WORKTREE_OWNER` (PR #215 reviewer F2 / auditor H1: `EmbeddedPaneController::create_stream_pane` dropped `opts.owner` on the floor entirely at the SHA this test was written, and this test stayed green through that bug). See `orchestration/identity/009` for the real-seam coverage. Also does not assert: the sentinel/absent-variable refusal behaviour of `worktree list --mine` itself (covered by `worktree/reclaim/027`/`028`/`029`); the issue-dispatch path's equivalent invariant (`issue-dispatch:<task>#<issue>` is threaded through the same `SpawnRequest::owner` field in-process, in the same function that writes the marker — not independently pinned here); session-restore, which passes the PERSISTED `owner` value through unchanged rather than recomputing it (see `session/restore/017`/`018`); the live-create path also has no single end-to-end test reaching a real spawned process's environment the way restore now does — the two create-path links share `create_pane_with_options`, so the chain is tight, but the asymmetry is worth naming rather than leaving implicit.
- **Platform coverage:** mac+linux.

##### orchestration/identity/009 — `AgentSpawnOptions::owner` reaches a genuinely spawned process's own environment as `DOT_AGENT_DECK_WORKTREE_OWNER` — not merely a test double's recorded field (PR #215 reviewer F2 / auditor H1).
- **Layer:** e2e (`tests/e2e_worktree_owner_env.rs`, `#[cfg(feature = "e2e")]` + `#[cfg(unix)]`). Spawns the real `dot-agent-deck daemon serve` BINARY as a subprocess (`common::spawn_daemon_serve`), attaches a real `EmbeddedPaneController` to it over its real Unix attach socket (the same client code the TUI uses), and calls `create_pane_with_options` with `AgentSpawnOptions::owner` set — the same production call path `src/ui.rs`/`src/tab.rs` use, spawning a genuine `portable_pty` child via `agent_pty::spawn`.
- **Agent:** none (a plain `echo` shell command, no LLM).
- **Asserts:** the spawned child's own stdout — read back over the attach socket via `common::wait_for_pane_text_on`, itself reading `$DOT_AGENT_DECK_WORKTREE_OWNER` out of the child's OWN environment — contains the exact owner string `create_pane_with_options` was called with. This is the join `orchestration/identity/008`'s mock cannot exercise: producer (`AgentSpawnOptions::owner`) and reader (`worktree/reclaim/024`–`028`, which set the variable by hand on a subprocess) were each tested; this is the missing middle.
- **Does not assert:** anything about the marker file or `owner_of` (covered by `orchestration/identity/008` and the `worktree/reclaim` series); LLM-agent behaviour (no agent is spawned); the interactive form/restore paths that populate `AgentSpawnOptions::owner` in the first place (covered by `orchestration/identity/001`–`008` and the PRD's M2.4 milestone text) — this test starts from an already-populated `AgentSpawnOptions`, proving only that the value, once set, survives to the child's real environment.
- **Platform coverage:** mac+linux.

#### orchestration/guard

##### orchestration/guard/001 — Opening an orchestration in a cwd that already hosts a live orchestration shows a non-blocking shared-resource warning pointing at worktrees (PRD #140).
- **Layer:** L1 (in-process `TestBackend` via `render_new_pane_orchestration_guard_to_buffer`; no PTY, no subprocess).
- **Agent:** none (the render seam supplies synthetic live-daemon orchestration cwd records).
- **Asserts:** an orchestration selected for a cwd matching an existing live orchestration renders a warning containing `.dot-agent-deck` and `worktree` while retaining `[Submit]`; the same form for a fresh cwd renders neither warning substring.
- **Does not assert:** exact warning copy or styling; daemon `list_agents` transport; worktree creation; blocking spawn behavior (the warning is informational).
- **Platform coverage:** mac+linux+windows.

##### orchestration/guard/002 — A name collision (the typed Name matches a name a live orchestration already holds) renders a BLOCKING refusal on the same guard seam `guard/001` uses; `[Submit]` renders present-but-INERT rather than removed, and `[Cancel]`'s clickable rect does not overlap it; a distinct typed name renders normally (fork#192 M1.0; contract corrected twice in review round 2 — see below).
- **Layer:** L1 (`src/ui.rs`'s own `#[cfg(test)] mod tests`, driving the private `render_new_pane_form` directly through a `TestBackend` to access the click-hit-test rects it returns; no PTY, no subprocess). **Moved from `tests/render_orchestration_guard.rs`** (fork#192 review round 2, F3/F8) — the public buffer-only seam `render_new_pane_orchestration_name_collision_to_buffer` this test previously used discards those rects, which the corrected contract needs.
- **Agent:** none (a synthetic typed Name plus synthetic live-orchestration names, as before).
- **Asserts:** on collision, the rendered buffer contains `[Submit]` (present, not removed) AND the blocking-refusal copy (`"already in use"`, `NAME_COLLISION_WARNING`); the click-hit-test rects `render_new_pane_form` returns do NOT contain an `Action::FormSubmit` entry (inert — excluded from hit-testing, the same mechanism `render_modal_button_row` already applies to any disabled button); WITHIN that same colliding render, `[Cancel]`'s clickable rect does not intersect `[Submit]`'s on-screen rect (located by scanning the rendered text grid, since an inert button carries no click rect). A distinct typed name's rects DO contain `Action::FormSubmit`.
- **Does not assert:** exact refusal copy beyond the `"already in use"` needle; the Name-field text content itself; the suggestion logic that produces a non-colliding name in the first place (`orchestration/identity/002`/`003`); the `Action::FormSubmit` DISPATCH-level guard (covered by `orchestration/guard/003`); any invariance of the modal's size or `[Cancel]`'s position across the colliding vs. distinct-name renders (withdrawn — see below).
- **CORRECTED CONTRACT (fork#192 review round 2, reviewer F3):** the original assertion was `[Submit]` GONE from the row entirely. `render_modal_button_row` lays buttons out left-aligned, so removing `[Submit]` slides `[Cancel]` into `[Submit]`'s exact former screen cells — a muscle-memory click on where Submit has always been now hits the destructive Cancel instead, discarding the whole form. The render seam already has an inert-but-visible mechanism for exactly this (a disabled button renders dimmed and is excluded from the click rects); the corrected contract uses that instead of removing the button. Precedent for revising a catalog assertion in place: `worktree/reclaim/008`.
- **CORRECTED CONTRACT, second pass (fork#192 review round 2, tester self-correction after the coder's fix round):** the first corrected contract still compared `[Cancel]`'s rect ACROSS the colliding and distinct-name renders and asserted equality. That comparison is wrong, not the production behavior: the two renders are legitimately different-sized popups — the collision's `NAME_COLLISION_WARNING` feeds both `desired_w` and `desired_h` (`src/ui.rs`'s `modal_rect` call site), which the distinct-name render never shows, so `modal_rect` re-centers differently. PRD #140's existing same-cwd warning reflows the modal the same way; this codebase deliberately does not guarantee modal-centring invariance under a warning appearing. The actual hazard (reviewer F3) never depended on cross-render position match — with `[Submit]` present-but-inert it still holds its row slot and lays out before `[Cancel]`, so `[Cancel]` structurally cannot occupy `[Submit]`'s cells regardless of the popup's size or position. The contract now pins that directly: WITHIN the colliding render, `[Cancel]`'s rect must not intersect `[Submit]`'s rect. Same precedent as the first correction: `worktree/reclaim/008`.
- **Platform coverage:** mac+linux+windows.

##### orchestration/guard/003 — The `Action::FormSubmit` click door independently refuses a colliding name — defense-in-depth for a state the render seam (`orchestration/guard/002`) is meant to make unreachable (fork#192 review F9).
- **Layer:** L1 (`src/ui.rs`'s own `#[cfg(test)] mod tests`, dispatching the real `Action::FormSubmit` through the production `dispatch_action` against a `CapturingPaneController`; no daemon, no PTY).
- **Agent:** none.
- **Asserts:** dispatching `Action::FormSubmit` against a form whose typed Name (`myproj-orchestrator-1`) matches a live orchestration name results in zero recorded orchestration identities on the `CapturingPaneController` (no pane spawned), `ui.mode` still `UiMode::NewPaneForm`, and `ui.new_pane_form` still `Some` (not taken).
- **Does not assert:** the render-layer inertness of the `[Submit]` button itself (covered by `orchestration/guard/002`); the Enter-key submit door (covered by `orchestration/identity/003`).
- **Platform coverage:** mac+linux+windows.

#### orchestration/lock

##### orchestration/lock/001 — `scope_command_entry_lock` claims `Ctrl+E` only on an Orchestration tab in command mode.
- **Layer:** L1 (pure function, `src/ui.rs`'s own `#[cfg(test)]` module — the scoping helper is module-private).
- **Agent:** none.
- **Asserts:** table-driven over the full cross product of `is_orchestration_tab` (true/false) × every `UiMode` variant × the action being `ToggleOrchestrationLock`, some other action (`Quit`), or `None`: the toggle survives ONLY at `(true, UiMode::Normal)`; every other action passes through untouched in EVERY cell (including `(false, non-Normal)`, ruling out a blanket "drop the action" implementation); `None` in always yields `None` out. The `UiMode` list is guarded by an exhaustive match so a new variant cannot silently drop out of the cross product.
- **Does not assert:** anything about a real pane — this is a mechanism test, present so a later failure localises. The real-pane proof is `orchestration/lock/009`.
- **Platform coverage:** mac+linux+windows.

##### orchestration/lock/002 — A freshly opened orchestration tab observes the deck-global command-entry lock LOCKED.
- **Layer:** L1 (real `Action::SpawnPane` dispatched through `dispatch_action` against a capturing pane controller).
- **Agent:** none (two-role `cat` stub orchestration config).
- **Asserts:** after a real spawn, the active tab is a `Tab::Orchestration` and `ui.command_entry_locked` is `true`. Locked-by-default is load-bearing: a lock you must remember to engage protects nothing.
- **Does not assert:** the gate's own behaviour (`orchestration/lock/006`/`008`); persistence across restarts (the lock is not persisted).
- **Platform coverage:** mac+linux+windows.

##### orchestration/lock/003 — `Ctrl+e` resolves to the toggle from command mode and flips the deck-global lock both ways.
- **Layer:** L1 (`key_action_for_mode`, the production `KeyEvent -> Action` seam, plus two real `dispatch_action` calls).
- **Agent:** none.
- **Asserts:** with the DEFAULT keybinding config, `Ctrl+e` in `UiMode::Normal` resolves to `Action::ToggleOrchestrationLock`; dispatching it once unlocks and twice re-locks `ui.command_entry_locked`.
- **Does not assert:** the full `is_orchestration_tab × mode` matrix (that is `orchestration/lock/001`); a user-remapped chord.
- **Platform coverage:** mac+linux+windows.

##### orchestration/lock/004 — Toggling the lock on ANY Orchestration tab changes what EVERY Orchestration tab observes, and a new tab adopts the current value.
- **Layer:** L1 (two real orchestration tabs spawned through `dispatch_action`, plus real `switch_to` round-trips).
- **Agent:** none.
- **Asserts:** tab A starts locked and toggling on A unlocks; a brand-new tab B ADOPTS the unlocked value rather than resetting to locked; switching back to A observes the same unlocked value; toggling FROM B and returning to A shows A observing B's change. Pins that unlocking never has to be repeated per tab.
- **Does not assert:** that the lock reaches beyond Orchestration tabs — deck-global storage moves where the value lives, not how far it reaches (`orchestration/lock/005`).
- **Platform coverage:** mac+linux+windows.

##### orchestration/lock/005 — Dashboard and Mode tabs are never gated, even while the deck-global lock is engaged.
- **Layer:** L1 (`gate_pane_input_key` called directly against a real Dashboard tab and a real spawned Mode tab).
- **Agent:** none.
- **Asserts:** with `ui.command_entry_locked = true` (the strongest case) and an EMPTY status map (so the `WaitingForInput` carve-out cannot fire and the pass-through can only come from the tab-kind match), `Action::ForwardToPane` passes through UNCHANGED on both tab types. Guards the obvious mis-reading of deck-global storage as deck-global reach.
- **Does not assert:** the Orchestration-tab gate itself (`orchestration/lock/006`).
- **Platform coverage:** mac+linux+windows.

##### orchestration/lock/006 — A locked non-orchestrator pane reporting `WaitingForInput` passes keystrokes through, and the gate re-engages the moment the status clears.
- **Layer:** L1 (`gate_pane_input_key` against a real two-role orchestration and a focus-echoing pane controller).
- **Agent:** none (synthetic `pane_id -> SessionStatus` maps).
- **Asserts:** walking both edges on the SAME worker pane — no recorded status (dropped, the baseline) → `WaitingForInput` (passes through unchanged) → `Working` (dropped again, so the hole cannot outlive the status that opened it). Also that the orchestrator pane's own input is never gated whatever status is attached to it (proving the never-gated rule is not reordered behind the new check), and that an unlocked deck ignores `WaitingForInput` entirely.
- **Does not assert:** that any particular agent actually emits `WaitingForInput` — that is the agent's contract, not this feature's. An agent that never reports it gets no carve-out and still needs a deliberate unlock.
- **Platform coverage:** mac+linux+windows.

##### orchestration/lock/007 — An ambiguous pane status (two sessions sharing one `pane_id`) DENIES the carve-out — fail closed, not fail open.
- **Layer:** L1 (`build_pane_status_for_gate` feeding the unchanged `gate_pane_input_key`, against a real locked orchestration with its worker focused).
- **Agent:** none (two synthetic `AppState`s standing in for the daemon-observed collision).
- **Asserts:** two sessions colliding on one `pane_id` and DISAGREEING on `WaitingForInput`-ness resolve to no exemption and the keystroke is dropped; a single, unambiguous `WaitingForInput` session still resolves to `WaitingForInput` and still passes the keystroke through — so failing closed cannot be bought by breaking the carve-out outright. The guard has to live in the producer: a `HashMap<&str, SessionStatus>` cannot represent the collision, so by the time the gate reads the map the ambiguity is already gone.
- **Does not assert:** the collision semantics of `build_pane_status` itself, which is deliberately left as-is — its consumers are cosmetic, and only the lock's feed hardens. The rule here is "any duplicate", not "any disagreeing duplicate".
- **Platform coverage:** mac+linux+windows.

##### orchestration/lock/013 — A `WaitingForInput` written by a producer that named no generation does NOT open the lock (issue #398, PR #443 review).
- **Layer:** L1 (in-process gate resolution against a real orchestration tab).
- **Agent:** none (a synthetic single `WaitingForInput` session plus the pane's untagged-status mark).
- **Asserts:** with exactly ONE unambiguous `WaitingForInput` session on the focused locked worker pane, `build_pane_status_for_gate` still omits the pane while its status provenance is untagged, and `gate_pane_input_key` drops the keystroke; clearing the mark — what an identified hook does — restores the carve-out and the keystroke passes through unchanged.
- **Does not assert:** the duplicate-session denial, which is a separate rule (`orchestration/lock/007`); that untagged status is hidden from cards or borders (it deliberately still renders).
- **Platform coverage:** mac+linux+windows.

##### orchestration/lock/008 — On a real locked orchestration tab the orchestrator's own input still reaches its PTY while a worker's does not, and `Ctrl+d`,`Ctrl+e` reverses that.
- **Layer:** L2 PTY-attached (the real binary through the vt100 `TuiDeck` harness).
- **Agent:** none (fixture `tests/fixtures/orch-deck`: two `cat` stub roles, no LLM tokens spent).
- **Asserts:** a sentinel typed into the focused orchestrator pane echoes on the grid even though the deck is LOCKED by default; after jumping to the non-orchestrator `worker` role, a second sentinel does NOT appear within 2s; the dropped keystroke's status message reads the corrected `Pane locked — Ctrl+d, Ctrl+e, Ctrl+d to type here` hint (issue #302 defect 2 — the old wording stopped one keypress short, naming only `Ctrl+d then Ctrl+e to unlock`); after `Ctrl+d` → `Ctrl+e` → `Ctrl+d`, a third sentinel typed into the still-focused worker pane echoes normally.
- **Does not assert:** what a real agent does with the forwarded bytes (`orchestration/lock/012`); the `WaitingForInput` carve-out (`orchestration/lock/011`).
- **Platform coverage:** mac+linux.

##### orchestration/lock/009 — `Ctrl+e` reaches a focused role pane's PTY in `PaneInput`, is claimed by the deck in command mode, and toggles the lock there.
- **Layer:** L2 PTY-attached (the real binary through the vt100 `TuiDeck` harness; rendered-grid observation).
- **Agent:** none (fixture `tests/fixtures/orch-deck`: two `cat` stub roles, no LLM tokens spent).
- **Asserts:** with a partial line typed into the focused orchestrator pane, `Ctrl+e` makes a literal `^E` appear immediately after it — the tty line discipline's own caret echo (`ECHOCTL`), proving `0x05` genuinely reached the PTY rather than being claimed as `Action::ToggleOrchestrationLock`. Then `Ctrl+d` into command mode and `Ctrl+e` again: the deck reports `Pane entry: unlocked`, NO second `^E` joins the first (claimed there means not forwarded — the mirror of the first half), and jumping to the worker role with `2` lets a sentinel reach its PTY, proving the chord still toggles the lock from the mode it IS claimed in.
- **Does not assert:** what a given program does with `0x05` once it arrives — that is the program's business. The oracle is deliberately the terminal's caret echo, not readline: an earlier revision drove a real `bash --noprofile --norc -i` role and asserted readline's `beginning-of-line`/`end-of-line` cursor moves, which fails outright wherever bash is built without readline (this repo's own devbox bash offers no `emacs` option, so `Ctrl+a` echoed `^A` and moved the cursor two columns the wrong way).
- **Platform coverage:** mac+linux.

##### orchestration/lock/010 — Global chords still fire while a worker pane is focused and the deck is locked.
- **Layer:** L2 PTY-attached.
- **Agent:** none (fixture `tests/fixtures/orch-deck`).
- **Asserts:** with the non-orchestrator worker role focused and the deck LOCKED, `Ctrl+t` (`toggle_layout`) surfaces its `Layout:` status message — global chords resolve before the PTY-forward fallback the lock gates. Regression guard against an overly-broad gate.
- **Does not assert:** the layout change itself (covered by the layout tests).
- **Platform coverage:** mac+linux.

##### orchestration/lock/011 — On a real locked pane, a reported `WaitingForInput` opens the gate and the status clearing closes it again.
- **Layer:** L2 PTY-attached; the status is injected as a bare `AgentEvent` over the hook socket — the SAME wire the real `dot-agent-deck agent-event` CLI rides.
- **Agent:** none, deliberately. A real agent would self-skip wherever credentials are absent, leaving this headline behaviour with ZERO automated CI coverage; the status arrives over the genuine production wire either way, and what a stand-in gives up is only proof that some particular agent emits that status.
- **Asserts:** the baseline drop with no status recorded; then, after injecting `WaitingForInput` for the worker's real `(pane_id_env, agent_id)` pair, a keystroke reaches the worker's PTY and echoes; then, after injecting `Thinking`, a re-focused worker drops keystrokes again. The injector blocks on `ListAgents`' live-status join rather than the daemon's broadcast, so the daemon's own state — not just its wire — is known to reflect the change before focus/echo is asserted.
- **Does not assert:** that any real agent emits `WaitingForInput`; the auto-focus steering that the same status also drives (`orchestration/focus/*`) — the worker is re-focused explicitly so this cannot ride that as a proxy.
- **Platform coverage:** mac+linux (unix-only: the injector writes to a Unix-domain hook socket).

##### orchestration/lock/012 — A REAL Claude agent never receives a directive typed at a locked worker pane, and does receive it once unlocked.
- **Layer:** L2 PTY-attached, real-agent tier. Runtime-skipped when the `claude` CLI or credentials are absent.
- **Agent:** REAL Claude Code (Haiku, `claude-haiku-4-5-20251001`, `--allowedTools Bash`) as the non-orchestrator `worker` role; the orchestrator stays `cat` (already proven never-gated) to keep the run to a single real agent turn. Fixture `tests/fixtures/orch-lock-live`.
- **Asserts:** a create-a-sentinel-file directive typed into the locked worker pane never results in that file existing (20s); after `Ctrl+d` → `Ctrl+e` → `Ctrl+d`, a second directive with a DIFFERENT sentinel does result in its file being created (120s); and the first sentinel STILL does not exist afterwards, proving gated keystrokes are dropped outright rather than queued for delivery once unlocked. On-disk file presence is the observable, so the assertion survives LLM phrasing and terminal-redraw variance.
- **Does not assert:** anything when skipped — where credentials are absent this test executes nothing, so `orchestration/lock/008`/`011` carry the CI-visible coverage.
- **Platform coverage:** mac+linux (real-agent tier is local-only).

##### orchestration/lock/015 — With NO project config discoverable anywhere (fork #346), the command-entry lock still holds.
- **Layer:** L2 (PTY end-to-end).
- **Agent:** none (`orch-deck` fixture, two stub `cat` roles). Launched with `DOT_AGENT_DECK_FEATURES_CONFIG` pointed at a path that provably does not exist (a `project` subdirectory never created under a real tempdir), forcing `features_config_path()`'s override branch to resolve to a missing file — the same `load_features_file` "not found" branch a real ancestor walk hits when no `.dot-agent-deck.toml` exists anywhere above the process cwd, which is fork #346's reported scenario (a deck launched from the maintainer's home directory).
- **Asserts:** on a real orchestration tab, a keystroke typed at the focused non-orchestrator worker pane does NOT reach its PTY and the `Pane locked` status message appears; the orchestrator pane's own input still reaches its PTY untouched. The lock is unconditional (fork #346's graduation), matching `orchestration/lock/008`'s behaviour with no flag or project config involved at all.
- **Does not assert:** the `Ctrl+e` binding resolution in this same no-config scenario (`orchestration/lock/016`); the `WaitingForInput` carve-out (`orchestration/lock/006`/`011`).
- **Platform coverage:** mac+linux.

##### orchestration/lock/016 — With NO project config discoverable anywhere (fork #346), `Ctrl+e` from command mode on a real Orchestration tab is still claimed as `Action::ToggleOrchestrationLock` — the mirror of `orchestration/lock/009`'s command-mode proof, with no flag involved at all.
- **Layer:** L2 (PTY end-to-end).
- **Agent:** none (`orch-deck` fixture, two stub `cat` roles). Same `DOT_AGENT_DECK_FEATURES_CONFIG`-pointed-at-a-missing-path mechanism as `orchestration/lock/015`.
- **Asserts:** `Ctrl+d` into command mode then `Ctrl+e` produces the deck's `Pane entry: unlocked` report. `Ctrl+e`'s binding resolution is unconditional (fork #346's graduation), so it still resolves with no config present. Also reads the persistent LOCKED/UNLOCKED chip immediately right of the mode chip before and after the toggle, confirming `lock_context_for_tab`'s render path is likewise unconditional with no project config present anywhere — the no-config coverage `orchestration/lock/018` does not itself provide.
- **Does not assert:** the forwarding gate itself (`orchestration/lock/015`); the caret-echo proof that `0x05` reaches a focused pane's PTY in `PaneInput` mode (`orchestration/lock/009`, unaffected by this flag either way since the orchestrator pane is never gated); cell styling of the chip (text-only via `snapshot_grid()`, same limitation as `orchestration/lock/018`).
- **Platform coverage:** mac+linux.

##### orchestration/lock/017 — A bracketed paste aimed at a locked worker pane is dropped exactly like an ordinary keystroke, and the drop leaves no side effects behind (issue #302 defect 1).
- **Layer:** L2 PTY-attached (the real binary through the vt100 `TuiDeck` harness).
- **Agent:** none (fixture `tests/fixtures/orch-deck`: two `cat` stub roles, no LLM tokens spent).
- **Asserts:** a bracketed paste (`\x1b[200~…\x1b[201~`) aimed at the focused non-orchestrator worker pane does NOT reach its PTY while LOCKED — `Event::Paste` previously called `embedded.write_raw_bytes` directly, bypassing `gate_pane_input_key` entirely. Two ordering details a careless fix gets wrong are pinned separately: after building real scrollback in the worker pane, re-locking, and scrolling back via the mouse wheel (the one scroll door that survives a `PaneInput` re-entry), a further dropped paste does NOT yank the view back to live output (`reset_scrollback` must not fire on a drop); and, in one burst, a dropped paste immediately followed by an unlock-and-forward of a BARE Enter (with its visible marker sent only AFTER the Enter — a marker sent before it would itself debounce the Enter unconditionally per `src/ui.rs`'s own `enter_following_recent_keystroke_sleeps_at_least_debounce_minus_elapsed` unit test, masking the thing being tested) reaches the grid no more than 100ms slower than a plain control keystroke's own round trip measured moments later on the same runner, proving the drop left no `last_pane_keystroke_at` stamp to trip the 150ms `SUBMIT_DEBOUNCE` — a relative comparison rather than a fixed bound, since render/harness-polling latency for the whole burst can itself exceed 150ms on a loaded CI runner (observed 433–678ms for this test's prior, marker-before-Enter form), which would make a fixed bound either flaky or vacuous.
- **Does not assert:** what a real agent does with a delivered paste (no real-agent variant exists for paste, unlike `orchestration/lock/012`'s keystroke coverage); the `WaitingForInput` carve-out for a paste (not requested by issue #302); an ABSOLUTE bound on debounce latency (deliberately relative — see Asserts).
- **Platform coverage:** mac+linux.

##### orchestration/lock/018 — A persistent LOCKED/UNLOCKED chip renders immediately right of the mode chip on the bottom bar in both lock states, and `Ctrl+e` is documented in the help overlay (issue #302 defect 3).
- **Layer:** L2 PTY-attached — NOT the L1 widget snapshot originally requested. No existing `_to_buffer` seam threads Orchestration-tab-and-lock context into `render_bottom_bar` (every current seam builds a bare `UiState` with no such parameter, and `UiState`/`render_bottom_bar` are private to `src/ui.rs`); adding that parameter is itself the production change this test exists to drive, which is out of a tester's reach. Consequently this pins TEXT CONTENT only — it cannot verify the task's styling requirement (reversed+bold locked vs dim unlocked), which remains open for an L1 snapshot once the seam exists.
- **Agent:** none (fixture `tests/fixtures/orch-deck`: two `cat` stub roles, no LLM tokens spent).
- **Asserts:** in `PaneInput` on a real orchestration tab, the text immediately following the ` TYPING ` mode chip on the bottom bar reads ` LOCKED ` while the command-entry lock is engaged (the default); after `Ctrl+d` → `Ctrl+e` → `Ctrl+d`, the same position reads ` UNLOCKED ` — both states are asserted, not just the locked default, so an indicator that only ever renders one state cannot pass. The `?` help overlay documents `Ctrl+e`.
- **Does not assert:** cell styling (reversed+bold / dim) — text-only via `snapshot_grid()`; `bottom_bar_rows`' height-budget accounting for the new chip (left for a follow-up L1 test once the render seam exists).
- **Platform coverage:** mac+linux.

##### orchestration/lock/019 — At 80 columns the dropped-keystroke unlock hint is fully visible, not truncated by the right-aligned `[Command Mode Ctrl+D]` button (issue #302 review finding F2).
- **Layer:** L2 PTY-attached — NOT the L1 widget snapshot originally requested. No existing `_to_buffer` seam threads an arbitrary status message into `render_bottom_bar`'s `PaneInput` arm (every current seam builds a bare `UiState` with `status_message` left at its default `None`, and `UiState::new`/`status_message` are private to `src/ui.rs`); adding that seam is itself a production-code change, out of a tester's reach — the same reasoning `orchestration/lock/018` already records for the LOCKED/UNLOCKED chip. Drives the real running binary at 80x40 instead, through the same dropped-keystroke path `orchestration/lock/008` pins at 120x40.
- **Agent:** none (fixture `tests/fixtures/orch-deck`: two `cat` stub roles, no LLM tokens spent).
- **Asserts:** at 80 columns, after a dropped keystroke at the locked worker pane, the full corrected hint (`Pane locked — Ctrl+d, Ctrl+e, Ctrl+d to type here`, 49 chars) appears as a contiguous string in the grid — not overwritten by the right-aligned `[Command Mode Ctrl+D]` button (21 chars) sharing the same row. The chip band widened 9→17 and the message 42→49 chars in this PR, moving the collision threshold from 72 to 87 columns, so 80 columns (previously safe) now truncates the message's tail (`type here`).
- **Does not assert:** any width other than 80; a fix's exact shape (shortened message vs. clipped `Paragraph`) — either satisfies this assertion.
- **Platform coverage:** mac+linux.

#### orchestration/focus

##### orchestration/focus/001 — Auto-focus follows the lowest-order `WaitingForInput` role pane on the active tab, and never touches another tab.
- **Layer:** L1 (`TabManager::auto_focus_waiting_pane` driven with synthetic `SessionStatus` maps; `src/tab.rs`).
- **Agent:** none (three-role orchestration: `orchestrator` < `alpha` < `beta`).
- **Asserts:** nothing waiting leaves manual focus alone; a newly-waiting pane steals focus; ties resolve to the LOWEST-order waiting pane, even stealing focus mid-input from a higher-order pane that is itself still waiting; an already-lowest focused pane is a no-op (no flicker); resolving the focused pane advances to the next-lowest still-waiting pane. A second orchestration tab then proves a background tab's newly-waiting pane has zero effect and never flips which tab is active.
- **Does not assert:** the all-clear return move (`orchestration/focus/002`); ordering by wait time — ascending `role_pane_ids` order is the contract, and "longest blocked first" would need a new per-pane timestamp.
- **Platform coverage:** mac+linux+windows.

##### orchestration/focus/002 — The all-clear move back to the orchestrator is edge-triggered, fires exactly once per waiting episode, and re-arms for the next.
- **Layer:** L1 (the real per-frame sequence — `observe_waiting_panes`, then `auto_focus_waiting_pane` → `auto_focus_all_clear` — gated exactly as the `src/ui.rs` render-loop site gates it).
- **Agent:** none.
- **Asserts:** a manual focus is left alone while nothing is waiting; a newly-waiting pane steals focus; once it resolves, focus snaps back to the orchestrator role exactly ONCE — not on every subsequent frame, and not again for a later manual focus change until a NEW pane starts and resolves waiting. A level-triggered version would pin focus to the orchestrator every frame and the human could never look at another pane at all. A second (background) orchestration tab proves the move never touches an inactive tab or switches which tab is active.
- **Does not assert:** the single-frame episode (`orchestration/focus/003`); the render-loop application of the returned id (`orchestration/focus/007`).
- **Platform coverage:** mac+linux+windows.

##### orchestration/focus/003 — A waiting episode observed in a SINGLE frame still edge-triggers the all-clear move.
- **Layer:** L1 (the real per-frame sequence).
- **Agent:** none.
- **Asserts:** a role goes `WaitingForInput` on one frame and resolves by the next, with no intervening frame in which it is both still waiting and already focused. The first frame steers focus onto it — so `auto_focus_waiting_pane` WINS the chain and `auto_focus_all_clear` never runs on the only frame the episode is observed — and the second frame must still fire the all-clear. This is why the observation lives OUTSIDE the chain: recording the edge inside `auto_focus_all_clear` loses it entirely and strands focus on the resolved pane.
- **Does not assert:** the multi-frame episode (`orchestration/focus/002`), which always has a still-waiting frame in between and is exactly where a dropped edge hides.
- **Platform coverage:** mac+linux+windows.

##### orchestration/focus/004 — While LOCKED, focus visits every waiting role in ascending order and returns to the orchestrator on the all-clear.
- **Layer:** L1 (the real per-frame sequence; four-role orchestration `orchestrator` < `alpha` < `beta` < `gamma`).
- **Agent:** none.
- **Asserts:** all three non-orchestrator roles go `WaitingForInput` together and focus lands on `alpha` first, advancing to `beta` then `gamma` as each resolves, then returning to the orchestrator once nothing is left waiting, with a further quiet frame moving nothing. Three concurrent waiters are needed: with fewer, "picked one" and "advanced through them in order" are indistinguishable.
- **Does not assert:** the unlocked half (`orchestration/focus/005`).
- **Platform coverage:** mac+linux+windows.

##### orchestration/focus/005 — While UNLOCKED no auto-focus branch fires at all, and re-locking must not replay a stale all-clear edge.
- **Layer:** L1 (the real per-frame sequence with the call site's `locked` gate modelled explicitly, plus `TabManager::clear_waiting_pane_latch`).
- **Agent:** none.
- **Asserts:** a waiting pane already in flight does not steal focus while unlocked, and its later resolution fires no all-clear either, so a manual focus choice survives the whole stretch untouched. Then THE STALE-LATCH ASSERTION: re-locking must NOT fire an all-clear for the episode the human already handled by hand — without the latch clearing, `observe_waiting_panes` compares its frozen `had_waiting_pane == true` against the now-idle status and misreads it as a fresh edge, yanking focus off where the human left it. Finally, re-locking resumes normal steering and all-clear pinning for a fresh episode.
- **Does not assert:** an episode that both begins AND ends inside the unlocked stretch — that case is already safe with no fix (the chain is fully skipped, so nothing touches the latch), which is why this test is written against the STRADDLING trace instead. A test written against the simpler wording passes without the fix and proves nothing.
- **Platform coverage:** mac+linux+windows.

##### orchestration/focus/006 — The locked→unlocked transition clears EVERY Orchestration tab's latch, not just the active one.
- **Layer:** L1 (two orchestration tabs; the real per-frame sequence with the `locked` gate modelled).
- **Agent:** none.
- **Asserts:** tab A latches a waiting episode while active and locked; the user switches to tab B and unlocks, so the deck-global toggle's latch-clearing call fires with B active, not A; A's worker resolves unobserved; on re-lock and return to A, A's first locked frame must treat the resolved role as old news rather than a fresh edge, leaving focus where the user left it. This is `orchestration/focus/005`'s bug reappearing across tabs whenever the clearing is scoped to the active tab instead of the deck-global lock it compensates for.
- **Does not assert:** the mechanism used to reach that outcome — only that every Orchestration tab's edge state is reset on the transition.
- **Platform coverage:** mac+linux+windows.

##### orchestration/focus/007 — The experimental command-entry-lock surface's whole focus contract on the real binary.
- **Layer:** L2 PTY-attached (the real binary through the vt100 `TuiDeck` harness), asserted purely on the rendered grid via the expanded-pane header `┌<role>`, which only the currently focused role ever draws.
- **Agent:** none (fixture `tests/fixtures/orch-focus-lifecycle`: `orchestrator` + `alpha` + `beta`, all `printf`+`sleep` stubs). Three roles are required: the "manual focus sticks" half needs a role OTHER than the one going `WaitingForInput`, since where the focused and waiting role are the same pane a genuine stick is indistinguishable from `auto_focus_waiting_pane`'s own same-pane no-op. `WaitingForInput` is injected over the hook socket exactly as `orchestration/lock/011` does.
- **Asserts:** (1) with the experimental command-entry-lock surface enabled, a freshly opened tab starts LOCKED and shows the orchestrator's expanded box; (2) injecting `WaitingForInput` for `alpha` visibly steers focus onto ITS box; (3) injecting `Thinking` visibly returns focus to the orchestrator — the all-clear edge; (4) `Ctrl+d`,`Ctrl+e` surfaces `Pane entry: unlocked`; (5) manually jumping to `beta` and then injecting a fresh `WaitingForInput`/`Thinking` pair for `alpha` moves focus NOWHERE — `beta`'s box survives both events, and a sentinel typed at the end appears inside `beta`'s own box, proving it still holds live PTY focus rather than merely still being drawn.
- **Does not assert:** the `TabManager`-level contract in isolation (`orchestration/focus/001`-`006`); the keystroke gate (`orchestration/lock/*`).
- **Platform coverage:** mac+linux (unix-only: the injector writes to a Unix-domain hook socket).

##### orchestration/focus/008 — The waiting-focus branch defers a focus steal, rather than applying it immediately, while a keystroke is still queued for the currently-focused waiting pane.
- **Layer:** L1 (in-process unit test; `src/tab.rs`, alongside `orchestration/focus/001`-`006`).
- **Agent:** none (mock `PaneController`; synthetic `SessionStatus` map, no panes/PTYs).
- **Asserts:** with a real `TabManager`-opened 3-role Orchestration tab (`orchestrator` < `alpha` < `beta`), `beta` (higher role order) goes `WaitingForInput` and steals focus with no input pending, as `orchestration/focus/001` pins; `alpha` (LOWER role order than `beta`) then ALSO goes `WaitingForInput` on a frame where `input_pending` is true (modeling a keystroke still queued for `beta`) — the steal to `alpha` must be deferred, returning `None` and leaving focus on `beta`, not yanked away from the pane the queued keystroke is aimed at; once `input_pending` clears on a later frame, the deferred steer to `alpha` must still fire, proving the guard DEFERS the move rather than dropping it, mirroring `TabManager::auto_focus_all_clear`'s existing "no one-shot latch" contract. Drives `TabManager::auto_focus_locked(pane_status, input_pending)`, the seam that folds both `auto_focus_waiting_pane` and `auto_focus_all_clear` behind ONE shared `input_pending` guard mirroring the real per-frame call site's shape.
- **Does not assert:** the real `src/ui.rs` per-frame call site actually computing `input_pending` from `crossterm::event::poll` or applying the result via `pane.focus_pane` (out of L1 `TabManager` reach — it would need a PTY-attached L2 test, and an L2 test was evaluated and rejected: the underlying terminal race is not economically reproducible there, since it requires a keystroke to be sitting in the terminal's input queue on the exact frame a lower-order pane transitions to `WaitingForInput`); the deck-global lock gate itself (`ui.command_entry_locked`, covered by `orchestration/focus/005`/`006`); the multi-waiter ordering contract, covered exhaustively by `orchestration/focus/001`/`004`.
- **Platform coverage:** mac+linux+windows.

#### orchestration/layout

##### orchestration/layout/001 — Seven decks fit the single-column orchestration card area without scrolling (PRD #147).
- **Layer:** L1 (ratatui `TestBackend`, buffer inspection + capacity math via the public `rendered_height` seam).
- **Agent:** none.
- **Asserts:** in the ~34%-width single-column orchestration card area at a typical ~48-row card height, the renderer's `visible_rows = available / card_height` fits all 7 decks with no scrolling and the 7th deck actually renders in the visible slice; a much larger deck count (20) still engages scrolling, so right-sizing the card height does not remove the scroll fallback.
- **Does not assert:** the full orchestration-tab frame (tab bar, side panes, stats bar); the `ORCHESTRATION_LEFT_PERCENT` width split or `grid_columns` thresholds (out of scope per PRD #147).
- **Platform coverage:** mac+linux+windows.

##### orchestration/layout/002 — `Ctrl+l` resolves to a split-cycle `Action` on an orchestration tab, and walking the `SplitStage` resolver through the full 3-stage cycle (Default 34/66 -> Narrow 25/75 -> Hidden 0/100 -> Default) pins each stage's geometry (PRD #336, extended to three stages by PRD #361 Item 4; PRD #387 M2/M3 makes the stage itself deck-global).
- **Layer:** L1 (pure-data `compute_frame_layout` geometry + `key_action_for_mode`, the `KeyEvent -> Action` seam the live event loop uses; no PTY, no TestBackend render).
- **Agent:** none.
- **Asserts:** an orchestration tab's default frame geometry is the fixed 34/66 split (`dashboard_area` / `panes_area` widths); resolving a simulated `Ctrl+l` `KeyEvent` through `key_action_for_mode` with the default keybinding config yields `Some` action; walking the pure `next_split_stage` resolver Default -> Narrow -> Hidden -> Default and recomputing the frame geometry at each step — via the single deck-global `ACTIVE_SPLIT_STAGE` thread-local, the SAME mirror `dashboard/layout/001` sets — pins the 25/75 Narrow split, the 0/100 Hidden split (sidebar fully collapsed, pane column full-width), and the wrap back to the original 34/66 Default split.
- **Does not assert:** the visible rendered grid (covered by the PTY-attached `tabs/orchestration/006`); per-tab scoping across multiple orchestration tabs (covered by `tabs/orchestration/006`); Dashboard-tab geometry (covered by `dashboard/layout/001`).
- **Platform coverage:** mac+linux+windows.

##### orchestration/layout/003 — A brand-new orchestration tab ADOPTS the ONE deck-global split stage another already-open orchestration tab was just toggled to, including at spawn time (PRD #387 M4a, inverting the #336 spawn-order regression this test used to guard as per-tab isolation).
- **Layer:** L1 (`dispatch_action` dispatched directly against a `CapturingPaneController`; no PTY, no TestBackend render).
- **Agent:** none.
- **Asserts:** dispatch `Action::SpawnPane` to open orchestration tab A at the default split, dispatch `Action::CycleSplitStage` to narrow the single deck-global `ui.split_stage`, then set `ACTIVE_SPLIT_STAGE` to simulate the render loop syncing it from `ui.split_stage` (the state a follow-up spawn would observe before the next frame runs); dispatch a second `Action::SpawnPane` to open a brand-new orchestration tab B. `ui.split_stage` still reads Narrow after B opens — there is no per-tab field left to isolate B from A, and opening a tab must adopt the current stage, not reset it; B's role panes' recorded `AgentSpawnOptions::cols` must equal the Narrow-derived pane-column width (calibrated against tab A's own Default-derived cols via `compute_frame_layout`, the same math `orchestration_role_pane_dims` mirrors), not tab A's original Default-derived cols — a mismatch means an agent wrapping its output to the wrong column.
- **Does not assert:** the visible rendered grid or a live render-loop resync (the thread-local's post-spawn correction on the next frame is out of scope — this test pins the SPAWN-TIME dims only); Mode-tab or Dashboard-tab spawn dims (Mode tabs have no split; Dashboard-tab adoption/sharing is covered by `orchestration/layout/006`).
- **Platform coverage:** mac+linux+windows.

##### orchestration/layout/004 — In a 7-role orchestration tab with `PaneLayout::Stacked`, the focused pane's rect covers the full pane-column height and no collapsed title-bar frames are drawn for the other 6 roles (PRD #311).
- **Layer:** L1 (in-process `compute_frame_layout` + `render_frame` driven through a real `ratatui::Terminal<TestBackend>`, via `EmbeddedPaneController::for_render_only_tests()`; no PTY, no subprocess). Lives in `src/ui.rs`'s own `#[cfg(test)]` module (same pattern as `tabs/orchestration/003-005`) because the geometry helpers under test (`pane_stack_rects`, `stacked_expanded_index`, `render_terminal_panes`) are module-private and unreachable from `tests/*.rs`.
- **Agent:** none (7 synthetic role pane ids, no backing PTYs).
- **Asserts:** with no pane explicitly focused (so `stacked_expanded_index` falls back to the first role, `orchestrator`), the expanded role's OUTER rect height equals the full pane-column height with no rows ceded to collapsed frames; none of the other 6 roles' pane ids appear anywhere in the rendered grid (i.e. no `Borders::TOP` collapsed title block is drawn for a non-focused pane).
- **Does not assert:** PTY resizing of the reclaimed area (`resize_panes_to_layout`); mode-tab side-pane geometry (covered by `tabs/mode/001`); the sidebar deck-card capacity math (covered by `orchestration/layout/001`).
- **Platform coverage:** mac+linux+windows.

##### orchestration/layout/005 — `scope_split_stage`, the pure function replacing the inline `claims_ctrl_l` match, claims `Action::CycleSplitStage` only in `UiMode::Normal` on a tab type with a sidebar split, and passes every other action through untouched everywhere else (PRD #387 M1).
- **Layer:** L1 (pure-function unit test; `src/ui.rs`'s own `#[cfg(test)]` module, no PTY, no TestBackend render).
- **Agent:** none.
- **Asserts:** table-driven over the full cross product of `has_split_sidebar` (true/false) x every `UiMode` variant (enumerated via an exhaustive match so a future variant can't silently drop out of the table) x the action being `Some(Action::CycleSplitStage)`, `Some(Action::Quit)` (a representative other action), or `None`: `CycleSplitStage` survives ONLY at `(has_split_sidebar = true, mode = UiMode::Normal)` and is un-resolved to `None` at every other cell; `Action::Quit` passes through completely untouched at EVERY cell, including `(has_split_sidebar = false, mode != Normal)` — the case that rules out a blanket "drop the action" implementation; `None` in always yields `None` out.
- **Does not assert:** dispatch through the real `KeyEvent -> Action` resolver or a live orchestration tab's rendered geometry (covered by `orchestration/layout/002`); the real-pane proof that the byte reaches a focused role pane's PTY (covered by the PTY-attached `tabs/orchestration/024`).
- **Platform coverage:** mac+linux+windows.

##### orchestration/layout/006 — A single `Ctrl+l` press moves the ONE deck-global split stage shared by every open tab, regardless of type — Dashboard and Orchestration each keep their OWN Default ratio (33/67 vs 34/66) but converge on the SAME Narrow (25/75) and Hidden (0/100) splits (PRD #387 M2/M3, decisions 2/3).
- **Layer:** L1 (`dispatch_action` dispatched directly against a `CapturingPaneController`, plus pure-data `compute_frame_layout` geometry; no PTY, no TestBackend render).
- **Agent:** none.
- **Asserts:** with a Dashboard tab (always tab 0) and a real, active Orchestration tab both open, dispatching `Action::CycleSplitStage` ONCE — while the ORCHESTRATION tab is active — advances the single `ui.split_stage` field, and BOTH tab types' `compute_frame_layout` geometry (sourced from the same `ACTIVE_SPLIT_STAGE` thread-local) move together even though only one tab was active when the chord fired; walking the full Default -> Narrow -> Hidden -> Default cycle pins the convergence-plus-divergence pattern — DIFFERENT Default ratios (Orchestration 34/66, Dashboard 33/67) but the SAME shared Narrow (25/75) and Hidden (0/100) splits at every other stage. This is the assertion that catches an implementation which accidentally flattens both tab types onto one ratio instead of sharing only the stage: a single flattened constant would fail the Default-stage divergence checks while still passing the Narrow/Hidden convergence checks alone.
- **Does not assert:** the visible rendered grid or per-tab isolation of the stage (both now the SAME shared value by design — the isolation case this test's inverse used to guard is covered historically by the now-inverted `orchestration/layout/003`); a live render-loop resync beyond the single manual thread-local sync per stage.
- **Platform coverage:** mac+linux+windows.

#### orchestration/dispatch

##### orchestration/dispatch/001 — An agent-callable `dispatch --orchestration <name>` makes a full orchestration TAB surface live on the deck, and that orchestration can actually DELEGATE to its own workers (PRD #220 / #222).
- **Layer:** L2 PTY-attached (`TuiDeck` on the `orch-deck` fixture) driving the REAL `dot-agent-deck dispatch` and `dot-agent-deck delegate` CLIs against the deck's own hook socket, exactly as an agent in a pane does — so the CLI parse, the wire hop, the daemon's shape resolution, the role spawn, the live tab surfacing and the delegate routing are all in the path.
- **Agent:** none — the fixture's two roles run `cat`, which stays alive on stdin. No LLM tokens.
- **Asserts:** the CLI exits 0; the orchestration TAB labelled `demo-orch` appears on the tab strip within 90s WITHOUT a reconnect; the sibling worktree `../<repo>-dispatch-<name>` exists; and `.dot-agent-deck/orchestrator-context.md` inside it carries the delegation protocol plus the caller's task under `## Your task`. Then the DELEGATION round trip, in both directions of the comparison: the same orchestration is ALSO opened the normal way (`Ctrl+N`) as a **control**, and `delegate --to worker` is run from each orchestrator's pane id — both workers must receive the daemon-authored pointer `Read .dot-agent-deck/worker-task-worker.md for your task.` in their own PTY. The control runs FIRST and its failure message says so, because a broken control means the harness is wrong and the dispatched result proves nothing. Finally the LOUD-FAILURE half: `delegate` from a pane the daemon holds no role for, and `delegate --to <role that has no pane>` from a valid orchestrator, must each exit NON-ZERO with stderr naming the pane id / the role — while a HALF-landed `delegate --to worker --to <role that has no pane>` must exit ZERO and name BOTH sides, because the worker really did receive it and a retry aimed at the whole delegation would dispatch it twice.
- **Why it exists:** three PRD #220 defects shipped green because the only dispatch coverage asserted a file on disk or the worktree's existence — never the tab the user actually looks at. A dispatched orchestration that comes up with no tab, or with an orchestrator that was never told it is one, passes every other assertion in this suite (the `reproduce-first` skill / CONTRIBUTING's "Reported bugs start with a failing test"). It then caught a FOURTH, reported by a user: a dispatched orchestration came up perfect and completely inert. `crate::spawn::spawn` reaches `spawn_agent` directly, and only the `AttachRequest::StartAgent` handler was populating the daemon's `pane_role_map` / `orchestrator_pane_ids`, so every `delegate` from a dispatched (or scheduled, or issue-dispatched) orchestrator was dropped with `delegate from unknown pane` — while `delegate` itself, being fire-and-forget, printed nothing and exited 0, so the orchestrator announced phantom progress and waited forever. Both halves are pinned here; reverting either fix alone turns this test red (verified), and reverting the registration fix now fails at the CLI's exit code rather than 90s later at the pointer, because the two fixes compose.
- **Does not assert:** the roles' own output, or an agent DECIDING to delegate — `cat` cannot initiate one, so the test invokes the real CLI with the orchestrator's `DOT_AGENT_DECK_PANE_ID` exactly as that pane's shell would (`orchestration/dispatch/002` owns the real-agent decision path, and the worker actually doing the delegated work). Also not asserted: the `work-done` return edge; cross-orchestration isolation (`orchestration/route/001` owns that).
- **Platform coverage:** mac+linux.

##### orchestration/dispatch/002 — A dispatched orchestration whose roles are REAL agents brings every role in the toml up as a live agent, names each one on its own card, and can delegate work its worker actually DOES (PRD #220).
- **Layer:** L2 PTY-attached (`TuiDeck` on the `dispatch-orch-real` fixture) driving the REAL `dot-agent-deck dispatch <name> --orchestration real-team` CLI against the deck's own hook socket, then reading the daemon's `ListAgents`, the rendered orchestration tab, and the dispatched worktree on disk.
- **Agent:** THREE real, fully interactive Claude Code panes pinned to Haiku (`orchestrator`, `coder`, `reviewer`) — no `-p`, no `cat` stand-in. Cost is three cold boots plus two short turns (the orchestrator decides to delegate; the coder does the work).
- **Asserts:** the CLI exits 0; a pane exists for every toml role; every role's own PTY shows a REAL agent booted (the Claude Code banner, which no shell or `cat` can print); and on the dispatched orchestration's TAB every role appears on a card as `<AgentType> · <role>`, so the user can tell the orchestrator from a worker. Then the whole point of an orchestration: the real orchestrator is asked (through the daemon's production `WriteAndSubmit`, the same path a user's keystrokes take) to delegate a sentinel-file task to its `coder`, and the `coder` must actually create the uniquely-named sentinel in the dispatched worktree. That last assertion is the user's altitude — "I dispatched an orchestration and the team got something done" — and it is the half only real agents can show: an orchestrator *deciding* to shell `dot-agent-deck delegate`, and a worker receiving the task and acting on it. Verified load-bearing: with the daemon-side role registration reverted, the coder never does the work and the assertion fails at its full 300s budget.
- **Why it exists:** `orchestration/dispatch/001`'s `cat` roles start instantly, need no credentials and have no cold start, so they cannot tell an agent from a `$SHELL` — which is how three PRD #220 defects shipped green. This test found a fourth: a dispatched orchestration labelled every card with claude's session UUID (`ClaudeCode · 6134822e-f2`) while the daemon knew all three role names, because only the interactive `Ctrl+n` path set a per-role display name. Fixed by naming the role on the spawn AND emitting the per-role synthetic `SessionStart` that carries the name to an already-attached TUI.
- **Does not assert:** `AgentRecord.live` — deliberately. It is `Some(Idle)` for every role within ~1.5s of the dispatch, before a byte reaches any of those PTYs (measured), so it is a pane-level fact and an assertion on it is vacuous. Also not asserted: the `work-done` RETURN edge (the worker's completion signal back to the orchestrator, and the feedback line the daemon writes into the orchestrator pane); delegation to more than one role, or fan-out to `reviewer`, which stays a booted-but-unused role here; cross-orchestration routing isolation (`orchestration/route/001` owns that); and the `delegate` CLI's failure exit codes, which `orchestration/dispatch/001` pins cheaply without spending tokens.
- **Platform coverage:** mac+linux.

#### dispatch/close

##### dispatch/close/001 — A dispatched single-agent card closes on the FIRST confirmed Ctrl+W, instead of surviving until the user closes it a second time (PRD #220 follow-up).
- **Layer:** L2 PTY-attached (`TuiDeck` on the `minimal` fixture) driving the REAL `dot-agent-deck dispatch --single` CLI, then closing the resulting card through the production Ctrl+W → confirm path.
- **Agent:** a REAL interactive Claude Code (Haiku) as the dispatched unit, launched through a **wrapper script** (`default_command = "agent-wrapper"`), never prompted — it only has to be running when the close lands, so the cost is one cold boot and no turns. The caller pane is `cat`; it is the caller, not the thing under test. The wrapper is load-bearing, not convenience: it mirrors the reported config, where every command is `devbox run agent-<role>`, which the deck cannot infer an agent type from and therefore does not wrap. A bare `claude` IS recognised and takes a different path through the session machinery — which is exactly why an earlier `cat`-based version of this test passed while the reported bug was live.
- **Stand-in, named:** a PATH `git` stub that sleeps on `status --porcelain` (and ONLY on that — the dispatch's own `git worktree add` runs at full speed). It supplies the one property of a real dispatched worktree a fixture cannot cheaply have: an agent has been working in it, so the status walk takes seconds, not milliseconds.
- **Asserts:** the dispatched agent really starts (its own PTY prints the Claude Code banner — NOT the card's `ClaudeCode` badge, which is inferred from the command at spawn and is on the card before the agent has executed anything); the CALLER card (which owns no worktree) closes on its first confirm — the control, so a later failure is attributable to the dispatched card specifically; then, after ONE confirmed close, NO card for the dispatched worktree remains. Matched on the worktree basename from the card's `Dir:` line rather than on its title, because the ghost card is titled `pane-sched-…` and a name-bound needle misses it.
- **Why it exists:** a user reported closing a dispatched agent leaving its card behind. It reproduced THREE independent defects, and the failure message distinguishes the first two by whether the daemon still holds the agent: (a) a daemon-spawned card has no local pane until focused, so `close_pane` returned `Pane <id> not found`, the PRD #92 F4 policy preserved the card, and the agent kept running; (b) with that fixed, the daemon still awaited the worktree cleanup before answering, blowing the TUI's 5s `CTRL_W_STOP_TIMEOUT`; (c) with BOTH fixed and a real agent behind a non-inferable command, the close removed only the session its card was built from and left the pane's *other* session rendering as a ghost card badged `No agent` — the symptom as reported. Reverting any one fix alone turns this test red (verified).
- **Does not assert:** the worktree's own removal (`KeepIfDirty` leaves a dirty one in place by design); the orchestration close path, where the last role's close is the cleanup trigger.
- **Platform coverage:** mac+linux.

##### dispatch/close/002 — A dispatched worktree the daemon kept on tab close (instead of removing it) reaches the user naming WHERE it survives, not just that something was kept (PRD 236).
- **Layer:** L1 (`process_pending_kept_worktrees` — the render-loop drain step — called directly against a `SharedState`/`UiState` pair; no PTY, no TestBackend render, no daemon).
- **Agent:** none.
- **Asserts:** with the pending-kept-worktrees queue empty, draining is a no-op (no spurious warning); after queuing a `WorktreeKeptNotice` the way the event subscriber does on a daemon `BroadcastMsg::WorktreeKept` (PRD 236 M1.1's typed `RemoveOutcome::Kept` reaching the wire), draining pushes a message into `ui.session_warnings` that contains the retained worktree's own path; and the queue is left empty afterward, so the same notice cannot be delivered twice.
- **Why it exists:** PRD 236 unifies the two dispatch producers on `RemovalPolicy::KeepIfDirty` (`worktree/reclaim/045`/`046`), so a dispatched worktree with uncommitted work now survives Ctrl+W instead of being force-removed. That is only a real recovery path if the user can find the worktree afterward — a message saying only "a worktree was kept" with no path is exactly as useless as no message at all, since the daemon's own worktree-registry layout is not something a user is expected to know.
- **Does not assert:** the daemon-side broadcast plumbing itself (`BroadcastMsg::WorktreeKept` construction, the `event_tx.send` in `daemon_protocol.rs`'s close handler, or the `apply_broadcast`/`queue_kept_worktree` hop that gets a notice from the wire into this queue) — those have no PTY/daemon harness in this fast-tier file; nor the exact rendered wording of `ui.session_warnings`'s eventual flush (unchanged, pre-existing `eprintln!` behavior).
- **Platform coverage:** mac+linux+windows.

#### orchestration/route

##### orchestration/route/001 — Two tabs of the SAME orchestration opened in the SAME directory are separate routing groups: each orchestrator's delegate reaches only its own worker and each worker's work-done reaches only its own orchestrator, with no cross-delivery in either direction (PRD #140 M5.1). [reel]
- **Layer:** L2 PTY-attached (the REAL `dot-agent-deck` binary driven through the vt100 `TuiDeck` harness — records a `full-stream.cast`, so it is demo-reel-eligible per PRD #180). Both tabs are opened through the PRODUCTION new-pane flow (`Ctrl+N` → picker → Space → Right → Enter → Enter) against the deck's own cwd, so their `(name, cwd)` identities are byte-identical and only the per-tab `orchestration_id` (PRD #140 M1.2, echoed back through `StartAgent` → daemon registry → `ListAgents`) tells them apart. Each delegate is issued by a REAL orchestrator agent that shells `dot-agent-deck delegate` after the production `WriteAndSubmit` RPC types the directive into its pane; each work-done is issued by the REAL worker agent from its task-file footer. Per-pane observation is the daemon's own `AttachRequest::Snapshot`, normalized wrap-insensitively (escape sequences stripped, then everything but `[A-Za-z0-9._/-]` dropped) so a pointer hard-wrapped inside a narrow role card still matches. The freshly-built binary's dir is prepended to the deck → daemon → agents PATH; Claude project-trust for the per-test tempdir cwd is seeded into the deck's HOME after launch (the cwd does not exist before it), so the six panes clear their first-run gates with no keystroke.
- **Fixture:** `tests/fixtures/orchestration-route` — one `[[orchestrations]] name = "route-iso"` with THREE roles (`orchestrator` start + `coder` + `reviewer`), all REAL interactive Haiku `claude` (`--allowedTools Bash Read Write`, no `-p`), workers at `clear = false` so their agent ids and scrollback stay stable across the delegate. Three roles rather than two because `.dot-agent-deck/worker-task-{role}.md` is keyed by ROLE within a cwd (PRD #140 keeps that layer explicitly out of scope), so two same-cwd tabs sharing a role name share that file (`work-done-{role}-<pane digest>.md` no longer does — upstream #331 + fork #76 added a per-pane digest — but the fixture keeps the role split regardless, since it is what makes occurrence-counting in a redrawing agent TUI unnecessary): driving tab A through `coder` and tab B through `reviewer` makes every no-cross-delivery check a presence/absence question about a pane that would otherwise have received NOTHING, and makes the two work-done feedback strings role-qualified and thus distinguishable inside one orchestrator pane.
- **Agent:** REAL Claude Code (Haiku, `claude-haiku-4-5-20251001`) ×6 interactive role panes across the two tabs; four short turns actually run (two orchestrators delegate, two workers create one file each). Flaky-tolerant pre-PR tier (real LLM) — run once, not looped (rule 4/5). Runtime-skipped (Decision 26) when the `claude` CLI/credentials are absent.
- **Asserts:** the second open of the same orchestration in the same directory renders PRD #140 M4.0's non-blocking same-cwd warning pointing at `/worktree-prd` (the M4.0 surface, live in the real form rather than through the L1 render seam); the daemon reports two orchestration tabs with DISTINCT `orchestration_id`s and three role panes each; then, with a task started in EACH tab CONCURRENTLY (the issue's own repro, and the state in which the pre-#140 `HashSet`-ordered work-done lookup was most non-deterministic), tab A's delegate pointer `worker-task-coder.md` lands in tab A's `coder` pane and NEVER in tab B's identically-named `coder` pane; tab A's coder really does its own task (uniquely-named sentinel `route_alpha_5f3c.txt` plus the daemon-written `.dot-agent-deck/work-done-coder-<pane digest>.md`); its work-done feedback (`Worker coder has completed their task`) reaches tab A's orchestrator pane and NEVER tab B's; and symmetrically for tab B → `reviewer` (`worker-task-reviewer.md`, `route_beta_9d21.txt`, `work-done-reviewer-<pane digest>.md`, `Worker reviewer has completed their task`), with a final sweep re-checking all four absences after both chains have run.
- **Does not assert:** WHICH pane wrote a shared coordination file — `worker-task-{role}.md` is role-and-cwd keyed by design (PRD #140 "Deferred: full same-directory isolation"; `work-done-{role}-<pane digest>.md` is additionally pane-keyed as of upstream #331 + fork #76), so the routing proof is the per-pane delegate/work-done delivery, not the file contents; the hydration round trip of two same-`(name, cwd)` tabs across a detach/reattach (M3.1, covered by the `partition_hydrated_panes` unit tests); the `NameCwd` older-client fallback (M5.2, the cross-version manual test); the exact task text each orchestrator forwards (only the literal sentinel filename has to survive LLM phrasing); the deterministic routing decision itself (mutation-checked unit tests on `delegate_targets` / `orchestrator_for_worker` in `src/state.rs`).
- **Platform coverage:** mac+linux (real-agent tier is local-only per Decision 8).
- **Cost note:** four short interactive Haiku turns (two delegates, two one-file tasks) — well under Decision 23's <$0.05/run bound.

##### orchestration/route/002 — Detach/reattach of two same-`(name, cwd)` orchestration tabs rebuilds TWO distinct tabs, each keeping its own routing group, while a token-less (pre-#140) pair still rebuilds as ONE (PRD #140 M3.1).
- **Layer:** L1/synthetic (warm in-process daemon + real attach socket, no PTY-attached binary and no LLM). Drives the production reattach chain end to end: `start_agent` stores `TabMembership` on the daemon's `AgentRecord` → `EmbeddedPaneController::hydrate_from_daemon` reads it back through `ListAgents` + `validate_tab_membership` → `partition_hydrated_panes` buckets by `OrchestrationIdentity` → `resolve_orch_config_for_hydration` / `OrchestrationConfig::synthesize_from_bucket_metadata` → `TabManager::open_orchestration_tab_with_existing_role_panes`. Synthetic is the right tier because the claim is about a hydration round trip, not about agent behaviour; the real-agent two-tab case is `orchestration/route/001`, which never detaches.
- **Agent:** none (six `sh -c 'sleep 30'` stand-ins: `orchestrator` + `coder` for each of tab A, tab B, and a token-less legacy pair, all sharing one orchestration name and one cwd).
- **Asserts:** every pane round-trips its own `orchestration_id` through the daemon echo; the partition yields THREE buckets (tab A, tab B, legacy) rather than one merged bucket, each holding exactly its own two panes; the two tokened buckets' `OrchestrationIdentity`s differ while the token-less bucket falls back to `NameCwd { name, cwd }`; rebuilding every bucket produces three orchestration tabs with each pane owned by exactly one tab; and (PRD #140 review) a dead role slot in each tokened tab mints a DISTINCT synthetic dead-slot id with its own placeholder card — pre-fix the `(cwd, orchestration_name)`-keyed id aliased across the two partitioned tabs onto one shared card — while the legacy identity keeps the pre-review byte format.
- **Does not assert:** live delegate/work-done routing across the reattach (that is `orchestration/route/001` and the `src/state.rs` routing unit tests); PTY attach or scrollback replay of the rebuilt panes; the same-cwd spawn warning (`orchestration/guard/001`); the on-disk snapshot restore branch.
- **Platform coverage:** linux+mac (the suite is `#![cfg(unix)]` — the mock attach servers bind Unix-domain sockets; Windows port tracked by #164).

#### orchestration/worktree

##### orchestration/worktree/001 — Submitting the orchestration form with a worktree slug typed in yields a request carrying the resolved sibling worktree path; a blank slug carries `None`, preserving today's exact behavior (fork #122 reopened — deliberately NOT PRD #220's shape).
- **Layer:** L1 (pure — `build_new_pane_request` against a `NewPaneFormState`; no PTY, no daemon, no filesystem).
- **Agent:** none.
- **Asserts:** with an orchestration selected and a non-blank worktree slug, the built `NewPaneRequest.orchestration_worktree_path` equals the sibling directory `<dir>-<slug>` next to the form's picked `dir`; the identical form with a blank slug yields `orchestration_worktree_path == None`.
- **Does not assert:** actually creating the worktree on disk (covered by `orchestration/worktree/004`, and `005` on the real binary); keyboard focus/typing into the slug field; branch naming.
- **Platform coverage:** mac+linux+windows.

##### orchestration/worktree/002 — When the resolved worktree fails to create (e.g. `dir` is not a git repository), the orchestration tab is refused and the error surfaces — never a silent fallback to the shared cwd (fork #122).
- **Layer:** L1 (in-process — dispatch the real `Action::SpawnPane` through `dispatch_action` against a `CapturingPaneController`; real `git` subprocess against a tempdir that is deliberately not a repository, no PTY, no real agent).
- **Agent:** none.
- **Asserts:** dispatching `Action::SpawnPane` for a request whose `orchestration_worktree_path` is `Some(..)` but whose creation fails leaves the active tab as `Tab::Dashboard` (no orchestration tab opened), spawns no role panes, and sets a non-empty `ui.status_message` describing the failure.
- **Does not assert:** the exact error wording; the TOCTOU/branch-exists probing `create_worktree` (`src/issue_dispatch_run.rs`) already covers for the scheduler path; recovery/retry UX.
- **Platform coverage:** mac+linux.

##### orchestration/worktree/003 — Role panes spawned for an orchestration whose request `dir` is already the resolved worktree path land rooted in that worktree: every pane's spawn `cwd` and every `pane_cwd_map` entry resolve to it, not the deck's own cwd (fork #122 — characterization of the existing cwd-threading mechanism this feature builds on).
- **Layer:** L1 (in-process — dispatch the real `Action::SpawnPane` through `dispatch_action` against a `CapturingPaneController`; no PTY, no real agent).
- **Agent:** none.
- **Asserts:** with `NewPaneRequest.dir` set to a worktree-like path, every role's `create_pane_with_options` call is recorded with that path as `cwd`, and every entry `AppState.pane_cwd_map` inserts for the orchestration's role panes equals that same path — the map `work-done` resolution keys off (CLAUDE.md rule 1 / fork #74's collision).
- **Does not assert:** how the worktree path was resolved (covered by `orchestration/worktree/001`) or actually created on disk (covered by `orchestration/worktree/004`, and `005` on the real binary); real daemon/PTY spawn; work-done file routing itself (`orchestration/route/*`).
- **Platform coverage:** mac+linux+windows.

##### orchestration/worktree/004 — Dispatching `Action::SpawnPane` for a request whose `dir` is a real git repository and whose `orchestration_worktree_path` is `Some(<sibling path>)` actually creates that worktree on disk and roots every role pane in it, not in `req.dir` (fork #122 — the actual feature, as opposed to `003`'s pre-existing-mechanism characterization).
- **Layer:** L1 (in-process — dispatch the real `Action::SpawnPane` through `dispatch_action` against a `CapturingPaneController`; real `git` subprocess against a tempdir-backed fixture repo with one commit, no PTY, no real agent).
- **Agent:** none.
- **Asserts:** after dispatch, the resolved worktree path exists on disk as a directory; every role's `create_pane_with_options` call is recorded with the worktree path (not `req.dir`) as `cwd`; every `AppState.pane_cwd_map` entry for the orchestration's role panes equals the worktree path. `req.dir` and the worktree path are deliberately distinct directories, so the assertions cannot pass by `003`'s coincidence of the two being equal.
- **Does not assert:** how the worktree path was resolved from a slug (covered by `orchestration/worktree/001`); the fail-loud refusal path (`orchestration/worktree/002`); branch naming or content; real daemon/PTY spawn; work-done file routing itself (`orchestration/route/*`).
- **Platform coverage:** mac+linux (spawns a real `git` subprocess).

##### orchestration/worktree/005 — Driving the real new-pane form's keyboard path end to end — `Ctrl+n` -> directory picker -> Mode cycled to an orchestration -> Tab to the Worktree field -> a typed slug -> submit — creates the worktree on disk and roots every role pane in it, on the real binary (fork #122, CLAUDE.md rule 4).
- **Layer:** L2 (real-binary PTY; real `git` subprocess against the fixture directory, committed inline before submission so `git worktree add -b` has a ref to branch from; no real agent).
- **Agent:** none (both roles dump their own `pwd` to a role-named log file, then `sleep 600` — no LLM tokens).
- **Asserts:** submitting the form with a typed Worktree slug creates the resolved sibling worktree directory on disk, and BOTH role panes' `pwd` logs — written by each role's own shell command before it sleeps — resolve to the created worktree, not the fixture directory the deck was launched in. This is the keyboard path whose Enter-chain regression earlier on this PR was found only as collateral damage in unrelated e2e helpers; this test exercises the Worktree field directly.
- **Does not assert:** the slug-to-path resolution in isolation (`orchestration/worktree/001`); the fail-loud refusal path (`orchestration/worktree/002`); the pre-existing cwd-threading mechanism (`orchestration/worktree/003`) or the `dispatch_action`-level creation proof (`orchestration/worktree/004`); branch naming or content; work-done file routing (`orchestration/route/*`).
- **Platform coverage:** mac+linux (spawns a real `git` subprocess).

##### orchestration/worktree/006 — `resolve_orchestration_worktree_path` rejects a slug containing a path separator, the literal `..`, a leading dash, or a NUL control character, and still resolves a plain alphanumeric-and-dash slug exactly as `001` expects (fork #122/#123 audit P1: the original bug let a slug like `x/../../../tmp/owned` against repo `/safe/repo` escape `/safe` entirely, and every role pane was then started with that escaped directory as its cwd).
- **Layer:** L1 (pure — direct calls to `resolve_orchestration_worktree_path`; no PTY, no daemon, no filesystem).
- **Agent:** none.
- **Asserts:** each of a slash-containing slug, `..`, a leading-dash slug, and a NUL-containing slug returns `Err`; a plain alphanumeric-and-dash slug still returns `Ok` with the exact sibling path `001` pins.
- **Does not assert:** the sibling-of-`dir` belt-and-braces check in isolation (both layers reject the escape cases here together); the refusal reaching the user (`ui.status_message`, the SpawnPane-level fail-loud path — covered by `002`'s shape for creation failures); branch naming.
- **Platform coverage:** mac+linux+windows.

##### orchestration/worktree/007 — `classify_worktree_add_result` classifies a timed-out `git worktree add` (directory present) as `TimedOut`, never `AlreadyClaimed`, while a genuine non-timeout failure with the directory present still classifies as `AlreadyClaimed` (fork #122/#123 re-audit P2: a timed-out add registers the worktree directory before it is killed, so collapsing that into `AlreadyClaimed` permanently wedged the slug — every later attempt saw the same present directory and refused it with no cleanup).
- **Layer:** L1 (pure — direct calls to `classify_worktree_add_result` with a synthetic `AddError`; no PTY, no daemon, no real `git` subprocess, no 30s wait).
- **Agent:** none.
- **Asserts:** a present worktree directory fed `Err(AddError::TimedOut(_))` classifies as `Ok(AddOutcome::TimedOut)`; the identical present directory fed `Err(AddError::Failed(_))` still classifies as `Ok(AddOutcome::AlreadyClaimed)`, preserving the pre-existing TOCTOU-claim behavior.
- **Does not assert:** the bounded best-effort `git worktree remove --force` cleanup that `create_worktree_sync` layers on top of a `TimedOut` classification, or its own timeout; the user-facing message built in `ui.rs`'s `SpawnPane` dispatch; a real hook actually exceeding the 30s bound (deliberately not exercised — slow and flaky).
- **Platform coverage:** mac+linux+windows.

##### orchestration/worktree/008 — A child spawned through `spawn_in_new_process_group` is a process-group leader — its pgid equals its pid — where a plainly-spawned `std::process::Command` child inherits the caller's group instead (fork #133: the new spawn-time seam the timeout-kill fix needs, so a subsequent `killpg` on the child's own pid reaches the right group).
- **Layer:** L1 (pure — spawns two real short-lived `sleep 30` processes directly via `std::process::Command`/the new helper and reads `getpgid`; no daemon, no PTY, no real `git`).
- **Agent:** none.
- **Asserts:** `getpgid` on the pid of a child spawned via `spawn_in_new_process_group` equals that pid; `getpgid` on a plainly-spawned `std::process::Command` child does NOT equal that child's own pid — asserted by contrast, so the first assertion cannot pass vacuously (e.g. if the test process itself happened to be a group leader).
- **Does not assert:** signal delivery or descendant teardown (covered by `009`); Windows (no equivalent spawn-time seam exists there — `AgentProcessGroup::adopt` already works against a plainly-spawned child post-hoc via the job object).
- **Platform coverage:** mac+linux (`#[cfg(unix)]` — the assertions read POSIX pgids).

##### orchestration/worktree/009 — After `terminate_child_with_grace_and_wait` on a child spawned through `spawn_in_new_process_group`, neither the child nor a grandchild it forked before termination survives (fork #133: PR #123's 30s timeout kill reaped only the direct `git` process, leaving hook grandchildren — e.g. `post-checkout` — running past the bound; this pins the fix's mechanism with a cheap shell stand-in rather than a slow, flaky real 30s hook).
- **Layer:** L1 (pure — a real `sh -c 'sleep 300 & sleep 300'` child spawned via the new helper, its backgrounded grandchild discovered through the repo's own `process_table`/`descendants` scan; no daemon, no PTY, no real `git`).
- **Agent:** none.
- **Asserts:** with a 200ms grace window, `terminate_child_with_grace_and_wait` leaves both the direct child's pid and its discovered grandchild's pid absent from the process table (`kill(pid, 0)` reports `ESRCH` for both, confirmed with a bounded poll rather than a single point-in-time read) — proving the process-group kill reaches a descendant a single-pid kill would orphan.
- **Does not assert:** a real 30s `git` hook actually exceeding the bound (deliberately not exercised — slow and flaky, same call as `007` on the previous PR); the `TimedOut` classification or cleanup path (`007`); Windows (the job-object mechanism there already reaps the whole tree via `TerminateJobObject`, unaffected by this fork-only Unix gap).
- **Platform coverage:** mac+linux (`#[cfg(unix)]` — the assertions read POSIX pgids/pids).

##### orchestration/worktree/010 — `terminate_child_with_grace_and_wait_forcing_group_backstop` still reaps a same-group descendant that ignores SIGTERM even though the direct child (the group leader) exits promptly on SIGTERM during the grace window (fork #133 P1, found independently by the reviewer and the auditor on PR #134: the plain `terminate_child_with_grace_and_wait` returns as soon as `try_wait` shows the direct child reaped, skipping the phase-3 SIGKILL entirely — the exact orphan #133 exists to kill, since `git` exits promptly on SIGTERM while a `post-checkout` hook can trap or ignore it; `009` cannot see this because `sleep` dies on SIGTERM the same as everything else in that scenario).
- **Layer:** L1 (pure — a real `sh -c '(trap "" TERM; exec sleep 300) & exec sleep 300'` child spawned via `spawn_in_new_process_group`, whose backgrounded descendant is discovered through the repo's own `process_table`/`descendants` scan; no daemon, no PTY, no real `git`).
- **Agent:** none.
- **Asserts:** with a 200ms grace window, `terminate_child_with_grace_and_wait_forcing_group_backstop` leaves both the direct child's pid (reaped inside the grace window, since it tail-`exec`s into a `sleep` with TERM's default disposition) and its discovered descendant's pid (reaped only by the forced SIGKILL backstop, since it tail-`exec`s into a `sleep` that inherited `trap "" TERM`'s SIG_IGN disposition across `exec`) absent from the process table (`kill(pid, 0)` reports `ESRCH` for both, confirmed with a bounded poll).
- **Does not assert:** the non-forcing `terminate_child_with_grace_and_wait` (unchanged; still used by the single-pane Ctrl+W/respawn path — see its doc comment); a real 30s `git` hook (deliberately not exercised, same reasoning as `009`); Windows (`terminate_child_with_grace_and_wait_forcing_group_backstop` there is a plain passthrough to the existing Windows `terminate_child_with_grace_and_wait`, which already reaches the whole Job Object unconditionally).
- **Platform coverage:** mac+linux (`#[cfg(unix)]` — the assertions read POSIX pids and rely on `trap`/`exec` signal-disposition semantics).

##### orchestration/worktree/011 — `terminate_child_with_grace_and_detached_reap_forcing_group_backstop` returns promptly even when the child's reap cannot complete immediately, instead of blocking the caller for however long the reap takes (fork #136: the worktree timeout path's forcing backstop ended its force phase with an unbounded `wait()`, so the 200ms grace it presents bounded only the time *before* SIGKILL, not the call itself — a process wedged in uninterruptible kernel I/O could still delay the return past the grace, reintroducing the render-loop freeze `WORKTREE_GIT_TIMEOUT` exists to prevent, through a narrower door).
- **Layer:** L1 (pure — a hand-written `portable_pty::Child` stand-in whose `wait()` sleeps 1s before completing; no real OS process, no daemon, no PTY. A genuinely unkillable process cannot be constructed in a test, so this pins the observable shape instead: the call returns quickly and the reap still completes on its own).
- **Agent:** none.
- **Asserts:** with a 50ms grace window and a stand-in whose `wait()` takes 1s, the call returns in well under 500ms (comfortably before the slow `wait()` could have completed synchronously) and the stand-in's `wait()` has NOT yet completed at that point (proving the test exercises the detached path, not an accidental fast path); a subsequent bounded poll (3s) confirms the stand-in's `wait()` does eventually complete, proving the reap was handed off rather than dropped.
- **Does not assert:** that a thread specifically performs the reap (implementation detail — the assertions are on observable call latency and eventual completion only); a real unkillable process (not constructible in a test); the non-detached `terminate_child_with_grace_and_wait_forcing_group_backstop` (unchanged, still covered by `010`); the agent Ctrl+W/respawn paths (unaffected — they keep calling the non-forcing `terminate_child_with_grace_and_wait`, untouched by this fix).
- **Platform coverage:** mac+linux (`#[cfg(unix)]`, matching `009`/`010`; the Windows counterpart function exists for compile parity but is not separately tested here).

##### orchestration/worktree/012 — The process-wide cap on outstanding detached reaps falls back to a synchronous reap when saturated, and that fallback still completes rather than dropping the child (fork #136 PR #137 review, findings 1+2: "one thread per timeout" was not itself a bound — a single stuck `create_worktree_sync` attempt can reach the detached-reap tail up to three times, and a retrying user repeats it — and the original fix's `let _ = Builder::spawn(...)` silently dropped the child, and with it any hope of ever reaping it, on a failed spawn).
- **Layer:** L1 (pure — drives `detach_reap_or_fallback_sync_with_cap`, the exact function both platforms' detached-reap backstops call, directly with the cap forced to 1 and a counter private to this test; two hand-written `portable_pty::Child` stand-ins recording their own reap completion; no real OS process, no daemon, no PTY, no dependence on or interference with the real process-wide cap).
- **Agent:** none.
- **Asserts:** with the cap forced to 1, a first reap (500ms `wait()`) takes the only slot and returns via the detached path in well under 300ms, with its completion flag still unset at that point (proving the detached branch, not an accidental fast path); a second reap (150ms `wait()`) handed off immediately afterward, while the first slot is still held, takes at least its own 150ms and has its completion flag already set the instant the call returns (proving the saturated-cap fallback reaps synchronously and inline, not merely scheduling the reap and returning early); a subsequent bounded poll (3s) confirms the first reap's completion flag does eventually become set too, proving the cap never turns into a second way to drop a child.
- **Does not assert:** the real process-wide `MAX_OUTSTANDING_DETACHED_REAPS` value or the production static counter (deliberately: the cap and counter are passed in, not reached for, so this cannot interfere with any other test using the production path); a genuine `Builder::spawn` failure (not constructible without exhausting the OS thread supply; saturation exercises the same synchronous-fallback branch honestly, per the review); that a thread specifically performs the detached reap (implementation detail, as in `011`); the SIGTERM/poll/SIGKILL phases before the tail reap (unchanged, still covered by `010`/`011`); Windows (the shared cap/fallback function is exercised identically there in production, but this test targets the Unix-only test harness shape, matching `009`-`011`).
- **Platform coverage:** mac+linux (`#[cfg(unix)]`, matching `009`-`011`).

##### orchestration/worktree/013 — The forcing teardown paths never reap the direct child before sending the phase-3 group signal (fork #143: `try_wait`'s underlying `waitpid(WNOHANG)` reaps as a side effect of merely checking exit status, releasing the direct child's pid back to the kernel for recycling before `killpg` used it — a sub-millisecond race needing pid-space wraparound that cannot be forced deterministically, so this pins the ordering invariant that makes it structurally impossible instead).
- **Layer:** L1 (pure — a hand-written `portable_pty::Child` stand-in with no real pid, recording every `try_wait`/`wait`/`kill` call into a shared log in call order; no real OS process, no daemon, no PTY, no `killpg`).
- **Agent:** none.
- **Asserts:** running both forcing entry points this fix touched — `terminate_child_with_grace_and_wait_forcing_group_backstop` (test-only) and `terminate_child_with_grace_and_detached_reap_forcing_group_backstop` (the one that ships, called from `issue_dispatch_run.rs`'s worktree-timeout escalation) — against independent instances of the stand-in (20ms grace) each produces a call log with zero `try_wait` calls (the forcing path must not poll — polling is exactly what reaps early) and exactly two `kill` calls (phase 1's SIGTERM, then phase 3's forcing SIGKILL), both occurring strictly before the single `wait` call that performs the actual reap; the detached half's `wait` runs on a background thread (fork #136), so that half polls the log for the reap to land before asserting.
- **Does not assert:** the real pid-recycling race itself (not deterministically forceable — see the headline); the non-forcing `terminate_child_with_grace_and_wait` (unaffected by this fix, still covered by `009`); the detached variant's own bounded-return *latency* property (that's `011`'s job — this test pins the detached variant's reap *ordering*, not how quickly it returns); a real `killpg` call or process group (the stand-in's `process_id()` returns `None`, routing every signal through the `ChildKiller::kill` fallback so this test needs no OS process, and no pid recycling is exercised at all); signal identity — with `process_id() -> None` both SIGTERM and SIGKILL collapse into the same unqualified `kill` call, so "two kill calls" proves two signal *attempts*, not confirmed SIGTERM-then-SIGKILL identity (`009`/`010` pin that against real processes).
- **Platform coverage:** mac+linux (`#[cfg(unix)]`, matching `009`-`012`).

#### orchestration/remit

##### orchestration/remit/001 — A `Compacting` event on the orchestrator start-role pane re-delivers the remit pointer a second time (upstream issue #423).
- **Layer:** L2 (real-binary PTY via the vt100 `TuiDeck` harness; the daemon's own `AppState::apply_event` and `deliver_orchestrator_prompt` render-loop path handle the injected event and the redelivery for real).
- **Agent:** none (`remit-reassert-orchestration` fixture — the `orchestrator` start role runs a synthetic script that declares itself live over the raw hook socket at boot and tees its stdin to `orchestrator-prompt.log`; `worker` is a plain `cat` stub; no LLM tokens spent).
- **Asserts:** after the spawn-time remit pointer (`Read .dot-agent-deck/orchestrator-context.md`) delivers once (confirmed via the log), injecting a synthetic `Compacting` `AgentEvent` for the SAME start-role pane/agent identity — confirmed applied via the daemon's own `ListAgents` live-status join before proceeding — causes the log to show the pointer a second time within 10s.
- **Does not assert:** that the trigger is scoped to compaction alone versus any other event type (that is the coder's pure-data unit test, per this PRD's task split); the guard against firing on a non-start-role pane (`002`); the readiness-gating/delivery-confirmation discipline of the re-assertion itself (`003`).
- **Platform coverage:** mac+linux (`#[cfg(unix)]` — the fixture script's `emit_target` helper is a POSIX shell function calling `python3`).

##### orchestration/remit/002 — A `Compacting` event on a non-start `worker` role's pane re-asserts nothing, while the same event on the orchestrator start role in the same orchestration still re-asserts (upstream issue #423's settled scope: the orchestrator start role only).
- **Layer:** L2 (real-binary PTY via the vt100 `TuiDeck` harness).
- **Agent:** none (`remit-reassert-orchestration` fixture, as `001`).
- **Asserts:** after the spawn-time remit pointer delivers once, injecting `Compacting` for the non-start `worker` role's pane/agent identity does not push the start role's delivery log to a second `Read .dot-agent-deck/orchestrator-context.md` line within a 900ms bounded wait; injecting `Compacting` immediately afterward for the orchestrator START role's own identity, in the SAME orchestration, DOES push the log to a second line within 10s — the positive control that makes the negative check meaningful rather than a vacuous pass against an unimplemented feature.
- **Does not assert:** a genuinely non-orchestration (plain agent/mode) pane's compaction re-asserting nothing — deliberately not exercised here since the worker-role case already proves the guard does not key off "any pane in the orchestration" and the settled scope names only the start role as a trigger; the readiness-gating/delivery-confirmation discipline of the re-assertion itself (`003`).
- **Platform coverage:** mac+linux (`#[cfg(unix)]`, matching `001`).

##### orchestration/remit/003 — A re-assertion triggered while the start-role pane is history-only does not write blindly: the pointer stays undelivered (with the same `History-only session cannot accept live input` feedback the spawn-time seed already surfaces for a non-applied `SendResult`) until the pane later reports itself live again, at which point the deferred re-assertion completes (upstream issue #423, the case the task calls out as mattering most — a blind write here would reintroduce issue #424's exact bug inside a feature whose entire purpose is reliability).
- **Layer:** L2 (real-binary PTY via the vt100 `TuiDeck` harness).
- **Agent:** none (`remit-reassert-orchestration` fixture; the orchestrator role's script additionally toggles its own declared liveness live -> history-only -> live on cue from control files the test writes into the fixture workdir).
- **Asserts:** with the start-role pane confirmed history-only, injecting `Compacting` for its identity does not push the delivery log to a second line within a 900ms bounded wait, and the rendered grid surfaces `History-only session cannot accept live input` within 5s; once the SAME pane subsequently reports itself live again, the log reaches a second `Read .dot-agent-deck/orchestrator-context.md` line within 10s — proving the re-assertion is gated on confirmed delivery rather than a direct, unconfirmed pane write. Deliberately asserts only on the rendered grid and the delivery-log line count — both pre-existing, stable observables — never on an internal helper or `SendResult` variant introduced by the concurrently in-flight `fix/424-seed-delivery-confirmation` branch, so this test's correctness does not depend on which internal shape #424 lands in.
- **Does not assert:** the pure liveness-toggle mechanism in isolation (covered generally by `prompt/pane-input/007`'s identical `emit_target` technique at spawn time); #424's own internal retry/backoff bookkeeping (out of scope by design, per the task's decoupling requirement); a genuinely dropped/lost re-assertion attempt distinct from a merely-deferred one (not constructible without the coder's implementation to compare against).
- **Platform coverage:** mac+linux (`#[cfg(unix)]`, matching `001`/`002`).

#### orchestration/hydration

##### orchestration/hydration/001 — Renaming an orchestration in the local `.dot-agent-deck.toml` while its tab is live surfaces an on-screen drift warning naming the orchestration when the TUI reattaches to the still-running daemon (fork issue #314 / upstream #554).
- **Layer:** L2 (real-binary PTY via the vt100 `TuiDeck` harness; a second real client attaches to the first client's still-running daemon, matching `session/live/006`'s reattach shape).
- **Agent:** none (`cat` stand-in roles; no LLM tokens).
- **Asserts:** with an Orchestration tab opened live against the `orch-deck` fixture's `demo-orch` orchestration, rewriting that project's `.dot-agent-deck.toml` on disk to rename the orchestration (the file still parses — this is config drift, not the legitimate `cfg.is_none()` remote-reconnect case `session/live/006`/PRD #111 covers) and then launching a fresh TUI client against the SAME daemon renders an in-session status-line warning naming the orchestration (`demo-orch`) once hydration rebuilds the tab from the daemon's synthesised config.
- **Does not assert:** the exact wording of the warning beyond the required substring; the legitimate silent `cfg.is_none()` remote-reconnect path (`session/live/006`); the snapshot-restore path's equivalent drift warning (that path already warns — this entry is the daemon-hydration path's counterpart); which config (local vs synthesized) wins the rebuild (unchanged either way).
- **Platform coverage:** mac+linux.

##### orchestration/hydration/002 — Corrupting the local `.dot-agent-deck.toml` into invalid TOML while an orchestration's tab is live surfaces an on-screen warning naming the config file when the TUI reattaches to the still-running daemon (fork issue #320).
- **Layer:** L2 (real-binary PTY via the vt100 `TuiDeck` harness; same reattach shape as `orchestration/hydration/001`).
- **Agent:** none (`cat` stand-in roles; no LLM tokens).
- **Asserts:** with an Orchestration tab opened live against the `orch-deck` fixture's `demo-orch` orchestration, rewriting that project's `.dot-agent-deck.toml` on disk with a syntax error (an unterminated string — the file still exists, it just no longer parses, distinct from both the legitimate silent `Absent` case and `001`'s "parses but doesn't list it" drift) and then launching a fresh TUI client against the SAME daemon renders an in-session status-line warning naming the local config file as unparseable. Pins the `LocalConfigState::Unparseable` branch of the `lookup_config` closure in `run_tui` (`src/ui.rs`), which previously mapped `Err(ProjectConfigError::Parse)` to `None` and took the branch reserved for an absent file — silently — until `6ef0269` introduced `LocalConfigState` to distinguish it.
- **Does not assert:** the exact wording of the warning beyond the required substring; the legitimate silent `Absent` remote-reconnect path; `001`'s "parses but doesn't list the orchestration" drift case (a different `LocalConfigState` branch); the live-surfacing call site's equivalent case (covered by `scheduler/live/005`).
- **Platform coverage:** mac+linux.

### Session restore

#### session/restore

##### session/restore/001 — No-flag startup auto-restores dashboard panes from the saved session (PRD #89 Phase 2).
- **Layer:** L2 (real-binary PTY; `DOT_AGENT_DECK_SESSION` redirected to a test-owned path).
- **Agent:** none (a saved `session.toml` with two panes running `sleep 600`; daemon is freshly spawned and empty).
- **Asserts:** launching with NO `--continue` flag against an empty daemon restores both saved panes as dashboard cards, with their saved display names. (Restore is unconditional now — the old `--continue` gate is gone.)
- **Does not assert:** the agents' inner state (not preserved per docs); the daemon-vs-snapshot precedence (deferred to Phase 2 M2.2).
- **Platform coverage:** mac+linux.

##### session/restore/002 — A saved mode tab is restored as a full mode tab when the project's `.dot-agent-deck.toml` still has the mode.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** after `--continue`, a tab with the mode's name appears and contains the persistent side panes.
- **Does not assert:** any reactive pane content.
- **Platform coverage:** mac+linux.

##### session/restore/003 — A saved mode whose `.dot-agent-deck.toml` no longer carries the mode falls back to a plain dashboard pane with a stderr warning.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** the saved pane becomes a dashboard card (not a mode tab); the harness's stderr capture contains a warning that names the missing mode.
- **Does not assert:** any rendering of the warning inside the TUI.
- **Platform coverage:** mac+linux.

##### session/restore/004 — A saved pane whose `dir` no longer exists is skipped with a stderr warning; other saved panes still restore.
- **Layer:** L2.
- **Agent:** none.
- **Asserts:** N-1 cards restore; stderr names the missing directory.
- **Does not assert:** which other panes survive (deterministic from the file order).
- **Platform coverage:** mac+linux.

##### session/restore/005 — Daemon-with-agents wins over the disk snapshot; snapshot restore is skipped (PRD #89 Phase 2 M2.2).
- **Layer:** pure-data (in-crate integration test on `ui::should_apply_snapshot` over `AppState.managed_pane_ids`; no TUI harness, runs in the fast tier).
- **Agent:** none.
- **Asserts:** with no hydrated managed panes `should_apply_snapshot` returns `true` (daemon empty → apply the disk snapshot); after one or more hydrated `managed_pane_id`s are registered it returns `false` (daemon owns the workspace → skip the snapshot so panes are not double-restored). Pins the M2.2 precedence as a structural decision, not a flag.
- **Does not assert:** the end-to-end cross-deck PTY hydration path (would need a daemon pre-seeded with an agent that a fresh deck hydrates — a harness primitive not yet built); the snapshot-apply mechanics themselves (covered by `session/restore/001`).
- **Platform coverage:** mac+linux+windows.

##### session/restore/006 — Empty daemon + no snapshot + no flag lands on a clean empty dashboard (PRD #89 Phase 2).
- **Layer:** L2 (real-binary PTY; `DOT_AGENT_DECK_SESSION` redirected to a test-owned path with no file staged).
- **Agent:** none.
- **Asserts:** with both restore sources empty (fresh empty daemon, no snapshot on disk) and no `--continue`, the deck lands on the "No active sessions" dashboard with no restore warning and remains interactive (Ctrl+N opens the new-pane directory picker). Locks the post-Phase-2 invariant that unconditional restore still falls through cleanly when there is nothing to restore.
- **Does not assert:** the daemon-with-agents-wins precedence (deferred to Phase 2 M2.2); the snapshot-restore path (covered by `session/restore/001`).
- **Platform coverage:** mac+linux.

##### session/restore/007 — A warm daemon carrying an orchestration hydrates the orchestrator + role panes in their saved order (PRD #89 Phase 2b M2b.1).
- **Layer:** in-process (real in-process attach daemon over a Unix socket; `EmbeddedPaneController::hydrate_from_daemon`; no real binary, no PTY drive). Runs in the fast tier.
- **Agent:** none (each role agent runs `sh -c 'sleep 30'`; no LLM).
- **Asserts:** spawning three orchestration role agents (orchestrator + coder + reviewer), each tagged with its `TabMembership::Orchestration` `role_index` / `role_name` / `is_start_role`, then hydrating a fresh controller from the warm daemon reproduces every role as a pane; placing each hydrated pane at its `role_index` yields the panes in their saved display order; and the start (orchestrator) role — the `start_role_index` cursor — is recoverable from `is_start_role`. Regression guard that warm-daemon orchestration hydration (PRD #76 M2.12 + #111) survives detach/reattach so M2b.3's snapshot fallback is only needed when the daemon is empty.
- **Does not assert:** the daemon-empty snapshot-fallback rebuild (`session/restore/008`); the orchestrator-prompt replay (intentionally NOT replayed on warm reconnect — `src/tab.rs` design decision 3); the full `OrchestrationConfig` re-resolution (the partition + `resolve_orch_config_for_hydration` path, exercised elsewhere).
- **Platform coverage:** mac+linux (Unix-only; `#![cfg(unix)]`).

##### session/restore/008 — A daemon-empty launch with an orchestration snapshot rebuilds the orchestration tab and replays the orchestrator prompt (PRD #89 Phase 2b M2b.3).
- **Layer:** L2 (real-binary PTY; `DOT_AGENT_DECK_SESSION` redirected to a test-owned path; daemon freshly spawned and empty).
- **Agent:** none (the orchestration's `coder`/`reviewer` roles run `sleep 600`; the `orchestrator` role runs a recorder shell script that self-posts `SessionStart` and appends its stdin to an absolute `record-orchestrator.log` — no LLM tokens).
- **Asserts:** with a hand-staged `session.toml` whose single pane carries a `[panes.orchestration]` block (`config_name`/`project_path` pointing at a test-owned orchestration config, `orchestrator_prompt = "Build the feature end to end"`, `start_role_index = 0`) and an empty daemon, launching with NO `--continue` REBUILDS the orchestration tab: the `coder` and `reviewer` role panes appear as deck cards in their saved display order, and — unlike warm hydration (`session/restore/007`) — the saved `orchestrator_prompt` is replayed to the start (orchestrator) role and recorded (echo-immune), which also proves the start role was identified from `start_role_index`.
- **Does not assert:** the warm-daemon hydration path (`session/restore/007`); the on-disk capture that produces the snapshot (`session/save/004`); the config-drift fallback (`session/restore/009`); the exact role-card styling / focus border.
- **Platform coverage:** mac+linux.

##### session/restore/009 — An orchestration snapshot whose config no longer resolves falls back to a plain dashboard pane with a `session_warnings` message naming the missing orchestration (PRD #89 Phase 2b M2b.3 drift).
- **Layer:** L2 (real-binary PTY; `DOT_AGENT_DECK_SESSION` redirected to a test-owned path; daemon freshly spawned and empty).
- **Agent:** none (the fallback pane runs `sleep 600`; no LLM).
- **Asserts:** with a hand-staged `session.toml` whose `[panes.orchestration]` block references `config_name = "tdd-cycle"` while the project config at `project_path` defines only a renamed `renamed-orch` (a re-resolution drift), launching against an empty daemon with no flag restores the saved pane as a PLAIN dashboard card (its saved name `orchestrator`, with no `coder`/`reviewer` role panes — never a half-broken tab) AND surfaces a clear `session_warnings` message naming the missing orchestration (`tdd-cycle`), flushed to stderr on detach-quit. Mirrors the mode-tab drift fallback (`session/restore/003`, PRD #69 Path D/E).
- **Does not assert:** the exact warning wording (only that it names the missing orchestration); the successful rebuild path (`session/restore/008`); which other panes survive when multiple are staged (only one is here).
- **Platform coverage:** mac+linux.

##### session/restore/010 — A snapshot re-resolving to a zero-role orchestration falls back to a plain dashboard pane with a warning, never panicking at startup (PRD #89 review-fix F2).
- **Layer:** L2 (real-binary PTY; `DOT_AGENT_DECK_SESSION` redirected to a test-owned path; daemon freshly spawned and empty).
- **Agent:** none (the fallback pane runs `sleep 600`; no LLM).
- **Asserts:** with a project config that still names `tdd-cycle` but whittled to an EXPLICIT empty role set (`roles = []`, which `load_project_config` accepts since it runs no `config_validation`) and a hand-staged snapshot whose saved role set is also empty (so the name+order drift guard passes — `[] == []`) with a `start_role_index` of 0 that is out of range, launching against an empty daemon with no flag does NOT panic/crash-loop: the saved pane restores as a PLAIN dashboard card (`orchestrator`) and a `session_warnings` message naming the orchestration (`tdd-cycle`) is flushed to stderr on a clean detach-quit. Pins that an empty/no-start-role re-resolution is treated as drift, never indexed unguarded at the start cursor.
- **Does not assert:** the exact warning wording (only that it names the orchestration); the successful rebuild path (`session/restore/008`); the non-empty role-set drift fallback (`session/restore/009`).
- **Platform coverage:** mac+linux.

##### session/restore/011 — A saved `start_role_index` that differs from the config default is honored on restore: the orchestrator prompt lands on the role at the saved index (PRD #89 review-fix F3).
- **Layer:** L2 (real-binary PTY; `DOT_AGENT_DECK_SESSION` redirected to a test-owned path; daemon freshly spawned and empty).
- **Agent:** none (both roles run a recorder shell script that self-posts `SessionStart` and appends its stdin to an absolute `record-<role>.log` — no LLM tokens).
- **Asserts:** with a `tdd-cycle` config whose default start role is `orchestrator` (index 0, `start = true`) and a recorder on BOTH roles, a hand-staged snapshot saving `start_role_index = 1` (`coder`) makes the replayed `orchestrator_prompt` land on and be recorded by the role at the SAVED index (`coder`, index 1) — and NOT by the config-default start role (`orchestrator`, index 0). Pins that restore reads `snap.start_role_index` rather than recomputing the start cursor from the live config's `start` flag.
- **Does not assert:** the drift/bounds handling when the saved index is out of range (`session/restore/010`); `started_role_indices` replay (captured but has no reader); the exact role-card styling / focus border.
- **Platform coverage:** mac+linux.

##### session/restore/012 — A snapshot whose `project_path` diverges from the saved pane `dir` does not auto-run the config planted at `project_path` (PRD #89 review-fix F1).
- **Layer:** L2 (real-binary PTY; `DOT_AGENT_DECK_SESSION` redirected to a test-owned path; daemon freshly spawned and empty).
- **Agent:** none (roles run `sleep 600`; no LLM).
- **Asserts:** with the saved pane `dir` pointing at a legitimate working dir (no orchestration config) while the `[panes.orchestration]` `project_path` points at a SEPARATE planted dir whose config defines a uniquely-named `phantom-reviewer` role, launching against an empty daemon with no flag does NOT execute the planted config — `phantom-reviewer` never materializes as a deck card — while the saved pane still restores as a PLAIN card (`orchestrator`). Pins that the un-cross-checked `project_path` cannot auto-run a config from an unexpected directory (capture always writes `project_path == saved_pane.dir`, so divergence only arises via tampering).
- **Does not assert:** which fix shape the coder chooses (drift fallback vs. re-resolving from `saved_pane.dir`) — only that the divergent config is not executed; path canonicalization edge cases (symlinks, `..`).
- **Platform coverage:** mac+linux.

##### session/restore/013 — A custom orchestration tab `display_title` saved in the snapshot is preserved on restore (PRD #89 review-fix F4, RED-pending-schema).
- **Layer:** L2 (real-binary PTY; `DOT_AGENT_DECK_SESSION` redirected to a test-owned path; daemon freshly spawned and empty).
- **Agent:** none (roles run `sleep 600`; no LLM).
- **Asserts:** with a hand-staged snapshot carrying a custom `display_title` (`MYDECKTITLE`) distinct from the canonical config name, the daemon-empty rebuild shows the user's saved title in the tab bar, not the canonical `tdd-cycle` config/cwd name. RED-pending-schema: `OrchestrationSnapshot` has no `display_title` field yet (the staged key parses but is dropped on load, since the struct sets no `deny_unknown_fields`) and restore passes `None` to `open_orchestration_tab`, so the tab comes back titled `tdd-cycle`; goes GREEN once the coder adds the field + capture + restore threading.
- **Does not assert:** the live-path title plumbing (already covered by the new-pane orchestration flow); the serde round-trip of the new field in isolation (a unit test the coder adds with the field).
- **Platform coverage:** mac+linux.

##### session/restore/014 — A restored pane whose command identifies a supported agent immediately shows that agent as Idle.
- **Layer:** L2 (real-binary PTY; `DOT_AGENT_DECK_SESSION` redirected to a test-owned path; daemon freshly spawned and empty).
- **Agent:** none (a test-owned executable named `opencode` runs `sleep 600`; no LLM or OpenCode hook event).
- **Asserts:** restoring a saved plain pane whose command basename is `opencode` immediately renders an `Idle` card and never requires a hook event to replace the `No agent` placeholder identity.
- **Does not assert:** OpenCode plugin delivery or later working/waiting transitions; restore fallback paths after a mode-tab failure.
- **Platform coverage:** mac+linux.

##### session/restore/015 — An orchestration tab captured while rooted in a worktree restores with its role panes still rooted in that worktree, not the deck's own cwd (fork #122).
- **Layer:** L1 (in-process — `resolve_orchestration_for_restore` against a real `.dot-agent-deck.toml` on disk, then `TabManager::open_orchestration_tab` against a `CapturingPaneController`; no PTY, no real agent).
- **Agent:** none.
- **Asserts:** an `OrchestrationSnapshot` whose `project_path` names a real worktree directory (carrying its own `.dot-agent-deck.toml`) re-resolves successfully via `resolve_orchestration_for_restore`, and feeding the resolved config into `open_orchestration_tab` with that same worktree path as `cwd` — exactly as the daemon-empty restore path calls it with `saved_pane.dir` — spawns every role pane's `create_pane_with_options` call rooted in the worktree. Fork #122's rooting was already implemented (restore always passes `saved_pane.dir`, which capture always writes as the worktree); this pins the property so it stays true across refactors.
- **Does not assert:** `pane_cwd_map`/`pane_role_map` population (a trivial `saved_pane.dir.clone()` insert in `run_tui`, not the rooting mechanism); the drift-fallback path (`session/restore/016`); the daemon-hydration restore path (`session/restore/007`/`008`); real daemon/PTY spawn.
- **Platform coverage:** mac+linux+windows.

##### session/restore/016 — When a captured orchestration tab's worktree has been removed from disk (e.g. via `git worktree remove`), restore re-resolution fails loud, naming the orchestration — the mechanism the caller's plain-dashboard-pane-with-a-warning fallback depends on (fork #122).
- **Layer:** L1 (in-process — `resolve_orchestration_for_restore` against a worktree path that was never created on disk; no PTY, no filesystem beyond the missing-path check).
- **Agent:** none.
- **Asserts:** an `OrchestrationSnapshot` whose `project_path` (and the saved pane `dir` passed alongside it) name a nonexistent directory makes `resolve_orchestration_for_restore` return `Err` — `canonicalize()` fails on the missing dir before any config load is attempted — and the error message names the orchestration (`snap.config_name`), matching every other drift reason this function produces.
- **Does not assert:** the caller's `session_warnings` push or the plain-dashboard-pane fallback itself (`run_tui`, exercised end to end by `session/restore/009`/`010`'s L2 drift coverage, which predate this worktree-specific removal scenario); that the deck ever auto-removes a worktree (it does not — product decision); real `git worktree remove`.
- **Platform coverage:** mac+linux+windows.

##### session/restore/017 — A worktree-owner identity persisted in an `OrchestrationSnapshot` survives a full write → read → restore round trip: TOML serialize/deserialize, then `open_orchestration_tab` with the recovered value passed through, exactly as the daemon-empty restore branch calls it (fork #166 M3.0 / PR #215 fixup).
- **Layer:** L1 (in-process — real `toml::to_string_pretty`/`toml::from_str` for the write/read half, then `resolve_orchestration_for_restore` against a real `.dot-agent-deck.toml` on disk and `TabManager::open_orchestration_tab` against a `CapturingPaneController` for the restore half; no PTY, no real agent).
- **Agent:** none.
- **Asserts:** a `SavedSession` whose orchestration pane's `OrchestrationSnapshot` carries `owner: Some("orchestration:my-feature")` round-trips that value unchanged through TOML serialize→deserialize (the real `session.toml` write/read path), and that feeding the recovered snapshot's `owner` through `open_orchestration_tab` as the `creator` argument — matching the production restore call in `run_tui` (`orch_snap.owner.as_deref()`) — records the identical string on every spawned role pane's `AgentSpawnOptions::owner`. Pins the milestone's headline claim: an orchestration tab, closed and reopened, restores under the SAME identity it stamped, so `--mine` still matches its earlier worktrees.
- **Does not assert:** the `None`-owner case (an orchestration that owned no worktree restores with no identity — implicit in `session/restore/015`/`016`'s snapshots, which all set `owner: None`); a pre-M3.0 snapshot with no `owner` key at all deserializing to `None` (covered by `config/saved-session/001`); the live-create path that first populates `owner` onto the snapshot when the tab is opened (not independently pinned — `orchestration_identity_008` and `orchestration_identity_009` cover the same `creator` local reaching the marker/env, but not this specific snapshot-field write); real daemon/PTY spawn.
- **Platform coverage:** mac+linux+windows.

##### session/restore/018 — On the restore path, `DOT_AGENT_DECK_WORKTREE_OWNER` reaches EVERY restored role pane's genuinely spawned process environment — not merely a test double's recorded field, and not only the start role (PR #215 review round follow-up, findings-215-restart-manual.md).
- **Layer:** e2e (`tests/e2e_worktree_owner_restore_env.rs`, `#[cfg(feature = "e2e")]` + `#[cfg(unix)]`). Spawns the real `dot-agent-deck daemon serve` BINARY as a subprocess (`common::spawn_daemon_serve`), attaches a real `EmbeddedPaneController` to it over its real Unix attach socket (the same client code the TUI uses), and calls `TabManager::open_orchestration_tab` with `creator: Some(owner)` — the exact argument shape `run_tui`'s daemon-empty restore branch uses (`orch_snap.owner.as_deref()`) once `resolve_orchestration_for_restore` succeeds — spawning two genuine `portable_pty` child processes via `agent_pty::spawn`.
- **Agent:** none (plain `echo` shell commands, no LLM).
- **Asserts:** both spawned role panes' own stdout — read back over the attach socket via `common::wait_for_pane_text_on`, itself reading `$DOT_AGENT_DECK_WORKTREE_OWNER` out of each child's OWN environment — contain the exact owner string `open_orchestration_tab` was called with. This is the join `session/restore/017`'s `CapturingPaneController` cannot exercise (producer proven, real-process reader not): the restore call reaches every role's real environment, not only the pane a manual spot-check happens to type into.
- **Does not assert:** the private `resolve_orchestration_for_restore` re-resolution itself — config drift/tamper checks, the `project_path`-vs-`saved_dir` anti-tampering guard — which is not `pub` and stays L1-only (`session/restore/015`–`017`); a real `SavedSession` save → process-exit → reload cycle (`session/restore/017` covers the TOML round trip); the `should_apply_snapshot` daemon-empty gate (`session/restore/005`); LLM-agent behaviour.
- **Platform coverage:** mac+linux.

### Live session status on reconnect (PRD #162)

These entries cover PRD #162: on TUI reconnect the daemon's `ListAgents` must attach the live, event-derived session state (a `SessionSnapshot` on each `AgentRecord`) so reconnected cards show real status instead of `Idle`/"No agent". The data already exists in `AppState.sessions` (built by `apply_event`, unchanged); this PRD only exposes it. The wire field `live: Option<SessionSnapshot>` is additive/optional — no `PROTOCOL_VERSION` bump.

#### session/live

##### session/live/001 — `SessionSnapshot` serde round-trips every `SessionStatus` and an older `AgentRecord` without the field decodes to `live == None` (PRD #162 M1.1).
- **Layer:** pure-data (serde round-trip; no daemon/TUI harness; runs in the fast tier).
- **Agent:** none.
- **Asserts:** a `SessionSnapshot` carrying each `SessionStatus` variant (Idle/Working/Thinking/WaitingForInput/Compacting/Error) round-trips through JSON with the status (and agent_type/active_tool/tool_count/prompts) preserved; an `AgentRecord` carrying `live = Some(snapshot)` round-trips with the snapshot intact; and a hand-crafted older-daemon `AgentRecord` JSON with no `live` key decodes via `#[serde(default)]` to `live == None` (back-compat, no protocol bump).
- **Does not assert:** the `ListAgents` join (session/live/002); newest-wins tie-break (session/live/003); the TUI-side seeding of the hydrated session (Phase 2).
- **Platform coverage:** mac+linux+windows.

##### session/live/002 — The `ListAgents` handler attaches the live event-derived snapshot; the dummy-state path yields `None` (PRD #162 M1.2).
- **Layer:** in-crate integration (in-process attach daemon over a Unix socket; fast tier; spawns a `sleep` PTY only to populate the registry record, does not drive vt100).
- **Agent:** none.
- **Asserts:** with a registry agent whose spawn-time `agent_type` is `None` and a live `AppState` session (same `agent_id` + `pane_id`) driven via `apply_event` to `Working` with an active tool, `tool_count > 0`, an event-derived `agent_type` (ClaudeCode) and a first prompt, the `ListAgents` response carries `AgentRecord.live = Some` with that status, the event-derived `agent_type` (even though the registry record's spawn-time `agent_type` is `None`), the active tool name, the tool count, and the first/last prompt. The empty dummy-state `serve_attach` path returns the same record with `live == None` — no harness regression and the older-daemon fallback shape.
- **Does not assert:** the pure serde shape (session/live/001); newest-wins (session/live/003); the TUI-side seeding (Phase 2).
- **Platform coverage:** mac+linux.

##### session/live/003 — When two sessions map to the same agent, the join attaches the newest-`last_activity` snapshot (PRD #162 M1.2 newest-wins).
- **Layer:** in-crate integration (in-process attach daemon over a Unix socket; fast tier; spawns a `sleep` PTY only to populate the registry record, does not drive vt100).
- **Agent:** none.
- **Asserts:** with two hand-built `SessionState`s in `AppState.sessions` that both map to the same agent (same `agent_id` + `pane_id`, e.g. a `/clear` restart leaving a stale entry) but different `last_activity` and distinguishing status/prompt, the `ListAgents` join attaches the snapshot from the entry with the most-recent `last_activity` (the live session), not the dead predecessor.
- **Does not assert:** the pure serde shape (session/live/001); the populated-vs-dummy contrast (session/live/002); the TUI-side seeding (Phase 2).
- **Platform coverage:** mac+linux.

##### session/live/004 — Hydrating a fresh controller seeds the reconnected card from the daemon's live snapshot (status/agent_type/active_tool/tool_count/prompts), and falls back to the bare placeholder when no snapshot is present (PRD #162 M2.1/M2.2).
- **Layer:** in-process (real in-process attach daemon over a Unix socket; `EmbeddedPaneController::hydrate_from_daemon`; spawns two `sleep` PTYs only to populate the registry, does not drive vt100). Runs in the fast tier.
- **Agent:** none.
- **Asserts:** a warm daemon carries agent A (spawn-time `agent_type = None`, the "No agent" case) driven via `apply_event` to a live `Working` session with an active `Edit` tool, `tool_count > 0`, an event-derived `ClaudeCode` type and a first prompt, plus agent B (spawn-time `OpenCode`) with NO live session. Hydrating a fresh controller threads the live `SessionSnapshot` through `HydratedPane.live` (`Some` for A, `None` for B); seeding each hydrated session via `AppState::seed_hydrated_session` — exactly as the `ui.rs` hydration loop does — makes agent A's card carry the snapshot's `status` (Working, not Idle) / `agent_type` (ClaudeCode, overriding the `None` spawn-time value, not "No agent") / `active_tool` / `tool_count` / `first_prompts` / `last_user_prompt`, with the PRD #110 `agent_id` minted on the card; agent B's snapshot-absent card falls back to today's bare placeholder (Idle, spawn-time `OpenCode`, no active tool). Each pane seeds exactly one card (no duplicate).
- **Does not assert:** the pure serde shape (session/live/001); the `ListAgents` join in isolation (session/live/002); newest-wins (session/live/003); the post-reconnect remap (session/live/005); the rendered-grid reconnect against a real daemon (session/live/006).
- **Platform coverage:** mac+linux.

##### session/live/005 — A post-reconnect `SessionStart` from the same agent remaps onto the snapshot-seeded card instead of spawning a duplicate (PRD #162 M2.2, PRD #110 property preserved).
- **Layer:** pure-state (in-process `AppState`; `seed_hydrated_session` + `apply_event`; no daemon/TUI harness). Runs in the fast tier.
- **Agent:** none.
- **Asserts:** after `AppState::seed_hydrated_session` seeds a card from a live `SessionSnapshot` (Working/ClaudeCode/active tool/prompts) with the PRD #110 `agent_id` minted on it, a subsequent `SessionStart` event carrying the SAME `pane_id` + `agent_id` but a distinct `session_id` remaps onto the hydrated card — exactly one session/pane survives for that agent (no duplicate) and the minted `agent_id` is preserved through the remap.
- **Does not assert:** the snapshot-seeding of the card's fields (session/live/004); the daemon-side join (session/live/002, session/live/003); the rendered-grid reconnect (session/live/006); the clear=true respawn (different `agent_id`) duplicate-retire path (PRD #110 tests).
- **Platform coverage:** mac+linux+windows.

##### session/live/006 — A fresh TUI reconnecting to a real daemon renders the live `Working` status on the rebuilt card immediately, not the `Idle`/"No agent" placeholder (PRD #162 M2.1/M2.2 end-to-end).
- **Layer:** L2 (real-binary PTY; a shared `dot-agent-deck daemon serve` driven over its hook + attach sockets, then a fresh real-binary TUI launched against the same daemon's sockets; `#[cfg(feature = "e2e")]`).
- **Agent:** none (the agent is a `sh -c 'sleep 600'` stub; the live status is taught via synthetic Claude Code hooks — no LLM tokens).
- **Asserts:** a daemon-owned agent (spawn-time `agent_type = None`, pane `pane-recon`, display name `recon-live-77`) is driven to a live `Working` session with an active `Read` tool by writing `session_start` + `tool_start` hooks (carrying the registry agent id so the `ListAgents` snapshot join matches) — with NO TUI attached. A FRESH TUI then launched against the same daemon, writing no further hook, rebuilds the dashboard card showing the live `Working` status and the agent's display name immediately on reconnect, and does not render the `No agent` placeholder for that live agent.
- **Does not assert:** a literal first-TUI detach cycle (the daemon owns the live state regardless of whether a TUI was ever attached); the in-process seeding seam (session/live/004); the active-tool tally/label beyond the status badge; the daemon-side join/serde (session/live/001–003).
- **Platform coverage:** mac+linux.

##### session/live/007 — `DaemonClient::list_agents` scrubs and clamps a hostile `AgentRecord.live` at the wire boundary so a malformed daemon can't corrupt the rebuilt card (PRD #162 review-fix, parallels embed/attach/005).
- **Layer:** in-crate integration (a hand-rolled mock attach daemon over a Unix socket advertises one hostile `AgentRecord`; the real `DaemonClient::list_agents` boundary sanitizer runs; fast tier; no PTY/vt100).
- **Agent:** none (the mock daemon hand-crafts the hostile `AttachResponse`).
- **Asserts:** a daemon advertises an `AgentRecord.live` whose `last_user_prompt`, every `first_prompts` entry, and `active_tool.name` / `.detail` carry ANSI escapes, NUL bytes, and other ASCII control chars AND are over-long (~100 KiB each), and whose `first_prompts` is oversized (6 entries — double the `MAX_FIRST_PROMPTS` cap of 3). `list_agents` returns the record with its live snapshot PRESERVED (the agent is real) but SCRUBBED — no byte `< 0x20` or `== 0x7f` survives in `last_user_prompt`, any `first_prompts` entry, or `active_tool.name` / `.detail` — and CLAMPED — every one of `last_user_prompt`, `active_tool.name`, `active_tool.detail`, and each `first_prompts` entry is length-bounded to <= 65536 bytes (not passed through verbatim), and `first_prompts` is cut to at most `MAX_FIRST_PROMPTS` (3) entries.
- **Does not assert:** the daemon-side join/serde (session/live/001–003); the seeding of the card's fields (session/live/004); the `agent_type` precedence fallback (session/live/008); the `tab_membership` scrub itself (embed/attach/005); the exact sanitized output beyond "no raw control bytes survive and the list is clamped".
- **Platform coverage:** mac+linux.

##### session/live/008 — An event-derived `AgentType::None` snapshot falls back to the spawn-time agent type on reconnect instead of seeding the card as "No agent" (PRD #162 review-fix).
- **Layer:** pure-state (in-process `AppState`; `SessionState::live_snapshot` + `AppState::seed_hydrated_session`; no daemon/TUI harness). Runs in the fast tier.
- **Agent:** none.
- **Asserts:** a live `SessionState` whose event-derived `agent_type` is `AgentType::None` (the agent emitted events but never identified itself) snapshots via `live_snapshot` to `agent_type == None` (Option::None, NOT `Some(AgentType::None)`), so when `seed_hydrated_session` seeds a reconnected card whose spawn-time `agent_type` is `Some(ClaudeCode)`, the snapshot does not shadow the spawn-time fallback and the card carries the REAL `ClaudeCode` type — not "No agent".
- **Does not assert:** the wire-boundary scrub/clamp (session/live/007); the full snapshot field seeding (session/live/004); the daemon-side newest-wins join (session/live/003); the post-reconnect remap (session/live/005).
- **Platform coverage:** mac+linux+windows.

##### session/live/009 — An unknown `SessionStatus` string on `AgentRecord.live.status` degrades gracefully instead of failing the whole record parse (PRD #162 Greptile review-fix, forward-compat).
- **Layer:** pure-data (serde decode of a hand-crafted wire JSON; no daemon/TUI harness; fast tier).
- **Agent:** none.
- **Asserts:** an `AgentRecord` wire JSON whose `live.status` is a string this build does not know (`"Hibernating"`) deserializes via `serde_json::from_str::<AgentRecord>` to `Ok` (NOT `Err`) and the record survives with its `id` / `pane_id_env` intact — a newer daemon's future status variant must not fail an older TUI's entire `AgentRecord` decode just because `live` is a present field. Mechanism-agnostic: does NOT pin whether the fix maps the unknown status to a catch-all variant (`live` stays `Some`) or drops `live` to `None`.
- **Does not assert:** which degrade mechanism is chosen (`#[serde(other)]` vs lenient `live -> None`); the older-shape back-compat (`live` absent -> `None`, session/live/001); the wire-boundary scrub/clamp (session/live/007).
- **Platform coverage:** mac+linux+windows.

##### session/live/010 — Rehydration preserves history-only and view-only writability across detach/reconnect (PRD #20, blocker 4).
- **Layer:** L1 state/wire integration (`SessionSnapshot` JSON deserialize + `AppState::seed_hydrated_session`).
- **Agent:** synthetic Codex snapshots.
- **Asserts:** reconnect snapshots carrying `history-only` and `none` live targets rebuild sessions with `Writable::HistoryOnly` and `Writable::None`, rather than reverting to Live.
- **Does not assert:** real socket reconnect rendering; the snapshot-to-state seam is the capability-loss boundary.
- **Platform coverage:** mac+linux+windows.

##### session/live/011 — Rehydration preserves the PRD #370 synthetic-`Working` provenance, so a card reconnected mid-`ShellBusy` is still reverted by the paired `ShellIdle` (fork issue #21).
- **Layer:** L1 state/wire integration (two `AppState`s modelling daemon and TUI, joined by a real `live_snapshot()` → JSON → `seed_hydrated_session` round-trip).
- **Agent:** synthetic Claude Code hook events plus the daemon's synthesized `ShellBusy`/`ShellIdle` shape — `session_id` resolved via `pane_hook_session_id` (the pane's current hook generation) and `agent_id: Some(..)` resolved independently from the pane's current card via `pane_session_id`, mirroring `run_shell_activity_monitor`.
- **Asserts:** a pane promoted to `Working` by `ShellBusy` and then rehydrated from the daemon's `SessionSnapshot` returns to `Idle` when the paired `ShellIdle` arrives, instead of reading `Working` forever; and a second pane whose `Working` came from a real `ToolStart` is NOT reverted by the same `ShellIdle`. Both are checked against the daemon-side `AppState` as a control, so the test fails if the daemon's own precedence changes. The shell pane goes through a same-agent `/clear` restart first, so the synthesized events are built while its hook generation and its stable card id disagree, and the single-card assertion covers that shape too.
- **Does not assert:** the wire-boundary scrub/clamp (`session/live/007`); rendered card output; a real socket reconnect (`session/live/006`); the pure `ShellBusy`/`ShellIdle` precedence rules in isolation (`src/state.rs`'s `shell_busy_idle_promote_and_revert_without_clobbering_real_status`); that `run_shell_activity_monitor` itself emits that shape — this test builds the event, so the production seam is pinned by `src/daemon.rs`'s `shell_activity_monitor_stamps_the_owning_agent_across_a_session_rollover`.
- **Platform coverage:** mac+linux+windows.

##### session/live/012 — A `ShellIdle` broadcast in the window between the `ListAgents` snapshot and the TUI's event stream coming up still reaches the rebuilt card (fork issue #36).
- **Layer:** L1 client/wire integration — the real production reconnect bootstrap (`reconnect::run_event_subscriber` + `EmbeddedPaneController::hydrate_from_daemon` + `AppState::seed_hydrated_session`) driven against a mock daemon over a real Unix socket.
- **Agent:** none (a mock daemon that serves one `AgentRecord` and one synthesized `ShellIdle`; no PTY, no LLM).
- **Asserts:** with a daemon that is deliberately slow to acknowledge `SubscribeEvents` (receiver registered immediately before the OK `RESP`, exactly as `handle_subscribe_events` does) and that broadcasts the paired `ShellIdle` the instant it has served the `ListAgents` snapshot, the reconnected card seeds `Working` from the already-stale snapshot and then comes back to `Idle`. That requires BOTH halves of the fix: hydration waits for the subscription before snapshotting (or the broadcast finds no receiver and `send` drops it), and the subscriber holds events until hydration has seeded (or `apply_event` drops the edge for an unregistered pane). The delay makes the window deterministic rather than scheduler-dependent, so a revert of either half fails the test rather than flaking.
- **Does not assert:** the provenance-marker half of the story (`session/live/011` — the marker surviving the wire); recovery of events lost during a LATER subscription outage (`session/live/013`, `session/live/014` — issues #49/#28); rendered card output; a real daemon's timing.
- **Platform coverage:** mac+linux.

##### session/live/013 — A `SubscribeEvents` connection that dies mid-session with the daemon's documented `KIND_STREAM_END "lagged"` tear-down is recovered by the reconnect loop draining a fresh `list_agents` snapshot, not left stuck on the pre-outage status (issues #49/#28).
- **Layer:** L1 client/wire integration — the real production reconnect bootstrap (`reconnect::run_event_subscriber` + `EmbeddedPaneController::hydrate_from_daemon` + `AppState::seed_hydrated_session`) driven against a mock daemon over a real Unix socket.
- **Agent:** none (a mock daemon serving one `AgentRecord` whose live status the test mutates mid-test; no PTY, no LLM).
- **Asserts:** after the real bootstrap seeds the card `Working` from the daemon's snapshot and the subscription is confirmed up, the daemon's OWN truth moves to `Idle` while the sole live `SubscribeEvents` connection is structurally incapable of observing it (it never calls `rx.recv()` — see `run_reconnect_teardown_server`'s doc comment, which makes the outage window exact by construction rather than by timing), then the connection ends with `KIND_STREAM_END "lagged"`. The reconnect loop's re-subscribe must be followed by a fresh `list_agents` drain that lands the card on `Idle`; today it isn't, so the card stays `Working` forever with nothing left to correct it.
- **Does not assert:** the BOOTSTRAP snapshot/subscribe window (`session/live/012`, fork issue #36 — a different, already-closed hazard); the daemon-side `"lagged"` reason itself, pinned in isolation by `daemon/protocol/001`; the no-reason transport-drop shape of the same hazard (`session/live/014`); rendered card output; a real daemon's timing.
- **Platform coverage:** mac+linux.

##### session/live/014 — A `SubscribeEvents` connection that dies mid-session WITHOUT a `lagged` reason — the connection just drops, standing in for a daemon restart or a bare transport failure — is recovered by the same reconnect re-hydration as the `lagged` case (issues #49/#28).
- **Layer:** L1 client/wire integration — same production reconnect bootstrap and mock-daemon harness as `session/live/013`.
- **Agent:** none (same mock daemon; no PTY, no LLM).
- **Asserts:** identical setup and outage-window construction to `session/live/013`, except the mock daemon's forced tear-down sends no `KIND_STREAM_END` frame at all — it just drops the connection (EOF), the shape the approved design ("always re-hydrate on reconnect", not just on `lagged`) exists to cover. The reconnected card must still converge on `Idle` off a fresh `list_agents` snapshot.
- **Does not assert:** the `lagged`-reason shape of the same hazard (`session/live/013`); the BOOTSTRAP snapshot/subscribe window (`session/live/012`); which client-side mechanism recovers the snapshot (only the observable end state — the status the TUI holds for the card — not internal call counts).
- **Platform coverage:** mac+linux.

### Session save (snapshot freshness, PRD #89 Phase 1)

These entries cover PRD #89 Phase 1: the saved-session snapshot must be kept continuously fresh — written on meaningful TUI state changes and on detach — not only at clean teardown/quit.

#### session/save

##### session/save/001 — A meaningful TUI state change (creating a new dashboard pane) writes a fresh saved-session snapshot to disk without quitting.
- **Layer:** L2 (real-binary PTY; `DOT_AGENT_DECK_SESSION` redirected to a test-owned path).
- **Agent:** none (the pane runs `sleep 600`; no LLM).
- **Asserts:** starting with no prior snapshot on disk, creating a new dashboard pane via the new-pane flow (Ctrl+N → dir-picker → form → submit) — and NOT quitting — causes a `session.toml` to be written that contains the newly created pane's command.
- **Does not assert:** the coalescing/debounce window (covered by `session/save/003`); restore-on-startup behavior (PRD #89 Phase 2).
- **Platform coverage:** mac+linux.

##### session/save/002 — Triggering a detach path (Ctrl+W close-pane) flushes a fresh snapshot reflecting the workspace, without quitting.
- **Layer:** L2 (real-binary PTY; `DOT_AGENT_DECK_SESSION` redirected to a test-owned path).
- **Agent:** none (panes run `sleep 600`; no LLM).
- **Asserts:** with two dashboard panes present and any prior snapshot removed, requesting a pane close with Ctrl+W and choosing Close with Down+Enter writes a fresh `session.toml` that still reflects the (non-empty) workspace — proving the detach path flushes the snapshot mid-session, not only at clean quit.
- **Does not assert:** which specific pane survives the close; the coalescing/debounce window (`session/save/003`).
- **Platform coverage:** mac+linux.

##### session/save/003 — A burst of meaningful state changes coalesces to at most one or two snapshot writes, not one per change.
- **Layer:** pure-data (in-crate `#[cfg(test)]` unit test on `config::SnapshotCoalescer`; no TUI harness, synchronous clock).
- **Agent:** none.
- **Asserts:** driving the coalescer (750 ms-style interval) with 50 rapid `mark_dirty` notifications observed at one instant — each followed by the loop's `is_due`/`record_write` check — produces only the leading-edge write; a single trailing check after the interval flushes the rest, for ≤2 total writes (and ≥1), and nothing is due once flushed.
- **Does not assert:** the production interval value, real wall-clock timing, or that the on-disk file content is correct (covered by `session/save/001`–`002`).
- **Platform coverage:** mac+linux+windows.

##### session/save/004 — Opening an orchestration tab captures its orchestration metadata into the saved-session snapshot (PRD #89 Phase 2b M2b.3 capture).
- **Layer:** L2 (real-binary PTY; `DOT_AGENT_DECK_SESSION` redirected to a test-owned path).
- **Agent:** none (the `orch-deck` fixture's `demo-orch` roles run `cat`; no LLM).
- **Asserts:** opening the fixture orchestration via the new-pane form (a Phase 1 M1.1 meaningful state change that flushes the coalesced snapshot) — and NOT quitting — writes a `session.toml` carrying a `[panes.orchestration]` block that records the resolved `config_name` (`demo-orch`), the roles (`orchestrator`, `worker`) in display order, and the `start_role_index` (`0`, the `start = true` orchestrator), so the daemon-empty restore path (`session/restore/008`) can rebuild the tab.
- **Does not assert:** the restore branch that consumes the metadata (`session/restore/008`–`009`); the serde round-trip of the schema in isolation (`config/saved-session/001`); the coalescing window (`session/save/003`).
- **Platform coverage:** mac+linux.

### Saved-session schema (orchestration metadata, PRD #89 Phase 2b)

This entry covers PRD #89 Phase 2b M2b.2: the saved-pane schema gains an `Option<OrchestrationSnapshot>` (role order, `start_role_index`, `orchestrator_prompt`, resolved config name + project path, `version`, and which roles were started) so the daemon-empty restore path can rebuild an orchestration tab. The field is `Option` + `#[serde(default)]` so old `session.toml` files still parse.

#### config/saved-session

##### config/saved-session/001 — An `OrchestrationSnapshot` on a saved pane, including its `owner` identity field, round-trips through TOML, and both a fully-legacy snapshot and a pre-`owner`-field snapshot still parse (PRD #89 Phase 2b M2b.2; `owner` coverage added fork #166 M3.0 / PR #215 fixup).
- **Layer:** pure-data (in-crate `#[cfg(test)]` unit test on `config::SavedSession` / `SavedPane` / `OrchestrationSnapshot`; no TUI harness, no I/O).
- **Agent:** none.
- **Asserts:** (a) a `SavedSession` whose pane carries an `OrchestrationSnapshot` (version, role order in display order, `start_role_index`, `orchestrator_prompt`, `config_name`, `project_path`, `started_role_indices`, `owner: Some("orchestration:tdd-cycle")`) serializes to TOML and deserializes back with every field intact, `owner` included; (b) a legacy `session.toml` string with no `orchestration` key parses with `orchestration == None` — the `#[serde(default)]` forward-compat guarantee for snapshots written before the block existed; (c) a `session.toml` string WITH an `[panes.orchestration]` block but no `owner` key (a snapshot written before that field existed) still parses, with `owner == None` — forward-compat for the field individually.
- **Does not assert:** the snapshot-fallback restore branch that consumes the metadata (M2b.3 / `session/restore/008`–`009`); the restore branch passing `owner` through to a spawned pane (`session/restore/017`); capture (populating the fields when writing the snapshot); any TUI rendering.
- **Platform coverage:** mac+linux+windows.

### CLI surface (PRD #89 Phase 3)

#### cli/continue-removed

##### cli/continue-removed/001 — `--continue` is removed from the CLI surface and rejected on use (PRD #89 Phase 3).
- **Layer:** L2 (thin real-binary subprocess spawn; no PTY drive).
- **Agent:** none.
- **Asserts:** `dot-agent-deck --help` no longer advertises `--continue`, and `dot-agent-deck --continue` exits non-zero with a message that references the flag (guiding the user toward the now-default auto-restore). Since auto-restore is unconditional, the flag has no remaining purpose.
- **Does not assert:** the exact wording of the rejection message (clap's default unknown-argument text or a custom friendly message both satisfy it).
- **Platform coverage:** mac+linux.

### Fresh-start escape hatch (PRD #89 Phase 4)

These entries cover PRD #89 Phase 4: with auto-restore now the default, a user who wants to start clean has one obvious action — `dot-agent-deck snapshot clear` (M4.2) — because the snapshot is a single GLOBAL file. `dot-agent-deck remote remove <name>` (M4.1) is registry-only and intentionally does NOT touch the snapshot (decided Option 1); there is no per-deck saved state to clear.

#### session/snapshot

##### session/snapshot/001 — `dot-agent-deck snapshot clear` deletes the local saved-session snapshot (PRD #89 Phase 4 M4.2).
- **Layer:** L2 (thin real-binary subprocess spawn; no PTY drive; `DOT_AGENT_DECK_SESSION` redirected to a test-owned path).
- **Agent:** none.
- **Asserts:** with a non-empty `session.toml` staged at the redirected path, running `dot-agent-deck snapshot clear` exits 0 and the snapshot file is gone afterward — the local fresh-start escape hatch. The command shape is a `snapshot` subcommand group with a `clear` action (decided; not `reset`/`--reset`).
- **Does not assert:** the subsequent no-flag startup landing on an empty dashboard (that follows from the deleted snapshot + `session/restore/006`); the exact stdout wording of the success message.
- **Platform coverage:** mac+linux.

##### session/snapshot/002 — `dot-agent-deck remote remove <name>` is registry-only and leaves the global snapshot intact (PRD #89 Phase 4 M4.1, Option 1).
- **Layer:** L2 (thin real-binary subprocess spawn; no PTY drive; `DOT_AGENT_DECK_SESSION` + `DOT_AGENT_DECK_REMOTES` redirected to test-owned paths).
- **Agent:** none.
- **Asserts:** with a remote deck `myhost` registered in the staged `remotes.toml` and a non-empty `session.toml` staged, running `dot-agent-deck remote remove myhost` exits 0 AND leaves the global snapshot intact — the file is still present afterward with byte-for-byte unchanged contents. The snapshot is a single GLOBAL file, so remove is registry-only (decided Option 1); there is no per-deck saved state to clear and `snapshot clear` (001) is the one fresh-start action.
- **Does not assert:** that the registry entry was removed (that is `remote remove`'s pre-existing behavior, exercised elsewhere); any per-deck keying of saved state (none exists — the snapshot is a single global file).
- **Platform coverage:** mac+linux.

### Chain-smoke (real-agent) coverage

#### codex/wrap

##### codex/wrap/001 — A synthetic Codex JSONL stream runs through the real wrapper, daemon event stream, and PTY-attached dashboard (PRD #20 M7).
- **Layer:** L2 PTY-attached (`TuiDeck`, real binary + daemon, deterministic shell stand-in, no authentication or LLM).
- **Agent:** synthetic stand-in wrapped with `dot-agent-deck wrap --agent codex`.
- **Asserts:** realistic turn-start and turn-completed lines become typed Codex `AgentEvent`s carrying `AGENT_EVENT_SCHEMA_VERSION`; because the wrapper is inside a daemon-managed pane, events declare `Pty/Live`; `WriteAndSubmit` returns Applied and the child records the submitted line; the rendered card visibly shows this pane's identity and transitions Thinking → Idle (fork-only: no agent-type badge — the card's identity is its own pane id, since no display name was given).
- **Does not assert:** model authentication or Codex CLI behavior (covered by `codex/live/001`).
- **Platform coverage:** mac+linux.

##### codex/wrap/002 — Wrapped children retain TTY identity, resize delivery, input, and Ctrl+C behavior (PRD #20, blocker 1 / finding 16).
- **Layer:** L2 PTY-attached (`TuiDeck` + deterministic shell probe under the real wrapper; no LLM).
- **Agent:** synthetic terminal probe wrapped as Codex.
- **Asserts:** the child observes `isatty(0/1/2) == true`, receives SIGWINCH after resize, records ordinary input, and handles Ctrl+C as SIGINT.
- **Does not assert:** Codex output parsing or model behavior (`codex/live/001`).
- **Platform coverage:** mac+linux.

##### codex/wrap/003 — Wrapped commands preserve each standard descriptor's independent TTY or redirection semantics (PRD #20, final review finding 11).
- **Layer:** L1/fast real-binary subprocess integration with controlled pseudo-terminals and files; no TUI or LLM.
- **Agent:** deterministic shell probes wrapped as Codex.
- **Asserts:** wholly non-interactive stdout/stderr remain separate and binary stdin remains byte-exact through EOF; redirecting only stderr sends child stderr to that file rather than merged PTY stdout; redirecting only stdout leaves child stdin and stderr attached to TTYs.
- **Does not assert:** interactive resize, ordinary input, or Ctrl+C behavior (`codex/wrap/002`).
- **Platform coverage:** mac+linux.

##### codex/wrap/004 — Catchable termination signals tear down and reap wrapped children on PTY and pipe paths (PRD #20, final review finding 12).
- **Layer:** L1/fast real-binary subprocess integration with a controlled pseudo-terminal for the interactive path and null descriptors for the pipe path; no TUI or LLM.
- **Agent:** deterministic lingering shell child wrapped as Codex.
- **Asserts:** after SIGTERM and SIGHUP are delivered to the wrapper, both interactive PTY and non-interactive pipe wrappers exit and their recorded child process is no longer running.
- **Does not assert:** the pre-spawn signal race or termios restoration during a signal arriving inside setup; those timing edges are not deterministic at this subprocess seam.
- **Platform coverage:** mac+linux.

##### codex/wrap/005 — Concurrent standalone wrappers emit unique session IDs (PRD #20 Greptile finding #4).
- **Layer:** L1/fast real-binary subprocess integration with a synthetic hook socket; no TUI or LLM.
- **Agent:** two overlapping deterministic shell probes wrapped with Codex identity and no pane environment ID.
- **Asserts:** the two wrapper lifecycles produce two distinct session IDs instead of reconciling onto one synthetic `wrap-<program>` ID.
- **Does not assert:** managed-pane session IDs, which intentionally remain pane-derived and are covered by `codex/wrap/001`.
- **Platform coverage:** mac+linux.

#### codex/trust

##### codex/trust/001 — No Codex launch form receives an invocation-global hook-trust bypass (PRD #20 Greptile P1 close-by-deletion).
- **Layer:** L1/fast real-binary subprocess integration with controlled Codex homes and executable stand-ins.
- **Agent:** deterministic bare `codex`, absolute `/path/codex`, launcher script, and `devbox` stand-ins.
- **Asserts:** every launch form inherits the pinned `CODEX_HOME`, and none receives `--dangerously-bypass-hook-trust`; the hazardous global mechanism is absent rather than launcher-identity-gated.
- **Does not assert:** scoped trust records (covered by `codex/trust/002`–`003`) or real Codex hook execution.
- **Platform coverage:** mac+linux.

##### codex/trust/002 — Scoped trust selects only pinned-home, unmanaged, deck-owned hook entries (PRD #20 §4.3.1/§4.3.6).
- **Layer:** L1/fast real-binary subprocess integration with a deterministic `codex app-server` JSON-RPC stand-in, exercised through both bare Codex and a launcher script.
- **Agent:** synthetic Codex hooks/list response containing one eligible deck hook plus a foreign command, a deck command from a different home, a managed entry, and a user command that merely mentions `dot-agent-deck`.
- **Asserts:** `[hooks.state]` contains only the eligible entry whose `sourcePath` is the pinned home's `hooks.json`, command ends in `hook --agent codex`, and `isManaged` is false; the global bypass never appears for either launch method.
- **Does not assert:** byte-preserving config edits or untrust behavior (covered by `codex/trust/003`).
- **Platform coverage:** mac+linux.

##### codex/trust/003 — Scoped trust config edits preserve user bytes, remain idempotent, and untrust only deck keys (PRD #20 §4.3.2).
- **Layer:** L1/fast real-binary subprocess integration with an isolated Codex home and deterministic app-server stand-in.
- **Agent:** synthetic deck hook identity plus a pre-existing user `config.toml` containing a comment, model selection, and foreign trust record.
- **Asserts:** trust appends exactly one hash-pinned deck table while preserving the existing config bytes verbatim; a second write creates no duplicate; Codex hook uninstall removes the deck key and retains the foreign key.
- **Does not assert:** Codex's runtime trust-status interpretation (covered by the real-agent green-confirm scenario).
- **Platform coverage:** mac+linux.

#### codex/spawn

##### codex/spawn/001 — Plain restored Codex panes launch through the Wrapper strategy (PRD #20, blocker 3).
- **Layer:** L2 synthetic PTY-attached restore with PATH recorder stubs.
- **Agent:** synthetic Codex recorder.
- **Asserts:** restoring persisted bare `codex` executes exactly `dot-agent-deck wrap --agent codex -- codex` and never the bare recorder.
- **Does not assert:** mode or orchestration paths (002–003).
- **Platform coverage:** mac+linux.

##### codex/spawn/002 — Mode-pane Codex commands launch through the Wrapper strategy (PRD #20, blocker 3).
- **Layer:** L2 synthetic PTY-attached new-pane mode flow with PATH recorder stubs.
- **Agent:** synthetic Codex recorder.
- **Asserts:** selecting a workload mode while Command is bare `codex` injects the wrapped command into the mode pane, never bare Codex.
- **Does not assert:** restore or orchestration paths.
- **Platform coverage:** mac+linux.

##### codex/spawn/003 — Orchestration role Codex commands launch through the Wrapper strategy (PRD #20, blocker 3).
- **Layer:** L2 synthetic PTY-attached new-pane orchestration flow with PATH recorder stubs.
- **Agent:** synthetic Codex recorder as the start role.
- **Asserts:** selecting an orchestration whose start-role command is bare `codex` launches the role through the wrapper exactly once.
- **Does not assert:** scheduler role spawning (`scheduler/spawn/006`) or respawn.
- **Platform coverage:** mac+linux.

##### codex/spawn/004 — Restored mode-pane Codex commands launch through the Wrapper strategy (PRD #20, blocker 3).
- **Layer:** L2 synthetic PTY-attached saved-session mode restore with PATH recorder stubs.
- **Agent:** synthetic Codex recorder.
- **Asserts:** a saved pane carrying `mode = "wrapped-mode"` and bare command `codex` rebuilds the mode tab and injects the wrapper command, never bare Codex.
- **Does not assert:** fresh mode creation (`codex/spawn/002`) or plain restore (`codex/spawn/001`).
- **Platform coverage:** mac+linux.

##### codex/spawn/005 — Respawning an existing pane as Codex launches through the Wrapper strategy (PRD #20, blocker 3).
- **Layer:** fast PTY registry integration (`AgentPtyRegistry::respawn_agent_for_pane` with PATH recorder stubs).
- **Agent:** synthetic Codex recorder.
- **Asserts:** replacing an existing pane process with bare command `codex` executes exactly `dot-agent-deck wrap --agent codex -- codex`.
- **Does not assert:** delegate routing that chooses respawn; it pins the respawn spawn boundary itself.
- **Platform coverage:** mac+linux.

##### codex/spawn/006 — An explicit Codex identity wraps a non-inferable custom launcher and remains the pane identity (PRD #20, R20-009).
- **Layer:** fast PTY registry integration (`AgentPtyRegistry::spawn_agent` with PATH recorder stubs).
- **Agent:** synthetic custom launcher explicitly declared as Codex.
- **Asserts:** a command whose basename is not `codex` still executes exactly through `dot-agent-deck wrap --agent codex -- ...` when the caller supplies `AgentType::Codex`, and the live registry records that pane as Codex.
- **Does not assert:** command-string inference (covered by the detection matrix) or real Codex behavior.
- **Platform coverage:** mac+linux.

##### codex/spawn/007 — A hook-learned Codex badge does not mutate a non-inferable pane's launch shape on respawn (PRD #225 M1).
- **Layer:** fast PTY registry integration (`AgentPtyRegistry::spawn_agent` + hook-path `set_agent_type` + `respawn_agent_for_pane`, with PATH recorder stubs).
- **Agent:** synthetic `devbox run codex-big` launcher whose basename intentionally does not infer an agent type.
- **Asserts:** the initial and replacement exec records are byte-identical `devbox run codex-big` lines even after the registry badge upgrades from `None` to `Some(Codex)`; no `dot-agent-deck wrap` line appears on respawn.
- **Does not assert:** daemon hook-socket ingestion of the badge (covered by `hooks/delivery/007`); an EDITED role command's effect on the wrap decision (`codex/spawn/008`); real Codex behavior.
- **Platform coverage:** mac+linux.

##### codex/spawn/008 — A respawn's wrap decision follows the command it is actually launching, so an explicit Codex identity can never wrap a different agent (PRD #225 review finding 1).
- **Layer:** fast PTY registry integration (`AgentPtyRegistry::spawn_agent` + two `respawn_agent_for_pane` calls, with PATH recorder stubs for `devbox`, `claude`, and `dot-agent-deck`).
- **Agent:** synthetic `devbox run codex-big` launcher spawned with an explicit `AgentType::Codex` identity, then respawned once with that same command and once with the role command edited to `claude --model haiku`.
- **Asserts:** the unchanged respawn relaunches byte-identically as `dot-agent-deck wrap --agent codex -- devbox run codex-big` (the frozen identity is the only thing that knows this launcher is Codex); the edited respawn executes a bare `claude --model haiku` and never `wrap --agent codex -- claude …`; and the pane badge follows the newly launched command (`ClaudeCode`) instead of still advertising the replaced agent. Both halves are load-bearing — replaying the frozen identity verbatim wraps Claude as Codex, and dropping it flips the unchanged pane to bare.
- **Does not assert:** the hook-learned badge path (`codex/spawn/007`); a launcher whose command implies no type AND whose underlying agent changed (`devbox run codex-big` → `devbox run claude-big`), which keeps its creation-time identity by documented design.
- **Platform coverage:** mac+linux.

#### codex/hooks

##### codex/hooks/001 — A real launcher-script interactive Codex turn reports native prompt/tool detail and becomes Idle without process exit (PRD #20 W1, R20-013/R20-014, §4.3.7). [reel]
- **Layer:** L2 PTY-attached (`TuiDeck`, reel-eligible); runtime-skipped unless `check_codex_available` verifies the binary, persisted auth, and a live model request.
- **Agent:** real interactive Codex on the cheap test model, launched through a recorder script named `codex` ahead of PATH with isolated credentials and a fresh Codex home, workspace-write sandbox, no approvals, network-enabled sandbox configuration, and low reasoning effort; launch passes through the normal Wrapper strategy seam.
- **Asserts:** the launcher handles both the deck's `app-server` trust probe and the interactive agent without receiving `--dangerously-bypass-hook-trust`; the fresh home trusts exactly the deck's ten scoped hook keys; those hooks emit a prompt-bearing Thinking event, shell ToolStart/ToolEnd events with sentinel command detail, and Stop-hook Idle; the dashboard visibly retains prompt/tool detail and shows Idle, the requested sentinel contains exact known content, and the Codex pane is still alive because the test never sends `/exit`.
- **Does not assert:** stdout JSONL classification (covered by `codex/wrap/001`) or exact model prose.
- **Platform coverage:** mac+linux (real-agent tier is local-only).
- **Cost note:** one minimal mini-model availability probe plus one short interactive shell-tool turn.

##### codex/hooks/002 — A script-launched Codex inherits its pinned home and exactly the deck's scoped trust records (PRD #20 §4.3.5).
- **Layer:** L2 synthetic real-binary subprocess under the `e2e` feature; deterministic launcher and `codex app-server` stand-ins, no LLM.
- **Agent:** launcher script executing a Codex stand-in with all ten deck hook identities returned by hooks/list.
- **Asserts:** `hooks.json` is installed, the child inherits the pinned `CODEX_HOME`, child argv has no global bypass, and `[hooks.state]` names exactly the ten deck keys.
- **Does not assert:** rendered dashboard behavior or real Codex hook execution.
- **Platform coverage:** mac+linux.

##### codex/hooks/003 — Command-agnostic startup integration delivers Codex events from a non-Codex-basename launcher (PRD #20 §4.2.1/§4.3.6).
- **Layer:** L2 PTY-attached (`TuiDeck`, real binary + daemon, deterministic launcher and app-server stand-ins, no LLM).
- **Agent:** restored `/bin/sh startup-parity-launcher.sh` pane whose command cannot infer Codex identity; the launcher emits a Codex hook event only after startup scoped trust exists.
- **Asserts:** daemon/TUI startup installs and trusts Codex hooks independently of the pane command basename, then the emitted prompt visibly creates a Codex card showing Thinking and the prompt sentinel.
- **Does not assert:** wrapper classification, explicit role `agent = "codex"`, or real Codex execution.
- **Platform coverage:** mac+linux.

##### codex/hooks/004 — The documented Codex hook-install CLI succeeds (PRD #20 §4.2.1).
- **Layer:** L1/fast real-binary subprocess integration with an isolated home and deterministic Codex app-server stand-in.
- **Agent:** synthetic Codex installation environment.
- **Asserts:** `dot-agent-deck hooks install --agent codex` exits successfully and creates `hooks.json` instead of reporting `No hook installer for agent Codex`.
- **Does not assert:** uninstall scoping (covered by `codex/trust/003`) or dashboard rendering.
- **Platform coverage:** mac+linux.

#### codex/live

##### codex/live/001 — A real interactive cheap-model Codex run launched through the normal new-pane flow works visibly and reports live status (PRD #20, rule 4 / finding 16). [reel]
- **Layer:** L2 PTY-attached (`TuiDeck`, reel-eligible); runtime-skipped unless `check_codex_available` verifies the binary, persisted auth, and a live model request.
- **Agent:** real interactive bare `codex` using `gpt-5.1-codex-mini`, isolated copied credentials, workspace-write sandbox, and low reasoning effort; automatic wrapping occurs at the normal pane spawn seam.
- **Asserts:** the interactive pane becomes ready, accepts a typed prompt, uses the shell to list the fixture and writes a proof file naming `codex_sentinel_a7c91f.txt`; after detach, the visible Codex card has traversed Thinking → Idle.
- **Does not assert:** exact model phrasing or token usage.
- **Platform coverage:** mac+linux (real-agent tier is local-only).
- **Cost note:** one minimal mini-model availability probe plus one short interactive directory-listing/file-write turn.

#### codex/worker

##### codex/worker/001 — A real wrapped Codex orchestration worker receives a delegated task, does the work, and signals work-done (PRD #20 parity gap #12).
- **Layer:** L2 headless in-process daemon plus a real interactive Codex PTY; runtime-skipped unless `check_codex_available` verifies the CLI, persisted auth, and model access.
- **Agent:** real `gpt-5.1-codex-mini` Codex configured as the `coder` role with workspace-write sandboxing, approval disabled, low reasoning effort, isolated copied credentials, and project trust; the common spawn seam automatically launches it through `dot-agent-deck wrap`.
- **Asserts:** Codex auto-submits the daemon-injected single-line `worker-task-coder.md` pointer, reads the delegated task, creates `codex_worker_sentinel_c81f2a.txt` with exact known contents, and runs the task footer's `dot-agent-deck work-done` command so the daemon writes `.dot-agent-deck/work-done-coder-<pane digest>.md`.
- **Does not assert:** exact model phrasing, token usage, or dashboard rendering (covered by `codex/live/001`).
- **Platform coverage:** mac+linux (real-agent tier is local-only).
- **Cost note:** one minimal mini-model availability probe plus one short worker turn.

#### devin/live

##### devin/live/001 — A real interactive Devin turn drives the dashboard card live through the deck's own installed hooks. [reel]
- **Layer:** L2 PTY-attached (`TuiDeck`, reel-eligible); runtime-skipped unless `check_devin_available` verifies the binary, persisted credentials, and a logged-in account.
- **Agent:** real interactive `devin` restored into a pane, using the account's default (cheap SWE-family) model with isolated copied credentials, the setup wizard pre-satisfied, workspace trust waived, and `--permission-mode auto`; launch goes through the normal `NativeHooks` seam with no wrapper.
- **Asserts:** the deck-written `"hooks"` block in Devin's own config is actually read and executed by the third-party binary — a typed prompt produces a Devin-stamped Thinking event and a visible Thinking card, an `exec` ToolStart carrying a non-empty tool detail, the pane showing `devin_live_sentinel_4c81de.txt`, and a Stop-driven Idle.
- **Does not assert:** exact model phrasing or token usage; hook payload parsing in isolation (covered by the fast-tier `devin_hook_ingestion` tests) or config-merge safety (covered by the `devin_hooks_manage` unit tests).
- **Platform coverage:** linux+mac (real-agent tier is local-only; `devin_config_dir` is Unix-only by design).
- **Cost note:** one inference-free `devin auth status` probe plus one short interactive directory-listing turn — measured at roughly 2.7s of agent time. No `--model` is pinned because a free-tier account rejects every explicit model.

#### chain-smoke/claude

##### chain-smoke/claude/001 — A real Claude Code agent run end-to-end emits hook events that drive the card through Thinking → Working → Idle.
- **Layer:** L2.
- **Agent:** Claude Code (`claude-haiku-4-5-20251001` per Decision 8).
- **Asserts:** card status traverses Thinking → Working → Idle within the test budget; tool name appears on the card during Working.
- **Does not assert:** any specific text the agent prints.
- **Platform coverage:** mac+linux (chain-smoke is local-only per Decision 8).
- **Cost note:** one Haiku invocation, ≲500 input + 200 output tokens — well under Decision 23's bound.

#### chain-smoke/opencode

##### chain-smoke/opencode/001 — A real OpenCode agent run end-to-end emits the OpenCode plugin's events and drives the card through Thinking → Working → Idle.
- **Layer:** L2.
- **Agent:** OpenCode (`openrouter/google/gemini-2.5-flash-lite` per Decision 8).
- **Asserts:** card status traverses Thinking → Working → Idle; OpenCode-format tool name appears on the card.
- **Does not assert:** any agent-generated text.
- **Platform coverage:** mac+linux.
- **Cost note:** one Gemini-Flash-Lite invocation via OpenRouter, ≲500 input + 200 output tokens.

#### chain-smoke/pi

##### chain-smoke/pi/001 — A REAL `pi` orchestrator, driving a real model, loads the bundled extension, calls the native `delegate` tool, the daemon routes to a REAL `claude` worker that creates a uniquely-named sentinel + signals `work-done`, and the Pi pane's status is tracked via `agent-event` with NO hook (PRD #201 M4.1, the flagship).
- **Layer:** L2 (in-process daemon whose hook loop routes `delegate`/`work-done`/`agent-event` and re-broadcasts `AgentEvent`s; real agent PTYs via `AgentPtyRegistry::spawn_agent` — the `e2e` tier, hits a real model). Mirrors `e2e_delegate_work_done_chain.rs` with the ORCHESTRATOR role swapped to `pi`: the worker (spawned + ready first) is a black-box `claude` with its hooks/CLI unchanged; the orchestrator is a real `pi` whose HOME carries the bundled extension (materialized via `orchestrator_ext::materialize`). `OPENROUTER_API_KEY` + `HOME` are explicitly propagated into the pi child's `opts.env` (the key is never printed).
- **Agent:** REAL `pi` 0.80.6 orchestrator (`--provider openrouter --model openai/gpt-5-nano --approve`, the cheapest GPT-5.x tier that reliably tool-calls) + REAL Claude Code (Haiku, `claude-haiku-4-5-20251001`, `--allowedTools Bash Read Write`) worker. Flaky-tolerant pre-PR tier (real LLM) — run once, not looped (rule 4/5). Runtime-skipped (Decision 26) when `pi`/`claude`/credentials/`OPENROUTER_API_KEY` are absent.
- **Asserts:** the directive-prompted pi calls the native `delegate` tool once (role `coder`), the daemon routes it into the pre-spawned worker pane, and the real worker creates the sentinel `pi_orch_sentinel_7c3f.txt` (contents `PI_ORCH_SENTINEL_OK`) via the delegated task (proves the full pi→daemon→worker route ran); the daemon writes `.dot-agent-deck/work-done-coder-<pane digest>.md` (work-done returned to the orchestrator); and a `Pi`-typed `AgentEvent` for the orchestrator pane rode the daemon's broadcast — status tracked through the extension's `agent-event` path with NO hook installed. Generous per-step timeouts (240s sentinel / 120s work-done) sized to confidence, not token cost (Design Decision #7).
- **Does not assert:** exact agent phrasing / the exact task text pi forwards (the sentinel filename + content are the literal tokens that must survive); the extension's per-event state mapping (covered deterministically by the TS unit tests + synthetic `status/agent-event/003`); the daemon's routing/role-guard internals (covered by `orchestration/delegate/*`).
- **Platform coverage:** mac+linux (real-agent tier is local-only per Decision 8).
- **Cost note:** one cheap gpt-5-nano turn (orchestrator delegates) + one short Haiku turn (worker creates a file + work-done) — well under Decision 23's <$0.05/run bound.

##### chain-smoke/pi/002 — A REAL `pi` WORKER receives a daemon-injected delegate (the agent-agnostic `worker-task-<role>.md` footer + `write_to_pane_and_submit` path), does the task (creates a uniquely-named sentinel), and signals `work-done` back — proving pi's SECOND role (PRD #201 completeness; the orchestrator role is pinned by `chain-smoke/pi/001` + `pi/live/002`).
- **Layer:** L2 (in-process daemon whose hook loop ingests `work-done` over the socket; a real pi worker PTY via `AgentPtyRegistry::spawn_agent` — the `e2e` tier, hits a real model). HEADLESS (a functional proof, not a reel clip — the orchestrator-role reel clip is `pi/live/002`). Reuses the real-pi machinery of `chain-smoke/pi/001`: the pi worker's HOME carries the bundled extension (materialized via `orchestrator_ext::materialize`), and `OPENROUTER_API_KEY` + `HOME` (+ pane/socket/PATH) are explicitly propagated into the pi child's `opts.env` (the key is never printed). The ORCHESTRATOR side is the DETERMINISTIC synthetic-delegate path — `AppState::handle_delegate` with a synthetic `DelegateSignal` from an un-spawned orchestrator pane (the pattern of `e2e_delegate_work_done_chain.rs`) — chosen because the WORKER is the thing under test and a real orchestrator would add LLM flakiness without adding to the worker proof (the genuine real-pi-orchestrator ⇄ real-worker mix is already pinned by `chain-smoke/pi/001` + `pi/live/002`). `clear = false` (no `.dot-agent-deck.toml` role config ⇒ `handle_delegate` role lookup returns `None` ⇒ no respawn): the pi worker is spawned ONCE and the delegate injects only after it is polled to genuine input-readiness — deliberately isolating the worker proof from the separately-tracked `clear = true`-respawn + 10s-`SESSION_START_WAIT`-fallback fragility (pi never emits `EventType::SessionStart`).
- **Agent:** REAL `pi` (`--provider openrouter --model openai/gpt-5-nano --approve`) spawned IDLE (no CLI-arg prompt) as the `coder` WORKER pane — seeded ONLY by the daemon-injected worker-task pointer. Flaky-tolerant pre-PR tier (real LLM) — run once, not looped (rule 4/5). Runtime-skipped (Decision 26) when `pi` / `OPENROUTER_API_KEY` is absent.
- **Asserts:** the full WORKER chain — the pi worker AUTO-SUBMITTED the daemon-injected single-line `worker-task-coder.md` pointer, read its task file, and created the sentinel `pi_worker_sentinel_9d2e.txt` (contents `PI_WORKER_SENTINEL_OK`) — proving it RECEIVED and DID the delegated task; and the daemon wrote `.dot-agent-deck/work-done-coder-<pane digest>.md`, proving the pi worker SIGNALLED work-done over the hook socket (via the footer `dot-agent-deck work-done` CLI or the extension's native `work_done` tool — either routes the same `WorkDone` signal, so the file's appearance is a path-agnostic proof). Generous per-step timeouts (240s sentinel / 120s work-done) sized to confidence, not token cost (Design Decision #7).
- **Does not assert:** exact agent phrasing / the exact task text (the sentinel filename + content are the literal tokens that must survive); WHICH work-done path pi took (CLI vs native tool — both produce the same file); the `clear = true`-respawn worker path with pi (isolated out via `clear = false`; that path's 10s-fallback fragility is tracked for the companion PRD); the extension's per-event status mapping (covered by the TS unit tests + synthetic `status/agent-event/003`).
- **Platform coverage:** mac+linux (real-agent tier is local-only per Decision 8).
- **Cost note:** one short gpt-5-nano worker turn (read a task file, create a file, work-done) — well under Decision 23's <$0.05/run bound.

#### pi/live

##### pi/live/001 — A REAL `pi` agent runs LIVE in a PTY-attached pane and its card renders a real, extension-driven status TRANSITION on the vt100 grid, with NO hook (PRD #201, CLAUDE.md rule 4 + PRD #180 reel-eligibility).
- **Layer:** L2 PTY-attached (the REAL `dot-agent-deck` binary driven through the vt100 `TuiDeck` harness — records a `full-stream.cast`, so it is demo-reel-eligible per PRD #180, unlike the HEADLESS `chain-smoke/pi/001` + `scheduler/pi/001`). Mirrors `e2e_issue_dispatch_real.rs` / `e2e_chain_smoke_claude.rs` and the reference `scheduler/dispatch/013`. The bundled extension is materialized into the per-test HOME BEFORE launch (`TuiDeckBuilder::with_pi_extension`) so the deck's lazy-spawned daemon — and the pi child it spawns, which inherits that HOME — auto-discovers it at boot; `OPENROUTER_API_KEY` + the built-binary PATH are threaded into the deck via `with_env` (the key is never printed). Launched with `DOT_AGENT_DECK_EXPERIMENTAL=1`.
- **Agent:** REAL `pi` (`--provider openrouter --model openai/gpt-5-nano --approve`, the cheapest GPT-5.x tier that reliably runs a directive turn) as a single interactive pane restored from a staged saved session. Flaky-tolerant pre-PR tier (real LLM) — run once, not looped (rule 4/5). Runtime-skipped (Decision 26) when `pi` / `OPENROUTER_API_KEY` is absent.
- **Asserts (on the rendered vt100 grid):** after detaching to the dashboard, the Pi pane's card shows a REAL, extension-driven status TRANSITION with NO hook installed — `Thinking` (extension `agent_start`→running), an Idle → running transition the daemon never produces on its own for a hook-less Pi pane (a freshly-spawned pane defaults to Idle, and `session_start` now reports Idle for parity with Claude/OpenCode/Codex, so `Thinking` — not the retired `Needs Input` — is the extension-only proof), then a settle back to `Idle` (extension `agent_settled`→finished, polled on the CURRENT grid so it can only be the post-turn settled frame — this is the turn-end→Idle mapping the fix changed, so a regression to "Needs Input" fails here) — and the card itself stays on screen throughout, identified by its pane id (fork-only: no agent-type badge, so there is no `Pi ·` identity to render). The `Thinking` step is scanned over the rolling byte history so a transient frame still matches; generous 180s ceilings sized to confidence (Design Decision #7).
- **Does not assert:** the orchestrator→worker delegation chain (a single live Pi pane fully satisfies rule 4; the delegate route is pinned headless by `chain-smoke/pi/001` and LIVE + injection-seeded by `pi/live/002`); any specific text pi prints; the directed sentinel file (`pi_live_sentinel_4b1a.txt`) is a best-effort/logged secondary signal, not a gate, since the rendered status transition already proves the pi turn ran.
- **Platform coverage:** mac+linux (real-agent tier is local-only per Decision 8).
- **Cost note:** one cheap gpt-5-nano directive turn (create one file) — well under Decision 23's <$0.05/run bound.

##### pi/live/002 — A REAL `pi` orchestrator AUTO-SUBMITS a daemon-INJECTED seed (the production restore-path injection, NOT a CLI arg) and drives a full orchestration LIVE on the vt100 grid: pi → native `delegate` → real `claude` Haiku worker creates a uniquely-named sentinel + signals `work-done` (PRD #201 parity GAP #1 + the real-usage orchestration reel clip).
- **Layer:** L2 PTY-attached (the REAL `dot-agent-deck` binary driven through the vt100 `TuiDeck` harness — records a `full-stream.cast`, demo-reel-eligible per PRD #180). Closes the gap left by `chain-smoke/pi/001` (headless, CLI-arg seeded) and `pi/live/001` (single live pane): a two-role orchestration is staged as a `.dot-agent-deck.toml` (`[[orchestrations]] name = "pi-parity"`, an `orchestrator` START role running an IDLE real `pi` + a `coder` role running an IDLE real `claude`, neither carrying a CLI-arg prompt) plus a `session.toml` whose `OrchestrationSnapshot.orchestrator_prompt` is the delegate directive; on the daemon-empty restore the deck spawns both role panes IDLE and REPLAYS the directive into the pi START role via the PRODUCTION `write_and_submit_to_pane` injection primitive (single-line write, SUBMIT_DELAY, then `\r`) — the exact auto-submit path shipped code relies on. The bundled Pi extension is materialized into the per-test HOME BEFORE launch (`with_pi_extension`); imported Claude credentials + `with_claude_project_trust` for the shared orchestration cwd clear the worker's first-run gates; `OPENROUTER_API_KEY` + the built-binary PATH are threaded in via `with_env` (the key is never printed). Launched with `DOT_AGENT_DECK_EXPERIMENTAL=1`.
- **Agent:** REAL `pi` orchestrator (`--provider openrouter --model openai/gpt-5-nano --approve`, seeded ONLY by injection — no CLI-arg prompt) + REAL Claude Code (Haiku, `claude-haiku-4-5-20251001`, `--allowedTools Bash Read Write`) worker with the NORMAL toolset (no `--no-builtin-tools`, no stand-ins). Flaky-tolerant pre-PR tier (real LLM) — run once, not looped (rule 4/5). Runtime-skipped (Decision 26) when `pi` / `claude` / credentials / `OPENROUTER_API_KEY` are absent.
- **Asserts:** AUTO-SUBMIT CHECKPOINT (GAP #1) — the daemon writes `.dot-agent-deck/worker-task-coder.md` ONLY inside `handle_delegate`, so its appearance is the isolated proof that pi AUTO-SUBMITTED the daemon-INJECTED seed and called the native `delegate` tool; the delegate pointer `worker-task-coder` renders LIVE in the worker pane on the orchestration grid (the user-visible "delegation happening + worker" reality); and the full chain landed — the delegated worker created the sentinel `pi_inject_orch_sentinel_5e8c.txt` (contents `PI_INJECT_ORCH_OK`) and the daemon wrote `.dot-agent-deck/work-done-coder-<pane digest>.md`. Generous per-step ceilings sized to confidence, not token cost (Design Decision #7).
- **Does not assert:** any agent-type card title (fork-only: no card carries one; a named orchestration role pane titles its card with the ROLE name regardless — the no-badge surface is pinned by `pi/live/001` + `dashboard/pane/007`); the exact task text pi forwards (the sentinel filename + content are the literal tokens that must survive LLM phrasing); the extension's per-event state mapping (covered by the TS unit tests + synthetic `status/agent-event/003`).
- **Platform coverage:** mac+linux (real-agent tier is local-only per Decision 8).
- **Cost note:** one cheap gpt-5-nano turn (orchestrator delegates) + one short Haiku turn (worker creates a file + work-done) — well under Decision 23's <$0.05/run bound.

### Mouse Parity (PRD #80)

These entries cover PRD #80 (mouse parity for keyboard actions): every keyboard-only TUI action gains a clickable affordance carrying its shortcut inline, funneled through the single `dispatch_action` action layer.

#### mouse/dispatch

##### mouse/dispatch/001 — Ctrl+N (key) and a click on a New-Pane button rect map to the same `Action::NewPane`.
- **Layer:** pure-data (plain logic, no TUI harness).
- **Agent:** none.
- **Asserts:** `global_ctrl_action(Ctrl+N)` and `hit_test_button` on a synthetic New-Pane button rect both yield `Action::NewPane`; a click that misses every rect yields `None`.
- **Does not assert:** rendering or end-to-end dispatch side effects.
- **Platform coverage:** mac+linux+windows.

#### mouse/button

##### mouse/button/001 — The Button widget renders its inline-shortcut label and dims a disabled button.
- **Layer:** L1 (ratatui `TestBackend`).
- **Agent:** none.
- **Asserts:** an enabled button renders `[Label Shortcut]` un-dimmed and returns its `(Action, Rect)` pair; a disabled button renders the label with the DIM modifier.
- **Does not assert:** click dispatch (covered by `mouse/dispatch/001`).
- **Platform coverage:** mac+linux+windows.

#### mouse/buttonbar

##### mouse/buttonbar/001 — At a comfortable width the global bar renders a button per command with its inline shortcut.
- **Layer:** L1.
- **Agent:** none.
- **Asserts:** the bottom row shows `[New Pane Ctrl+N]`, `[Close Ctrl+W]`, `[Toggle Layout Ctrl+T]`, `[Help ?]`, and `[Quit Ctrl+C]`.
- **Does not assert:** click behavior (covered by `mouse/buttonbar/003`).
- **Platform coverage:** mac+linux+windows.

##### mouse/buttonbar/002 — On a narrow/windowed terminal the full bar WRAPS to multiple rows keeping full labels (PRD #144 — no shortcut-only chips).
- **Layer:** L1.
- **Agent:** none (renders the full global + dashboard context bar at 80 cols into a multi-row area).
- **Asserts:** at a narrow/windowed 80 cols the full `[Label Shortcut]` set (~133 cells) does not fit one row, so PRD #144 has the bar WRAP to multiple rows keeping the full label of every button — `[New Pane Ctrl+N]`, `[Close Ctrl+W]`, `[Toggle Layout Ctrl+T]`, `[Help ?]`, `[Quit Ctrl+C]`, and `[Scheduled Tasks s]` all render somewhere across the rows — the shortcut-only `[Ctrl+N]` chip is absent, and the bar occupies ≥2 rows. Inverts the pre-#144 shortcut-only degradation.
- **Does not assert:** exact column widths; which button lands on which row; the exact row count beyond "more than one".
- **Platform coverage:** mac+linux+windows.

##### mouse/buttonbar/003 — Clicking the New Pane bar button opens the directory picker, like Ctrl+N.
- **Layer:** L2 (PTY end-to-end).
- **Agent:** none (synthetic — empty dashboard).
- **Asserts:** clicking `[New Pane Ctrl+N]` opens the `Select Directory` picker.
- **Does not assert:** the rest of the new-pane flow (covered by `mouse/form/001`).
- **Platform coverage:** mac+linux.

##### mouse/buttonbar/004 — A Scheduled Tasks bar button is present and clicking it opens the manager dialog (PRD #127 finding #4 — mouse parity).
- **Layer:** L2 (PTY end-to-end).
- **Agent:** none (fixture global `schedules.toml` via `DOT_AGENT_DECK_SCHEDULES`).
- **Asserts:** the bottom button bar renders a Scheduled Tasks button (label starting `[Scheduled …`); clicking it opens the "Scheduled Tasks" manager dialog (confirmed by the seeded task name appearing in the dialog list), the same outcome as the keyboard open-shortcut — proving click→action parity for the open-shortcut, like `[New Pane Ctrl+N]`.
- **Does not assert:** the in-dialog action clicks (covered by `mouse/modal/001`); the exact button label/shortcut beyond the `[Scheduled` prefix; the bar's narrow-width degradation for the new button.
- **Platform coverage:** mac+linux.

##### mouse/buttonbar/005 — The Scheduled Tasks open button is shown on the dashboard even with ZERO schedules configured (fix/scheduler-single-agent-card — the manager is how you create the first one).
- **Layer:** L1.
- **Agent:** none (renders `dashboard_context_buttons` with `has_schedules = false`).
- **Asserts:** at a comfortable 200-column width (so the full global+context bar fits and overflow is not in play), the bottom button bar renders a Scheduled Tasks open button (label starting `[Scheduled`) even though no schedules exist — because that button opens the manager, which is itself the way to CREATE the first schedule.
- **Does not assert:** the exact label/shortcut beyond the `[Scheduled` prefix; click behavior (covered by `mouse/buttonbar/004`); the bar's narrow-width degradation.
- **Platform coverage:** mac+linux+windows.

##### mouse/buttonbar/006 — At the default 120-col PTY width the FULL dashboard button set WRAPS to a second row keeping full labels (PRD #144 — no shortcut-only chips, Scheduled Tasks not special-cased).
- **Layer:** L1.
- **Agent:** none (renders the full global + dashboard context bar, including the always-shown Scheduled Tasks button, into a multi-row area).
- **Asserts:** at 120 cols (`DEFAULT_COLS`) the full set (~133 cells) overflows one row, so PRD #144 has the bar WRAP to a second row keeping EVERY button's full label — the full `[New Pane Ctrl+N]` label is present and the shortcut-only `[Ctrl+N]` chip is absent — and the bar occupies ≥2 rows. Degradation is uniform: `[Scheduled Tasks s]` is full-labelled like the rest, NOT special-cased to keep its label while others chip. Inverts the pre-#144 collapse-to-chips behavior at the reference width.
- **Does not assert:** the exact column widths; click behavior; which button lands on which row; the exact ceded row count (pinned by `render/layout/004`); the full-label rendering at roomy widths (covered by `mouse/buttonbar/001` / `005`).
- **Platform coverage:** mac+linux+windows.

##### mouse/buttonbar/007 — The dimmed Close button is inert outside command mode.
- **Layer:** L2 (real-binary PTY with production button rendering and SGR mouse hit-testing).
- **Agent:** none (continued `cat` pane).
- **Asserts:** Help mode still visibly renders `[Close Ctrl+W]`; clicking it arms neither the pane-scoped nor tab-scoped close confirmation; Help's own `[Close]` then dismisses the overlay normally; the daemon agent remains alive.
- **Does not assert:** the DIM cell modifier itself (covered through the live buffer path by `keybindings/hints/003`).
- **Platform coverage:** mac+linux.

#### mouse/tabstrip

##### mouse/tabstrip/001 — Clicking a tab header switches to that tab.
- **Layer:** L2.
- **Agent:** none (synthetic Mode tab).
- **Asserts:** with Dashboard + a Mode tab open, clicking the inactive `Dashboard` header switches to it (the empty-dashboard state returns).
- **Does not assert:** the `[×]` close affordance (covered by `mouse/tabstrip/002`).
- **Platform coverage:** mac+linux.

##### mouse/tabstrip/002 — Mode/Orchestration tabs carry a clickable `[×]` close affordance (Dashboard has none); clicking it closes the tab.
- **Layer:** L1 (glyph presence/absence) + L2 (click-to-close).
- **Agent:** none.
- **Asserts:** the strip renders exactly one `×` per closeable tab and none for the Dashboard; clicking a Mode tab's `[×]` leaves the tab intact behind the tab-scoped `Close this tab and all its panes?` Cancel-default confirmation, and Down+Enter then closes it.
- **Does not assert:** which tab gets focus after close.
- **Platform coverage:** mac+linux (L1 half: +windows).

##### mouse/tabstrip/003 — An inactive tab's `×` binds confirmation to that stable tab while modal navigation is suppressed.
- **Layer:** L2 (real-binary PTY with two distinct synthetic Mode tabs and production SGR mouse/key dispatch).
- **Agent:** none (the `alpha` and `beta` fixture modes run long-lived side panes with unique rendered sentinel text).
- **Asserts:** with `BETA_TAB_SENTINEL` active, clicking the inactive alpha tab's `×` arms alpha with tab-scoped copy; Ctrl+PageUp and Ctrl+PageDown leave beta rendered; confirmation removes alpha while beta and its single remaining `×` survive.
- **Does not assert:** dashboard-session identity replacement (covered by `prompt/close-confirm/005`).
- **Platform coverage:** mac+linux.

#### mouse/dashboard

##### mouse/dashboard/001 — Single-click selects a card; double-click focuses its pane.
- **Layer:** L2.
- **Agent:** none (synthetic hook card + a real `--continue` pane).
- **Asserts:** single-click moves the `▸` selection marker to the clicked card; double-click focuses its pane and enters PaneInput.
- **Does not assert:** selection wrap behavior (keyboard-covered).
- **Platform coverage:** mac+linux.

##### mouse/dashboard/002 — The dashboard exposes clickable Filter / Rename / Generate buttons.
- **Layer:** L2.
- **Agent:** none (synthetic card with cwd).
- **Asserts:** clicking `[Filter /]` enters filter mode (typed text echoes), `[Rename r]` enters rename, `[Generate g]` opens the config-gen prompt.
- **Does not assert:** the downstream filter/rename/generate outcomes (keyboard-covered).
- **Platform coverage:** mac+linux.

#### mouse/modal

##### mouse/modal/001 — Modal dialog buttons fire their action like the keyboard.
- **Layer:** L2.
- **Agent:** none (synthetic card for config-gen; fixture `schedules.toml` via `DOT_AGENT_DECK_SCHEDULES` for the Scheduled Tasks manager).
- **Asserts:** quit-confirm `[Cancel]` dismisses (app stays), config-gen `[Never]` sets the "Config prompt suppressed" status, help `[Close]` closes the overlay, and the "Scheduled Tasks" manager dialog's `[Delete]` button surfaces the definition-only delete-confirmation (`Delete schedule '<name>'?`) like pressing `d` (PRD #127 finding #4 — modal mouse parity).
- **Does not assert:** the destructive quit-confirm `[Detach]`/`[Stop]` (process-exit, keyboard-tested) or the star-prompt (not deterministically triggerable); the manager dialog's other clickable actions — `[Add]`/`[Edit]`/`[Run now]` — which the coder must also wire (and whose click outcomes are deferred).
- **Platform coverage:** mac+linux.

##### mouse/modal/002 — Each modal renders explicit buttons alongside its existing selection list / hint.
- **Layer:** L1.
- **Agent:** none.
- **Asserts:** quit-confirm `[Detach] [Stop] [Cancel]`, config-gen `[Yes] [No] [Never]`, star `[Star] [Snooze] [Dismiss]`, and help `[Close]` render while the existing list / hint text is still present (additive).
- **Does not assert:** click outcomes (covered by `mouse/modal/001`).
- **Platform coverage:** mac+linux+windows.

#### mouse/inline

##### mouse/inline/001 — Inline filter/rename rows gain Apply/Save/Cancel buttons; PaneInput gains `[Command Mode Ctrl+D]`.
- **Layer:** L1 (button render) + L2 (click outcomes).
- **Agent:** none (synthetic card + a real `--continue` pane for detach).
- **Asserts:** the filter row renders `[Apply]`/`[Cancel]` and the rename row `[Save]`/`[Cancel]` alongside the input; clicking them commits/abandons like Enter/Esc; clicking inside the field keeps it focused (typing stays keyboard); `[Command Mode Ctrl+D]` returns from PaneInput to the dashboard.
- **Does not assert:** cursor pixel position within the field.
- **Platform coverage:** mac+linux (L1 half: +windows).

#### mouse/picker

##### mouse/picker/001 — The directory picker is mouse-operable (rows, parent, Confirm/Cancel/Filter).
- **Layer:** L1 (affordance render) + L2 (click outcomes).
- **Agent:** none.
- **Asserts:** the picker renders `[Confirm]`/`[Cancel]`/`[Filter]`; single-click selects a row, double-click descends, clicking `..` goes up, `[Cancel]` closes to the dashboard, `[Confirm]` opens the new-pane form, `[Filter]` opens the filter input.
- **Does not assert:** filter-narrowing correctness (keyboard-covered).
- **Platform coverage:** mac+linux (L1 half: +windows).

#### mouse/form

##### mouse/form/001 — The new-pane form is mouse-operable (field focus, mode chips, Submit/Cancel).
- **Layer:** L1 (chip + button render) + L2 (click outcomes).
- **Agent:** none (fixture with two modes).
- **Asserts:** the form renders one clickable chip per mode option plus `[Submit]`/`[Cancel]`; clicking a field focuses it (typing lands there), clicking a chip selects that mode (title reflects it), `[Submit]` creates the pane, `[Cancel]` discards.
- **Does not assert:** command-field validation.
- **Platform coverage:** mac+linux (L1 half: +windows).

#### mouse/preserve

##### mouse/preserve/001 — Existing pane mouse behavior survives the button layer.
- **Layer:** L2.
- **Agent:** none (real `--continue` pane).
- **Asserts:** double-click still focuses a card's pane (PaneInput); a non-button click in the pane region is not swallowed into a button action; a scroll in the pane region reaches the scroll path, not the button hit-test.
- **Does not assert:** mode-tab click-to-focus, text-selection drag, Ctrl+click hyperlink, child-app forwarding (deferred in the test body with reasons).
- **Platform coverage:** mac+linux.

##### mouse/preserve/002 — Button clicks short-circuit; misses fall through.
- **Layer:** L2.
- **Agent:** none (synthetic cards).
- **Asserts:** clicking a card (missing every button) falls through to card selection; clicking the `[New Pane Ctrl+N]` bar button fires its action and does NOT also act on the cards underneath.
- **Does not assert:** per-region hit-test internals.
- **Platform coverage:** mac+linux.

#### mouse/help

##### mouse/help/001 — The `?` help overlay documents the canonical post-button-bar shortcut set.
- **Layer:** L1.
- **Agent:** none.
- **Asserts:** the overlay documents the global commands the button bar advertises (Ctrl+N / Ctrl+W / Ctrl+T, `?`, Ctrl+C) plus the key dashboard / navigation actions, matched case-insensitively.
- **Does not assert:** exact overlay layout / wording.
- **Platform coverage:** mac+linux+windows.


### Theme contrast

Under PRD #13's terminal-relative color model there is no baked light/dark palette, so the per-theme snapshot *pairs* collapse into structural-property assertions: the dashboard may emit no absolute `Color::Rgb(..)` on any contrast-critical surface — backgrounds resolve to `Color::Reset` (the terminal's own background) and selection/active-tab highlights are cued without an absolute background tint.

#### theme/contrast

##### theme/contrast/001 — Overlay/prompt surfaces render in the terminal's reference frame (Reset background, Reset/ANSI foregrounds, no absolute Rgb).
- **Layer:** L1 (ratatui `TestBackend` + `insta`, color-aware capture).
- **Agent:** none.
- **Asserts:** the five overlay/prompt surfaces (stats bar, Quit-confirm, Stop-confirm, star prompt, config-gen prompt) emit no absolute `Color::Rgb(..)` token (foreground or background) — every cell is `Color::Reset` or a named ANSI color, so the surfaces inherit the terminal's own background and theme.
- **Does not assert:** accent/status colors (Cyan/Green/Yellow/Red/Blue/Magenta), which are named ANSI and remain by design; popup geometry beyond what the buffer captures.
- **Platform coverage:** mac+linux+windows.

#### theme/guard

##### theme/guard/001 — No absolute background on any cheaply-seamable surface; command-mode selection is cued by the terminal's own foreground plus a thickened border, not an absolute fill.
- **Layer:** L1 (ratatui `TestBackend` + `insta`, color-aware capture).
- **Agent:** none.
- **Asserts:** rendering the five overlay seams plus a session card in both the unselected and selected states **in command mode** (`UiMode::Normal`), (a) no cell carries a `Color::Rgb(..)` background — backgrounds must be `Color::Reset`; and (b) the selected card is distinguished from the unselected one by terminal-relative cues — the `▸ ` title prefix, a border in the terminal's own foreground (`Color::Reset`), and a thickened `┃` glyph where the unselected card draws `│` — rather than an absolute `selected_bg` fill, and that the selected border is never `DIM` (issue #442).
- **Does not assert:** named-ANSI accents/status colors; the `render_frame` canvas/tab-bar fills (not cheaply reachable through a render seam — guarded by `theme/guard/002`); the per-mode emphasis of the selection (covered by `mode/deck/001`); the all-statuses sweep proving a selected border never inherits a low-contrast status colour (covered by `theme/palette/006`).
- **Platform coverage:** mac+linux+windows.

##### theme/guard/002 — `src/ui.rs` carries no forbidden absolute-background patterns (source lint).
- **Layer:** L1 (source lint — reads `src/ui.rs` from disk; no rendering).
- **Agent:** none.
- **Asserts:** `src/ui.rs` contains none of `bg(Color::Rgb`, `bg(palette.terminal_bg)`, `bg(palette.selected_bg)`, `bg(palette.tab_bar_bg)` — guarding the `render_frame` canvas/tab-bar fills that paint the whole window and aren't cheaply reachable through a render seam.
- **Does not assert:** runtime rendering behavior (covered by `theme/guard/001` and `theme/contrast/001`); absolute colors in other source files.
- **Platform coverage:** mac+linux+windows.

##### theme/guard/003 — The deck-card, embedded-pane and stats-bar render paths resolve colors through the centralized palette, not inline status literals (source lint).
- **Layer:** L1 (source lint — reads `src/ui.rs` and `src/terminal_widget.rs` from disk; no rendering).
- **Agent:** none.
- **Asserts:** both render paths reference the centralized `palette`; the deck-card status mapping (`status_style`) and border resolver (`render_session_card`) in `src/ui.rs` carry no inline status/accent `Color::Green/Blue/Yellow/Red/Cyan`/`Color::Magenta` literals; the embedded-pane path (`src/terminal_widget.rs`) carries no inline status `Color::Green/Blue/Yellow/Red` literal; and the stats bar (`render_stats_bar` in `src/ui.rs`) carries no inline status `Color::Green/Blue/Yellow/Red` literal — the palette is the single source of truth (PRD #155 M4 tightening).
- **Does not assert:** the palette module's exact API/shape (the rendered-color tests `theme/palette/001-004` cover behavior); absolute backgrounds (covered by `theme/guard/002`); the stats bar's legitimate non-status `Color::Cyan` (active-count) and `Color::LightMagenta` (mode-label) accents, which are not status roles; inline literals in render paths other than the deck-card/pane/stats-bar status colors.
- **Platform coverage:** mac+linux+windows.

#### theme/palette

##### theme/palette/001 — Deck-card border encodes status via the centralized palette roles.
- **Layer:** L1 (ratatui `TestBackend` + `insta`, color-aware capture).
- **Agent:** none (six live session fixtures, one per status).
- **Asserts:** rendering a deck card (not selected, not focused) for each agent status resolves its border to the matching centralized status role — working=`Color::Green`, thinking=`Color::Blue`, compacting=`Color::Blue` (shares the thinking role), waiting=`Color::Yellow`, error=`Color::Red`, idle=`Color::DarkGray`; and that no status border reuses the `focused` accent (`Color::Cyan`) or the retired `selected` accent (`Color::Magenta`), so a status never collides with focus.
- **Does not assert:** the per-card status badge text/glyph; the selection glyph and the focus accent (covered by `theme/palette/003-004`, `theme/palette/006`); the palette module's internal API (reads the rendered border color).
- **Platform coverage:** mac+linux+windows.

##### theme/palette/002 — Embedded-pane border uses the SAME status color the deck card uses (deck/pane consistency).
- **Layer:** L1 (ratatui `TestBackend` + `insta`, color-aware capture).
- **Agent:** none (six live session fixtures + a `TerminalWidget` per status).
- **Asserts:** for each agent status (including compacting, which shares the thinking/Blue role), the embedded pane's border color (neither selected nor focused) equals the deck card's border color for that status, and both equal the palette status role — so a given state looks identical as a deck card and as an embedded pane (PRD #155 success criterion #2).
- **Does not assert:** pane content/title rendering; the focused/selected pane accents (covered by `theme/palette/004` / `theme/guard/001`).
- **Platform coverage:** mac+linux+windows.

##### theme/palette/003 — Selected deck-card border in command mode is the terminal's own foreground, with a thick glyph + BOLD + marker.
- **Layer:** L1 (ratatui `TestBackend` + `insta`, color-aware capture).
- **Agent:** none (one selected live session fixture).
- **Asserts:** rendering a selected deck card for a Working agent **in command mode** (`UiMode::Normal`) resolves its border to `palette::SELECTED` (`Color::Reset`, the terminal's own foreground) — explicitly NOT the working-status `Color::Green`, the retired Magenta accent, or the focused-pane Cyan — carried together with a thick `┃` glyph, `Modifier::BOLD` and a `▸ ` title marker, and with no `Modifier::DIM` (issue #442).
- **Does not assert:** the status badge (still shows status independent of selection); the absolute-background guard (covered by `theme/guard/001`); the PaneInput emphasis of the same selection (covered by `mode/deck/001`); the all-statuses/both-modes sweep (covered by `theme/palette/006`).
- **Platform coverage:** mac+linux+windows.

##### theme/palette/004 — Focused-pane border is the dedicated `focused` accent (Cyan), distinct from every status and from `selected`.
- **Layer:** L1 (ratatui `TestBackend` + `insta`, color-aware capture).
- **Agent:** none (one focused `TerminalWidget`).
- **Asserts:** rendering a focused embedded pane resolves its border to `Color::Cyan`, and that this color is distinct from every status role (green/blue/yellow/red/dark-gray) and from the retired Magenta accent — focus keeps the only accent HUE while deck selection uses the terminal's own foreground plus thickness, so status/selection/focus stay provably distinct (PRD #155 success criterion #3, issue #442). Also asserts the PRECEDENCE invariant: a pane that is focused AND carries a present `Working` status still renders the focused accent (Cyan), never the Working/Green status color — focus OVERRIDES a present status in the unified border precedence (Option A).
- **Does not assert:** unfocused-pane status coloring (covered by `theme/palette/002`); pane content rendering; the command-mode half of the focus precedence (covered by `theme/palette/005`).
- **Platform coverage:** mac+linux+windows.

##### theme/palette/005 — A focused pane in command mode drops the Cyan accent for its status color and thickens its border.
- **Layer:** L1 (ratatui `TestBackend` + `insta`, color-aware capture).
- **Agent:** none (one `TerminalWidget` rendered twice, live vs. command mode).
- **Asserts:** rendering the SAME focused pane with `input_active=true` (`UiMode::PaneInput`) vs. `input_active=false` (command mode) produces visually distinguishable borders — live resolves to `Color::Cyan` on a thin `│` (`BorderType::Plain`) border, command mode falls through to the agent's status role (`Working`=`Color::Green`) on a thick `┃` (`BorderType::Thick`) border — and that the two colors differ, so colour encodes whether keystrokes reach the pane while thickness still encodes which pane is focused. Also asserts an UNFOCUSED pane keeps the thin border in BOTH modes, so thickness stays exclusive to the focused pane.
- **Does not assert:** that the inner area / PTY size is unaffected by the border weight (`BorderType` never feeds `Block::inner`, and the PRD #84 invariant-3 contract assert covers a regression there); the bottom-bar and hint-string mode cues (covered by the PRD #241 M4 button-bar specs); the status-less focused pane's dim fallback.
- **Platform coverage:** mac+linux+windows.

##### theme/palette/006 — A selected deck card is visible at every status: terminal-foreground border, thickened glyph, never dimmed.
- **Layer:** L1 (ratatui `TestBackend` + `insta`, color-aware capture).
- **Agent:** none (one live session fixture per status, rendered selected and unselected in both modes).
- **Asserts:** for every agent status and in BOTH `UiMode::Normal` and `UiMode::PaneInput`, a SELECTED card's border resolves to `palette::SELECTED` (`Color::Reset`) and never to that status's role colour, thickens its glyph from `│` to `┃`, and never carries `Modifier::DIM`. The CONTROL in the same loop is that the UNSELECTED card is untouched — still its status role, still `│` — so an idle agent keeps receding. Guards issue #442 in both of its reported forms: selection dimmed into the `palette::STATUS_IDLE` band (the original report), and a selected idle card inheriting DarkGray so that thickening its border changed nothing (the follow-up).
- **Does not assert:** the `▸ ` title marker (covered by `theme/palette/003` / `theme/guard/001`); the BOLD-vs-plain mode emphasis (covered by `mode/deck/001`); embedded-pane borders (covered by `theme/palette/002`, `004`, `005`).
- **Platform coverage:** mac+linux+windows.


### Mode indication (PRD #341)

#### mode/cursor

##### mode/cursor/001 — The painted terminal cursor appears only while pane input is active.
- **Layer:** L1 (in-process `TerminalWidget` rendered into a `ratatui::buffer::Buffer`; no PTY, no subprocess).
- **Agent:** none (one focused vt100 fixture rendered twice).
- **Asserts:** with `input_active=true`, the known cursor cell retains today's exact black-on-`LightGreen` bold block styling; with `input_active=false`, the same cell is styled identically to its neighbouring non-cursor cells and carries no cursor modifier, so command mode renders no painted cursor of any kind.
- **Does not assert:** the terminal emulator's own cursor (covered by `mode/cursor/002`); pane-border mode styling (covered by `theme/palette/005`).
- **Platform coverage:** mac+linux+windows.

##### mode/cursor/002 — The terminal emulator cursor is hidden in command mode.
- **Layer:** L1 (ratatui `TestBackend` frame rendering through the production focused-pane path; no PTY subprocess).
- **Agent:** none (one in-memory focused pane fixture).
- **Asserts:** the same focused-pane frame requests a visible terminal cursor in `UiMode::PaneInput` and no terminal cursor in `UiMode::Normal`, proving command mode skips `Frame::set_cursor_position`.
- **Does not assert:** painted cursor-cell styling (covered by `mode/cursor/001`); cursor shape; unfocused panes; modal input cursors outside the terminal-pane path.
- **Platform coverage:** mac+linux+windows.

#### mode/chip

##### mode/chip/001 — The bottom bar persistently names the current mode.
- **Layer:** L1 (ratatui `TestBackend` + `insta`, rendered through `render_button_bar_for_mode_to_buffer` and the live `render_bottom_bar` path).
- **Agent:** none.
- **Asserts:** command mode begins with ` COMMAND ` and PaneInput begins with ` TYPING `; both chips use `Modifier::REVERSED | Modifier::BOLD`, carry no `Color::Rgb`, and the snapshot pins the complete production bar in both modes.
- **Does not assert:** behavior after clicking the adjacent destination button; narrow-width wrapping; banner or pane-dimming behavior.
- **Platform coverage:** mac+linux+windows.

##### mode/chip/002 — The current-mode chip is universal and coexists with the destination button.
- **Layer:** L1 (ratatui `TestBackend` through the production global-only and context-rich bottom-bar paths).
- **Agent:** none.
- **Asserts:** Dashboard, Mode, and Orchestration contexts place the chip at the same left-edge position; command mode shows ` COMMAND ` with `[Back to Pane Ctrl+D]`, while PaneInput shows ` TYPING ` with `[Command Mode Ctrl+D]`, so the current-state label never replaces the destination affordance.
- **Does not assert:** click dispatch for the destination button; exact spacing after the chip; context-specific buttons after the universal prefix.
- **Platform coverage:** mac+linux+windows.

##### mode/chip/003 — Narrow mode chips disappear symmetrically without changing the bar's row budget.
- **Layer:** L1 (ratatui `TestBackend` through the production bottom-bar renderer; no PTY or subprocess).
- **Agent:** none.
- **Asserts:** across every width 0–24, ` COMMAND ` is present if and only if ` TYPING ` is present, both are absent below the shared 10-column threshold and present at or above it, and Normal/PaneInput/Filter/Rename rendering never panics; the command bar's reserved and rendered rows remain 11/5/3/2/1 at widths 19/40/80/120/200.
- **Does not assert:** click dispatch, exact button placement within each wrapped row, or full-frame card geometry (covered by `render/layout/004`).
- **Platform coverage:** mac+linux+windows.

#### mode/banner

##### mode/banner/001 — A fresh command-mode entry dims only the focused pane and centres the full block banner without erasing agent output.
- **Layer:** L1 (in-process production focused-pane renderer through a `TestBackend`-backed buffer seam plus `insta` style-aware capture).
- **Agent:** none (one synthetic vt100 pane rendered focused in command mode and PaneInput, then unfocused in command mode).
- **Asserts:** a roomy focused command pane selects the full block-letter tier, centres its REVERSED block region and `Ctrl+D to type` subtitle, retains readable underlying agent output, and applies DIM throughout the inner area except where the banner overlays it; the same focused PaneInput pane and an unfocused command pane have neither banner nor DIM, and no rendered cell uses `Color::Rgb`.
- **Does not assert:** timed decay or input-driven collapse (covered by `mode/banner/003`); narrow fallback geometry (covered by `mode/banner/002` and `/004`); terminal-specific visual support for DIM or the live binary path (M6 L2 scope).
- **Platform coverage:** mac+linux+windows.

##### mode/banner/002 — The narrow-pane fallback ladder is pure, monotonic, safe, and always fits.
- **Layer:** unit/L1 (pure `command_banner_tier(width, height)` width/height sweep; no renderer, PTY, or subprocess).
- **Agent:** none.
- **Asserts:** all five tiers own a reachable size band in the documented order; 0×0, 1×1, very-wide/one-row, and very-tall/three-column areas safely omit; every selected tier reports rendered dimensions within the available inner area; increasing either dimension never selects a lower tier.
- **Does not assert:** glyph shapes, centring, modifiers, or clipping in the actual buffer (covered by `mode/banner/001` and `/004`).
- **Platform coverage:** mac+linux+windows.

##### mode/banner/003 — Banner decay is deterministic, asymmetric for bound versus unbound keys, and re-arms on every entry.
- **Layer:** L1 state-machine unit using an injected `Instant`; no sleep, PTY, or subprocess.
- **Agent:** none.
- **Asserts:** the named TTL is 2.5 seconds; fresh entry is expanded until the TTL and collapsed at expiry; a command-mode Action and a bottom-bar click collapse early; an unbound printable holds the banner before decay and re-asserts it with a fresh clock after collapse; leaving hides/clears it and re-entry expands it again.
- **Does not assert:** command keybinding resolution itself; rendering or persistent DIM (covered by `mode/banner/001` and `/004`); wall-clock scheduling in the 16ms live loop.
- **Platform coverage:** mac+linux+windows.

##### mode/banner/004 — Every degraded and collapsed banner state stays inside the focused pane.
- **Layer:** L1 (production pane render seam under `catch_unwind` at tier-boundary sizes plus `insta` text/style capture).
- **Agent:** none (synthetic vt100 content in small-but-valid focused panes).
- **Asserts:** nonempty 0×0, 1×1, 2×2, 1×40, and 40×1 pane renders do not panic and return the exact requested buffer size; all three release-exposed controller seam paths resolve a single axis just above `PTY_RESIZE_DIM_MAX` to the safe 24×80 parser fallback; tiers 2–4 render their exact block-COMMAND, full reversed line, and reversed word fallbacks entirely inside the inner area; tier 5 omits safely; all valid bordered sizes retain DIM and avoid `Color::Rgb`; after decay the pane stays dim/readable with no banner while the bottom bar still carries the persistent ` COMMAND ` chip.
- **Does not assert:** the full tier-1 banner (covered by `mode/banner/001`); the transition rules that produce Collapsed (covered by `mode/banner/003`); M6 PTY/real-agent behavior.
- **Platform coverage:** mac+linux+windows.

##### mode/banner/005 — Same-drain mode edges preserve the command banner's real key semantics.
- **Layer:** L1 (in-process production `handle_key_event` burst observer with no render between keys; no PTY or subprocess).
- **Agent:** none (one inert focused pane).
- **Asserts:** a queued double-`Ctrl+D` burst traverses Normal → PaneInput → Normal and re-expands the banner; `Ctrl+D` then bound `Ctrl+T` from PaneInput lands Normal → Normal and stays Collapsed; single bound `Ctrl+T`, bound-then-unbound-printable, and single PaneInput exit control rows retain their distinct Collapsed/Expanded outcomes, with the before-burst visibility pinned for every case.
- **Does not assert:** mouse bursts, wall-clock TTL expiry (covered by `mode/banner/003`), or rendered banner geometry.
- **Platform coverage:** mac+linux+windows.

#### mode/deck

##### mode/deck/001 — The selected deck-card emphasis is full-strength only in command mode, and is never dimmed.
- **Layer:** L1 (ratatui `TestBackend` + `insta`, colour-and-modifier-aware card capture through the production renderer).
- **Agent:** none (one synthetic selected Working session rendered in both modes).
- **Asserts:** command mode remains byte-identical to the legacy selected-card seam and carries `palette::SELECTED` (`Color::Reset`) on a thick `┃` border with BOLD and `▸ `; PaneInput keeps the same colour, the same thick glyph and `▸ ` but drops BOLD; NEITHER mode carries `Modifier::DIM`, since dimming the selection is what made it read as an idle card (issue #442); neither rendering contains `Color::Rgb`; the snapshot pins both styled cards.
- **Does not assert:** unselected-card styling (covered by `theme/palette/006`); focused terminal-pane styling; statuses other than `Working`.
- **Platform coverage:** mac+linux+windows.

#### mode/scroll

##### mode/scroll/001 — Focused agent-pane wheel routing obeys the full mode × child-mouse matrix.
- **Layer:** L1 (in-process synthetic pane with real vt100 scrollback and a recording child-input channel; no PTY subprocess).
- **Agent:** none (one in-memory focused pane with synthetic history).
- **Asserts:** PaneInput forwards a wheel report only when the child has mouse reporting enabled and otherwise moves dot-agent-deck scrollback; command mode moves dot-agent-deck scrollback for both child-mouse states and emits no mouse-protocol bytes, explicitly pinning the Normal+mouse-enabled safety cell.
- **Does not assert:** wheel-down direction (the same production route receives a direction parameter); side-pane hit-testing, which already works in every mode; real terminal mouse-report decoding.
- **Platform coverage:** mac+linux+windows.

##### mode/scroll/002 — PageUp/PageDown provide a remappable command-mode keyboard equivalent for focused agent-pane scrollback.
- **Layer:** L1 (in-process production keybinding resolution plus synthetic focused-pane scroll observation).
- **Agent:** none (one in-memory focused pane with synthetic history).
- **Asserts:** the default PageUp/PageDown bindings move focused-agent scrollback away from/toward live output in `UiMode::Normal` without writing to the child; `[dashboard] scroll_pane_up` and `scroll_pane_down` remaps parse without warnings, disable the old defaults, and move scrollback on their replacement chords.
- **Does not assert:** PaneInput key forwarding; help-overlay or bottom-bar discoverability; filesystem loading of `keybindings.toml`.
- **Platform coverage:** mac+linux+windows.

##### mode/scroll/003 — PaneInput snaps newly targeted panes back to live output without disabling deliberate scrolling.
- **Layer:** L1 (in-process two-frame reconcile through two real synthetic vt100 panes; no PTY subprocess).
- **Agent:** none (two in-memory panes with synthetic history and production focus changes).
- **Asserts:** command-mode scrollback is nonzero before entering PaneInput and zero afterward; an unchanged PaneInput target deliberately retains its offset; moving PaneInput focus snaps only the newly targeted second pane while leaving the first at live output; an unchanged command-mode target deliberately retains its offset. Every case pins both pre- and post-reconcile offsets for both panes.
- **Does not assert:** hardware-cursor rendering after the reset (covered by `mode/live/002`); key dispatch for entering PaneInput; real-agent output.
- **Platform coverage:** mac+linux+windows.

##### mode/scroll/004 — PaneInput without a focused pane settles in command mode exactly once.
- **Layer:** L1 (in-process two-frame production scrollback reconcile plus command-banner edge observer with injected `Instant`s; no PTY or subprocess).
- **Agent:** none (a controller with no panes).
- **Asserts:** a no-focus PaneInput frame lands in Normal with an Expanded banner, remains Normal on the next frame, and reports Collapsed exactly at the TTL so the entry instant was not re-stamped; equal frame instants remain Expanded, and an already-Normal initial mode produces the identical idempotent result.
- **Does not assert:** how focus vanished, focus replacement policy when another pane exists, rendered banner geometry, or real-agent behavior.
- **Platform coverage:** mac+linux+windows.

#### mode/live

##### mode/live/001 — A real PTY-attached deck keeps the persistent mode chip after the command banner collapses.
- **Layer:** L2 PTY (the real `dot-agent-deck` binary in the isolated `TuiDeck` harness, asserted on the rendered vt100 grid and terminal attributes).
- **Agent:** none (synthetic `printf; sleep` stand-in pane).
- **Asserts:** Ctrl+D enters command mode with readable DIM pane content, the expanded banner, and the left-anchored ` COMMAND ` chip; the bound `j` action collapses the banner without removing the chip or content; Ctrl+D returns to a banner-free ` TYPING ` chip.
- **Does not assert:** a genuine agent boot or agent response; real-agent cursor and scroll behavior (covered by `mode/live/002`); exact block-glyph shapes or subtitle position.
- **Platform coverage:** mac+linux.

##### mode/live/002 — A real interactive Haiku agent visibly traverses typing, command-mode reading and scrollback, then typing again. [reel]
- **Layer:** L2 PTY (the real `dot-agent-deck` binary in the isolated `TuiDeck` harness, asserted on the rendered vt100 grid and terminal attributes; flaky-tolerant pre-PR real-agent tier).
- **Agent:** REAL interactive Claude Code on Haiku (`claude-haiku-4-5-20251001`, `--ax-screen-reader`, `--allowedTools Bash Read`, no `-p`), with isolated imported credentials plus onboarding/project trust seeded in the per-test HOME; the supported accessibility renderer keeps genuine interactive output in terminal scrollback instead of repainting it out of the vt100 history.
- **Asserts:** the live prompt accepts typed keystrokes and exposes both cursor channels with ` TYPING `; the submitted prefix-glob directive makes Haiku inspect and visibly list a uniquely named fixture sentinel; Ctrl+D hides the hardware cursor and removes the painted block while retaining readable DIM output, the expanded banner, and ` COMMAND `; wheel-up reveals older real-agent filename output through deck scrollback rather than the child mouse path; Ctrl+D restores the cursor treatment and ` TYPING `.
- **Does not assert:** exact model prose, tool-call wording, response timing, pixel-level DIM appearance, light-versus-dark terminal rendering, or command-mode indication on all three tab types (covered at L1 by the mode suites and manually validated across tabs).
- **Platform coverage:** mac+linux.

### Scheduled tasks (PRD #127)

#### scheduler/reload

##### scheduler/reload/001 — A `ReloadSchedules` control message re-reads the global config and diff/replaces the registered task set without a daemon restart (PRD #127 M1.3).
- **Layer:** L2.
- **Agent:** none (drives `daemon serve` over the attach socket).
- **Asserts:** after editing the global `schedules.toml` to drop one task and add another and sending `ReloadSchedules`, the response is ok and the registered (enabled) task set contains the added task and not the removed one — with the same daemon process.
- **Does not assert:** persistence across an actual daemon restart (out of scope per PRD #127); the cron-firing behavior of the reloaded tasks.
- **Platform coverage:** mac+linux.

##### scheduler/reload/002 — A prompt-ONLY edit (same name + cron, new `prompt`) followed by `ReloadSchedules` is honored on the next fire: the spawned agent receives the NEW prompt, not the value captured at first registration (PRD #127 finding).
- **Layer:** L2.
- **Agent:** none (rewrites the global `schedules.toml`, sends `ReloadSchedules`, then drives a run-now fire; observes `ListAgents` + the spawned single-agent card's PTY prompt echo).
- **Asserts:** after registering a single-agent task with prompt `PROMPT_ALPHA`, rewriting the file to change ONLY the prompt to `PROMPT_BRAVO`, and reloading, a run-now fire spawns exactly one agent whose PTY echoes `PROMPT_BRAVO` and never the stale `PROMPT_ALPHA`.
- **Does not assert:** cron-change reload behavior (covered by `scheduler/reload/001`); reuse vs new-tab semantics; the exact reload diff mechanism (black-box on delivered prompt only).
- **Platform coverage:** mac+linux.

#### scheduler/cli

##### scheduler/cli/002 — `dot-agent-deck schedule add` from an arbitrary cwd writes the global `schedules.toml` and triggers a live daemon reload (PRD #127 M1.5).
- **Layer:** L2.
- **Agent:** none (runs the `schedule` CLI subprocess against a live `daemon serve`).
- **Asserts:** running `schedule add` from a directory that is not the global config dir writes the entry to the fixed global path (and not under the cwd), and the running daemon registers the new task via the add-triggered reload (probed via `schedule run-now`).
- **Does not assert:** cron validation / rename rejection / atomic-write internals (covered by the pure-data `scheduler/cli/001` unit tests alongside the CLI).
- **Platform coverage:** mac+linux.

##### scheduler/cli/003 — `dot-agent-deck schedule add` rejects a missing `--command` with a non-zero exit and a clear "command required" error (PRD #127 follow-up).
- **Layer:** L2.
- **Agent:** none (runs the `schedule` CLI subprocess against a live `daemon serve`).
- **Asserts:** running `schedule add` with a complete, valid flag set (name/cron/working-dir/prompt/enabled) but no `--command` exits non-zero and prints a stderr error indicating that `--command` is required — so the writer no longer silently accepts a task that would fall back to a bare `$SHELL`.
- **Does not assert:** the exact error wording (loose substring on "command" + "required"); validation of any other field; on-disk write effects.
- **Platform coverage:** mac+linux.

##### scheduler/cli/004 — `dot-agent-deck schedule add` accepts the issue-dispatch flags (`--repo`/`--max-per-run`/`--label`/`--query`, `--command` optional) and writes a `[scheduled_tasks.issue_dispatch]` sub-table that round-trips + reloads (PRD #120).
- **Layer:** L2.
- **Agent:** none (runs the `schedule` CLI subprocess against a live `daemon serve`).
- **Asserts:** running `schedule add --repo acme/widgets --max-per-run 2 --label … --query …` (plus name/cron/working-dir/prompt) WITHOUT `--command` succeeds; the global `schedules.toml` gains a `[scheduled_tasks.issue_dispatch]` sub-table whose repo/max_per_run/label/query round-trip back into an `IssueDispatchConfig` through the loader; the running daemon registers the task via the add-triggered reload; and a malformed `--repo` (not `owner/name`) exits non-zero with a clear error. RED until the flags exist: today `schedule add` has no `--repo`/`--max-per-run`/`--label`/`--query`, so clap rejects the unknown `--repo` and the add exits non-zero.
- **Does not assert:** the dispatch flow on fire (covered by `scheduler/dispatch/*`); the exact malformed-repo wording (loose substring on "repo" + owner/name/slug).
- **Platform coverage:** mac+linux.

#### scheduler/spawn

##### scheduler/spawn/001 — A fire into a missing working_dir creates it (`mkdir -p`) then spawns; a fire into an uncreatable path surfaces a notification without crashing the daemon, and other tasks keep working (PRD #127 M2.1).
- **Layer:** L2.
- **Agent:** none (run-now drives the fire; observes the daemon registry + on-disk effects + daemon stderr).
- **Asserts:** firing a task whose working_dir does not exist creates the directory and spawns an agent; firing a task whose working_dir is uncreatable (parent is a regular file) leaves the daemon alive, does not create the path, surfaces a failure notification, and a sibling healthy task still spawns afterward.
- **Does not assert:** the exact notification message text (loose substring on the offending path).
- **Platform coverage:** mac+linux.

##### scheduler/spawn/002 — A fire into a dir with `[[orchestrations]]` opens an orchestration tab and delivers the prompt to the `orchestrator` role; a fire into a dir without one opens a single-agent card with the prompt delivered (PRD #127 M2.1).
- **Layer:** L2.
- **Agent:** none (run-now; observes `ListAgents` tab_membership + PTY prompt echo).
- **Asserts:** the orchestration fire registers an agent tagged as the orchestration's `orchestrator` role and the prompt is echoed by its PTY; the plain fire registers a non-orchestration single-agent card and the prompt is echoed by its PTY.
- **Does not assert:** orchestration role layout beyond the orchestrator slot; any LLM behavior (commands are plain `cat`).
- **Note:** every task carries a `command` (required to LOAD even for orchestration targets, whose fire is driven by the target dir's role command — so the task `command` is ignored at fire time).
- **Platform coverage:** mac+linux.

##### scheduler/spawn/003 — A fire spawns the task's configured `command` (its on-disk marker appears) (PRD #127 M2.1; command-required follow-up).
- **Layer:** L2.
- **Agent:** none (run-now; observes the on-disk marker side effect of the spawned command).
- **Asserts:** a task with an explicit `command` runs that command (its marker file appears), proving the scheduler spawns the configured command itself.
- **Does not assert:** any `$SHELL` fallback — `command` is now a required field, so there is no implicit-shell case (the former omitted-command fallback was removed); prompt delivery for this case (covered by spawn/002 + spawn/004).
- **Platform coverage:** mac+linux.

##### scheduler/spawn/004 — A single fire calls spawn exactly once and delivers the configured prompt (no double-spawn, no missed delivery) (PRD #127 M2.3).
- **Layer:** L2.
- **Agent:** none (run-now; observes registry agent count + PTY prompt echo).
- **Asserts:** one run-now spawns exactly one agent (count stays at 1 across a short window) and the configured prompt is echoed by that agent's PTY.
- **Does not assert:** tab-reuse vs `new_tab_per_fire` semantics (Phase 2B).
- **Platform coverage:** mac+linux.

##### scheduler/spawn/005 — A scheduled single-agent fire does NOT deliver its prompt until the agent's `SessionStart` is observed; delivery is gated on readiness, not a flat 300ms timer (PRD #127 scheduled-prompt readiness bug).
- **Layer:** L2.
- **Agent:** none (run-now; observes PTY prompt echo + injects the agent's real `SessionStart` hook carrying the spawned pane's `pane_id` + registry `agent_id`).
- **Asserts:** firing a `cat` task (no hook of its own) leaves the prompt UNDELIVERED for a window well past the old flat 300ms buffer while no matching `SessionStart` has been observed; once the real `SessionStart` hook (pane_id + agent_id) is injected, the prompt IS delivered (echoed by `cat`), well inside the 10s gate fallback so delivery is attributable to readiness, not the timeout.
- **Does not assert:** the 10s fallback-on-timeout delivery path (a separate readiness facet); orchestration-tab delivery gating (covered structurally by spawn/002).
- **Platform coverage:** mac+linux.

##### scheduler/spawn/006 — Scheduled single-agent and orchestration-role Codex commands both launch through the Wrapper strategy (PRD #20, blocker 3).
- **Layer:** L2 headless daemon `RunNow` with PATH recorder stubs.
- **Agent:** synthetic Codex recorders for one plain scheduled task and one scheduled orchestration start role.
- **Asserts:** both bare `codex` commands execute exactly as `dot-agent-deck wrap --agent codex -- codex`; neither path launches the bare recorder.
- **Does not assert:** issue-dispatch worktree creation or prompt delivery content.
- **Platform coverage:** mac+linux.

#### scheduler/dispatch

##### scheduler/dispatch/001 — Firing an `issue_dispatch` task clones the repo, creates a per-issue worktree on `agent/issue-<n>`, and spawns an agent into it with the substituted prompt (PRD #120 M2.1–M2.3).
- **Layer:** L2 (headless `dot-agent-deck daemon serve` driven via the `RunNow` control message — no PTY/grid, same shape as `scheduler/spawn/*`). All GitHub access is isolated offline behind a stub `gh` on PATH (`issue list`/`pr list` → canned JSON; `repo clone` → `git clone` of a local one-commit fixture remote that carries a committed `.dot-agent-deck.toml`).
- **Agent:** none (run-now; the fixture orchestration role runs `cat`, which echoes the delivered prompt).
- **Asserts:** the repo is cloned to `<working_dir>/<name>`, the worktree appears at `<clone>/.worktrees/issue-7` with branch `agent/issue-7` (via `git`), and an `orchestrator`-role agent rooted at that worktree (`orchestration_cwd`) receives the substituted per-issue prompt (`ISSUEDISPATCH-7`, echoed by `cat`).
- **Does not assert:** the single-agent-card branch (covered by `scheduler/dispatch/004`); fetch+pull refresh of an existing clone; the exact `gh` argv (covered by the pure-data `issue_dispatch` unit tests).
- **Platform coverage:** mac+linux.

##### scheduler/dispatch/002 — A second fire with no intervening close skips an issue whose worktree already exists: no re-clone error, no duplicate spawn, and the skip is surfaced (PRD #120 M2.2 idempotency, primary signal).
- **Layer:** L2 (as `scheduler/dispatch/001`).
- **Agent:** none (run-now; observes the registry orchestrator count + on-disk worktree/clone + daemon stderr).
- **Asserts:** the first fire creates the issue-7 worktree and one orchestrator agent; a second fire leaves the worktree and clone in place, does NOT grow the orchestrator count beyond one (no duplicate spawn), and surfaces a skip for the already-claimed issue.
- **Does not assert:** the open-PR secondary signal (covered by `scheduler/dispatch/003`); the exact skip-message wording (loose substring on the issue key / "skip").
- **Platform coverage:** mac+linux.

##### scheduler/dispatch/003 — An issue whose `gh pr list` reports an open PR on `agent/issue-<n>` is skipped while a sibling issue with no PR dispatches (PRD #120 M2.2 idempotency, secondary signal).
- **Layer:** L2 (as `scheduler/dispatch/001`; the stub `gh pr list --head agent/issue-7` returns a non-empty array, while issue 8 returns `[]`).
- **Agent:** none (run-now; observes per-issue worktrees + orchestrator count).
- **Asserts:** issue 8 (no PR) dispatches — worktree present, orchestrator agent running — proving the flow ran; issue 7 (open PR) is skipped — no `issue-7` worktree, and the run's orchestrator count is one.
- **Does not assert:** parsing `Closes #n` from PR bodies (the check keys on the deterministic head branch only); the worktree-exists primary signal (covered by `scheduler/dispatch/002`).
- **Note:** a control issue (8, no PR) is included so "the flow ran AND issue 7 was skipped" is observable from end-state alone.
- **Platform coverage:** mac+linux.

##### scheduler/dispatch/004 — Issue-dispatch orchestration-role and single-agent Codex spawns both use the Wrapper strategy (PRD #120 M2.3, PRD #20 blocker 3).
- **Layer:** L2 (as `scheduler/dispatch/001`; two `issue_dispatch` tasks, one fixture remote with a committed Codex orchestration and one without; `default_command = codex`; PATH recorders execute `cat` after recording argv so prompt delivery remains observable).
- **Agent:** synthetic Codex recorders (run-now; observes `ListAgents` tab_membership + spawn cwd + PTY prompt echo + launch argv).
- **Asserts:** the orchestration clone spawns an `orchestrator`-role agent in its worktree and receives `ORCHDISP-11`; the plain clone spawns a non-orchestration card in its worktree and receives `PLAINDISP-22`; both launch records are exactly `dot-agent-deck wrap --agent codex -- codex`, never bare Codex.
- **Does not assert:** the clone/worktree/branch derivation (covered by `scheduler/dispatch/001`); the orchestration-vs-card branch outside the dispatch path (covered by `scheduler/spawn/002`).
- **Platform coverage:** mac+linux.

##### scheduler/dispatch/005 — When `gh` returns more open issues than `max_per_run`, only the first N (in returned order) get worktrees + spawns; the rest are left untouched (PRD #120 M3.1 cap).
- **Layer:** L2 (as `scheduler/dispatch/001`; the stub returns five issues while `max_per_run = 2`, so the flow's own cap — not the stub — bounds the run).
- **Agent:** none (run-now; observes per-issue worktrees + orchestrator count).
- **Asserts:** issues 1 and 2 are dispatched (worktrees present), issues 3–5 are left untouched (no worktrees), and exactly two orchestrator agents exist.
- **Does not assert:** issue ordering/scoring beyond "returned order" (out of scope per the PRD); the label/query filters (pure-data `issue_dispatch` argv tests cover those).
- **Platform coverage:** mac+linux.

##### scheduler/dispatch/006 — Closing a dispatched tab removes its worktree from disk and `git worktree list` while preserving the clone (PRD #120 M2.4 tab-close → cleanup plumbing).
- **Layer:** L2 (as `scheduler/dispatch/001`; close is driven via the `StopAgent` control message on the dispatched orchestrator).
- **Agent:** none (run-now to dispatch; `StopAgent` to close; observes on-disk worktree/clone + `git worktree list`).
- **Asserts:** after dispatch the issue worktree exists; after closing the tab the worktree is gone from disk and from `git worktree list`, while the clone directory remains.
- **Does not assert:** the in-deck close gesture (`Ctrl+w`) — the daemon-side close→cleanup contract is exercised over the protocol; auto-restoration of dispatched tabs (out of scope per the PRD).
- **Platform coverage:** mac+linux.

##### scheduler/dispatch/007 — One issue's dispatch failing (a simulated `gh` error for that issue) does not abort the others, and the failure is surfaced as a notification, not swallowed (PRD #120 M3.2 per-issue resilience).
- **Layer:** L2 (as `scheduler/dispatch/001`; the stub `gh pr list --head agent/issue-11` exits non-zero while issue 10 is healthy).
- **Agent:** none (run-now; observes survivor worktrees + orchestrator count + daemon stderr).
- **Asserts:** issue 10 still dispatches (worktree + orchestrator agent) despite issue 11 failing; issue 11 produces no worktree; and a failure referencing issue 11 is surfaced through the notifier (daemon stderr).
- **Does not assert:** cross-repo fan-out resilience (one repo per task — removed from scope); the exact failure-message wording (loose substring on the issue 11 key).
- **Platform coverage:** mac+linux.

##### scheduler/dispatch/008 — An issue dispatched, then closed without a PR (worktree removed, branch left behind), is re-dispatched on a later fire: the worktree is re-created and an agent spawns again, with no failure surfaced (PRD #120 B1 — `worktree add` must tolerate the leftover `agent/issue-<n>` branch).
- **Layer:** L2 (as `scheduler/dispatch/001`; first run-now to dispatch, `StopAgent` to close, second run-now while the stub still reports the issue open with no PR).
- **Agent:** none (run-now ×2 + `StopAgent`; observes the re-created worktree, a re-spawned orchestrator, and daemon stderr).
- **Asserts:** after close the worktree is gone but branch `agent/issue-7` survives; the second fire re-creates the issue-7 worktree and spawns the orchestrator again; no per-issue failure (`failed:` / "already exists") is surfaced.
- **Does not assert:** the exact branch-reattach git mechanics (probe vs. retry-without-`-b`) — only the observable re-dispatch; behavior when an open PR exists (covered by `scheduler/dispatch/003`).
- **Platform coverage:** mac+linux.

##### scheduler/dispatch/009 — Closing ONE role of a multi-role orchestration dispatch leaves the shared issue worktree on disk; only closing the LAST role removes it, clone preserved (PRD #120 S1 — refcount the worktree, remove on last close).
- **Layer:** L2 (as `scheduler/dispatch/001`; the fixture remote commits a two-role `[[orchestrations]]` config — `orchestrator` + `reviewer`, both `cat` — so a dispatch opens two role panes sharing one `orchestration_cwd`).
- **Agent:** none (run-now to dispatch; `StopAgent` per role; observes on-disk worktree + `git worktree list` + clone dir).
- **Asserts:** both role panes spawn into the same issue worktree; closing the reviewer leaves the worktree present (disk + `git worktree list`); closing the orchestrator (last role) removes the worktree while the clone directory remains.
- **Does not assert:** the refcount/registry internals (counted at spawn, decremented per close) — only the observable last-close-removes contract; the single-role close path (covered by `scheduler/dispatch/006`).
- **Platform coverage:** mac+linux.

##### scheduler/dispatch/010 — A successful dispatch writes the `in-progress` label on the issue and posts exactly one claim comment naming the claiming task, and the label write genuinely SUCCEEDS when the label already exists on the repo (PRD #421 M1.0/M1.1); PRD fork#235 M2 extends this to also assert the assignee is written, with the pre-existing single-agent comment wording/assertions left byte-identical; round 3 additionally asserts the comment names the dispatched worktree's canonical absolute path AND its branch — the complete two-field identity (CLAUDE.md rule 23), not the decorative task name alone — matched against the delimited fragment production's OWN `claim_comment_body` renders for the `Identity` production would build from `derive_issue_paths`' lexically-built path, its two fields located by their structural backtick delimiters rather than by searching for the surrounding prose (fork#243 round 4), not two independently `contains()`-checked substrings.
- **Layer:** L2 (as `scheduler/dispatch/001`). The stub `gh` records every invocation verbatim to `$GHSTUB_DIR/gh-calls.log` (before any argv parsing), and unconditionally accepts `issue comment` calls — no canned response is needed since the tests assert on WHAT `gh` was asked to do, not on read-back state. `issue edit --add-label <name>` only succeeds when `<name>` is already in the repo's known label set (PRD #421 review fix — mirrors real `gh`'s label-name-to-ID resolution), tracked via `GhStub::seed_labels`/`gh label create`; this fixture pre-seeds `in-progress` so the claim write succeeds cleanly. PRD fork#235 M2: the stub also tracks `--add-assignee`/`--remove-assignee` writes per issue (`GhStub::assignees`).
- **Agent:** none (run-now; observes the dispatched orchestrator agent + the stub's recorded `gh` invocations + `GhStub::label_applied` + `GhStub::assignees`).
- **Asserts:** after a successful dispatch (worktree + orchestrator agent present, as in `scheduler/dispatch/001`), the recorded `gh` calls include an `issue edit ... --add-label in-progress` for the issue AND an `issue comment` whose body names the claiming task (`ScheduledTask.name`, rendered as DECORATION on the round-3 identity) AND contains the delimited fragment `` working `<path>` on branch `<branch>` `` — the dispatched worktree's canonical absolute path and its branch (`agent/issue-7`), backtick-delimited exactly as production renders them — via the shared `assert_claim_names_worktree_identity`/`worktree_identity_fragment` helpers, which locate the fragment's two fields structurally rather than by searching for the prose around them (fork#243 round 4); because the fixture pre-seeds `in-progress`, the add-label call must have actually SUCCEEDED (`GhStub::label_applied`), not merely been attempted — see `scheduler/dispatch/020` for the unseeded/failure counterpart. PRD fork#235 M2 additionally asserts a non-empty assignee list is recorded for the issue.
- **Does not assert:** the exact identity/host/timestamp formatting beyond the task-name decoration and the two-field identity; the human-orchestration (`Instance{id,name}`) claimant write point, out of this task's scope; which of the claiming task or the bound orchestration's own name is used as decoration (this fixture is single-agent, so the two are the same distinction `scheduler/dispatch/021` exists to pin); the delimited-fragment predicate's own rejection behavior against a path sharing only a prefix, or against a path containing the literal text `" on host "`, pinned separately by the synthetic-input helper self-tests `worktree_identity_fragment_rejects_a_path_sharing_only_a_prefix` and `worktree_identity_fragment_rejects_the_round_3_marker_search_truncation` (no `GhStub`, no daemon).
- **Note:** M1.0/M1.1 landed — `claim_issue` writes the `in-progress` label via `gh issue edit --add-label` and posts the claim comment via `gh issue comment`; GREEN today across all assertions, including the two-field identity. Round 1 asserted the branch alone (a regression omitting the path passed); round 2 asserted `contains(path) && contains(branch)` as two independent substring checks (a regression naming a DIFFERENT worktree whose path merely shared the expected path as a PREFIX, e.g. `<path>-stale`, alongside the correct branch still passed); round 3 (fork#243) matched the delimited fragment production renders instead of those two independent substrings, but located the fragment's end by searching for the prose `" on host "` — a marker that is not actually structural, since a legitimately-named worktree path can contain that exact text, which let a regression dropping the rest of the path and the whole branch clause pass undetected as long as it wrote that truncated prefix; round 4 (fork#243) instead locates both fields by their backtick delimiters, which `sanitize_claimant_name` guarantees never appear inside either field, and adds the synthetic counterexample test above pinning that specific truncation so the predicate's own rejection behavior is pinned rather than merely asserted in prose.
- **Platform coverage:** mac+linux.

##### scheduler/dispatch/011 — A fired `issue_dispatch` task surfaces its per-issue card LIVE on an already-attached TUI — the user-visible showcase (and demo-reel clip) the headless `scheduler/dispatch/001-009` family can't observe (PRD #120 M2.3 live surfacing).
- **Layer:** L2 PTY (the real `dot-agent-deck` binary in an isolated PTY via the `TuiDeck` harness, asserted on the rendered vt100 grid — same harness as `scheduler/live/*`, NOT the headless `daemon serve` of `scheduler/dispatch/001-009`). Composes the OFFLINE GitHub seam (stub `gh` on PATH: `issue list`/`pr list` → canned JSON, `repo clone` → `git clone` of a local one-commit fixture remote with NO `.dot-agent-deck.toml`) with the live-fire seam (`DOT_AGENT_DECK_SCHEDULES` loaded by the lazily-spawned daemon; fire via the `RunNow` control message over the deck's attach socket). The dispatch behavior is ungated, so the env carries no `DOT_AGENT_DECK_EXPERIMENTAL`; `default_command = cat` (via `DOT_AGENT_DECK_CONFIG`) makes the dispatched single-agent card a long-lived `cat`.
- **Agent:** none (run-now; the dispatched single-agent card runs `cat`, no real LLM, no real GitHub).
- **Asserts:** after the fire the daemon registers the dispatched agent under the schedule's friendly name `github-issues` (precondition), then a per-issue card surfaces LIVE on the rendered dashboard — its `Dir:` line shows the issue worktree basename `issue-7` (the per-issue identity) and its title shows the schedule name `github-issues`.
- **Does not assert:** the clone/worktree/branch derivation or skip/dedup/cap/cleanup logic (covered by the headless `scheduler/dispatch/001-009`); the orchestration-tab dispatch path (NOT live-surfaced by `spawn` — rebuilt by the TUI's hydration path on reconnect, the #140 session-partitioning concern); prompt-echo delivery into the card.
- **Platform coverage:** mac+linux.

##### scheduler/dispatch/012 — A worktree-present second fire short-circuits to a SKIP BEFORE the open-PR check, so a transient `gh pr list` error on that issue never surfaces as a failure (PRD #120 / Greptile P1 regression guard — primary signal short-circuits the secondary, commit 212bc73).
- **Layer:** L2 (as `scheduler/dispatch/001`; first run-now dispatches issue 7, then the stub is armed so `gh pr list --head agent/issue-7` exits non-zero, then a second run-now fires with the worktree already present).
- **Agent:** none (run-now ×2; observes the orchestrator count + on-disk worktree/clone + daemon stderr).
- **Asserts:** the second fire does NOT grow the orchestrator count (no duplicate spawn/re-creation), surfaces an `IssueDispatchSkipped` ("already-claimed issue #7") for the present worktree, does NOT surface an `IssueDispatchFailed` ("issue #7 … failed") despite the armed `gh pr list` error, and leaves the worktree and clone in place.
- **Does not assert:** the worktree-absent path that DOES consult the open-PR signal and propagates a `gh` error as a failure (covered by `scheduler/dispatch/007`); the plain worktree-present skip without a PR-check hazard (covered by `scheduler/dispatch/002`); the exact skip/failure wording (loose substring on the issue-7 key).
- **Note:** the fix is in current code, so this is GREEN as a regression guard, not RED-first; it pins that the primary (worktree-exists) signal short-circuits the secondary (open-PR) check, which `scheduler/dispatch/002` cannot catch because it never forces the PR check to error.
- **Platform coverage:** mac+linux.

##### scheduler/dispatch/013 — A fired `issue_dispatch` task against an ORCHESTRATION repo drives the GENUINE `gh` → clone → per-issue worktree → real-agent path against LIVE GitHub, and the dispatched orchestration must surface LIVE as an orchestration TAB (with its orchestrator + worker role panes) on the already-attached TUI — the real-scenario multi-agent showcase (CLAUDE.md rule 4) a `cat`/stub stand-in can never prove (PRD #120). RED until the daemon live-surfaces a dispatched orchestration tab.
- **Layer:** L2 PTY (the real `dot-agent-deck` binary in an isolated PTY via the `TuiDeck` harness, asserted on the rendered vt100 grid — same harness as `scheduler/dispatch/011`). REAL seams, no stand-ins: REAL `gh` on the normal PATH (no `gh` stub) really enumerates/PR-checks/clones against live GitHub, with `GITHUB_TOKEN` threaded through the scrubbed deck env so the daemon's `gh` inherits auth; the clone's `[[orchestrations]]` resolves to two FULLY INTERACTIVE `claude` role panes pinned to Haiku (`claude-haiku-4-5-20251001`, `--allowedTools Bash`, no `-p`); the freshly-built `dot-agent-deck` binary's dir is prepended to the deck→daemon→agents PATH (`with_env("PATH", …)` wins over the harness scrub) so the orchestrator's `dot-agent-deck delegate --to worker` resolves. The dispatch behavior is ungated, so the env carries no `DOT_AGENT_DECK_EXPERIMENTAL`; the fire is driven by `RunNow` over the attach socket.
- **Fixture:** the permanent public repo `vfarcic/dot-agent-deck-tests` — a committed `DISPATCH_E2E_SENTINEL.md`, a `.dot-agent-deck.toml` with `[[orchestrations]] name = "issue-work"` (roles `orchestrator` (start) + `worker`, both Haiku `claude`; the orchestrator's `prompt_template` delegates the task to the worker), and a PERMANENT open issue #1 labelled `agent-dispatch-test`. The schedule filters on that label with `max_per_run = 1`, so ONLY issue #1 is enumerated (deterministic). Both role panes share the per-issue worktree cwd (pre-trusted in the per-test HOME so claude's first-run gates clear with no keystroke). Clone + worktree live under a `common::harness_tempdir()` removed on drop.
- **Agent:** REAL Claude Code (Haiku) ×2 role panes, cheap interactive turns (<$0.05/run). Flaky-tolerant pre-PR tier (real LLM + real network) — run once, not looped (rule 4). Runtime-skipped (Decision 26) when the `claude` CLI/credentials or `GITHUB_TOKEN` are absent.
- **Asserts:** after the fire the daemon registers BOTH of the dispatched orchestration's role agents, each under its own ROLE NAME — `orchestrator` and `worker` (precondition — proves the live clone + worktree + spawn happened). Until `orchestration/dispatch/002` this looked for the shared schedule name `github-issues` on a role pane, which is what a dispatched role's `display_name` used to be; role panes now carry their role name (matching the interactive `Ctrl+n` path), and requiring both names is strictly stronger — one shared name could be satisfied by a single spawned pane. The dispatched ORCHESTRATION then surfaces LIVE as an orchestration TAB labelled `issue-work` (the fixture's `[[orchestrations]] name`) in the attached TUI's tab strip, with no reconnect/relaunch — RED today, because `spawn::spawn`'s orchestration branch does not call `surface_spawned_pane` and orchestration tabs are rebuilt only at hydration, so the role panes appear only as flat dashboard cards and no `issue-work` tab paints live. Best-effort (once GREEN, logged not gated): switching to the orchestration tab, the worker (delegated to by the orchestrator) lists the cloned repo's files including the committed sentinel `DISPATCH_E2E_SENTINEL.md`; and the fixture repo has no pushed `agent/issue-1` branch afterward (NO REMOTE WRITES).
- **Does not assert:** the delegation chain / sentinel as a hard gate (logged best-effort — too LLM/timing-dependent); exact agent phrasing; the clone/worktree/branch derivation or skip/dedup/cap/cleanup logic (covered by the headless `scheduler/dispatch/001-009` and the deterministic-stub `scheduler/dispatch/011-012`); the single-agent live-surfacing path (covered by `scheduler/dispatch/011`).
- **Platform coverage:** mac+linux.

**scheduler/dispatch/014 — retired 2026-08-15 (fork #194/#341).** Formerly "Concurrent single-agent dispatch seeds survive a deterministic boot-window swallow and are confirmed after retry": three concurrent `dispatch --single` panes against a synthetic stand-in that swallowed its first submitted line and only confirmed a later retry's resubmission. `MAX_PAYLOAD_SUBMISSIONS = 1` (fork #194, `src/prompt_delivery.rs`) removed the bounded replacement payload that resubmission depended on — every attempt past the first is now an empty-payload submit-only probe, so a launcher that genuinely consumed the first write is never given a second payload to read, and the stand-in's `while IFS= read -r submitted` loop never receives the confirming line the assertion waited on. The property itself is gone, not merely renumbered, so the test and its now-orphaned `write_swallowing_agent`/`delivery_log_states` helpers were removed outright rather than narrowed. `scheduler/dispatch/015` exercises the identical mechanism against a real Claude Code agent and is expected to regress the same way — already priced in as an accepted loss in `MAX_PAYLOAD_SUBMISSIONS`'s own doc comment, since `/015` self-skips in CI for lack of credentials and so never turns the board red. Recovering a launcher that genuinely consumes the first write is deferred to fork issue #343.

##### scheduler/dispatch/015 — Three concurrent real interactive Claude dispatches each genuinely submit their seed prompt.
- **Layer:** L2 REAL PTY-attached (real deck and daemon, three sibling dispatch worktrees, imported isolated credentials, and project trust pre-seeded for every predicted worktree). A bootstrap launcher mirrors the field report's nested `devbox` startup seam: it announces an explicitly launcher-origin (`wrapper_fork`) `SessionStart`, consumes and records exactly one early PTY submission while the real agent is not yet running, then `exec`s Claude.
- **Agent:** REAL interactive Claude Code ×3 pinned to `claude-haiku-4-5-20251001` with `--allowedTools Bash` and no `-p`, reached through the deterministic one-write-swallowing bootstrap launcher; runtime-skipped when the CLI or credentials are absent and flaky-tolerant in the pre-PR tier.
- **Asserts:** all three bootstrap launchers record their distinct first seed as swallowed, then each real Claude pane's durable native `UserPromptSubmit` state exactly carries the retried sentinel-bearing seed, so neither an unexercised startup window nor a healthy Idle pane with only PTY echo can pass.
- **Failure diagnostics:** every failing path reports, per pane, the full expected prompt, the exact durable confirmed value, whether the first submission was swallowed, and the complete bootstrap attempt log, plus the final rendered grid.
- **Does not assert:** exact model response phrasing, ordering between the three agents, or a fixed boot duration.
- **Expected-red pending fork #343 (marked 2026-08-15, fork #194/#341):** the "retried sentinel-bearing seed" assertion above describes the pre-#194 behaviour and is now impossible under `MAX_PAYLOAD_SUBMISSIONS = 1` — the bootstrap launcher's swallow leaves nothing for a later attempt to resubmit, since every attempt past the first is an empty-payload probe (identical mechanism to the retired `scheduler/dispatch/014`, see its retirement note above). This is the loss `MAX_PAYLOAD_SUBMISSIONS`'s own doc comment already prices in and is priced in the PR body's "Accepted trade" section. Unlike `/014` it is not removed: it self-skips in CI for lack of credentials (rule 5's carve-out (a)), so it will go red only on a human's machine running the real-agent tier locally, with nothing on the CI board to explain why. Do not "fix" it by editing the assertion to match current behaviour — the point is that the property it was pinning is gone, and a maintainer hitting red here should land on fork issue #343, not conclude a new regression shipped.
- **Platform coverage:** mac+linux.

##### scheduler/dispatch/016 — Detached prompt retries stop on terminal targets/evidence and do not arm from an unauthenticated producer claim.
- **Layer:** L1 (in-process detached spawn confirmation task with real registry-owned platform-native shell/byte-observation PTYs and synthetic hook events).
- **Agent:** none (the real platform-native PTYs — `/bin/sh` and `/bin/cat` on Unix, `cmd.exe` and `more.com` on Windows — are observation targets, not agent stand-ins).
- **Asserts:** replacement, a bound `SessionEnd`, broadcast lag, and broadcast closure each terminally stop the watch without stale retry bytes; pane close and daemon shutdown cancel registered watches; a newer same-pane delivery aborts the older single flight before it retries; an unmarked event merely claiming a reporting `AgentType` cannot arm a replacement-payload retry into a hookless byte sink.
- **Does not assert:** TUI-owned automatic seed/orchestrator delivery (covered by `prompt/pane-input/028`) or finer same-agent generation tracking without `SessionEnd` (provisional behavior intentionally not pinned).
- **Platform coverage:** mac+linux+windows.

##### scheduler/dispatch/017 — Cap-exhaustion notices reach a hookless card that exists only in the attached TUI's broadcast state.
- **Layer:** L1 (in-process production delivery-notice sink, daemon/AppState split, attached-client broadcast consumer, and real registry-owned `/bin/cat` PTY).
- **Agent:** none.
- **Asserts:** a `surface_spawned_pane`-shaped `SessionStart` makes the card visible only in the attached client's state while daemon state stays empty; publishing the exact 257th-delivery cap notice through the production sink broadcasts an `Error` that visibly marks that existing client card.
- **Does not assert:** the cap counter's publication branch itself (the existing `abandonment_reports_state_and_never_writes_into_the_pane` unit test fills all 256 slots and proves that the 257th publishes this notice).
- **Platform coverage:** mac+linux.

##### scheduler/dispatch/018 — User typing after a detached automatic payload disarms the next retry.
- **Layer:** L1 (in-process detached spawn confirmation task with a real registry-owned byte-observation PTY: `/bin/cat` on Unix, `more.com` on Windows).
- **Agent:** none.
- **Asserts:** attempt 1 is applied and physically reaches the pane before any automatic-write timestamp exists; an unsent user draft after attempt 1 prevents the next automatic attempt's submit-only probe from submitting the draft, proven by an unchanged PTY byte snapshot.
- **Does not assert:** TUI-owned seed delivery (covered by `prompt/pane-input/032`) or the internal location of the clock comparison. **Retired 2026-08-15 (fork #194/#341):** this test used to carry a second "replacement pane" sub-scenario exercising `write_guarded`'s non-empty-payload branch (`registry.user_typed_since_writing_payload`) on attempt 2, the one bounded replacement payload. `MAX_PAYLOAD_SUBMISSIONS = 1` (fork #194, `src/prompt_delivery.rs`) makes that branch structurally unreachable past attempt 1, so the sub-scenario's assertion had gone vacuous — it would have passed even with `user_typed_since_writing_payload` deleted outright — and it was removed rather than kept as a false positive. The guard now stays covered independent of `MAX_PAYLOAD_SUBMISSIONS` by `scheduler/dispatch/031`, which calls it directly against the registry. Recovering a launcher that genuinely consumes attempt 1 — what would make the retired branch reachable again — is deferred to fork issue #343.
- **Platform coverage:** mac+linux+windows.

##### scheduler/dispatch/019 — Releasing an attached user's pane writer cannot expose unstamped input to an automatic retry.
- **Layer:** L1 (in-process production registry guard with a real byte-observation PTY — `/bin/cat` on Unix, `more.com` on Windows — and a deterministic writer-lock handoff).
- **Agent:** none.
- **Asserts:** while an attached input writer holds the pane lock, an automatic replacement is queued behind it; the user's unsent draft is physically present before the writer is released; the queued replacement then owns the exact write-to-clock handoff window and must be refused with no snapshot change before the test allows the user-input clock stamp to run.
- **Does not assert:** socket frame parsing or scheduler timing; the test directly forces the ordering produced inside the attach STREAM_IN handler after a successful write and flush.
- **Platform coverage:** mac+linux+windows.

##### scheduler/dispatch/020 — Automatic payload guards distinguish submitted turns from unsent drafts across delivery overlap, guarded writes, paste, and newline controls.
- **Layer:** L1 (in-process production guarded submits with real byte-observation PTYs: `/bin/cat` on Unix, `more.com` on Windows).
- **Agent:** none.
- **Asserts:** after delivery A and a completed user turn, a later delivery B's first attempt carrying the same fixed pointer text is applied and physically writes; user input invalidates delivery A even when a different guarded submit B intervenes; production-shaped bracketed paste, Ctrl+J, and Claude Alt+Enter frames leave drafts unsent and therefore do not let replacements append or submit bytes; a genuine plain Enter drains the completed turn and admits a later automatic payload; and when two active deliveries write the same payload, superseding A after B's write does not let B's retry append to or submit a later user draft.
- **Does not assert:** the internal representation of delivery identity, payload hashes, record lists, paste parsing strategy, or which guard rejects the unsafe writes; every safety assertion compares PTY bytes before and after the attempted retry.
- **Platform coverage:** mac+linux+windows.

##### scheduler/dispatch/021 — A detached writer-held user-input refusal is visibly reported.
- **Layer:** L1 (in-process detached confirmation loop with paused time, a held production pane writer, a real byte-observation PTY — `/bin/cat` on Unix, `more.com` on Windows — and the delivery-notice sink).
- **Agent:** none.
- **Asserts:** paused time deterministically completes the confirmation window while the writer is held, user input is stamped only after the caller's precheck has run and before the writer-held backstop proceeds, and that backstop refusal publishes one durable `DeliveryNotice` instead of becoming a log-only `target went stale` stop.
- **Does not assert:** the notice sink's daemon-to-TUI rendering (covered by `scheduler/dispatch/017`) or exact log wording.
- **Platform coverage:** mac+linux+windows.

##### scheduler/dispatch/022 — A dispatch whose spawn FAILS leaves the issue completely unmarked — no label, no comment (PRD #421 M1.0 risk mitigation: a false claim on a failed dispatch would make the issue permanently un-dispatchable once the label is read back).
- **Layer:** L2 (as `scheduler/dispatch/001`; the fixture remote's orchestration role names a nonexistent binary (`dad-nonexistent-binary-421`, a single bare word so it is exec'd directly with no shell), so `spawn`'s `spawn_command` fails synchronously on exec resolution — deterministic, no timing/race dependency).
- **Agent:** none (run-now; observes the created-then-orphaned worktree, the surfaced `IssueDispatchFailed`, and the stub's recorded `gh` invocations).
- **Asserts:** the worktree is created (it precedes the spawn attempt) but the spawn fails and is surfaced as an `IssueDispatchFailed` for the issue; the stub's recorded `gh` calls carry NO `issue edit`/`issue comment` invocation for it.
- **Does not assert:** N/A.
- **Note:** the write path (M1.0/M1.1, `claim_issue`) has since landed, called only AFTER a successful spawn, so the "no label/comment" half of this assertion is no longer vacuously true — it is a real regression guard, exactly like `scheduler/dispatch/012`'s own "GREEN as a regression guard, not RED-first" framing: it stays GREEN for a coder who correctly claims only after checking spawn's result, and would go RED for one who (wrongly) claims before that check.
- **Platform coverage:** mac+linux.

##### scheduler/dispatch/023 — An issue already labelled `in-progress` is skipped even with BOTH other signals absent (no worktree on disk, no open PR) — the label read as a third idempotency signal (PRD #421 M1.2).
- **Layer:** L2 (as `scheduler/dispatch/001`; the stub seeds the label via BOTH a `labels` field in the `gh issue list` response and a per-issue `gh issue view` fixture — `GhStub::set_issues_with_labels`). M1.2 settled on the list-embedded mechanism: the label is read from the `labels` field off the already-made `gh issue list --json number,labels` call (no extra `gh` invocation); the stub still seeds both shapes because the per-issue `gh issue view` fixture remains in use for the separate claimant lookup (`scheduler/dispatch/024`).
- **Agent:** none (run-now; observes the (absence of an) on-disk worktree + orchestrator count).
- **Asserts:** an issue pre-labelled `in-progress`, with no worktree and no open PR, is never dispatched (no worktree created, no orchestrator spawned) — proving the label alone excludes it.
- **Does not assert:** the claimant-reporting text on the skip (covered by `scheduler/dispatch/024`); the per-issue `gh issue view` claimant lookup, used only once the label is already known present (covered by `scheduler/dispatch/024`).
- **Platform coverage:** mac+linux.

##### scheduler/dispatch/024 — An issue labelled `in-progress` by a human/external tool (no deck claim comment) skips identically to a deck-made claim, and the skip specifically reports that no claimant was recorded (PRD #421 M1.2 + claimant reporting, RED until both land).
- **Layer:** L2 (as `scheduler/dispatch/023`; the seeded per-issue `gh issue view` fixture carries an empty `comments` array, so no claim comment is discoverable for the issue).
- **Agent:** none (run-now; observes daemon stderr for the skip's rendered reason + the absent worktree).
- **Asserts:** the skip is surfaced and its rendered text contains "no claimant" (loose substring), distinguishing an externally-applied label from one this deck itself claimed; the issue is not dispatched.
- **Does not assert:** the exact claimant-known skip wording for comparison (covered by `scheduler/dispatch/025`'s distinctness check, using its own issue); the mechanism by which comments are queried.
- **Platform coverage:** mac+linux.

##### scheduler/dispatch/025 — Three of PRD #421's four skip causes (worktree exists, open PR, `in-progress` label) render DISTINGUISHABLY from one another — no two collapse to the same text once the issue number/branch are normalized out (PRD #421 M1.2 + M1.3).
- **Layer:** L2 (as `scheduler/dispatch/001`; three issues in one repo — 31 no signal, 32 an open PR, 33 pre-labelled `in-progress` — fired twice: the first fire dispatches 31 (proving the flow ran) and skips 32/33; the second re-fires so 31's now-present worktree yields the worktree-exists cause. Each cause's rendered stderr line is captured, issue-number/branch stripped via `normalize_skip_line`, and compared pairwise).
- **Agent:** none (run-now ×2; observes daemon stderr, diffed against a pre-second-fire snapshot so lines are unambiguously attributed to a specific fire).
- **Asserts:** a skip line is rendered for EACH of the three causes (M1.2/M1.3 landed, so the label cause — issue 33 — renders like the other two rather than staying silent); once normalized, no two of the three causes' rendered text are equal.
- **Does not assert:** the fourth cause — a concurrent creator winning the `git worktree add` TOCTOU race (`WorktreeCreation::AlreadyClaimed` in `issue_dispatch_run.rs`) — deliberately left uncovered: it has no deterministic black-box trigger through this harness (forcing it needs either genuine concurrent fires racing on real subprocess timing, which cannot be tuned without a local test run this fork's tests forbid, or a production-side test seam this role may not add). Flagged to the orchestrator rather than guessed at.
- **Platform coverage:** mac+linux.

##### scheduler/dispatch/026 — Triage ENABLED (`triage = true`): a successful dispatch ensures PRD #421 M2.0's seven triage labels exist and the prompt delivered to the dispatched agent carries the full vocabulary plus the `needs-triage` uncertainty rule (PRD #421 M2.1/M2.2).
- **Layer:** L2 (as `scheduler/dispatch/001`; the fixture task's `[scheduled_tasks.issue_dispatch]` table carries `triage = true` — `IssueDispatchConfig::triage` is a real, wired field; `deny_unknown_fields` is still not set, so an unrelated unrecognized key would still parse-and-ignore rather than error). The stub `gh` accepts any `gh label create`/`gh label list` invocation unconditionally (recorded verbatim, same discipline as the `issue edit`/`issue comment` stanza), so the label-create mechanism is not dictated.
- **Agent:** none (run-now; observes the dispatched orchestrator agent's delivered prompt via a new `attach_and_capture_output` capture-once harness helper, plus the stub's recorded `gh` invocations).
- **Asserts:** after a successful dispatch (worktree + orchestrator agent present, as in `scheduler/dispatch/001`), the recorded `gh` calls include a `label` COMMAND-GROUP invocation naming each of the seven vocabulary labels (`priority-high`, `priority-medium`, `priority-low`, `size-high`, `size-medium`, `size-low`, `needs-triage`); the prompt delivered to the dispatched agent contains all seven vocabulary terms AND states the uncertainty rule (loose keyword match: an "uncertain"/"not confident"/"unsure" term together with an "unset"/"no priority"/"leave priority" term).
- **Does not assert:** that a spawned agent actually applies a correct priority/size label — that is LLM behavior, not deck behavior, and is not synthetically testable; the exact `gh label` mechanism (create vs. list-then-create-missing vs. `--force`); the exact prompt wording beyond the vocabulary + loose uncertainty-rule keywords.
- **Note:** M2.1/M2.2 landed — `triage = true` ensures the seven-label vocabulary via `gh label ...` and appends the triage instruction to the delivered prompt; GREEN today.
- **Platform coverage:** mac+linux.

##### scheduler/dispatch/027 — Triage OFF by default (no `triage` key): no `gh label ...` call naming the triage vocabulary is ever made and the delivered prompt carries none of PRD #421 M2.0's seven triage-vocabulary terms — a regression guard against the triage feature leaking into the default dispatch path (PRD #421 M2.1/M2.2).
- **Layer:** L2 (as `scheduler/dispatch/001`; plain `dispatch_task` with no `triage` key).
- **Agent:** none (run-now; observes the dispatched orchestrator agent's delivered prompt via `attach_and_capture_output`, plus the stub's recorded `gh` invocations).
- **Asserts:** after a successful dispatch, the delivered prompt contains none of the seven triage-vocabulary terms, and no recorded `gh` call belongs to the `label` command group AND names one of the seven vocabulary labels — narrowed from "no `label` call at all" (PRD #421 review fix): the unconditional `in-progress` claim (M1.0) also uses `gh label create`/`gh issue edit --add-label`, and is not triage, so it is deliberately exempt from this guard.
- **Does not assert:** the unconditional `in-progress` claim itself, which does use the `label`/`issue edit` command groups — covered by `scheduler/dispatch/010` and `scheduler/dispatch/028`.
- **Note:** GREEN as a regression guard, not RED-first, exactly like `scheduler/dispatch/012`/`022` — M2.1/M2.2 landed with triage correctly gated behind `cfg.triage`, so this pins that the default (off) path never leaks the vocabulary. The vocabulary-only narrowing (see Asserts) is a faithful expression of the test's original intent — "the triage feature does not leak into the default path" — not a weakening of it: `in-progress` was never triage vocabulary.
- **Platform coverage:** mac+linux.

##### scheduler/dispatch/028 — On a repo that does NOT already carry the `in-progress` label, a successful dispatch still ends with the issue genuinely claimed: the label is ensured to exist before the add, so `gh issue edit --add-label in-progress` actually succeeds (PRD #421 review fix — RED until the claim ensures the label first).
- **Layer:** L2 (as `scheduler/dispatch/001`); deliberately does NOT call `GhStub::seed_labels`, so the repo starts with no labels at all — the PRD #421 review's own counterexample (`vfarcic/dot-ai`, the docs' own example target, carries none of the seven-label vocabulary and would silently no-op pre-fix). The stub's tightened `issue edit --add-label <name>` mirrors real `gh`'s label-name-to-ID resolution (`cli/cli` v2.97.0, `LabelsToIDs`): it hard-errors before any mutation when `<name>` isn't already a known repo label.
- **Agent:** none (run-now; observes the dispatched orchestrator agent, the stub's recorded `gh` invocations, and `GhStub::label_applied` — the stub's own applied-label record, written only on a successful add).
- **Asserts:** the dispatch succeeds as normal (worktree + orchestrator agent present, as in `scheduler/dispatch/001`); an `issue edit ... --add-label in-progress` call is attempted for the issue; and — the actual defect this test pins — the add-label call SUCCEEDS (`GhStub::label_applied` reports it applied), which only happens if `in-progress` is ensured to exist first.
- **Does not assert:** the claim-comment write (covered by `scheduler/dispatch/010`); the read-back skip signal once labelled (covered by `scheduler/dispatch/023`); WHICH mechanism the coder uses to ensure the label exists (alongside the triage vocabulary, on demand after a failed add, or otherwise) — any of them satisfies this.
- **Note:** RED today — nothing ensures `in-progress` exists before the unconditional add, so the tightened stub's real-`gh`-accurate rejection fires and the claim silently fails (swallowed into `tracing::warn!`, so the run still reports success). This is the exact defect the PRD #421 review flagged: on any repo without a pre-existing `in-progress` label, the headline claim feature silently does nothing.
- **Platform coverage:** mac+linux.

##### scheduler/dispatch/029 — An ORCHESTRATION dispatch's claim comment names the orchestration's own typed name as DECORATION, and separately names the complete round-3 identity — the dispatched worktree's canonical absolute path AND its branch (PRD fork#235 round 3, test strengthened per fork#243 — decoration comes from the bound spawn handle's `SpawnKind`, not from `task_name`; the identity itself is the dispatched worktree regardless of spawn kind).
- **Layer:** L2 (as `scheduler/dispatch/001`; the fixture remote carries `ORCH_TOML`'s single-role orchestration named `dispatch-orch`, fired by a `ScheduledTask` deliberately named something else, `claim-task-021`).
- **Agent:** none (run-now; observes the stub's recorded `gh` invocations).
- **Asserts:** after a successful orchestration dispatch, a recorded `issue comment` call names `dispatch-orch` (the orchestration's own typed name, as decoration) AND contains the delimited fragment `` working `<path>` on branch `<branch>` `` — the dispatched worktree's canonical absolute path and its branch, backtick-delimited exactly as production's own `claim_comment_body` renders them (the complete round-3 identity, via the shared `assert_claim_names_worktree_identity`/`worktree_identity_fragment` helpers, which locate the fragment's two fields by their structural backtick delimiters rather than by searching for the surrounding prose, fork#243 round 4) — PRD #421 named the scheduled task exclusively; fork#235 derives the decoration from the spawn handle instead.
- **Does not assert:** the exact identity string format beyond the canonical path and branch; the single-agent path, which never has the decoration distinction to make (covered by `scheduler/dispatch/010`); that the scheduled task's name (`claim-task-021`) is absent from the comment — `derive_issue_paths` keys the clone directory on the task name for every `SpawnKind`, so the canonical worktree path asserted above legitimately contains `claim-task-021` as a path segment, making the absence unassertable without contradicting that path assertion. Do not reinstate it; the delimited-fragment predicate's own rejection of a path sharing only a prefix, or of a path containing the literal text `" on host "` — the exact cases round 2's independent-substring version and round 3's marker-search version each missed in turn — is pinned once, by `scheduler/dispatch/010`'s own catalog note, not re-pinned per test.
- **Platform coverage:** mac+linux.

##### scheduler/dispatch/030 — When `gh api user` fails, the label and claim comment still land (naming the round-3 worktree-path-plus-branch identity, which needs no `gh api user` call) and the dispatch is NOT reported as failed, but no assignee is ever written (PRD fork#235 M2 — a claim-identity resolution failure must never turn an already-successful dispatch into `IssueDispatchFailed`).
- **Layer:** L2 (as `scheduler/dispatch/001`; `GhStub::fail_api_user` arms `gh api user --jq .login` to exit non-zero, standing in for a read-only or expired token).
- **Agent:** none (run-now; observes the dispatched single-agent card, the stub's recorded `gh` invocations, `GhStub::label_applied`, `GhStub::assignees`, and the daemon's stderr).
- **Asserts:** the dispatch succeeds as normal (worktree + single-agent card present); the `in-progress` label write still SUCCEEDS; a claim comment is still posted and contains the dispatched worktree's absolute path AND its branch (round 3's identity resolves straight from the worktree, with no `gh` call, so a login failure cannot block it); the issue's assignee list stays empty throughout; the daemon's stderr never reports issue 51 as failed.
- **Does not assert:** the exact wording of the best-effort warning surfaced for the skipped assignee write; the successful-login assignee path (covered by `scheduler/dispatch/010`).
- **Note:** the label/comment/no-assignee/not-failed assertions were GREEN from the start (like `scheduler/dispatch/012`) since `claim_issue` does not yet call `gh api user` or write an assignee at all; the round-3 worktree-path-plus-branch identity assertion is a NEW addition this round and is RED until the identity is re-keyed, independent of the assignee logic this test otherwise pins. (Fork #194/#341: this note previously also cited `scheduler/dispatch/014`, an id that never covered claim ordering — it was the swallowed-seed prompt-delivery test — and has since been retired outright; the citation is dropped rather than repointed.)
- **Platform coverage:** mac+linux.

##### scheduler/dispatch/031 — `user_typed_since_writing_payload` is exercised directly against the registry, independent of `MAX_PAYLOAD_SUBMISSIONS`.
- **Layer:** L1 (in-process production registry with a real byte-observation PTY — `/bin/cat` on Unix, `more.com` on Windows).
- **Agent:** none.
- **Asserts:** with nothing written yet, the guard reports false; after a guarded payload write with no user input since, it still reports false; after the user types an unsent draft, it reports true for that exact payload text; after the delivery releases its record on a terminal outcome (`note_payload_settled`), the same text reports false again. Calls the registry method directly rather than going through the attempt-count retry loop (`crate::prompt_delivery::attempt_writes_payload`), so this stays true regardless of what `MAX_PAYLOAD_SUBMISSIONS` is set to — added fork #194/#341 round 2 after `scheduler/dispatch/018`'s replacement-pane sub-scenario, which drove the same predicate through the attempt-count loop, went vacuous and was retired. `scheduler/dispatch/019` and `/020` also reach the underlying predicate (`user_typed_since_writing_encoded` → `PaneInputState::user_typed_since_writing`) through `write_and_submit_guarded`'s non-empty-payload branch called directly, so this is not the guard's only other coverage — it is the only coverage that pins the four-step lifecycle (unwritten → written → user-typed → released) as a single sequence.
- **Does not assert:** the attempt-count wiring that decides whether a live delivery reaches this branch in production (covered, for the reachable attempt-1 case, by `scheduler/dispatch/018`; the launcher-consumes-attempt-1 recovery this would otherwise also protect is deferred to fork issue #343).
- **Platform coverage:** mac+linux+windows.

#### scheduler/pi

##### scheduler/pi/001 — A SCHEDULED, UNATTENDED real `pi` job (no TUI client attached) boots and its bundled extension reports the Pi pane's status via `agent-event`, re-broadcast on the daemon's event stream (PRD #201 M4.2).
- **Layer:** L2 (real `daemon serve` via the `DaemonProc` harness — no PTY, no attached TUI). The schedule's `command` is a REAL `pi` (`--provider openrouter --model openai/gpt-5-nano --approve -p ready`, a cheap non-interactive turn); the bundled extension is materialized into the daemon's HOME (via `orchestrator_ext::materialize`) so the scheduler-spawned pi (which inherits that HOME) auto-discovers it. `OPENROUTER_API_KEY` (never printed) + the freshly-built binary dir on PATH are propagated into the daemon via `spawn_daemon_serve_with_env` and inherited by the spawned pi. The fire is driven by `RunNow`; status is observed via an unattended `SubscribeEvents` consumer.
- **Agent:** REAL `pi` 0.80.6 (cheap gpt-5-nano `-p` turn). Flaky-tolerant pre-PR tier — run once, not looped. Runtime-skipped (Decision 26) when `pi`/`OPENROUTER_API_KEY` are absent.
- **Asserts:** after `RunNow`, the scheduled pi boots and its real extension shells `dot-agent-deck agent-event`, which the daemon ingests and re-broadcasts as a `Pi`-typed `AgentEvent` in one of the extension's mapped states (`WaitingForInput`/`Thinking`/`Idle`) carrying the scheduler-injected pane id — proving a scheduled, unattended (no-client) real pi is status-tracked through the same `AgentEvent` contract every client consumes. The match EXCLUDES `SessionStart`: the scheduler's `surface_spawned_pane` broadcasts a synthetic `SessionStart` with the `from_command`-guessed `Pi` type the instant the pane spawns (before pi's runtime boots), so requiring a non-`SessionStart` state is what makes the pass attributable to the REAL extension rather than the daemon's spawn-time guess.
- **Does not assert:** the delegate/work-done chain (covered by `chain-smoke/pi/001`); the exact lifecycle→state mapping across running/waiting/finished (covered synthetically by `status/agent-event/003` and the TS unit tests); a dashboard-attached Pi pane (the synthetic dashboard render is `dashboard/pane/007`; the real-agent unattended path is the M4.2 value here).
- **Platform coverage:** mac+linux (real-agent tier is local-only per Decision 8).
- **Cost note:** one cheap gpt-5-nano `-p` turn (and the status assertion resolves on boot, before the turn completes) — well under Decision 23's <$0.05/run bound.

#### scheduler/reuse

##### scheduler/reuse/001 — Two fires of a `new_tab_per_fire = false` task reuse one tab and re-deliver the prompt into the same pane (PRD #127 M2.2).
- **Layer:** L2.
- **Agent:** none (run-now ×2; observes registry agent count + PTY prompt-echo occurrence count).
- **Asserts:** across two fires the agent count for the task stays at 1 (never grows to 2), and the prompt marker is echoed twice by the single reused PTY (the second fire delivers into the existing pane).
- **Does not assert:** behavior after the reused tab is closed (stale-entry eviction is unit-tested by the coder).
- **Platform coverage:** mac+linux.

##### scheduler/reuse/002 — Two fires of a `new_tab_per_fire = true` task open two distinct tabs, each receiving the prompt (PRD #127 M2.2).
- **Layer:** L2.
- **Agent:** none (run-now ×2; observes registry agent count + per-pane prompt echo).
- **Asserts:** the agent count goes 1 → 2 (two distinct panes) and each pane receives the prompt.
- **Does not assert:** ordering of the two tabs; tab titles.
- **Platform coverage:** mac+linux.

##### scheduler/reuse/003 — On a reuse fire, a recent user keystroke debounces delivery until the pane goes idle; with no recent input the prompt is delivered immediately (PRD #127 M2.2, Q6).
- **Layer:** L2.
- **Agent:** none (run-now + simulated STREAM_IN keystroke; observes PTY prompt-echo occurrence count over time). Debounce window injected via `DOT_AGENT_DECK_REUSE_DEBOUNCE_MS` so the test is fast.
- **Asserts:** after a simulated keystroke, a reuse fire's prompt is NOT delivered within the debounce window and IS delivered into the same pane once the window elapses; a later fire with no recent input is delivered immediately.
- **Does not assert:** the production default debounce duration (the test injects a short one); queue depth beyond the latest prompt.
- **Platform coverage:** mac+linux.

#### scheduler/manager

##### scheduler/manager/001 — The "Scheduled Tasks" manager dialog lists schedules with a live/idle/disabled status indicator and a next-fire time, and its action buttons show their shortcut keys (PRD #127 M3.3).
- **Layer:** L2 (no public L1 dialog render seam — same constraint as `prompt/new-pane/007`; the real TUI is driven via PTY keystrokes and asserted on the rendered vt100 grid). Opened with the `S` keybinding.
- **Agent:** none (fixture global `schedules.toml` via `DOT_AGENT_DECK_SCHEDULES`).
- **Asserts:** pressing `S` opens a "Scheduled Tasks" dialog listing the configured tasks; an enabled-but-not-live task shows an `idle` status; a disabled task shows the `disabled` indicator with a `—` next-fire placeholder; each action button advertises its keyboard shortcut alongside the label (`[Add a]` / `[Edit e]` / `[Delete d]` / `[Run now r]`), mirroring the `[Scheduled Tasks s]` button-bar button.
- **Does not assert:** the exact next-fire timestamp formatting for enabled tasks; live-status rendering when a reused tab exists; the action buttons' click behavior (covered by `mouse/modal/001`).
- **Platform coverage:** mac+linux.

##### scheduler/manager/002 — Editing a schedule reuses the Ctrl+n dir-picker + mode-locked Edit Schedule form; submitting spawns the seeded authoring agent running the CONFIGURED command (`default_command`), pre-filled with the row's current values (PRD #127 M3.3; PRD #170 M2.1 + unified Add/Edit flow).
- **Layer:** L2 (same no-L1-seam reason for the manager dialog; the mode-locked form's render is covered at L1 by `scheduler/form/001`). Two shims are on PATH: a distinctive `default_command` (e.g. `stub-authoring`) shimmed to a recorder that posts SessionStart and records its delivered seed, and `claude` shimmed to a separate neutralizing recorder (so the host's real `claude` is never invoked and so a fall-back-to-`claude` regression is observable).
- **Agent:** the shimmed authoring agent (records the gated-delivered seed, mirroring how `tabs/mode/005` observes seed delivery).
- **Asserts:** with `default_command` set to the distinctive stub, pressing `e` on a row opens the directory picker (` Select Directory `); confirming the dir with Space opens the mode-locked ` Edit Schedule ` form (Command pre-filled from `default_command`); submitting via `[Submit]` spawns the seeded authoring agent running THAT configured command — its recorder receives the authoring seed carrying the row's current prompt value (pre-fill), AND the `claude` recorder receives nothing (the confirmed command came from `default_command`). RED until the unified flow exists: today `e` opens the deleted pick-agent modal, so the dir picker's ` Select Directory ` chrome never renders and the wait times out.
- **Does not assert:** the full authoring seed-prompt text; that the agent ultimately calls `schedule update` (covered by the CLI + seed-delivery mechanism); the add (blank) path (covered by `scheduler/form/002` / `scheduler/manager/010`); the spawn-in-picked-dir / working_dir pre-seed (covered by `scheduler/form/002` / `scheduler/form/003`); the mode-locked form's render (covered by `scheduler/form/001`).
- **Platform coverage:** mac+linux.

##### scheduler/manager/003 — `d` + confirm removes the schedule definition but does NOT close an already-open tab for it (PRD #127 M3.3).
- **Layer:** L2 (same no-L1-seam reason). Drives the real dialog + observes the global `schedules.toml` and the daemon registry.
- **Agent:** none (the schedule's own `cat` agent, opened by a prior run-now, stands in for an open tab).
- **Asserts:** after `d` then confirm (`y`), the definition is gone from `schedules.toml`, AND a tab/agent opened for that task before the delete is still live in the registry.
- **Does not assert:** the confirmation dialog's exact wording; rename behavior (forbidden, unit-tested).
- **Platform coverage:** mac+linux.

##### scheduler/manager/004 — `r` on a row triggers an immediate run-now fire of the selected task (PRD #127 M3.3).
- **Layer:** L2 (same no-L1-seam reason). Drives the real dialog + observes the daemon registry.
- **Asserts:** pressing `r` in the manager fires the selected task, which spawns its tab/agent (registered under the task's display name).
- **Does not assert:** prompt delivery content (covered by `scheduler/spawn/004`); reuse vs new-tab on the fire.
- **Platform coverage:** mac+linux.

##### scheduler/manager/005 — The delete confirmation stays contained within the modal even for a long schedule name (PRD #127 finding).
- **Layer:** L2 (same no-L1-seam reason). Drives the real dialog via `S` + `d` and asserts on the rendered vt100 grid.
- **Agent:** none (fixture global `schedules.toml` via `DOT_AGENT_DECK_SCHEDULES`, one enabled task with a deliberately long name).
- **Asserts:** after arming delete (`d`) on a long-named row, the confirmation's trailing `(y/n)` prompt — the only `(y/n)` in the app — still renders, proving the message is contained within the modal. Under PRD #144 the confirmation sits on two fixed natural lines (the name line; the `… (y/n)` trailer) and the content-sized modal grows in WIDTH to contain the long name line (clamped to ≤90% of the terminal), so the trailer is never clipped off the right border — superseding the PRD #127 wrap-to-grow-height band-aid.
- **Does not assert:** the modal's precise content-sized width / clamp fraction; the confirmation wording beyond the `(y/n)` tail and `Delete schedule` prefix.
- **Platform coverage:** mac+linux.

##### scheduler/manager/006 — Clicking a schedule row moves the selection to that row (PRD #127 finding — mouse parity).
- **Layer:** L2 (same no-L1-seam reason). Drives the real dialog via `S`, then a left-click SGR mouse report on a row, asserting on the rendered vt100 grid.
- **Agent:** none (fixture global `schedules.toml` via `DOT_AGENT_DECK_SCHEDULES`, two enabled tasks).
- **Asserts:** with two rows (`alpha` auto-selected, `bravo` not), clicking the `bravo` row moves the `▶` selection marker to it (`▶ bravo` renders and `▶ alpha` is gone), proving a row click hit-tests and re-selects.
- **Does not assert:** that the click also fires an action (it only selects); keyboard j/k navigation (the pre-existing selection path); scroll-into-view when the clicked row is off-window.
- **Platform coverage:** mac+linux.

##### scheduler/manager/007 — The manager dialog auto-sizes to its content and renders all fields un-clipped at both a roomy and a windowed width (PRD #144).
- **Layer:** L2 (no public L1 dialog render seam — same constraint as `scheduler/manager/001`; the real TUI is driven via PTY keystrokes and asserted on the rendered vt100 grid, at two PTY sizes via `with_pty_size`). Opened with the `S` keybinding.
- **Agent:** none (fixture global `schedules.toml` via `DOT_AGENT_DECK_SCHEDULES`, one enabled task whose name is longer than the legacy fixed-width name cell).
- **Asserts:** opening the manager at a roomy (200-col) terminal AND at a windowed (80-col) terminal renders the task's FULL name un-clipped on the grid at both widths — proving the dialog auto-sizes to its content (PRD #144 shared modal sizing helper, clamped within the windowed terminal) instead of truncating the field to the fixed 72-col modal. RED today: the modal is hard-capped at 72 cols and the name is truncated to 21 chars (`truncate_cell`), so the full name never appears.
- **Does not assert:** the exact modal width / clamp fraction at each terminal size; the `[min, max]` bounds of the shared helper (covered by the coder's pure-data unit test); the delete-confirmation containment (covered by `scheduler/manager/005`).
- **Platform coverage:** mac+linux.

##### scheduler/manager/010 — A blank/unset `default_command` falls back to `claude` (`DEFAULT_AUTHORING_COMMAND`) for the authoring agent, NOT a bare `$SHELL` (PRD #170 R1 fallback, via the unified Add flow).
- **Layer:** L2 (drives the real manager + dir-picker + mode-locked form via PTY; observed via a `claude` recorder shim on disk).
- **Agent:** the shimmed `claude` authoring agent (records the gated-delivered seed).
- **Asserts:** with `default_command = ""` (the unconfigured-user case), pressing `a` (Add) opens the directory picker (` Select Directory `); confirming the dir with Space opens the mode-locked ` New Schedule ` form whose Command pre-fills via the resolved authoring command (a blank default → `claude`); submitting via `[Submit]` spawns `claude` — its recorder receives the base authoring seed (`throwaway authoring session`) — proving the blank command resolves to the default authoring command instead of spawning a bare login shell that cannot act on the seed. RED until the unified flow exists: today `a` opens the deleted pick-agent modal, so the dir picker never appears and the ` Select Directory ` wait times out.
- **Does not assert:** the whitespace-only variant of the fallback (the same code path); the mode-locked form's render (covered by `scheduler/form/001`).
- **Platform coverage:** mac+linux.

##### scheduler/manager/016 — Wheel input over the Scheduled Tasks dialog does not scroll a mode-tab side pane behind the modal (issue #142).
- **Layer:** L2 (real TUI in a PTY; opens a synthetic `scroll` mode tab whose persistent right-hand side pane is filled with deterministic scrollback, then sends precise SGR wheel reports over the overlapping manager dialog).
- **Agent:** none (the mode side pane runs a synthetic shell command; no LLM is invoked).
- **Asserts:** after the side pane is scrolled into history and the manager is opened over it, wheel-down must first move the manager selection from `alpha` to `bravo`, then wheel-up must move it back to `alpha`, while the exposed side-pane marker sequence remains unchanged; the modal consumes the wheel events instead of leaking them to the pane behind it.
- **Does not assert:** focused dashboard-pane wheel behavior; child-app mouse forwarding; the manager list viewport behavior (covered by `scheduler/manager/017`).
- **Platform coverage:** mac+linux.

##### scheduler/manager/017 — Wheel input over a windowed Scheduled Tasks list moves its selection and derived viewport (issue #142).
- **Layer:** L2 (real TUI in a constrained-height PTY; a fixture global `schedules.toml` contains 30 distinct tasks, more than the manager can render at once, and the first visible task row supplies the coordinate for precise SGR wheel reports over the list viewport).
- **Agent:** none (fixture global `schedules.toml` via `DOT_AGENT_DECK_SCHEDULES`).
- **Asserts:** the first row starts selected and `wheel-task-13` starts below the viewport; twelve wheel-down reports over the list move the `▶` marker to `wheel-task-13`, which drags the selection-derived viewport until that initially hidden row is visible.
- **Does not assert:** an independent list scroll offset (none exists); wheel-up wrapping at the first row; background side-pane isolation (covered by `scheduler/manager/016`).
- **Platform coverage:** mac+linux.

#### scheduler/form

##### scheduler/form/001 — The new-pane form mode-locked to schedule renders ONLY Dir + Command (no Mode cycler, no Name field) and titles itself ` New Schedule ` (Add) / ` Edit Schedule ` (Edit) (PRD #170 unified Add/Edit flow).
- **Layer:** L1 (ratatui `TestBackend` via a new public `render_new_pane_form_schedule_to_buffer(edit, w, h)` seam, mirroring `render_new_pane_form_to_buffer`). RED is a COMPILE error until the coder adds the seam + the `NewPaneFormState::new_schedule_locked` constructor and locked render branches it drives.
- **Agent:** none.
- **Asserts:** the schedule-locked form renders the Dir field, the (free-text) Command field, and the `[Submit]`/`[Cancel]` buttons, with the Mode cycler HIDDEN (no `No mode` chip) and the Name field HIDDEN (no `Name:`); its title is ` New Schedule ` in the Add variant (`edit = false`) and ` Edit Schedule ` in the Edit variant (`edit = true`). RED until the locked render branches exist: today the form always shows the Mode cycler + Name field and titles itself ` New Agent `.
- **Does not assert:** the Command pre-fill value (configured-command resolution is covered at L2 by `scheduler/manager/002`/`010`); the spawn on submit (covered by `scheduler/form/002`/`003`); insta byte-snapshot identity (plain substring assertions, matching `mouse/form/001`).
- **Platform coverage:** mac+linux+windows.

##### scheduler/form/002 — Manager Add reuses the Ctrl+n dir-picker + mode-locked ` New Schedule ` form; submitting spawns the seeded authoring agent IN the picked directory (PRD #170 unified Add/Edit flow).
- **Layer:** L2 (drives the real manager → dir picker → mode-locked form via PTY; observed via distinct-name recorder shims on disk that record their spawn `pwd` then the delivered seed). `default_command = "stub-add-authoring"` (a recorder shim) with a `claude` neutralizer on PATH.
- **Agent:** the shimmed `stub-add-authoring` authoring agent (records spawn cwd + the gated-delivered seed).
- **Asserts:** pressing `a` (Add) opens the directory picker (` Select Directory `); confirming the current dir with Space opens the mode-locked ` New Schedule ` form (Command pre-filled from `default_command`); submitting via `[Submit]` spawns the seeded authoring agent — its recorder receives the base authoring seed (`throwaway authoring session`) AND its recorded `pwd` carries the picked dir's basename (the agent spawned IN the confirmed directory), while the `claude` neutralizer stays empty. RED until the unified flow exists: today `a` opens the deleted pick-agent modal, so the dir picker never appears and the ` Select Directory ` wait times out.
- **Does not assert:** the Edit pre-fill / working_dir-from-row behavior (covered by `scheduler/form/003`); the blank-default→`claude` fallback (covered by `scheduler/manager/010`); the mode-locked form's render (covered by `scheduler/form/001`).
- **Platform coverage:** mac+linux.

##### scheduler/form/003 — Manager Edit starts the dir picker at the row's `working_dir`, pre-fills the authoring seed with the existing schedule's values, and spawns the agent IN that working_dir (PRD #170 unified Add/Edit flow).
- **Layer:** L2 (drives the real manager → dir picker → mode-locked form via PTY; observed via distinct-name recorder shims on disk that record their spawn `pwd` then the delivered seed). `default_command = "stub-edit-authoring"` (a recorder shim) with a `claude` neutralizer on PATH; the fixture row's `working_dir` is a distinctively-named existing dir (`.../EDITWORKDIR`) and its prompt is `EDITPROMPTMARKER`.
- **Agent:** the shimmed `stub-edit-authoring` authoring agent (records spawn cwd + the gated-delivered seed).
- **Asserts:** pressing `e` (Edit) opens the directory picker which STARTS at the row's `working_dir`; confirming it with Space (no navigation) opens the mode-locked ` Edit Schedule ` form; submitting via `[Submit]` spawns the seeded authoring agent — its recorder receives the row's distinctive prompt `EDITPROMPTMARKER` (the seed is PRE-FILLED with the existing schedule's values) AND its recorded `pwd` carries `EDITWORKDIR` (the picker started at, and pre-seeded as the spawn cwd, the row's working_dir), while the `claude` neutralizer stays empty. RED until the unified flow exists: today `e` opens the deleted pick-agent modal, so the dir picker never appears and the ` Select Directory ` wait times out.
- **Does not assert:** the Add (blank-context) path (covered by `scheduler/form/002`); the configured-command vs `claude` resolution beyond the neutralizer check (covered by `scheduler/manager/002`); the mode-locked form's render (covered by `scheduler/form/001`).
- **Platform coverage:** mac+linux.

##### scheduler/form/004 — Cancelling a MANAGER-originated schedule flow at the DIRECTORY PICKER (Esc / `q`) returns to the Scheduled-Tasks manager dialog, not the bare dashboard (PRD #170 round 4, reviewer F5).
- **Layer:** L2 (drives the real manager → dir picker via PTY; asserted on the rendered vt100 grid plus the daemon registry). A benign `default_command = "cat"` so any erroneous spawn never invokes the host's real `claude`.
- **Agent:** none (the flow is cancelled before any authoring agent spawns).
- **Asserts:** opening the manager (`S`), pressing `a` (Add) or `e` (Edit) opens the directory picker (` Select Directory `); pressing Esc (Add + Edit) or `q` (Add) from the picker returns to the MANAGER dialog — its `NEXT FIRE` header re-renders — with the picker chrome (` Select Directory `) gone and NO `schedule` authoring agent spawned. RED until cancel is intent-aware: today the picker's Esc/`q` handlers unconditionally set `UiMode::Normal` (dashboard), so `NEXT FIRE` never reappears and the wait times out. Restores the intent the removed `scheduler/manager/011` (Esc) / `013` (`q`) pinned, re-targeted at the unified flow.
- **Does not assert:** the form cancel point (covered by `scheduler/form/005`); a `Ctrl+n`-origin cancel still dropping to the dashboard (unchanged, out of scope); the spawn/seed on submit (covered by `scheduler/form/002`/`003`).
- **Platform coverage:** mac+linux.

##### scheduler/form/005 — Cancelling a MANAGER-originated schedule flow at the mode-locked FORM (Esc / click `[Cancel]`) returns to the Scheduled-Tasks manager dialog, not the bare dashboard (PRD #170 round 4, reviewer F5).
- **Layer:** L2 (drives the real manager → dir picker → mode-locked form via PTY; asserted on the rendered vt100 grid plus the daemon registry). A benign `default_command = "cat"` so any erroneous spawn never invokes the host's real `claude`.
- **Agent:** none (the flow is cancelled before any authoring agent spawns).
- **Asserts:** opening the manager (`S`), pressing `a` (Add) or `e` (Edit) → confirming a dir with Space opens the mode-locked schedule form (` New Schedule ` / ` Edit Schedule `, with `[Submit]`); pressing Esc (Add + Edit) or clicking `[Cancel]` (Add) from the form returns to the MANAGER dialog — its `NEXT FIRE` header re-renders — with the form chrome (`[Submit]`) gone and NO `schedule` authoring agent spawned. RED until cancel is intent-aware: today the form's Esc/`[Cancel]` handlers unconditionally set `UiMode::Normal` (dashboard), so `NEXT FIRE` never reappears and the wait times out. Restores the intent the removed `scheduler/manager/015` (click `[Cancel]`) pinned, re-targeted at the unified flow.
- **Does not assert:** the picker cancel point (covered by `scheduler/form/004`); a `Ctrl+n`-origin cancel still dropping to the dashboard (unchanged, out of scope); the spawn/seed on submit (covered by `scheduler/form/002`/`003`).
- **Platform coverage:** mac+linux.

##### scheduler/form/006 — On Edit, re-picking a DIFFERENT working_dir makes that picked dir WIN in the authoring seed — no conflicting old-vs-new working_dir (PRD #170 round 4, reviewer F3).
- **Layer:** L2 (drives the real manager → dir picker → mode-locked form via PTY; observed via a distinct-name recorder shim on disk that records its spawn `pwd` then the delivered seed). `default_command = "stub-repick-authoring"` (a recorder shim) with a `claude` neutralizer on PATH; the fixture row's `working_dir` is a distinctively-named existing dir (`.../ROWDIRALPHA`) with a sibling re-pick target (`.../PICKDIRBRAVO`) and the row's prompt is `EDITPROMPTF3`.
- **Agent:** the shimmed `stub-repick-authoring` authoring agent (records spawn cwd + the gated-delivered seed).
- **Asserts:** pressing `e` (Edit) opens the dir picker started at the row's `working_dir` (`ROWDIRALPHA`); going UP one level (`h`) and descending into the DIFFERENT sibling `PICKDIRBRAVO` (double-click, confirmed via its `INNERMARK` child) then confirming with Space, and submitting via `[Submit]`, spawns the seeded authoring agent whose recorded seed — once delivered through its `EDITPROMPTF3` prompt line (which follows the `working_dir:` line) — carries `PICKDIRBRAVO` but ZERO occurrences of the row's stale `ROWDIRALPHA`. RED today: the edit seed appends the row's `working_dir: .../ROWDIRALPHA` as a conflicting current value alongside the picked `working_dir DEFAULT: .../PICKDIRBRAVO`.
- **Does not assert:** the unchanged-pick / pre-fill path (covered by `scheduler/form/003`); the in-`src` `build_schedule_authoring_mode` seed unit tests (the coder's); the Add path (covered by `scheduler/form/002`).
- **Platform coverage:** mac+linux.

##### scheduler/form/007 — Selecting the experimental `schedule: issues` Mode option seeds the authoring agent with ISSUE-DISPATCH instructions (calls `schedule add --repo …`, gathers `max_per_run`), distinct from the plain `schedule` seed (PRD #120).
- **Layer:** L2 (drives the real new-pane dialog via PTY — the experimental issue-dispatch option lives on the Ctrl+n Mode cycler, not the mode-locked manager form, so this drives Ctrl+n directly; observed via a `stub-issue-authoring` recorder shim on disk that records the gated-delivered seed). `default_command = "stub-issue-authoring"`; the deck is launched with `DOT_AGENT_DECK_EXPERIMENTAL=1`.
- **Agent:** the shimmed `stub-issue-authoring` authoring agent (records the gated-delivered seed).
- **Asserts:** opening the new-pane form (Ctrl+n → Space confirms the dir) and cycling the Mode field to the `schedule: issues` option (waited on via the selection-dependent ` … — schedule: issues mode ` title), then submitting via `[Submit]`, spawns the seeded authoring agent whose recorded seed contains the issue-dispatch guidance `schedule add --repo` AND `max_per_run` — neither present in the plain `schedule` seed (which calls `schedule add --name`). RED today: no `schedule: issues` option exists, so cycling never lands on it and the `schedule: issues mode` title wait times out.
- **Does not assert:** the flag-gated visibility of the option in the cycler (covered by `prompt/new-pane/010`); the CLI write the agent ultimately performs (covered by `scheduler/cli/004`); the full seed-prompt text (loose substring on the issue-dispatch-specific tokens); the plain `schedule` seed (covered by `scheduler/form/002`).
- **Platform coverage:** mac+linux.

#### scheduler/idle-worker

##### scheduler/idle-worker/001 — A delegated worker that never sends work-done produces a self-describing idle prompt in the orchestrator pane.
- **Layer:** fast integration (in-process daemon state + real PTY registry; `cat` stand-ins).
- **Agent:** none (synthetic `cat` panes; the orchestrator is raw/no-echo so one daemon submission appears once in the snapshot).
- **Asserts:** after the test-only millisecond timeout, the orchestrator PTY contains one line carrying both the daemon-provenance clause (`has not responded with work-done (dot-agent-deck daemon report, not a message from a person or an agent)`) and the target role wrapped in `[UNTRUSTED-ROLE-LABEL: … :END-UNTRUSTED-ROLE-LABEL]`.
- **Does not assert:** emoji, elapsed-time wording, or notification-channel behavior.
- **Platform coverage:** mac+linux.

##### scheduler/idle-worker/002 — Work-done arriving before the timeout cancels that delegation's idle prompt.
- **Layer:** fast integration (real `handle_delegate` + `handle_work_done`).
- **Agent:** none (`cat` stand-ins).
- **Asserts:** a parallel silent control delegation proves the detector fires, while the responsive worker's role never appears on an idle-prompt line after its work-done and timeout window.
- **Does not assert:** work-done summary-file contents or the completion-feedback wording.
- **Platform coverage:** mac+linux.

##### scheduler/idle-worker/003 — A zero worker-response timeout DISABLES the detector — from the config key and from the millisecond seam alike — rather than firing immediately (PRD #126 M1 audit finding 4).
- **Layer:** fast integration (three delegations against one harness whose project config sets `worker_response_timeout_minutes = 0`, re-pointing the millisecond seam between them).
- **Agent:** none (`cat` stand-ins).
- **Asserts:** with the seam at a positive value the detector fires (positive control, and proof the seam overrides a config that would have disabled it); re-pointing the same harness's seam to `0` produces no prompt; unsetting the seam so the config's own `0` is consulted produces no prompt either; exactly one prompt exists at the end.
- **Does not assert:** that a *file* `0` is decisive against a file positive value — no config value below one minute exists, so that comparison is unobservable behaviorally and is covered at resolution level by `scheduler/idle-worker/007`.
- **Platform coverage:** mac+linux.

##### scheduler/idle-worker/004 — An outstanding delegation produces only one idle prompt and never re-nags.
- **Layer:** fast integration (in-process daemon state + raw/no-echo orchestrator PTY).
- **Agent:** none (`cat` stand-ins).
- **Asserts:** the first idle prompt appears, then the ASCII idle needle still occurs exactly once after another timeout window.
- **Does not assert:** behavior after a later re-delegation (covered by `scheduler/idle-worker/005`).
- **Platform coverage:** mac+linux.

##### scheduler/idle-worker/005 — Re-delegating to the same worker pane replaces the first timer without a premature or duplicate prompt.
- **Layer:** fast integration (real repeated `handle_delegate` calls against one worker pane).
- **Agent:** none (`cat` stand-ins).
- **Asserts:** no prompt appears after delegation one's old deadline but before delegation two's deadline; delegation two then produces exactly one role-bearing idle prompt.
- **Does not assert:** concurrent delegation to different worker panes.
- **Platform coverage:** mac+linux.

##### scheduler/idle-worker/006 — Closing a delegated worker through StopAgent cancels its outstanding idle timer.
- **Layer:** fast integration with an in-process attach server and the real StopAgent request.
- **Agent:** none (`cat` stand-ins).
- **Asserts:** a silent control worker proves the detector fires, while the stopped worker never appears on an idle-prompt line after the timeout.
- **Does not assert:** worktree cleanup or TUI close-key behavior.
- **Platform coverage:** mac+linux.

##### scheduler/idle-worker/007 — The worker-response timeout resolves env-over-file-over-default, prefers the orchestration cwd, defaults to 120 minutes, and REJECTS an out-of-range value in favour of the default instead of clamping it.
- **Layer:** fast unit-level (calls the real `worker_response_timeout` resolver directly against purpose-built config directories; no PTY).
- **Agent:** none.
- **Asserts:** an absent key (and a cwd with no config file at all) resolves to 120 minutes; the orchestration cwd's value wins over the worker cwd's and the worker cwd is the fallback when the orchestration cwd has no config; a `20000`-minute file value resolves to the 120-minute DEFAULT, not to the 10080-minute ceiling; an in-range millisecond seam overrides the file; a below-floor (`50`) and an above-ceiling (`604800001`) seam value are both ignored so resolution continues to the file/default rather than clamping; `0` from either source resolves to `None` (detector disabled); the `1`-minute and `10080`-minute bounds themselves are honored.
- **Does not assert:** the delegate-time behavior of a disabled detector (covered by `scheduler/idle-worker/003`); non-integer or negative TOML values (rejected earlier, at parse time).
- **Platform coverage:** mac+linux.

##### scheduler/idle-worker/008 — After the ORCHESTRATOR pane closes, an unrelated agent that inherits its pane id receives nothing — the dead orchestration's idle prompt is never auto-submitted into a stranger's session (PRD #126 M1 review finding 1 / audit finding 2).
- **Layer:** fast integration with an in-process attach server, the real StopAgent request, and a second raw/no-echo `cat` spawned onto the freed `pane_id_env`.
- **Agent:** none (`cat` stand-ins; the successor is raw/no-echo so any submitted byte is directly observable in its scrollback).
- **Asserts:** the successor's own readiness marker is present (so absence of anything else is meaningful) while its PTY carries zero occurrences of the daemon clause and no fragment of the dead orchestration's role name, after two full timeout windows during which the successor owned the pane.
- **Does not assert:** which of the two layered guards refused — the record sweep over orchestrator-side records at `begin_pane_close`, or the `write_and_submit_guarded` agent-id gate. Both must be removed before a stray submit appears on THIS (StopAgent) path, because the sweep drops the record before any timer can wake; the identity gate on its own is isolated by `scheduler/idle-worker/014`, which reaches the same pane-reuse state through an orchestrator exit that runs no sweep at all.
- **Platform coverage:** mac+linux.

##### scheduler/idle-worker/009 — A timer whose deadline falls inside a pane's SIGTERM grace window does not fire the nudge that the deliberate close exists to suppress (PRD #126 M1 review finding 1).
- **Layer:** fast integration with an in-process attach server and the real StopAgent request against a worker that IGNORES SIGTERM, so `close_agent` spends its full three-second grace with the pane marked closing.
- **Agent:** none (`cat` for the control; `trap '' TERM; exec cat` under a pinned `/bin/sh` for the TERM-resistant worker).
- **Asserts:** first, as a precondition, that the close window genuinely bracketed the detector deadline (close started before it and finished after it), so the test cannot pass for the wrong reason; then that a parallel silent control produced a prompt while the closing worker produced none.
- **Does not assert:** SIGKILL escalation timing, or the close outcome for a worker that exits promptly on SIGTERM (covered by `scheduler/idle-worker/006`).
- **Platform coverage:** mac+linux.

##### scheduler/idle-worker/010 — A delegate that lands while a pane is mid-close is refused arming, so the close cannot be raced into leaving a record behind it (PRD #126 M1 review finding 1).
- **Layer:** fast integration; a SIGTERM-ignoring worker holds the close transition open for three seconds and the test barriers on `is_pane_closing` before delegating, then re-asserts the mark is still set after the delegate returns.
- **Agent:** none (`cat` for the control; `trap '' TERM; exec cat` for the closing worker).
- **Asserts:** the delegate provably landed inside the close transition, and after the timeout the control has a prompt while the closing worker has none.
- **Does not assert:** the registry-level `arm_outstanding_delegation` → `None` contract in isolation (covered by the in-`src` unit test `begin_pane_close_cancels_records_targeting_the_closing_orchestrator`).
- **Platform coverage:** mac+linux.

##### scheduler/idle-worker/011 — A silent delegated worker's idle prompt is visible in a PTY-attached orchestration pane.
- **Layer:** L2 PTY (real `dot-agent-deck` binary and lazy daemon, rendered through the vt100 `TuiDeck` harness).
- **Agent:** none (the `orch-deck` fixture uses live `cat` stand-ins; synthetic Delegate injected over the real hook socket, so this entry is intentionally not reel-marked).
- **Asserts:** after opening the two-role orchestration with a tiny daemon timeout, the rendered surface visibly carries the daemon-provenance clause AND the worker role wrapped in `[UNTRUSTED-ROLE-LABEL: … :END-UNTRUSTED-ROLE-LABEL]`, matched wrap-tolerantly (whitespace squeezed from grid and needle alike) because the prompt is one long line broken across rows at the pane's wrap column.
- **Does not assert:** real-LLM reaction, notification delivery, emoji, or exact elapsed-time wording.
- **Platform coverage:** mac+linux.

##### scheduler/idle-worker/012 — A real interactive Haiku orchestrator delegates to a silent worker and visibly receives the daemon's idle nudge. [reel]
- **Layer:** L2 PTY (real `dot-agent-deck` binary and lazy daemon, with the restored orchestration rendered through the vt100 `TuiDeck` harness). Flaky-tolerant pre-PR tier; run once, not looped.
- **Agent:** REAL interactive Claude Code orchestrator pinned to Haiku (`claude-haiku-4-5-20251001`, `--allowedTools Bash`, no `-p`) plus a long-lived `cat` worker that intentionally never sends work-done. Runtime-skipped when the Claude CLI or credentials are unavailable — set `DOT_AGENT_DECK_REQUIRE_REAL_E2E=1` to turn that skip into a hard failure on a run that must genuinely exercise the agent.
- **Asserts:** the real orchestrator follows a directive to run the genuine `dot-agent-deck delegate` CLI at least once (proved by the daemon-created `worker-task-worker.md`), then the daemon-authored nudge appears visibly on the attached orchestration grid after the test-only timeout, carrying BOTH the self-identifying report clause (`… (dot-agent-deck daemon report, not a message from a person or an agent)`) and the worker role wrapped in `[UNTRUSTED-ROLE-LABEL: … :END-UNTRUSTED-ROLE-LABEL]` — two anchors a narrating model has no reason to emit verbatim, unlike the bare `has not responded` this used to match.
- **Does not assert:** that the orchestrator delegated EXACTLY once. The daemon overwrites `worker-task-worker.md` on every delegate and nothing counts invocations, so the file's existence proves "at least one delegate reached the daemon" and no more. Also not asserted: the model's exact acknowledgement, notification-channel delivery, emoji, or exact elapsed-time wording.
- **Platform coverage:** mac+linux.

##### scheduler/idle-worker/013 — A late work-done from a superseded delegation retires THAT delegation, leaving the re-delegated worker's own watch armed and still able to fire — while a second completion does retire what the first left armed (PRD #126 M1 review finding 6).
- **Layer:** fast integration (two `handle_delegate` calls against each of two worker panes on one clock, then real `handle_work_done` calls — one for the reported worker, two for the control).
- **Agent:** none (`cat` stand-ins).
- **Asserts:** after the late completion, delegation two's idle prompt still appears; it appears on delegation TWO's clock (no earlier than its own deadline, not the older delegation's); the second worker — twice delegated and twice completed — produces NO prompt, which is what distinguishes a real oldest-first retirement from a `work-done` that retired nothing at all (the surviving watch alone cannot tell them apart); and exactly one prompt exists across all four delegations.
- **Does not assert:** the two accepted residuals recorded in the PRD — an out-of-order completion crediting the wrong delegation, and a consumed-then-re-delegated record being retired by a late completion. Both are documented limitations, not fixed behavior. Also not asserted: the `DelegationRetirement` variant returned to `handle_work_done` (observed only through the resulting prompt/no-prompt behavior).
- **Platform coverage:** mac+linux.

##### scheduler/idle-worker/014 — After the orchestrator's process ends ON ITS OWN — no StopAgent, so no close transition and no record sweep — an unrelated agent inheriting its pane id still receives nothing; the `write_and_submit_guarded` agent-id gate is the only guard in play (PRD #126 M1 audit finding 2).
- **Layer:** fast integration; the orchestrator stub is a polling shell that exits when the test drops a flag file in its cwd (a genuine process exit, not a signalled close), after which a raw/no-echo `cat` takes the freed `pane_id_env`. No attach server, so no `StopAgent` exists in this test at all.
- **Agent:** none (`cat` worker stand-in; the successor is raw/no-echo so any submitted byte is directly observable in its scrollback).
- **Asserts:** two preconditions that stop it passing for the wrong reason — the orchestrator pane is NOT in a close transition after the exit (so the close-time sweep is provably not what suppresses the prompt), and the successor owned the pane before the delegation's deadline (so a stray timer had a live target to mis-deliver to) — then that after two further timeout windows the successor's PTY carries its own readiness marker, zero occurrences of the daemon clause, and no fragment of the dead orchestration's role name.
- **Does not assert:** the pane-reuse-after-`StopAgent` path (covered by `scheduler/idle-worker/008`); the orchestration-membership half of the delivery revalidation (the successor is spawned without `tab_membership`, so that check legitimately abstains and the agent-id gate is what refuses).
- **Platform coverage:** mac+linux.

##### scheduler/idle-worker/015 — A silent-worker notice cannot launder user input into a later blind submit probe.
- **Layer:** L1 (in-process production silent-worker watch and guarded notice/submit paths with a real `/bin/cat` byte-observation PTY).
- **Agent:** none.
- **Asserts:** an automatic payload lands, the user types an unsent draft, and the real silent-worker watch then writes its fixed daemon notice; a following submit-only probe is refused and leaves the draft-plus-notice snapshot unchanged rather than submitting it.
- **Does not assert:** the broader idle-worker detection policy or the exact diagnostic prose, only that the production notice caller cannot reauthorize a blind probe.
- **Platform coverage:** mac+linux.

#### scheduler/live

##### scheduler/live/001 — A scheduled fire surfaces its card LIVE to an already-attached TUI, without a disconnect/reconnect (PRD #127 finding #2).
- **Layer:** L2 (real TUI driven via PTY; observed on the rendered vt100 grid — the only surface where the bug shows, since the daemon registry holds the agent in both states). Fixture global `schedules.toml` via `DOT_AGENT_DECK_SCHEDULES`; fired with the `RunNow` control message over the deck's attach socket.
- **Agent:** none (a plain `cat` command — no hooks — so the only path that could surface a card is a new-agent broadcast, not a hook event).
- **Asserts:** after firing a `cat`-command schedule into the daemon the attached TUI is connected to, the agent is registered in the daemon (precondition), AND a card for it appears on the already-attached dashboard live (the task name renders) — no detach/reattach.
- **Does not assert:** prompt delivery content; the card's status badge / body layout; behavior after a reconnect (which already masks the bug via startup hydration).
- **Platform coverage:** mac+linux.

##### scheduler/live/002 — A scheduled (daemon-spawned) card survives being focused — focus re-hydrates it instead of deleting it (PRD #127 finding #2).
- **Layer:** L2 (real TUI driven via PTY; observed on the rendered vt100 grid). Fixture global `schedules.toml` via `DOT_AGENT_DECK_SCHEDULES`; fired with `RunNow`. A `SessionStart` hook carrying the daemon-spawned agent's own `DOT_AGENT_DECK_PANE_ID` (read back from the registry) is injected to paint the card — faithfully mirroring what a real agent's hook does.
- **Agent:** none (long-lived `cat`; the hook is injected by the harness with the agent's real pane id so the card is backed by a live daemon agent but not a local TUI pane — the orphan-card condition).
- **Asserts:** the hook paints a card on the attached dashboard (precondition, holds in the broken state too), and pressing the `1` jump key to focus that card keeps it usable — the TUI enters PaneInput mode on the re-hydrated pane (the card is not deleted).
- **Does not assert:** the exact pane contents after focus; the live-surfacing path for the non-hook case (covered by `scheduler/live/001`).
- **Platform coverage:** mac+linux.

##### scheduler/live/003 — A live-surfaced scheduled card's TITLE shows the schedule's friendly name, not the truncated spawn pane-id (PRD #127 finding #2 regression).
- **Layer:** L2 (real TUI driven via PTY; observed on the rendered vt100 grid). Fixture global `schedules.toml` via `DOT_AGENT_DECK_SCHEDULES`; fired with the `RunNow` control message over the deck's attach socket. The schedule's `working_dir` basename (`runbox`) is deliberately unrelated to its name (`morning-digest`) so the friendly name can only reach the grid through the card title — not the Dir line.
- **Agent:** none (a plain `cat` command — no hooks; the card surfaces via the new-agent broadcast as in `scheduler/live/001`).
- **Asserts:** after a fire into the attached daemon, the agent is registered under its friendly name (precondition) and the card surfaces live (its Dir line shows the cwd basename), AND the card TITLE shows the friendly name `morning-digest` — matching a reconnect — and NOT the truncated spawn pane-id form (`… · sched-morni…`).
- **Does not assert:** the surfacing path itself (covered by `scheduler/live/001`); focus survival (covered by `scheduler/live/002`); the title after a reconnect (which already masks the bug via startup hydration); the card's status badge / body layout.
- **Platform coverage:** mac+linux.

##### scheduler/live/004 — A live-surfaced scheduled card's friendly TITLE SURVIVES being superseded by the agent's real `SessionStart` hook — it does not revert to the session-id hash (PRD #127 finding #2, hook-supersession gap).
- **Layer:** L2 (real TUI driven via PTY; observed on the rendered vt100 grid). Fixture global `schedules.toml` via `DOT_AGENT_DECK_SCHEDULES`; fired with `RunNow`. The schedule's `working_dir` basename (`runbox`) is deliberately unrelated to its name (`morning-digest`) so the friendly name can only reach the grid through the card title. After the synthetic placeholder surfaces, a real `SessionStart` hook is injected carrying the spawned pane's pane id AND its spawn-injected registry agent id (both read back from the registry) and NO display_name metadata — faithfully reproducing what a hook-emitting claude/opencode agent emits.
- **Agent:** none (a plain `cat` command; the synthetic placeholder surfaces via the new-agent broadcast as in `scheduler/live/001`, then the harness injects the agent's real `SessionStart` hook — a `Some(agent_id)` distinct from the placeholder's `None` — to drive the supersession the primary hook-emitting scheduler case hits).
- **Asserts:** after the placeholder surfaces with the friendly title `morning-digest` and the real hook supersedes it (the "No agent" placeholder becomes a live ClaudeCode card), the card TITLE STILL shows `morning-digest` (matching a reconnect) and has NOT reverted to the session-id hash form (`… · 9f8e7d6c-5b…`).
- **Does not assert:** the surfacing path itself (covered by `scheduler/live/001`); focus survival (covered by `scheduler/live/002`); the no-hook title case (covered by `scheduler/live/003`); the title after a reconnect (which already masks the bug via startup hydration); the card's status badge / body layout.
- **Platform coverage:** mac+linux.

##### scheduler/live/005 — A daemon-surfaced orchestration whose local `.dot-agent-deck.toml` no longer lists it by the time the ALREADY-ATTACHED TUI processes the live-surfacing broadcast renders an on-screen drift warning naming the orchestration (fork issue #318).
- **Layer:** L2 (real TUI driven via PTY; observed on the rendered vt100 grid). Fixture global `schedules.toml` via `DOT_AGENT_DECK_SCHEDULES`; fired with `RunNow`. The target directory's `.dot-agent-deck.toml` is a named pipe (`mkfifo`) so the test can hand the daemon's spawn-time `decide_target` read and the attached TUI's later `surface_one_orchestration` read two deliberately different bodies with no timing race (a FIFO `open()` blocks until a reader/writer pair up).
- **Agent:** none (two `cat`-command roles — orchestrator + worker — no hooks).
- **Asserts:** firing a schedule whose target directory defines a two-role `[[orchestrations]]` entry named `demo-orch` spawns and registers the orchestrator role (precondition, confirms the daemon's config read already saw "demo-orch"); with the local config then rewritten (via the second FIFO rendezvous) to no longer list `demo-orch` before the attached TUI's `surface_one_orchestration` reads it, the SAME on-screen substring `orchestration/hydration/001` pins (`orchestration 'demo-orch' not found in local config`) appears on the rendered grid.
- **Does not assert:** the reconnect-hydration call site (covered by `orchestration/hydration/001`/`002`, a different call site entirely — nothing races there); the exact wording of the warning beyond the required substring; the unparseable-config case for this call site (only the "parses but doesn't list it" case is constructed here, matching what a FIFO rendezvous can deterministically drive); the card's status badge / body layout.
- **Platform coverage:** mac+linux (`mkfifo` / POSIX named pipes; the L2 tier is already Unix-only per CLAUDE.md rule 2).


### Experimental feature flag (PRD #139)

#### features/gating

##### features/gating/001 — Dashboard rendered with the experimental flag forced ON shows the `experimental: on` footer.
- **Layer:** L1 (ratatui `TestBackend` + `insta`).
- **Agent:** none.
- **Asserts:** `render_experimental_footer_to_buffer(&Features::test_with(true), 80, 1)` renders a buffer containing the exact label `experimental: on`; the stringified buffer matches the committed snapshot.
- **Does not assert:** the footer's absolute placement within the full dashboard layout (the seam renders the standalone footer region); colour/style of the label.
- **Platform coverage:** mac+linux+windows.

##### features/gating/002 — Dashboard rendered with the experimental flag forced OFF shows NO footer (blank pre-feature baseline).
- **Layer:** L1 (ratatui `TestBackend` + `insta`).
- **Agent:** none.
- **Asserts:** `render_experimental_footer_to_buffer(&Features::test_with(false), 80, 1)` renders a buffer containing no `experimental` text; the stringified buffer matches the committed blank-baseline snapshot — identical to how the region looked before the surface existed.
- **Does not assert:** the ON path (covered by `features/gating/001`); any behavioural difference beyond the rendered footer region.
- **Platform coverage:** mac+linux+windows.

##### features/gating/003 — `DOT_AGENT_DECK_EXPERIMENTAL=1` surfaces the `experimental: on` footer end-to-end; the default (OFF) hides it.
- **Layer:** L2 (real TUI driven via PTY; observed on the rendered vt100 grid). The flag is injected through the spawned binary's env (`with_env("DOT_AGENT_DECK_EXPERIMENTAL", "1")`); a control launch sets no env var. The harness `env_clear`s the child env, so the control run is a clean OFF.
- **Agent:** none (`minimal` fixture; empty dashboard).
- **Asserts:** with the env var set, the rendered grid shows the `experimental: on` footer once the dashboard is up; the control launch (no env var) never shows it once the dashboard is up and quiescent.
- **Does not assert:** the TOML-file enable path or env-vs-file precedence (covered by `features/reload/001` and the unit suite); the footer's absolute grid coordinates.
- **Platform coverage:** mac+linux.

#### features/reload

##### features/reload/001 — A live `[features]` flip from OFF to ON re-surfaces the footer on the next render, no restart.
- **Layer:** L1 (in-process `TestBackend` + a synthetic config-file event; PRD #139 M2.2).
- **Agent:** none.
- **Asserts:** starting from a shared `Features` value (M1.2's per-process `Arc<RwLock<Features>>`) with `experimental = false`, the wrapper `features::show_experimental_footer()` reports hidden and the rendered footer is absent; after a synthetic `.dot-agent-deck.toml` change flips `experimental -> true` (modeled via `features::set_for_test(..)`), the wrapper re-evaluates to visible and the next render shows the `experimental: on` footer — with no process restart.
- **Does not assert:** the real file-watcher / debounce mechanics (the synthetic event stands in for the watcher's apply step); env-override precedence; partial/invalid-TOML reload handling (unit-covered).
- **Platform coverage:** mac+linux+windows.

#### features/config

##### features/config/001 — The experimental flag resolves against the PROJECT directory's `.dot-agent-deck.toml`, not the process's own (nested) launch cwd (fork issue #303).
- **Layer:** L2 (real TUI driven via PTY; observed on the rendered vt100 grid). Fixture `features-project-dir` carries `[features] experimental = true` at its root; the harness launches the binary from `nested/launch/dir` INSIDE that root (`with_launch_subdir`), not the root itself. `DOT_AGENT_DECK_EXPERIMENTAL` is left unset so only the file-resolution path is under test.
- **Agent:** none (no modes/orchestrations; empty dashboard).
- **Asserts:** once the dashboard is up (`No active sessions`), the rendered grid shows the `experimental: on` footer — proving `features_config_path()` found the project root's config rather than resolving (or failing to resolve) one relative to the process's own nested cwd.
- **Does not assert:** the `DOT_AGENT_DECK_FEATURES_CONFIG` override precedence or the exact resolved path (unit-covered in `src/config.rs`); the OFF/default case (covered by `features/gating/002`/`003`); directory-walk depth limits.
- **Platform coverage:** mac+linux.

#### features/status

These entries cover fork issue #303 Phase 2: `dot-agent-deck features status`, an on-demand diagnostic that reuses `features_config_path()` / `load_features_file()` / `resolve_features()` verbatim (no reimplementation) so it can never disagree with what the deck actually does.

##### features/status/001 — `dot-agent-deck features status` names the env override as the winning source and reports the resolved value as ON (fork issue #303).
- **Layer:** L2 (thin real-binary subprocess spawn; no PTY drive). An isolated tempdir with no `.dot-agent-deck.toml` in its ancestry is the process cwd; `DOT_AGENT_DECK_EXPERIMENTAL=1` is set.
- **Agent:** none.
- **Asserts:** stdout names `DOT_AGENT_DECK_EXPERIMENTAL env override` as the winning source and reports `experimental: on`.
- **Does not assert:** the resolved config path's existence or naming (covered by `features/status/002`–`003`); the `DOT_AGENT_DECK_FEATURES_CONFIG` path-override axis.
- **Platform coverage:** mac+linux.

##### features/status/002 — `dot-agent-deck features status` finds a project `.dot-agent-deck.toml`, names it as the winning source, and reports the resolved value as ON (fork issue #303).
- **Layer:** L2 (thin real-binary subprocess spawn; no PTY drive). An isolated tempdir carrying `[features] experimental = true` is the process cwd; no env override is set.
- **Agent:** none.
- **Asserts:** stdout reports `config path exists: true`, names `(project file)` as the winning source, and reports `experimental: on`.
- **Does not assert:** the env-override case (`features/status/001`); the no-config-found case (`features/status/003`); the ancestor-walk mechanism itself (unit-covered in `src/config.rs`, and `features/config/001`).
- **Platform coverage:** mac+linux.

##### features/status/003 — `dot-agent-deck features status` reports the no-config-found default when no `.dot-agent-deck.toml` exists anywhere in the process cwd's ancestry, with no env override (fork issue #303 — the exact silent-failure state the issue was filed against, now visible on demand).
- **Layer:** L2 (thin real-binary subprocess spawn; no PTY drive). An isolated, empty tempdir with no `.dot-agent-deck.toml` in its ancestry is the process cwd; no env override is set.
- **Agent:** none.
- **Asserts:** stdout reports `config path exists: false`, names `default (no .dot-agent-deck.toml found)` as the source, and reports `experimental: off`.
- **Does not assert:** the env-override or project-file cases (`features/status/001`–`002`); the `DOT_AGENT_DECK_FEATURES_CONFIG` path-override axis (`features/status/004`).
- **Platform coverage:** mac+linux.

##### features/status/004 — `dot-agent-deck features status` names `DOT_AGENT_DECK_FEATURES_CONFIG` (not the ancestor walk) as the path source and the override target (not "project file") as the value source, when the override is set to an existing file (fork #303/#349 review — reviewer F4/auditor L2: the override axis had zero coverage at any tier).
- **Layer:** L2 (thin real-binary subprocess spawn; no PTY drive). Two isolated tempdirs: the process cwd (no `.dot-agent-deck.toml` in its own ancestry) and a separate directory holding `override.toml` (`[features] experimental = true`), pointed at via `DOT_AGENT_DECK_FEATURES_CONFIG`.
- **Agent:** none.
- **Asserts:** stdout names `DOT_AGENT_DECK_FEATURES_CONFIG override` as the path source, `(override target)` — distinct from `(project file)` — as the value source, and `experimental: on`.
- **Does not assert:** the no-override cases (`features/status/001`–`003`); an override pointed at a missing/malformed/non-regular target (unit-covered via `describe_features_file` in `src/config.rs`).
- **Platform coverage:** mac+linux.

#### features/startup-warning

These entries cover fork issue #303 Phase 2's other diagnosability surface: a startup warning on stderr, conditional on the ancestor walk finding no `.dot-agent-deck.toml` anywhere, requiring neither `DOT_AGENT_DECK_LOG` nor a restart to see — replacing the pre-fix behavior where the only signal was a `tracing::warn!` gated behind file logging.

##### features/startup-warning/001 — The deck's TUI startup path prints a missing-config warning to stderr when no `.dot-agent-deck.toml` exists anywhere in the process cwd's ancestry, with no `DOT_AGENT_DECK_LOG` set (fork issue #303).
- **Layer:** L2 (thin real-binary subprocess spawn; no PTY drive — driven through `init_and_watch` and the daemon handshake via `DOT_AGENT_DECK_EXIT_AFTER_HANDSHAKE`, mirroring `lifecycle/handshake/004`'s non-PTY drive). An isolated tempdir with no `.dot-agent-deck.toml` in its ancestry is both the process cwd and `HOME`; `DOT_AGENT_DECK_LOG` is left unset.
- **Agent:** none.
- **Asserts:** the process exits 0 and stderr contains the missing-config warning naming `.dot-agent-deck.toml` and `experimental flags default to OFF`.
- **Does not assert:** the silent case when a config is present (`features/startup-warning/002`); the exact wording beyond the required substrings; the `features status` subcommand's own output (`features/status/00N`).
- **Platform coverage:** mac+linux.

##### features/startup-warning/002 — The deck's TUI startup path is completely silent on stderr when a `.dot-agent-deck.toml` is present at the launch directory (fork issue #303 — proportionate: no warning for the common case).
- **Layer:** L2 (thin real-binary subprocess spawn; no PTY drive — same `DOT_AGENT_DECK_EXIT_AFTER_HANDSHAKE` drive as `features/startup-warning/001`). An isolated tempdir carrying `[features] experimental = false` is both the process cwd and `HOME`.
- **Agent:** none.
- **Asserts:** the process exits 0 and stderr contains no missing-config warning.
- **Does not assert:** the missing-config case (`features/startup-warning/001`); any behavior of the `experimental` value itself (covered elsewhere in this section); the `DOT_AGENT_DECK_FEATURES_CONFIG` override axis (`features/startup-warning/003`).
- **Platform coverage:** mac+linux.

##### features/startup-warning/003 — The deck's TUI startup path is completely silent on stderr when `DOT_AGENT_DECK_FEATURES_CONFIG` is set, even to a target that does not exist, and no `.dot-agent-deck.toml` exists in the process cwd's own ancestry either (fork #303/#349 review — reviewer F4/auditor L2: `missing_config_warning`'s override branch had zero coverage at any tier, and this is the distinction the design singled out — an operator-supplied override pointing at a missing file is a different problem from nobody having configured one).
- **Layer:** L2 (thin real-binary subprocess spawn; no PTY drive — same `DOT_AGENT_DECK_EXIT_AFTER_HANDSHAKE` drive as `features/startup-warning/001`–`002`). An isolated, empty tempdir with no `.dot-agent-deck.toml` in its ancestry is both the process cwd and `HOME`; `DOT_AGENT_DECK_FEATURES_CONFIG` is set to a sibling path inside that same tempdir that is never created.
- **Agent:** none.
- **Asserts:** the process exits 0 and stderr contains no missing-config warning.
- **Does not assert:** the no-override cases (`features/startup-warning/001`–`002`); an override pointed at an existing-but-unusable target (malformed TOML, non-regular, oversized — unit-covered via `describe_features_file` in `src/config.rs`, which backs the `features status` labeling for the same outcomes).
- **Platform coverage:** mac+linux.

### Docs cross-reference skips

Per Decision 27, documented user-facing behaviors that are deliberately not catalogued at M1:

| Doc behavior | Why skipped |
|---|---|
| Idle ASCII art rendering on cards ([docs/configuration.md#idle-ascii-art](../docs/configuration.md), [docs/configuration.md#standalone-cli](../docs/configuration.md)) | LLM-driven side feature; lives outside the deck/daemon/PTY surface the harness covers. Reconsider in M4+ if the feature warrants its own catalog section. |
| `dot-agent-deck connect <remote>` end-to-end SSH flow ([docs/remote-environments.md](../docs/remote-environments.md), [docs/remote-recipes.md](../docs/remote-recipes.md)) | Requires a remote-harness shape that does not exist yet. Catalogued at M4+ when remote testing lands. Local quit-dialog coverage (`prompt/quit/001`–`005`) already pins the Detach / Stop / Cancel behavior; remote attach adds only the daemon-side log distinction. |
| `dot-agent-deck remote add / list / upgrade / remove` ([docs/remote-environments.md](../docs/remote-environments.md)) | Same — remote-harness territory; the lib already covers the pure-data slices (URL parsing, command construction, error classification) in the kept tests. **Security properties deferred to M4+ end-to-end coverage:** shell-metacharacter quoting on remote-CLI argv assembly (unit-covered by `system_ssh_executor_quotes_arguments_safely`), `remotes.toml` written at mode 0o600 (covered by the now-moved `remotes_toml_written_at_0o600` test — restore at M4+), `DOT_AGENT_DECK_VIA_DAEMON=1` propagation on the remote shell (unit-covered by `build_connect_command_has_t_flag_and_via_daemon_env`). |
| `dot-agent-deck ascii` CLI subcommand ([docs/configuration.md#standalone-cli](../docs/configuration.md)) | Non-TUI subcommand; tested as a CLI smoke in M4+ if it warrants coverage. |
| `dot-agent-deck validate` CLI subcommand ([docs/workspace-modes.md#config-validation](../docs/workspace-modes.md)) | Non-TUI; the underlying validator is exhaustively covered by the pure-data `config_validation` tests. |
| `dot-agent-deck watch` CLI subcommand ([docs/workspace-modes.md#dot-agent-deck-watch](../docs/workspace-modes.md)) | Non-TUI subcommand; an L2 test would only exercise its output formatting against a real shell — low value compared to the deck-rendering surface. |
| `dot-agent-deck config get` / `config set` ([docs/configuration.md](../docs/configuration.md)) | Non-TUI; the underlying config field reflection is covered by pure-data tests (`*_get_set_field`, `*_get_set_fields`). |
| `dot-agent-deck hooks install` / `uninstall` CLI commands ([docs/troubleshooting.md#hooks](../docs/troubleshooting.md)) | Auto-install path is catalogued as `hooks/install/001`–`003`; the explicit subcommand variants share the same install/uninstall code. A targeted L2 test will be added only if a divergence appears. |
| Ghostty-specific Shift+Enter terminal config ([docs/troubleshooting.md#shiftenter-submits-instead-of-inserting-a-newline](../docs/troubleshooting.md)) | **No longer a skip** — PRD #227 showed the break was deck-side (`keyevent_to_bytes` collapsed `Enter + SHIFT` to a bare CR), so there IS a deck-side surface: it is now covered by `embed/key-forwarding/001`. Only the outer-terminal *configuration* itself (what a user types into `ghostty/config`) remains untestable here. |
| Mode-tab card jump via `Enter` (broken per docs note → [#68](https://github.com/vfarcic/dot-agent-deck/issues/68)) | Documented as broken. The catalog will gain an entry once the bug is closed; until then leaving it uncovered avoids pinning the broken behavior. |
| `--continue` "dashboard-first landing" detail ([docs/session-management.md#resuming-sessions](../docs/session-management.md)) | Implicit consequence of `session/restore/001`; not separately worth a catalog ID. Reconsider if the landing-tab logic ever has its own surface. |
