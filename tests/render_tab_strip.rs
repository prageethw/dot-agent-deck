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

/// Locate the **column** of the leading pad space in `label`'s own
/// `" {label} "` span in the rendered single-row tab strip. Walks the row
/// cell-by-cell and compares symbols directly, rather than concatenating
/// symbols into a `String` and taking a **byte** offset into it — the `│`
/// divider is 3 bytes but 1 cell, and `[×]`'s `×` is 2 bytes but 1 cell, so a
/// byte offset used as an x coordinate skews right of the true column by 2
/// for every preceding divider (fork issue #377 fix-round F1/F2: this bug
/// previously lived inline in `tab_pad_fg` and `close_glyph_fg_after`).
fn label_span_start_col(buffer: &ratatui::buffer::Buffer, label: &str) -> u16 {
    let area = buffer.area();
    let cells: Vec<String> = (0..area.width)
        .map(|x| buffer[(x, 0)].symbol().to_string())
        .collect();
    let needle: Vec<String> = format!(" {label} ")
        .chars()
        .map(|c| c.to_string())
        .collect();
    (0..cells.len().saturating_sub(needle.len().saturating_sub(1)))
        .find(|&i| cells[i..i + needle.len()] == needle[..])
        .map(|i| i as u16)
        .unwrap_or_else(|| {
            panic!(
                "label {label:?} not found in rendered tab strip row: {:?}",
                cells.concat()
            )
        })
}

/// The foreground color of the padding space immediately preceding `label`'s
/// first character in the rendered single-row tab strip. Used to assert that
/// a tab's status tint fills the whole `" {label} "` span, not merely the
/// label's own characters.
fn tab_pad_fg(buffer: &ratatui::buffer::Buffer, label: &str) -> Color {
    let start = label_span_start_col(buffer, label);
    buffer[(start, 0)].fg
}

/// The foreground color of the `[×]` close glyph belonging to `label`'s tab —
/// the first `×` cell found at or after `label`'s own `" {label} "` span in
/// the rendered single-row tab strip. Used to assert the close glyph carries
/// the same status tint as the label text next to it.
fn close_glyph_fg_after(buffer: &ratatui::buffer::Buffer, label: &str) -> Color {
    let area = buffer.area();
    let start = label_span_start_col(buffer, label);
    (start..area.width)
        .find(|&x| buffer[(x, 0)].symbol() == "×")
        .map(|x| buffer[(x, 0)].fg)
        .unwrap_or_else(|| {
            panic!("no × close glyph found at or after label {label:?} starting at column {start}")
        })
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
/// Idle/Unknown(`palette::STATUS_IDLE`) (PRD #333, fork issue #351). Covers:
/// (a) one `Error` among `Idle` panes -> Red; (b) one `WaitingForInput` among
/// `Working`/`Idle` (no Error) -> Yellow; (c) all `Idle` -> `STATUS_IDLE`
/// (maintainer decision 2026-08-15: colour is a total function of status,
/// including Idle, reversing PRD #333 defect B); (d) `Thinking` + `Working`
/// (no higher-priority state) -> Green, since Working outranks Thinking.
/// Also asserts a tab with no status data (`None` — the Dashboard case)
/// never gets colorized by this feature.
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

    // (c) Every pane Idle -> paints palette::STATUS_IDLE. Idle now paints
    // STATUS_IDLE by maintainer decision (2026-08-15), deliberately
    // reversing PRD #333's "an aggregate that resolves to Idle is not
    // painted grey" and, before that, upstream's maintainer requiring the
    // carve-out on review of upstream PR #356. Accepted trade-off:
    // STATUS_IDLE is an absolute DarkGray and reads low-contrast on a dark
    // terminal — this was chosen, not overlooked.
    let buf = render_tab_bar_to_buffer(
        &["Dashboard", "squad"],
        &[false, true],
        0,
        80,
        &[None, Some(&[Idle, Idle, Idle])],
    );
    assert_eq!(
        tab_label_fg(&buf, "squad"),
        palette::STATUS_IDLE,
        "an all-Idle orchestration tab must render palette::STATUS_IDLE"
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

    // A tab entry with no status data (the Dashboard case, now that Mode
    // tabs also carry status data via `Some(..)`) is unaffected: a `None`
    // entry must render with the SAME base label color as any other
    // unaffected tab in the same row, never a status color.
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
        "a tab with no status data (the Dashboard case) must render with the same base label \
         color as any other unaffected tab, not a status color"
    );
}

/// Scenario: Render the tab strip with an orchestration tab made the ACTIVE
/// tab (unlike `orchestration_009`, which always leaves Dashboard active) and
/// give it a non-idle (`Error`) pane — assert its label renders its status
/// `fg` tint (Red) as ordinary foreground text, cued as the active tab via
/// `BOLD` ONLY — explicitly asserting the absence of `UNDERLINED` as well as
/// `REVERSED` (fork issue #377: the maintainer dropped the underline from the
/// active-tab cue as a preference, not a defect; `REVERSED` was already ruled
/// out by issue #306, since it would invert an fg status tint into a
/// background). Also asserts the padding spaces and the `[×]` close glyph
/// around the label carry the SAME status fg as the label text itself, so
/// the tint fills the whole tab rather than only the letters. Also asserts an
/// INACTIVE orchestration tab whose aggregate status is `Idle` renders
/// `palette::STATUS_IDLE` (`Color::DarkGray`), not the base label color (fork
/// issue #351, maintainer decision 2026-08-15: colour is now a total function
/// of status, including Idle — reversing PRD #333 defect B), and that an
/// INACTIVE orchestration tab with a non-idle (`Error`) aggregate status
/// still colors its label text as today, with neither `REVERSED` nor `BOLD`
/// nor `UNDERLINED` — pinning the active cue from both sides, so an inactive
/// tab can never become indistinguishable from an active one (reviewer
/// finding F3 on PR #307/issue #306).
#[spec("tabs/orchestration/010")]
#[test]
fn orchestration_010_active_status_tint_bold_and_idle_coloured() {
    use SessionStatus::*;

    // Case 1 (fork issue #377, on top of issue #306): an ACTIVE orchestration
    // tab with a non-idle (Error) pane must render its status fg tint (Red)
    // as ordinary foreground text, cued as active via BOLD only — never
    // REVERSED, which would invert an fg tint into a background, and never
    // UNDERLINED, which the maintainer dropped as a preference. The tint
    // must also fill the tab's padding and its [×] close glyph, not just
    // the label letters.
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
        active_modifier.contains(Modifier::BOLD) && !active_modifier.contains(Modifier::UNDERLINED),
        "an ACTIVE orchestration tab must carry BOLD as its active cue and NOT UNDERLINED (fork \
         issue #377 dropped the underline), got {active_modifier:?}"
    );
    assert!(
        !active_modifier.contains(Modifier::REVERSED),
        "an ACTIVE orchestration tab must NOT carry REVERSED — REVERSED would invert an fg \
         status tint into a background, got {active_modifier:?}"
    );
    assert_eq!(
        tab_pad_fg(&active_orch_buf, "squad"),
        Color::Red,
        "an ACTIVE orchestration tab's padding spaces must carry the same status fg as its \
         label, so the tint fills the whole tab"
    );
    assert_eq!(
        close_glyph_fg_after(&active_orch_buf, "squad"),
        Color::Red,
        "an ACTIVE orchestration tab's [×] close glyph must carry the same status fg as its \
         label, so the tint fills the whole tab"
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
    // REVERSED nor BOLD nor UNDERLINED — so the BOLD cue that marks a tab
    // active (case 1) cannot leak onto an inactive one.
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
/// status data (`tab_statuses = [None]`) — assert its
/// label carries `BOLD` ONLY as the active cue (explicitly no `UNDERLINED`,
/// dropped as a maintainer preference per fork issue #377), contains no
/// `REVERSED`, and carries no absolute foreground color (`Color::Reset`, the
/// same as an ordinary unstyled tab) — proving the active cue applies
/// uniformly to the Dashboard tab, which carries no status data (fork issue
/// #351 narrows this from "tabs this feature doesn't touch" now that Mode
/// tabs also carry status data via `tab_status_data`; the Dashboard remains
/// the deliberate scope boundary).
#[spec("tabs/orchestration/012")]
#[test]
fn orchestration_012_active_dashboard_tab_bold_no_reversed() {
    let buf = render_tab_bar_to_buffer(&["Dashboard"], &[false], 0, 80, &[None]);
    let modifier = tab_label_modifier(&buf, "Dashboard");
    assert!(
        modifier.contains(Modifier::BOLD) && !modifier.contains(Modifier::UNDERLINED),
        "an ACTIVE Dashboard tab must carry BOLD as its active cue and NOT UNDERLINED (fork \
         issue #377 dropped the underline), got {modifier:?}"
    );
    assert!(
        !modifier.contains(Modifier::REVERSED),
        "an ACTIVE Dashboard tab must NOT carry REVERSED, got {modifier:?}"
    );
    assert_eq!(
        tab_label_fg(&buf, "Dashboard"),
        Color::Reset,
        "an ACTIVE Dashboard tab must carry no absolute foreground color"
    );
}

/// Scenario: Render the tab strip with an orchestration tab made the ACTIVE
/// tab and give it an all-`Idle` aggregate — assert its label renders
/// `palette::STATUS_IDLE` (fork issue #351, maintainer decision 2026-08-15:
/// colour is now a total function of status, extending that to the active
/// tab and reversing PRD #333 defect B / issue #306's active-tab carve-out),
/// while still carrying `BOLD` ONLY as its active cue — explicitly no
/// `UNDERLINED` (dropped as a maintainer preference, fork issue #377) and no
/// `REVERSED` — the active cue's colour is unchanged, only its modifier.
#[spec("tabs/orchestration/014")]
#[test]
fn orchestration_014_active_idle_coloured_bold() {
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
        modifier.contains(Modifier::BOLD) && !modifier.contains(Modifier::UNDERLINED),
        "an ACTIVE orchestration tab must carry BOLD as its active cue even when Idle, and NOT \
         UNDERLINED (fork issue #377 dropped the underline), got {modifier:?}"
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
/// ordinary foreground text, still cued active via `BOLD` ONLY with no
/// `UNDERLINED` (dropped as a maintainer preference, fork issue #377) and no
/// `REVERSED`. The color assignment itself is a regression guard, GREEN from
/// the start: the render half (`render_tab_strip`) already colors any tab
/// whose `tab_statuses` slot is `Some(..)` regardless of tab kind, exercised
/// here directly via `render_tab_bar_to_buffer`. That `tab_status_data`
/// actually produces `Some(&[Working])` for a live Mode tab is
/// `tabs/label/001`'s concern; that `run_tui`'s call site wires the two
/// together is covered by no test.
#[spec("tabs/label/002")]
#[test]
fn label_002_active_tab_with_status_data_renders_status_color_bold() {
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
        modifier.contains(Modifier::BOLD) && !modifier.contains(Modifier::UNDERLINED),
        "the active tab must still carry BOLD as its active cue and NOT UNDERLINED (fork issue \
         #377 dropped the underline), got {modifier:?}"
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
/// `orchestration_014_active_idle_coloured_bold`). The no-panes-yet
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
