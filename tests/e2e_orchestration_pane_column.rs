#![cfg(feature = "e2e")]

//! L2 end-to-end coverage for the Orchestration tab's pane column.
//!
//! - PRD #336/#361/#387: spawns the real `dot-agent-deck` binary against the
//!   `orch-deck` fixture and drives the Ctrl+l chord through the PTY,
//!   asserting on the rendered vt100 grid's column geometry as the deck-global
//!   sidebar/pane-column split cycles through its three stages (Default,
//!   Narrow, Hidden), and that the chord forwards to the PTY when it isn't
//!   claimed.
//! - PRD #311: `PaneLayout::Stacked` pane column coverage — removing the
//!   non-focused roles' collapsed 1-row title frames must not touch agent
//!   lifecycle. Every role's PTY stays open, keeps running, and keeps
//!   reporting status regardless of whether its pane is currently drawn.
//!
//! Decision 6: gated behind the `e2e` feature so `cargo test-fast` never
//! compiles it.

mod common;

use std::time::Duration;

use common::TuiDeck;
use spec::spec;

/// Drive the new-pane dialog to open the (single) orchestration in the
/// `orch-deck` fixture. Mirrors `e2e_dashboard_selection.rs::open_orchestration`
/// — with no `[[modes]]` defined the Mode chip row is `[No mode] [Orch: …]
/// [schedule]`, so ONE Right selects the orchestration; selecting an
/// orchestration hides the Command field, so a second Enter submits the form.
fn open_orchestration(deck: &TuiDeck) {
    deck.send_keys(b"\x0e"); // Ctrl+n -> directory picker
    deck.send_keys(b" "); // Space -> confirm current dir -> new-pane form
    deck.wait_for_string("No mode"); // form up, Mode field focused at "No mode"
    deck.send_keys(b"\x1b[C"); // Right -> [Orch: demo-orch]
    deck.send_keys(b"\r"); // Mode -> Name
    deck.send_keys(b"\r"); // submit (Command hidden for an orchestration)
}

/// Scenario: Open two real orchestration tabs (120-col PTY, tab A then tab
/// B) and Ctrl+l cycle through Default (34/66) -> Narrow (25/75) -> Hidden
/// (sidebar collapsed) -> Default, confirming the pane column's left-edge
/// boundary at each stage. PRD #387 decision 2: the split stage is ONE
/// deck-global value, not per-tab — tab B opens AT tab A's current (Narrow)
/// stage rather than resetting to its own default, and cycling on tab B
/// moves tab A too (and vice versa), proving "toggle anywhere, changes
/// everywhere" against the real render loop. PRD #387 M1 scopes Ctrl+l to
/// command mode on every tab type (not just Dashboard), and opening an
/// orchestration tab always lands the deck in PaneInput mode focused on its
/// start-role pane — so Ctrl+D precedes each toggle press that follows a
/// tab open, entering Normal mode first exactly as a real user now must.
/// PRD #387 M5: at tab B's Hidden stage, also assert directly that NEITHER
/// role's sidebar card marker is present on the grid — proving Hidden
/// genuinely renders no sidebar content, not merely that the pane column's
/// left edge reached column 0.
#[spec("tabs/orchestration/006")]
#[test]
fn orchestration_006_ctrl_l_cycles_pane_column_split_stages() {
    let deck = TuiDeck::builder()
        .with_pty_size(120, 40)
        .launch_with_fixture("orch-deck");
    deck.wait_for_string("No active sessions");

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent"); // new-pane form closed -> tab A is up

    // Baseline: the default 34/66 split puts tab A's pane-column left edge at
    // 34% of the 120-col frame (col 40 or 41, depending on Percentage
    // rounding).
    let default_edge = pane_column_left_edge(&deck.snapshot_grid());
    assert!(
        (40..=41).contains(&default_edge),
        "expected tab A's default 34/66 split's pane-column edge near col \
         40/41, got {default_edge}\nGrid:\n{}",
        deck.snapshot_grid()
    );

    // Ctrl+l on tab A: Default -> Narrow (25/75, edge ~col 30).
    // `open_orchestration` left the deck in PaneInput mode, focused on tab
    // A's start-role pane — PRD #387 M1 scopes Ctrl+l to command mode on
    // every tab type, so Ctrl+D must enter Normal mode first or the byte
    // forwards straight to the pane instead of cycling the split.
    deck.send_bytes(b"\x04"); // Ctrl+D -> Normal mode
    deck.send_bytes(b"\x0c"); // Ctrl+l == 0x0c
    let a_narrowed = deck.wait_for_grid_predicate_within(Duration::from_secs(3), |grid| {
        (29..=30).contains(&pane_column_left_edge(grid))
    });
    assert!(
        a_narrowed,
        "Ctrl+l did not narrow tab A's sidebar to the 25/75 split within 3s \
         — pane-column edge stayed at {}\nGrid:\n{}",
        pane_column_left_edge(&deck.snapshot_grid()),
        deck.snapshot_grid()
    );

    // Open a SECOND orchestration tab (tab B) in the same directory. PRD
    // #387 decision 2: a fresh tab now ADOPTS the current deck-global stage
    // — tab B must open already-Narrow, matching tab A's toggled stage, not
    // its own untoggled Default.
    open_orchestration(&deck);
    deck.wait_for_absence("New Agent"); // new-pane form closed -> tab B is up

    let b_narrow_edge = pane_column_left_edge(&deck.snapshot_grid());
    assert!(
        (29..=30).contains(&b_narrow_edge),
        "a brand-new orchestration tab B must open AT the deck-global Narrow \
         stage tab A was just toggled to, not its own untoggled Default, \
         got {b_narrow_edge}\nGrid:\n{}",
        deck.snapshot_grid()
    );

    // Continue the SHARED cycle from tab B: Narrow -> Hidden. Opening tab B
    // re-lands the deck in PaneInput mode on ITS OWN start-role pane,
    // regardless of tab A having left the deck in Normal mode above
    // (SpawnPane's orchestration arm sets `ui.mode = UiMode::PaneInput`
    // unconditionally), so Ctrl+D is required again here.
    deck.send_bytes(b"\x04"); // Ctrl+D -> Normal mode
    deck.send_bytes(b"\x0c");
    let b_hidden = deck.wait_for_grid_predicate_within(Duration::from_secs(3), |grid| {
        pane_column_left_edge(grid) == 0
    });
    assert!(
        b_hidden,
        "Ctrl+l did not collapse tab B's sidebar to the Hidden stage within \
         3s — pane-column edge stayed at {}\nGrid:\n{}",
        pane_column_left_edge(&deck.snapshot_grid()),
        deck.snapshot_grid()
    );

    // PRD #387 M5: `pane_column_left_edge(grid) == 0` above proves the pane
    // column's own box starts at column 0, which is necessary but not
    // sufficient — it does not rule out sidebar content still being drawn
    // (e.g. a stray card fragment) that this substring search never looks
    // for. Directly assert NEITHER role's sidebar card marker
    // (`" <role> "`, the same bounded needle `has_role_status` below uses to
    // find a sidebar deck card's title row) is present anywhere on the
    // settled grid, so Hidden is proven to render NO sidebar at all.
    let hidden_grid = deck.snapshot_grid();
    assert!(
        !hidden_grid.contains(" orchestrator ") && !hidden_grid.contains(" worker "),
        "Hidden must render NO sidebar content at all — found a sidebar \
         role-card marker on the grid even though the pane column's left \
         edge reported column 0\nGrid:\n{hidden_grid}"
    );

    // Switch back to tab A (Shift+Tab -> previous tab). Shared-stage proof
    // (PRD #387 decision 2): tab A must NOW ALSO read Hidden — toggling on
    // tab B moved the SAME deck-global stage tab A reads, even though tab A
    // itself was never touched after its own Default -> Narrow press above.
    // The deck is ALREADY in Normal mode here (from the Ctrl+D sent before
    // tab B's toggle above), which `cycle_tab_action` requires for
    // Shift+Tab to switch tabs rather than forward the bytes to the pane —
    // so no further Ctrl+D is needed. Sending one anyway would be actively
    // harmful: Ctrl+D TOGGLES, and since tab B's start-role pane is still
    // the deck's resume target from Normal mode, a second press here would
    // re-enter PaneInput on tab B and break the Shift+Tab below.
    deck.send_bytes(b"\x1b[Z"); // Shift+Tab -> previous tab -> tab A
    let a_also_hidden = deck.wait_for_grid_predicate_within(Duration::from_secs(3), |grid| {
        pane_column_left_edge(grid) == 0
    });
    assert!(
        a_also_hidden,
        "toggling tab B's split must move tab A's split too — expected tab \
         A ALSO Hidden (edge 0) after switching back, got {}\nGrid:\n{}",
        pane_column_left_edge(&deck.snapshot_grid()),
        deck.snapshot_grid()
    );

    // Finish the shared cycle from tab A: Hidden -> Default. No Ctrl+D
    // needed: switching tabs (`switch_tab_with_focus`) never touches
    // `UiMode`, so the deck is still in Normal mode from above.
    deck.send_bytes(b"\x0c");
    let a_restored = deck.wait_for_grid_predicate_within(Duration::from_secs(3), |grid| {
        (40..=41).contains(&pane_column_left_edge(grid))
    });
    assert!(
        a_restored,
        "Ctrl+l did not restore tab A's 34/66 default split within 3s — \
         pane-column edge stayed at {}\nGrid:\n{}",
        pane_column_left_edge(&deck.snapshot_grid()),
        deck.snapshot_grid()
    );

    // Switch to tab B one last time (Right -> CycleTabNext, the established
    // forward tab-cycle chord — see `e2e_dashboard_selection.rs`'s SC1
    // round-trip) and confirm IT is ALSO back at Default — completing the
    // loop in both directions: A's final toggle propagated to B just as B's
    // earlier toggles propagated to A.
    deck.send_bytes(b"\x1b[C"); // Right -> next tab -> tab B
    let b_also_restored = deck.wait_for_grid_predicate_within(Duration::from_secs(3), |grid| {
        (40..=41).contains(&pane_column_left_edge(grid))
    });
    assert!(
        b_also_restored,
        "toggling tab A's split back to Default must move tab B's split \
         too — expected tab B ALSO Default (edge ~col 40/41) after \
         switching to it, got {}\nGrid:\n{}",
        pane_column_left_edge(&deck.snapshot_grid()),
        deck.snapshot_grid()
    );
}

/// Scenario: Launch a real interactive Bash/readline pane on the Dashboard (a
/// NON-orchestration tab) with the `minimal` fixture, print a unique sentinel
/// line, then press Ctrl+l. Bash's readline binds Ctrl+l to `clear-screen`, so
/// if the byte reaches the PTY the terminal clears and the sentinel line
/// disappears from the rendered grid. The PRD #336 keybinding (`global_action`,
/// src/ui.rs) must claim Ctrl+l as `Action::ToggleOrchestrationSplit` ONLY on an
/// orchestration tab; today it claims Ctrl+l unconditionally (no tab-context
/// check), so on this Dashboard pane the keystroke never reaches the shell —
/// `dispatch_action`'s handler no-ops outside `Tab::Orchestration` — and the
/// sentinel line survives. RED today: the wait for the sentinel to disappear
/// times out because Ctrl+l is swallowed instead of forwarded.
#[spec("tabs/orchestration/007")]
#[test]
fn orchestration_007_ctrl_l_forwards_to_pty_on_non_orchestration_tab() {
    const SENTINEL: &str = "CTRLL_FWD_SENTINEL_9f3c";

    let deck = TuiDeck::builder()
        .with_continue_session(
            "ctrl-l-dashboard-shell",
            "env PS1='CTRLL> ' bash --noprofile --norc -i",
        )
        .launch_with_fixture("minimal");
    deck.wait_for_string("[Command Mode Ctrl+D]"); // live PTY, PaneInput mode
    deck.wait_for_string("CTRLL>");

    deck.send_keys(format!("echo {SENTINEL}\r").as_bytes());
    deck.wait_for_string(SENTINEL);

    deck.send_bytes(b"\x0c"); // Ctrl+l == 0x0c
    let cleared = deck
        .wait_for_grid_predicate_within(Duration::from_secs(3), |grid| !grid.contains(SENTINEL));
    assert!(
        cleared,
        "Ctrl+l did not reach the shell pane on a non-orchestration tab — \
         readline's clear-screen never ran, so the sentinel line is still \
         visible after 3s. The global keybinding resolver claimed Ctrl+l as \
         Action::ToggleOrchestrationSplit even though the active tab is not \
         an orchestration tab (PRD #336 scope violation).\nGrid:\n{}",
        deck.snapshot_grid()
    );
}

/// Scenario: Open a real orchestration tab from the `orch-bash-role`
/// fixture, whose orchestrator (start) role runs an interactive bash/
/// readline shell instead of a `cat` stub. `open_orchestration` lands the
/// deck in `PaneInput` mode with that role pane already focused (mirroring
/// `e2e_orchestration_lock.rs::open_orchestration`'s documented landing
/// state) — no unlock is needed because the lock never gates the
/// orchestrator's own pane. Print a unique sentinel, then press Ctrl+l.
/// Bash's readline binds Ctrl+l to `clear-screen`, so if the raw byte
/// reaches the PTY the terminal clears and the sentinel disappears from the
/// rendered grid. RED today (PRD #387 Defect 1): the inline `claims_ctrl_l`
/// match (`src/ui.rs`, ~9077-9086) claims Ctrl+l as
/// `Action::CycleSplitStage` on EVERY orchestration tab regardless of UI
/// mode, so the byte never reaches the focused role pane and the sentinel
/// survives.
#[spec("tabs/orchestration/024")]
#[test]
fn orchestration_024_ctrl_l_forwards_to_pty_on_focused_orchestration_role_pane() {
    const SENTINEL: &str = "CTRLL_ORCH_ROLE_FWD_SENTINEL_6d2a";

    let deck = TuiDeck::builder()
        .with_pty_size(120, 40)
        .launch_with_fixture("orch-bash-role");
    deck.wait_for_string("No active sessions");

    open_orchestration(&deck);
    deck.wait_for_absence("New Agent"); // new-pane form closed -> tab up, orchestrator focused
    deck.wait_for_string("[Command Mode Ctrl+D]"); // live PTY, PaneInput mode
    deck.wait_for_string("CTRLL>");

    deck.send_keys(format!("echo {SENTINEL}\r").as_bytes());
    deck.wait_for_string(SENTINEL);

    deck.send_bytes(b"\x0c"); // Ctrl+l == 0x0c
    let cleared = deck
        .wait_for_grid_predicate_within(Duration::from_secs(3), |grid| !grid.contains(SENTINEL));
    assert!(
        cleared,
        "Ctrl+l did not reach the focused orchestration role pane's PTY — \
         readline's clear-screen never ran, so the sentinel line is still \
         visible after 3s. The global keybinding resolver claimed Ctrl+l as \
         Action::CycleSplitStage even though a role pane was focused in \
         PaneInput mode — orchestration tabs claim Ctrl+l mode-independently \
         (PRD #387 Defect 1).\nGrid:\n{}",
        deck.snapshot_grid()
    );
}

/// A collapsed `Stacked` pane renders a `Block` with `Borders::TOP` and
/// `.title(format!(" {title} "))` — no other cell content. On the settled
/// grid that row, after trimming ONLY the leading blank columns (the sidebar
/// area to its left), reads exactly `"<role> ─..."` — the title text directly
/// followed by the border-fill dashes, nothing else. A sidebar deck card's
/// title line ("┌ N role ──── ● status ┐") can never match: after trimming
/// leading whitespace it starts with the card's own border glyph
/// (`┌`/`┏`/`│`), not the bare role name. This is what makes the check
/// specific to the collapsed PANE-COLUMN frame rather than any other on-screen
/// occurrence of the role name.
fn has_collapsed_frame(grid: &str, role: &str) -> bool {
    let prefix = format!("{role} \u{2500}");
    grid.lines()
        .any(|line| line.trim_start().starts_with(&prefix))
}

/// A sidebar deck card's title row (`render_session_card` in `src/ui.rs`)
/// carries the role's display name and its live status word on the SAME
/// rendered line: the identity segment (` {num} {name} ` — no agent-type
/// badge or `\u{00b7}` separator since the fork-only `349e895` removed both)
/// sits left-aligned in the card's top border, the status segment
/// (` {dot} {status} `) right-aligned on the same `Block::title`. The
/// identity segment always carries a space immediately before AND after the
/// role name (the numeric shortcut prefix ends in one, `identity_text` ends
/// in the other), so `" {role} "` is a safe bounded needle. Search for it
/// plus `status` together on one line so the check is scoped to `role`'s own
/// card rather than any occurrence of `status` anywhere on the settled grid
/// (e.g. another role's card, or pane content).
fn has_role_status(grid: &str, role: &str, status: &str) -> bool {
    let role_needle = format!(" {role} ");
    grid.lines()
        .any(|line| line.contains(&role_needle) && line.contains(status))
}

/// Overwrite the fixture's `beta-agent.sh` placeholder with the ABSOLUTE path
/// of the freshly built test binary baked in (mirrors `write_card_agent` in
/// `e2e_dashboard_selection.rs`), rather than relying on `dot-agent-deck`
/// resolving correctly on PATH — a dev machine may have a separately
/// installed `dot-agent-deck` shadowing the build under test.
fn write_beta_agent(deck: &TuiDeck) {
    let bin = env!("CARGO_BIN_EXE_dot-agent-deck");
    let body = format!(
        "#!/bin/sh\n\
         printf 'BETA_ROLE_SENTINEL\\n'\n\
         {session_start}\
         sleep 1\n\
         {pre_tool_use}\
         sleep 600\n",
        session_start = common::claude_session_start_line(bin, "beta-sess"),
        pre_tool_use = common::claude_hook_line(
            bin,
            r#"{"hook_event_name":"PreToolUse","session_id":"beta-sess","tool_name":"Bash"}"#,
        ),
    );
    let path = deck.workdir().join("beta-agent.sh");
    std::fs::write(&path, body).expect("overwrite beta-agent.sh with resolved binary path");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod beta-agent.sh");
    }
}

/// Drive the new-pane dialog to open the (single) orchestration in the
/// `orch-focus-lifecycle` fixture. With no `[[modes]]` defined the Mode chip
/// row is `[No mode] [Orch: focus-lifecycle] [schedule]`, so ONE Right
/// selects the orchestration; selecting an orchestration hides the Command
/// field, so a second Enter submits the form.
fn open_focus_lifecycle_orchestration(deck: &TuiDeck) {
    deck.send_bytes(b"\x0e"); // Ctrl+n -> directory picker
    deck.send_bytes(b" "); // Space -> confirm current dir -> new-pane form
    deck.wait_for_string("No mode");
    deck.send_bytes(b"\x1b[C"); // Right -> [Orch: focus-lifecycle]
    deck.send_bytes(b"\r"); // Mode -> Name
    deck.send_bytes(b"\r"); // submit (Command hidden for an orchestration)
}

/// Scenario: Open the `orch-focus-lifecycle` fixture's 3-role orchestration
/// (`orchestrator` start role, plus `alpha` and `beta`) in the deck's default
/// `PaneLayout::Stacked`. (b) Confirm a non-focused role keeps running and its
/// sidebar status transitions live: `beta`'s status card goes from idle to
/// `Working` purely through its own self-posted hook events while its pane is
/// NOT the expanded/focused slot. (a) Assert the settled grid carries NO
/// collapsed title-bar frame for either non-focused role (`alpha`, `beta`) —
/// PRD #311 removes that arm of `PaneLayout::Stacked` entirely. (c) Drive `j`
/// twice (Normal mode) to move the deck's focus orchestrator -> alpha -> beta,
/// then `k` twice back to orchestrator, asserting each role's own sentinel
/// text is visible once it becomes the focused/expanded pane — proving no lost
/// scrollback or stale fragment survives a focus round trip. RED today: (a)
/// fails because `render_terminal_panes`' Stacked else-arm
/// (`src/ui.rs:11890-11908`) still draws a `Borders::TOP` titled block for
/// every non-focused role.
#[spec("tabs/orchestration/008")]
#[test]
fn orchestration_008_stacked_pane_column_hides_collapsed_frames_while_agents_stay_live() {
    let deck = TuiDeck::builder()
        .with_pty_size(160, 45)
        .launch_with_fixture("orch-focus-lifecycle");
    write_beta_agent(&deck);
    deck.wait_for_string("No active sessions");
    open_focus_lifecycle_orchestration(&deck);
    deck.wait_for_string("orchestrator");
    deck.wait_for_string("alpha");
    deck.wait_for_string("beta");

    // (b) The non-focused `beta` role keeps running and its sidebar status
    // transitions live (Idle -> Working) purely from its own self-posted hook
    // events, while its pane is not the expanded/focused slot.
    assert!(
        common::wait_until(Duration::from_secs(15), || {
            has_role_status(&deck.snapshot_grid(), "beta", "Working")
        }),
        "the non-focused beta role's sidebar status never transitioned to \
         Working while its pane was collapsed/not drawn:\n{}",
        deck.snapshot_grid()
    );

    // (a) With `orchestrator` focused/expanded (the start role), neither
    // non-focused role may render a collapsed title-bar frame.
    let grid = deck.snapshot_grid();
    assert!(
        !has_collapsed_frame(&grid, "alpha"),
        "non-focused role 'alpha' must not render a collapsed title-bar frame \
         in PaneLayout::Stacked:\n{grid}"
    );
    assert!(
        !has_collapsed_frame(&grid, "beta"),
        "non-focused role 'beta' must not render a collapsed title-bar frame \
         in PaneLayout::Stacked:\n{grid}"
    );

    // (c) Switching focus between roles preserves each agent's rendered
    // content, with no lost scrollback / stale fragment across the round
    // trip: orchestrator -> alpha -> beta -> alpha -> orchestrator.
    deck.send_bytes(b"\x04"); // Ctrl+D -> Normal mode
    deck.send_bytes(b"j"); // orchestrator -> alpha
    deck.wait_for_string("ALPHA_ROLE_SENTINEL");
    deck.send_bytes(b"j"); // alpha -> beta
    deck.wait_for_string("BETA_ROLE_SENTINEL");
    deck.send_bytes(b"k"); // beta -> alpha
    deck.wait_for_string("ALPHA_ROLE_SENTINEL");
    deck.send_bytes(b"k"); // alpha -> orchestrator
    deck.wait_for_string("ORCH_ROLE_SENTINEL");
}

// ---------------------------------------------------------------------------
// PRD #336 — `Ctrl+l` toggles the orchestration sidebar/pane-column split.
// ---------------------------------------------------------------------------

/// Column index of the orchestration tab's role-pane column's LEFT edge: the
/// role-pane box drawn for the fixture's `start = true` role ("orchestrator")
/// renders its title fused into the top border as `┌orchestrator───…`, so the
/// column of that `┌` is exactly `panes_area.x` — the boundary between the
/// sidebar (role list) and the pane column that `orchestration_split_percents`
/// controls. Distinct from the sidebar's own truncated `orchestrat…` card
/// label, so there is no collision risk.
///
/// The corner glyph is NOT fixed: PRD #341 ("make UI modes unmistakable")
/// renders the focused pane's border heavier in command mode, so the same box
/// reads `┌orchestrator` in PaneInput and `┏orchestrator` in command mode.
/// [`common::orchestration_pane_left_edge`] matches any weight — this helper is
/// about the box's COLUMN, not its styling, and pinning one glyph made the test
/// fail on a mode switch that was working correctly. That scan lives in the
/// harness rather than here because `e2e_idle_worker_detector.rs` needs the same
/// anchor to crop on, and two copies is one copy too many for a scan whose glyph
/// set has already had to change once (review of #465, S1).
fn orchestrator_box_edge(grid: &str) -> Option<u16> {
    common::orchestration_pane_left_edge(grid).map(|column| column as u16)
}

/// Panicking form of [`orchestrator_box_edge`], for use outside a predicate
/// where the box must exist.
fn pane_column_left_edge(grid: &str) -> u16 {
    orchestrator_box_edge(grid).unwrap_or_else(|| {
        panic!("orchestrator role-pane box top border not found in grid:\n{grid}")
    })
}

