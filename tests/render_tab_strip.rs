//! PRD #80 M3 — L1 widget test for the tab-strip close affordance.
//!
//! Per PRD #77 Decision 2 this is an in-process test driving the
//! production tab-strip renderer through `render_tab_bar_to_buffer` (a
//! `TestBackend` wrapper, mirroring `render_button_bar_to_buffer`). No
//! subprocess, no PTY. File-layout-mirrors-catalog (Decision 7): catalog
//! ID `mouse/tabstrip/002`'s presence/absence half lands here with a
//! function name `<sub-area>_<NNN>_<short_suffix>` (Decision 17). The
//! click→close behavior half lives in `tests/e2e_mouse_tabstrip.rs`.
//!
//! M3 contract: Mode and Orchestration tabs carry a clickable `[×]` close
//! affordance; the Dashboard tab (always index 0) carries NONE. The
//! `closeable` mask passed to the renderer encodes that — `false` for the
//! Dashboard tab, `true` for Mode/Orchestration tabs.

use dot_agent_deck::palette;
use dot_agent_deck::state::SessionStatus;
use dot_agent_deck::ui::render_tab_bar_to_buffer;
use ratatui::style::{Color, Modifier};
use spec::spec;

/// Count the `×` close glyphs in the rendered single-row tab strip.
fn close_glyph_count(buffer: &ratatui::buffer::Buffer) -> usize {
    let area = buffer.area();
    (0..area.width)
        .filter(|&x| buffer[(x, 0)].symbol() == "×")
        .count()
}

/// The foreground color of `label`'s first character in the rendered
/// single-row tab strip. Locates the `" {label} "` padded segment
/// `render_tab_strip` writes for every tab and samples the cell right after
/// the leading pad space, so it survives label reordering / width changes as
/// long as the label text itself is unique in the row.
fn tab_label_fg(buffer: &ratatui::buffer::Buffer, label: &str) -> Color {
    let area = buffer.area();
    let row: String = (0..area.width)
        .map(|x| buffer[(x, 0)].symbol().to_string())
        .collect();
    let needle = format!(" {label} ");
    let start = row
        .find(&needle)
        .unwrap_or_else(|| panic!("label {label:?} not found in rendered tab strip row: {row:?}"));
    // `start` is the leading pad space; the label's own first character sits
    // one column to the right of it.
    buffer[((start + 1) as u16, 0)].fg
}

/// The `Modifier` flags of `label`'s first character in the rendered
/// single-row tab strip. Mirrors `tab_label_fg`'s cell-location logic so the
/// two stay in lockstep; used to assert `REVERSED`/`BOLD` presence/absence,
/// which a plain `fg` check cannot express.
fn tab_label_modifier(buffer: &ratatui::buffer::Buffer, label: &str) -> Modifier {
    let area = buffer.area();
    let row: String = (0..area.width)
        .map(|x| buffer[(x, 0)].symbol().to_string())
        .collect();
    let needle = format!(" {label} ");
    let start = row
        .find(&needle)
        .unwrap_or_else(|| panic!("label {label:?} not found in rendered tab strip row: {row:?}"));
    buffer[((start + 1) as u16, 0)].modifier
}

/// Scenario: Render the tab strip twice. First with only the Dashboard tab
/// (`closeable = [false]`) — the strip must contain NO `×` close glyph,
/// proving the Dashboard tab has no close affordance. Then with Dashboard
/// plus a Mode tab and an Orchestration tab (`closeable = [false, true,
/// true]`) — exactly two `×` glyphs must render, one per closeable tab and
/// none for the Dashboard. RED until M3 renders the `[×]` affordance (today
/// `render_tab_strip` draws no close glyph at all).
#[spec("mouse/tabstrip/002")]
#[test]
fn tabstrip_002_close_glyph_on_mode_orchestration_not_dashboard() {
    // Dashboard alone: never closeable → no close glyph anywhere.
    let dashboard_only = render_tab_bar_to_buffer(&["Dashboard"], &[false], 0, 80, &[None]);
    assert_eq!(
        close_glyph_count(&dashboard_only),
        0,
        "Dashboard tab must render no [×] close affordance, got {:?}",
        dashboard_only_text(&dashboard_only)
    );

    // Dashboard + Mode + Orchestration: only the two non-Dashboard tabs get
    // a close glyph, so exactly two `×` render. A third would mean the
    // Dashboard wrongly gained one; zero means the affordance is missing.
    let three_tabs = render_tab_bar_to_buffer(
        &["Dashboard", "demo", "squad"],
        &[false, true, true],
        0,
        80,
        &[None, None, None],
    );
    assert_eq!(
        close_glyph_count(&three_tabs),
        2,
        "Mode and Orchestration tabs must each render a [×] (and the Dashboard none), got {:?}",
        dashboard_only_text(&three_tabs)
    );
}

/// Stringify the rendered row for assertion messages.
fn dashboard_only_text(buffer: &ratatui::buffer::Buffer) -> String {
    let area = buffer.area();
    (0..area.width)
        .map(|x| buffer[(x, 0)].symbol())
        .collect::<String>()
}

/// Scenario: Render the tab strip with a Dashboard tab plus an orchestration
/// tab whose panes carry a mix of `SessionStatus` values, and assert the
/// orchestration tab's label renders in the `palette::status_color()` of the
/// SINGLE highest-priority status among its panes — fixed order Error(Red) >
/// WaitingForInput(Yellow) > Working(Green) > Thinking/Compacting(Blue) >
/// Idle/Unknown(no tint) (PRD #333). Covers: (a) one `Error` among `Idle`
/// panes -> Red; (b) one `WaitingForInput` among `Working`/`Idle` (no Error)
/// -> Yellow; (c) all `Idle` -> the SAME base label color as an ordinary
/// tab, NOT `Color::DarkGray` (PRD #333 defect B: PRD #13 reserves DarkGray
/// for purely-decorative elements, not label text); (d) `Thinking` +
/// `Working` (no higher-priority state) -> Green, since Working outranks
/// Thinking. Also asserts a non-orchestration tab (`None`) never gets
/// colorized by this feature. RED today on (c): the Idle branch still
/// applies `.fg(palette::status_color(Idle))` (`DarkGray`) instead of
/// falling through to the base style.
#[spec("tabs/orchestration/009")]
#[test]
fn orchestration_009_tab_label_colored_by_highest_priority_status() {
    use SessionStatus::*;

    // (a) Error outranks everything, even surrounded by several Idle panes.
    let buf = render_tab_bar_to_buffer(
        &["Dashboard", "squad"],
        &[false, true],
        0,
        80,
        &[None, Some(&[Idle, Error, Idle])],
    );
    assert_eq!(
        tab_label_fg(&buf, "squad"),
        Color::Red,
        "a tab with an Error pane among Idle panes must render its label Red"
    );

    // (b) No Error present; WaitingForInput outranks Working and Idle.
    let buf = render_tab_bar_to_buffer(
        &["Dashboard", "squad"],
        &[false, true],
        0,
        80,
        &[None, Some(&[Working, WaitingForInput, Idle])],
    );
    assert_eq!(
        tab_label_fg(&buf, "squad"),
        Color::Yellow,
        "a tab with a WaitingForInput pane (no Error present) must render its label Yellow"
    );

    // (c) Every pane Idle -> falls through to the base/unstyled label color,
    // exactly like an ordinary tab. PRD #13 reserves DarkGray for
    // purely-decorative, non-read elements only; a tab label is text, so an
    // all-Idle orchestration tab must NOT be DarkGray (PRD #333 defect B).
    let buf = render_tab_bar_to_buffer(
        &["Dashboard", "squad"],
        &[false, true],
        0,
        80,
        &[None, Some(&[Idle, Idle, Idle])],
    );
    let base_buf = render_tab_bar_to_buffer(
        &["Dashboard", "demo"],
        &[false, false],
        0,
        80,
        &[None, None],
    );
    assert_eq!(
        tab_label_fg(&buf, "squad"),
        tab_label_fg(&base_buf, "demo"),
        "an all-Idle orchestration tab must render with the same base label color as an \
         ordinary tab, not DarkGray"
    );

    // (d) Thinking and Working present, nothing higher-priority -> Green,
    // since Working (priority 3) outranks Thinking (priority 4).
    let buf = render_tab_bar_to_buffer(
        &["Dashboard", "squad"],
        &[false, true],
        0,
        80,
        &[None, Some(&[Thinking, Working])],
    );
    assert_eq!(
        tab_label_fg(&buf, "squad"),
        Color::Green,
        "a tab with Working and Thinking panes (no higher-priority state) must render its \
         label Green — Working outranks Thinking"
    );

    // Non-orchestration tabs are unaffected: a `None` entry must render with
    // the SAME base label color as any other unaffected tab in the same row,
    // never a status color, even though "demo" here also happens to carry no
    // status data.
    let buf = render_tab_bar_to_buffer(
        &["Dashboard", "demo"],
        &[false, false],
        0,
        80,
        &[None, None],
    );
    assert_eq!(
        tab_label_fg(&buf, "Dashboard"),
        tab_label_fg(&buf, "demo"),
        "a non-orchestration tab (None) must render with the same base label color as any \
         other unaffected tab, not a status color"
    );
}

/// Scenario: Render the tab strip with an orchestration tab made the ACTIVE
/// tab (unlike `orchestration_009`, which always leaves Dashboard active) and
/// give it a non-idle (`Error`) pane — assert its label renders its status
/// `fg` tint (Red) as ordinary foreground text, cued as the active tab via
/// `UNDERLINED | BOLD` rather than `REVERSED` (issue #306: `REVERSED` swaps
/// fg/bg, so a stacked status `fg` used to invert into a BACKGROUND at
/// display time instead of coloring text — the reason the active tab used to
/// drop the tint entirely). Also asserts an INACTIVE orchestration tab whose
/// aggregate status is `Idle` renders `palette::STATUS_IDLE` (`Color::DarkGray`),
/// not the base label color (fork issue #351, maintainer decision 2026-08-15:
/// colour is now a total function of status, including Idle — reversing PRD
/// #333 defect B), and that an INACTIVE orchestration tab with a non-idle
/// (`Error`) aggregate status still colors its label text as today, with
/// neither `REVERSED` nor `BOLD` nor `UNDERLINED` — pinning the active cue
/// from both sides, so an inactive tab can never become indistinguishable
/// from an active one (reviewer finding F3 on PR #307/issue #306).
#[spec("tabs/orchestration/010")]
#[test]
fn orchestration_010_active_status_tint_underlined_and_idle_coloured() {
    use SessionStatus::*;

    // Case 1 (issue #306): an ACTIVE orchestration tab with a non-idle
    // (Error) pane must render its status fg tint (Red) as ordinary
    // foreground text, cued as active via UNDERLINED | BOLD — never
    // REVERSED, which would invert an fg tint into a background.
    let active_orch_buf = render_tab_bar_to_buffer(
        &["Dashboard", "squad"],
        &[false, true],
        1,
        80,
        &[None, Some(&[Idle, Error, Idle])],
    );
    assert_eq!(
        tab_label_fg(&active_orch_buf, "squad"),
        Color::Red,
        "an ACTIVE orchestration tab with an Error pane must render its label fg Red, the same \
         tint an inactive one gets"
    );
    let active_modifier = tab_label_modifier(&active_orch_buf, "squad");
    assert!(
        active_modifier.contains(Modifier::UNDERLINED) && active_modifier.contains(Modifier::BOLD),
        "an ACTIVE orchestration tab must carry UNDERLINED | BOLD as its active cue, got \
         {active_modifier:?}"
    );
    assert!(
        !active_modifier.contains(Modifier::REVERSED),
        "an ACTIVE orchestration tab must NOT carry REVERSED — REVERSED would invert an fg \
         status tint into a background, got {active_modifier:?}"
    );

    // Case 2 (fork issue #351, reversing defect B): an INACTIVE orchestration
    // tab whose aggregate status is Idle must render palette::STATUS_IDLE —
    // colour is a total function of status, including Idle.
    let idle_buf = render_tab_bar_to_buffer(
        &["Dashboard", "squad"],
        &[false, true],
        0,
        80,
        &[None, Some(&[Idle, Idle, Idle])],
    );
    assert_eq!(
        tab_label_fg(&idle_buf, "squad"),
        palette::STATUS_IDLE,
        "an INACTIVE orchestration tab whose aggregate status is Idle must render \
         palette::STATUS_IDLE"
    );

    // Case 3 (no regression, and F3: pin the inactive side of the active
    // cue): an INACTIVE orchestration tab with a non-idle (Error) aggregate
    // status still colors its label text, exactly as today, with neither
    // REVERSED nor BOLD nor UNDERLINED — so the UNDERLINED cue that marks a
    // tab active (case 1) cannot leak onto an inactive one.
    let err_buf = render_tab_bar_to_buffer(
        &["Dashboard", "squad"],
        &[false, true],
        0,
        80,
        &[None, Some(&[Idle, Error, Idle])],
    );
    assert_eq!(
        tab_label_fg(&err_buf, "squad"),
        Color::Red,
        "an INACTIVE orchestration tab with an Error pane must still render its label fg Red"
    );
    let inactive_modifier = tab_label_modifier(&err_buf, "squad");
    assert!(
        !inactive_modifier.contains(Modifier::REVERSED)
            && !inactive_modifier.contains(Modifier::BOLD)
            && !inactive_modifier.contains(Modifier::UNDERLINED),
        "an inactive orchestration tab must carry neither REVERSED nor BOLD nor UNDERLINED, got \
         {inactive_modifier:?}"
    );
}

/// Scenario: Render the tab strip with only the Dashboard tab, active, and no
/// orchestration status data (`orchestration_statuses = [None]`) — assert its
/// label carries `UNDERLINED | BOLD` as the active cue, contains no
/// `REVERSED`, and carries no absolute foreground color (`Color::Reset`, the
/// same as an ordinary unstyled tab) — proving the active cue applies
/// uniformly to the Dashboard tab, which carries no status data (fork issue
/// #351 narrows this from "tabs this feature doesn't touch" now that Mode
/// tabs also carry status data via `tab_status_data`; the Dashboard remains
/// the deliberate scope boundary). RED today: the active style is still
/// `Modifier::REVERSED | Modifier::BOLD` with no `UNDERLINED`.
#[spec("tabs/orchestration/012")]
#[test]
fn orchestration_012_active_non_orchestration_tab_underlined_no_reversed() {
    let buf = render_tab_bar_to_buffer(&["Dashboard"], &[false], 0, 80, &[None]);
    let modifier = tab_label_modifier(&buf, "Dashboard");
    assert!(
        modifier.contains(Modifier::UNDERLINED) && modifier.contains(Modifier::BOLD),
        "an ACTIVE non-orchestration tab must carry UNDERLINED | BOLD as its active cue, got \
         {modifier:?}"
    );
    assert!(
        !modifier.contains(Modifier::REVERSED),
        "an ACTIVE non-orchestration tab must NOT carry REVERSED, got {modifier:?}"
    );
    assert_eq!(
        tab_label_fg(&buf, "Dashboard"),
        Color::Reset,
        "an ACTIVE non-orchestration tab must carry no absolute foreground color"
    );
}

/// Scenario: Render the tab strip with an orchestration tab made the ACTIVE
/// tab and give it an all-`Idle` aggregate — assert its label renders
/// `palette::STATUS_IDLE` (fork issue #351, maintainer decision 2026-08-15:
/// colour is now a total function of status, extending that to the active
/// tab and reversing PRD #333 defect B / issue #306's active-tab carve-out),
/// while still carrying `UNDERLINED | BOLD` as its active cue and no
/// `REVERSED` — the active cue is unchanged, only the colour changes.
#[spec("tabs/orchestration/014")]
#[test]
fn orchestration_014_active_idle_coloured_underlined() {
    use SessionStatus::*;

    let active_idle_buf = render_tab_bar_to_buffer(
        &["Dashboard", "squad"],
        &[false, true],
        1,
        80,
        &[None, Some(&[Idle, Idle, Idle])],
    );
    assert_eq!(
        tab_label_fg(&active_idle_buf, "squad"),
        palette::STATUS_IDLE,
        "an ACTIVE orchestration tab whose aggregate status is Idle must render \
         palette::STATUS_IDLE"
    );
    let modifier = tab_label_modifier(&active_idle_buf, "squad");
    assert!(
        modifier.contains(Modifier::UNDERLINED) && modifier.contains(Modifier::BOLD),
        "an ACTIVE orchestration tab must carry UNDERLINED | BOLD as its active cue even when \
         Idle, got {modifier:?}"
    );
    assert!(
        !modifier.contains(Modifier::REVERSED),
        "an ACTIVE orchestration tab must NOT carry REVERSED, got {modifier:?}"
    );
}

/// Scenario: Render the tab strip with a Dashboard tab plus a second tab made
/// ACTIVE and carrying `Some(&[Working])` — the shape `tab_status_data`
/// (fork issue #351) produces for a Mode tab whose agent pane is Working —
/// and assert its label renders in `palette::STATUS_WORKING` (Green) as
/// ordinary foreground text, still cued active via `UNDERLINED | BOLD` with
/// no `REVERSED`. This is a regression guard, GREEN from the start: the
/// render half (`render_tab_strip`) already colors any tab whose
/// `orchestration_statuses` slot is `Some(..)` regardless of tab kind — the
/// defect is one level up, in the inline builder that currently maps every
/// Mode tab to `None` before it ever reaches this renderer.
#[spec("tabs/label/002")]
#[test]
fn label_002_active_tab_with_status_data_renders_status_color_underlined() {
    use SessionStatus::*;

    let buf = render_tab_bar_to_buffer(
        &["Dashboard", "demo"],
        &[false, true],
        1,
        80,
        &[None, Some(&[Working])],
    );
    assert_eq!(
        tab_label_fg(&buf, "demo"),
        palette::STATUS_WORKING,
        "a tab carrying Some(&[Working]) status data must render its label in STATUS_WORKING"
    );
    let modifier = tab_label_modifier(&buf, "demo");
    assert!(
        modifier.contains(Modifier::UNDERLINED) && modifier.contains(Modifier::BOLD),
        "the active tab must still carry UNDERLINED | BOLD as its active cue, got {modifier:?}"
    );
    assert!(
        !modifier.contains(Modifier::REVERSED),
        "the active tab must NOT carry REVERSED, got {modifier:?}"
    );
}

/// Scenario: Render the tab strip with a second tab carrying `Some(&[Idle])`
/// and, separately, `Some(&[])` — the shape `tab_status_data` (fork issue
/// #351) produces for a Mode tab whose agent is Idle, and for one whose
/// agent pane isn't live yet — and assert both render `palette::STATUS_IDLE`
/// (maintainer decision 2026-08-15: colour is a total function of status,
/// including Idle, following the same colour rule pinned in
/// `orchestration_014_active_idle_coloured_underlined`). The no-panes-yet
/// empty-slice case is intended to paint the same as an explicit Idle:
/// `palette::highest_priority_status(&[])` also resolves to Idle.
#[spec("tabs/label/003")]
#[test]
fn label_003_idle_and_empty_status_data_coloured() {
    use SessionStatus::*;

    let idle_buf = render_tab_bar_to_buffer(
        &["Dashboard", "demo"],
        &[false, false],
        0,
        80,
        &[None, Some(&[Idle])],
    );
    assert_eq!(
        tab_label_fg(&idle_buf, "demo"),
        palette::STATUS_IDLE,
        "a tab carrying Some(&[Idle]) must render palette::STATUS_IDLE"
    );

    let empty_buf = render_tab_bar_to_buffer(
        &["Dashboard", "demo"],
        &[false, false],
        0,
        80,
        &[None, Some(&[])],
    );
    assert_eq!(
        tab_label_fg(&empty_buf, "demo"),
        palette::STATUS_IDLE,
        "a tab carrying Some(&[]) (no panes live yet) must render palette::STATUS_IDLE"
    );
}
