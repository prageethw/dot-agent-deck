//! L1 widget / layout snapshot tests for the dashboard renderer.
//!
//! Per PRD #77 Decision 2 these are in-process tests using ratatui's
//! `TestBackend` plus `insta` file snapshots. No subprocess, no PTY.
//! File-layout-mirrors-catalog (Decision 7): catalog IDs of the form
//! `dashboard/*/NNN` land in this file with function names
//! `<sub-area>_<NNN>_<short_suffix>` per Decision 17.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use dot_agent_deck::event::{AgentEvent, AgentType, EventType};
use dot_agent_deck::state::{ActiveTool, AppState, DashboardStats, SessionState, SessionStatus};
use dot_agent_deck::tab::Tab;
use dot_agent_deck::terminal_widget::TerminalWidget;
use dot_agent_deck::ui::{
    CardDensityKind, UiMode, card_stats_border_label, render_card_for_mode_to_buffer,
    render_card_grid_to_buffer, render_card_to_buffer, render_config_gen_prompt_to_buffer,
    render_dashboard_cards_to_buffer, render_quit_confirm_to_buffer, render_star_prompt_to_buffer,
    render_stats_bar_to_buffer, render_stop_confirm_to_buffer, sync_and_derive_selection,
};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use ratatui::widgets::Widget;
use spec::spec;
use unicode_width::UnicodeWidthStr;

/// Stringify the rendered buffer — one line per row, with cells joined
/// into the symbol layer. `insta` then captures this representation, so
/// snapshot diffs read like the rendered card itself rather than as
/// opaque byte streams. Helper extracted only because it's slightly
/// awkward inline; the docs generator skip-lists it so it doesn't
/// appear in the .md Steps section.
fn buffer_to_text(buffer: &ratatui::buffer::Buffer) -> String {
    let area = buffer.area();
    let mut out = String::with_capacity((area.width as usize + 1) * area.height as usize);
    for y in 0..area.height {
        for x in 0..area.width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

/// Scenario: Render a single dashboard card for a Working agent with a Read
/// tool and recent prompt into an 80-column, Normal-density L1 buffer, then
/// snapshot it. The title row must carry the card number and status badge,
/// with the display name on its own body row directly beneath (PRD fork#405
/// M1), while the compact Last/Tools counters occupy the bottom border.
#[spec("dashboard/pane/004")]
#[test]
fn pane_004_card_title_row() {
    // PRD #77 catalog: dashboard/pane/004 — Card title row carries
    // card number and a status badge, with the display name on its own
    // body row directly beneath (PRD fork#405 M1). Snapshot a single
    // Working session in Normal density. The session fixture is
    // inlined per M4.1 reviewer S1 (single-use test-data builder
    // doesn't need its own fn — keeping the test body
    // self-contained also reads as cleaner generated `.md` Steps).
    //
    // Both timestamps derive from one current instant so the fixture starts
    // with a compact `0s` elapsed value; a fixed calendar instant previously
    // drifted into a large hour count as the test aged (M3 fix). `last_activity`
    // is nudged 30s into the *future* of that instant rather than left equal to
    // it (issue #15): the bottom border's `Last:` field is computed from
    // `Utc::now()` at render time (`format_elapsed`, src/ui.rs), not at fixture
    // build time, and that renderer clamps a negative elapsed delta to zero
    // (`delta.num_seconds().max(0)`). Seeding `last_activity == now` put the
    // rendered value exactly on the boundary between `0s` and `1s` — any
    // scheduling delay between building the fixture and the snapshot render
    // (routine under parallel test load) could tip it over intermittently. A
    // *past* offset (e.g. `now - 500ms`) would only shrink that safety margin
    // further; nudging forward instead means the render-time delta stays
    // negative (clamped to `0s`) for any render happening within 30s of this
    // instant, which comfortably covers ordinary scheduling jitter.
    let now = chrono::Utc::now();
    let session = SessionState {
        session_id: "sess-abc123".to_string(),
        agent_type: AgentType::ClaudeCode,
        cwd: Some("/home/dev/example-project".to_string()),
        status: SessionStatus::Working,
        active_tool: Some(ActiveTool {
            name: "Read".to_string(),
            detail: Some("src/main.rs".to_string()),
        }),
        started_at: now,
        last_activity: now + chrono::Duration::seconds(30),
        recent_events: VecDeque::new(),
        tool_count: 7,
        last_user_prompt: Some("fix the login bug".to_string()),
        first_prompts: vec!["fix the login bug".to_string()],
        pane_id: Some("pane-1".to_string()),
        agent_id: Some("1".to_string()),
        display_name: None,
        pending_permission_tool: None,
        shell_synthetic_working: false,
        model: None,
        expects_agent_report: false,
    };
    // The 80-cell buffer leaves ample room for the full bottom-border stats
    // title. Height comes from the density tier itself so the snapshot's
    // geometry tracks the production layout module (no magic numbers).
    let width: u16 = 80;
    let density = CardDensityKind::Normal;
    let height = density.rendered_height();
    let buffer = render_card_to_buffer(
        &session,
        Some("example-coder"),
        Some(1),
        density,
        0,     // animation tick
        false, // not selected
        width,
        height,
    );
    insta::assert_snapshot!(buffer_to_text(&buffer));
}

/// Build a live card fixture whose stable values make the card-stats layout
/// assertions readable: one hour since activity, fourteen tools, one prompt,
/// and one active Read tool. The hour form remains `1h` for 60 seconds of drift.
fn card_stats_session(cwd: &str) -> SessionState {
    let now = chrono::Utc::now();
    SessionState {
        session_id: "sess-card-stats".to_string(),
        agent_type: AgentType::ClaudeCode,
        cwd: Some(cwd.to_string()),
        status: SessionStatus::Thinking,
        active_tool: Some(ActiveTool {
            name: "Read".to_string(),
            detail: Some("src/ui.rs".to_string()),
        }),
        started_at: now - chrono::Duration::hours(2),
        last_activity: now - chrono::Duration::hours(1),
        recent_events: VecDeque::new(),
        tool_count: 14,
        last_user_prompt: Some("move the stats into the border".to_string()),
        first_prompts: vec!["move the stats into the border".to_string()],
        pane_id: Some("pane-card-stats".to_string()),
        agent_id: Some("agent-card-stats".to_string()),
        display_name: Some("api-svc".to_string()),
        pending_permission_tool: None,
        shell_synthetic_working: false,
        model: None,
        expects_agent_report: false,
    }
}

/// Scenario: Render an over-long directory in a narrow L1 card, then render a
/// multiline prompt in a 14-column card. Dir must use its full row and end in
/// an ellipsis, while the newline costs no cell and `abcdef` fits without one.
#[spec("dashboard/pane/006")]
#[test]
fn pane_006_card_fields_and_full_width_ellipsized_dir() {
    let session = card_stats_session(
        "/home/dev/this-is-an-extremely-long-project-directory-name-that-needs-truncation",
    );
    let width = 50;
    let density = CardDensityKind::Normal;
    let buffer = render_card_to_buffer(
        &session,
        Some("api-svc"),
        Some(1),
        density,
        0,
        false,
        width,
        density.rendered_height(),
    );
    let rendered = buffer_to_text(&buffer);

    for label in ["Dir:", "Last:", "Tools:", "Prmt:"] {
        assert!(
            rendered.contains(label),
            "card must retain the `{label}` field:\n{rendered}"
        );
    }
    let dir_row = rendered
        .lines()
        .find(|row| row.contains("Dir:"))
        .expect("rendered card must contain a Dir row");
    assert!(
        dir_row.ends_with("…│"),
        "Dir must use the full inner width and truncate with an ellipsis immediately before the right border:\n{rendered}"
    );

    insta::assert_snapshot!(rendered);

    let mut multiline_session = card_stats_session("/home/dev/api-svc");
    multiline_session.last_user_prompt = Some("abc\ndef".to_string());
    multiline_session.first_prompts = vec!["abc\ndef".to_string()];
    let multiline = buffer_to_text(&render_card_to_buffer(
        &multiline_session,
        Some("api-svc"),
        Some(1),
        CardDensityKind::Compact,
        0,
        false,
        14,
        CardDensityKind::Compact.rendered_height(),
    ));
    assert!(
        multiline.contains("│Prmt: abcdef│"),
        "a newline must consume zero rendered cells, so the six visible prompt cells fit without an ellipsis:\n{multiline}"
    );
}

/// Scenario: Render a comfortably wide live card and a wide placeholder card
/// in L1 buffers. Both must retain full Last/Tools counters in the bottom
/// border, with the live card snapshot proving they consume no content row.
#[spec("dashboard/card-stats/001")]
#[test]
fn card_stats_001_wide_card_places_full_stats_in_bottom_right_border() {
    let session = card_stats_session("/home/dev/api-svc");
    let width = 80;
    let density = CardDensityKind::Normal;
    let buffer = render_card_to_buffer(
        &session,
        Some("api-svc"),
        Some(1),
        density,
        0,
        false,
        width,
        density.rendered_height(),
    );
    let rendered = buffer_to_text(&buffer);
    let rows: Vec<&str> = rendered.lines().collect();
    let bottom = rows.last().expect("card must have a bottom border");
    let inner = rows[1..rows.len() - 1].join("\n");

    assert!(
        bottom.contains(" Last: 1h  Tools: 14 "),
        "wide card bottom border must contain the full stats label:\n{rendered}"
    );
    let stats_column = bottom
        .find("Last: 1h")
        .expect("the full stats label was asserted above");
    assert!(
        stats_column > width as usize / 2,
        "stats label must be aligned to the right half of the bottom border:\n{rendered}"
    );
    assert!(
        !inner.contains("Last:") && !inner.contains("Tools:"),
        "stats must not consume a content row:\n{rendered}"
    );

    insta::assert_snapshot!(rendered);

    let placeholder = buffer_to_text(&placeholder_card(false));
    let placeholder_bottom = placeholder
        .lines()
        .last()
        .expect("placeholder card must have a bottom border");
    assert!(
        placeholder.contains("No agent"),
        "fixture must render the placeholder identity:\n{placeholder}"
    );
    assert!(
        placeholder_bottom.contains("Last:") && placeholder_bottom.contains("Tools: 0"),
        "placeholder cards must retain full Last/Tools counters in the bottom border:\n{placeholder}"
    );
}

/// Scenario: Render a card at the narrowest realistic 20-column width. The
/// bottom-right stats title must use the `1h · 14 tools` fallback while both
/// bottom corner glyphs remain intact and no stats row appears in the content.
#[spec("dashboard/card-stats/002")]
#[test]
fn card_stats_002_narrow_card_degrades_label_without_overrunning_corners() {
    let session = card_stats_session("/home/dev/api-svc");
    let width = 20;
    let density = CardDensityKind::Compact;
    let buffer = render_card_to_buffer(
        &session,
        Some("api-svc"),
        Some(1),
        density,
        0,
        false,
        width,
        density.rendered_height(),
    );
    let rendered = buffer_to_text(&buffer);
    let rows: Vec<&str> = rendered.lines().collect();
    let bottom = rows.last().expect("card must have a bottom border");
    let inner = rows[1..rows.len() - 1].join("\n");

    assert!(
        bottom.starts_with('└') && bottom.ends_with('┘'),
        "narrow stats title must preserve both bottom corner glyphs:\n{rendered}"
    );
    assert!(
        bottom.contains(" 1h · 14 tools "),
        "18 usable columns must select the 15-column short stats form:\n{rendered}"
    );
    assert!(
        !inner.contains("Last:") && !inner.contains("Tools:"),
        "narrow stats must not fall back to a dedicated content row:\n{rendered}"
    );

    insta::assert_snapshot!(rendered);
}

/// Scenario: Render the same Normal-density fixture immediately below and
/// above the old 60-column inner-width breakpoint. Both real card renders must
/// expose the same field labels on the same rows.
#[spec("dashboard/card-stats/003")]
#[test]
fn card_stats_003_old_sixty_column_boundary_is_structurally_inert() {
    let session = card_stats_session("/home/dev/api-svc");
    let below_width = 61; // 59 inner columns
    let above_width = 63; // 61 inner columns
    let density = CardDensityKind::Normal;

    let render = |width| {
        buffer_to_text(&render_card_to_buffer(
            &session,
            Some("api-svc"),
            Some(1),
            density,
            0,
            false,
            width,
            density.rendered_height(),
        ))
    };
    let below = render(below_width);
    let above = render(above_width);
    let labels = ["Dir:", "Prmt:", "Last:", "Tools:"];
    let label_rows = |rendered: &str| {
        labels
            .iter()
            .map(|label| {
                let row = rendered
                    .lines()
                    .position(|line| line.contains(label))
                    .unwrap_or_else(|| panic!("rendered card is missing `{label}`:\n{rendered}"));
                (*label, row)
            })
            .collect::<Vec<_>>()
    };
    let below_rows = label_rows(&below);
    let above_rows = label_rows(&above);

    assert_eq!(
        below_rows, above_rows,
        "field labels must retain the same row structure across the old 60-column boundary\nBELOW:\n{below}\nABOVE:\n{above}"
    );
}

/// Scenario: Pin both sides of the reference label's 9/15/21 transitions, then
/// sweep varied elapsed strings and tool counts across every relevant display
/// width. Every result must fit and equal the first, widest fitting form.
#[spec("dashboard/card-stats/004")]
#[test]
fn card_stats_004_label_ladder_transitions_at_display_width_boundaries() {
    let shortest = Some(" 2m · 14 ".to_string());
    let medium = Some(" 2m · 14 tools ".to_string());
    let full = Some(" Last: 2m  Tools: 14 ".to_string());

    for (lower, upper, wider) in [
        (8, 9, shortest.clone()),
        (14, 15, medium.clone()),
        (20, 21, full.clone()),
    ] {
        assert_ne!(
            card_stats_border_label(lower, "2m", 14),
            wider,
            "below the {lower}/{upper} transition boundary, width {lower} must not admit the wider form"
        );
        assert_eq!(
            card_stats_border_label(upper, "2m", 14),
            wider,
            "at the {lower}/{upper} transition boundary, width {upper} must admit the wider form"
        );
    }

    for usable_width in 0..=25 {
        let expected = match usable_width {
            0..=8 => None,
            9..=14 => Some(" 2m · 14 ".to_string()),
            15..=20 => Some(" 2m · 14 tools ".to_string()),
            _ => Some(" Last: 2m  Tools: 14 ".to_string()),
        };
        assert_eq!(
            card_stats_border_label(usable_width, "2m", 14),
            expected,
            "wrong degradation-ladder form at usable display width {usable_width}"
        );
    }

    for (last, tools) in [
        ("2m", 14),
        ("1h 5m", 1234),
        ("2m", 999_999),
        ("", 14),
        ("项目e\u{301}", 14),
    ] {
        let candidates = [
            format!(" Last: {last}  Tools: {tools} "),
            format!(" {last} · {tools} tools "),
            format!(" {last} · {tools} "),
        ];
        let max_candidate_width = candidates
            .iter()
            .map(|candidate| candidate.width())
            .max()
            .expect("the candidate list is non-empty");

        for usable_width in
            0..=u16::try_from(max_candidate_width + 2).expect("test labels fit in u16")
        {
            let actual = card_stats_border_label(usable_width, last, tools);
            if let Some(ref label) = actual {
                assert!(
                    label.width() <= usable_width as usize,
                    "selected form overruns width {usable_width} for last={last:?}, tools={tools}: {label:?}"
                );
            }
            let expected = candidates
                .iter()
                .find(|candidate| candidate.width() <= usable_width as usize)
                .cloned();
            assert_eq!(
                actual, expected,
                "selector must return the first (widest) fitting form at width {usable_width} for last={last:?}, tools={tools}"
            );
        }
    }
}

/// Color-aware sibling of [`buffer_to_text`]. `buffer_to_text` captures the
/// symbol layer ONLY, so it cannot detect a foreground/background color
/// change — exactly the kind of regression PRD #13 is about. This helper
/// run-length-encodes each row into `[fg/bg]"text"` segments so the snapshot
/// pins color: a cell painted with a hardcoded `White` shows up as `White`,
/// distinct from a palette-resolved `Black`. The fully-default terminal
/// background (Reset fg + Reset bg + whitespace) is dropped so the snapshots
/// stay focused on the styled glyphs rather than acres of empty margin.
fn buffer_to_color_text(buffer: &ratatui::buffer::Buffer) -> String {
    use std::fmt::Write as _;

    fn flush(line: &mut String, fg: Color, bg: Color, run: &str) {
        if run.trim().is_empty() && fg == Color::Reset && bg == Color::Reset {
            return;
        }
        let _ = write!(line, "[{fg:?}/{bg:?}]{run:?} ");
    }

    let area = buffer.area();
    let mut out = String::new();
    for y in 0..area.height {
        let mut line = String::new();
        let mut run = String::new();
        let mut run_fg = buffer[(0, y)].fg;
        let mut run_bg = buffer[(0, y)].bg;
        for x in 0..area.width {
            let cell = &buffer[(x, y)];
            if cell.fg != run_fg || cell.bg != run_bg {
                flush(&mut line, run_fg, run_bg, &run);
                run.clear();
                run_fg = cell.fg;
                run_bg = cell.bg;
            }
            run.push_str(cell.symbol());
        }
        flush(&mut line, run_fg, run_bg, &run);
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// Render the five PRD #13 overlay/prompt surfaces — stats bar, Quit and Stop
/// confirm dialogs, star prompt, config-generation prompt — into color-aware
/// buffers and return one `(label, buffer)` per surface. Both
/// `theme/contrast/001` and `theme/guard/001` drive these same seams; the
/// label is only used to point assertion failures at the offending surface.
fn overlay_buffers() -> Vec<(&'static str, ratatui::buffer::Buffer)> {
    // Representative mix so every status segment renders, 140 cells wide so the
    // whole bar fits without truncation (mirrors the prior contrast fixtures).
    let stats = DashboardStats {
        active: 6,
        working: 1,
        thinking: 1,
        waiting: 1,
        errors: 1,
        idle: 1,
        compacting: 1,
        total_tools: 42,
    };
    vec![
        (
            "stats",
            render_stats_bar_to_buffer(&stats, Some("plan"), 140, 1),
        ),
        ("quit", render_quit_confirm_to_buffer(0, 80, 24)),
        ("stop", render_stop_confirm_to_buffer(0, 2, 80, 24)),
        ("star", render_star_prompt_to_buffer(80, 24)),
        ("config_gen", render_config_gen_prompt_to_buffer(0, 80, 24)),
    ]
}

/// Build a placeholder ("No agent") card fixture with a current activity time;
/// its compact Last/Tools counters use the same bottom-border path as live cards.
///
/// PRD #341 M4 made the selection accent mode-dependent, so the mode is passed
/// explicitly: `UiMode::Normal` — command mode, where the keyboard drives the deck
/// and the accent renders at full strength.
///
/// PRD fork#405 auditor S2: carries a non-empty display name (a scheduled task
/// surfacing its friendly name before an agent's `SessionStart` hook fires is a
/// real production case) so the identity body row actually renders — an empty
/// `display_name`/`session_id` pair falls to the `Line::from("")` branch and
/// `palette::ROLE_NAME` never appears in the buffer this fixture feeds to
/// `theme/guard/001`. `is_placeholder`'s "No agent" status label reads only
/// `agent_type`, so this does not change that assertion.
fn placeholder_card(selected: bool) -> ratatui::buffer::Buffer {
    let now = chrono::Utc::now();
    let placeholder = SessionState {
        session_id: String::new(),
        agent_type: AgentType::None,
        cwd: None,
        status: SessionStatus::Idle,
        active_tool: None,
        started_at: now,
        last_activity: now,
        recent_events: VecDeque::new(),
        tool_count: 0,
        last_user_prompt: None,
        first_prompts: Vec::new(),
        pane_id: None,
        agent_id: None,
        display_name: Some("placeholder-agent".to_string()),
        pending_permission_tool: None,
        shell_synthetic_working: false,
        model: None,
        expects_agent_report: false,
    };
    let width: u16 = 40;
    let density = CardDensityKind::Normal;
    let height = density.rendered_height();
    render_card_for_mode_to_buffer(
        &placeholder,
        Some("placeholder-agent"),
        None,
        density,
        0,
        selected,
        UiMode::Normal,
        width,
        height,
        false,
    )
}

/// Scenario: Render the five dot-agent-deck overlay/prompt surfaces — the stats
/// bar, the Quit and Stop confirm dialogs, the star prompt, and the
/// config-generation prompt — into color-aware buffers and assert that NO cell
/// carries an absolute `Color::Rgb(..)` foreground or background. Under the
/// PRD #13 terminal-relative model every overlay must paint its canvas with
/// `Color::Reset` (inheriting the terminal's own background) and use only
/// `Reset` / named-ANSI foregrounds, so any `Rgb` token in the color dump is a
/// reference-frame violation. A regression that reintroduced an absolute fill
/// (e.g. `bg(Color::Rgb(..))`) would surface as an `Rgb` token and fail here.
#[spec("theme/contrast/001")]
#[test]
fn contrast_001_overlays_reference_frame() {
    for (label, buf) in overlay_buffers() {
        let dump = buffer_to_color_text(&buf);
        assert!(
            !dump.contains("Rgb"),
            "overlay `{label}` emits an absolute Color::Rgb(..) — \
             terminal-relative surfaces must use Reset/ANSI only:\n{dump}"
        );
    }
}

/// Scenario: Render the five overlay seams plus a session card in both the
/// unselected and SELECTED states, in **command mode** (`UiMode::Normal`, where
/// the keyboard drives the deck), then assert three terminal-relative
/// properties. (a) NO rendered cell across any surface has a `Color::Rgb(..)`
/// background — backgrounds must be `Color::Reset` so the terminal's own
/// background shows through. (b) Selection is cued by a `▸ ` title prefix, a
/// border in the terminal's own foreground (`Color::Reset`), and a THICKER glyph
/// (`┃` where an unselected card draws `│`) — never an absolute background tint
/// and never a DIM. A regression that filled any surface with an absolute
/// background, or that let the selected border fall back to a fixed White/Black
/// or a low-contrast status colour, would fail one of these assertions (issue
/// #442). (c) PRD fork#405 auditor S2: on the card surfaces, no cell foreground
/// is `Color::Rgb(..)` either, and `palette::ROLE_NAME` is the ONLY non-`Reset`,
/// non-named-ANSI foreground that appears — converting the palette module's
/// documented WCAG exception (one deliberate `Color::Indexed` constant, and
/// only one) from prose into an assertion a future diff has to argue with.
#[spec("theme/guard/001")]
#[test]
fn guard_001_no_absolute_backgrounds() {
    // (a) No Color::Rgb(..) BACKGROUND on any cheaply-seamable surface. The
    //     color dump renders each cell as `[fg/bg]"text"`, so an Rgb background
    //     shows up as the `/Rgb(` token (Rgb in the bg position after the `/`).
    let unselected = placeholder_card(false);
    let selected = placeholder_card(true);
    let mut surfaces = overlay_buffers();
    surfaces.push(("card", unselected.clone()));
    surfaces.push(("card_selected", selected.clone()));
    for (label, buf) in &surfaces {
        let dump = buffer_to_color_text(buf);
        assert!(
            !dump.contains("/Rgb"),
            "surface `{label}` paints an absolute Color::Rgb(..) background — \
             backgrounds must be Color::Reset:\n{dump}"
        );
    }

    // (b) Selection is signalled by a `▸ ` title prefix, the terminal's own
    //     foreground on the border, and a THICKER glyph — all terminal-relative
    //     cues, NOT an absolute background fill. Read the left-border cell (at a
    //     mid-height row) of each card: the selected one must DIFFER from the
    //     unselected one, must be `Color::Reset` (which contrasts on any theme,
    //     unlike a fixed White/Black or a low-contrast status colour), must not
    //     be DIM, and must thicken. Issue #442; `mode/deck/001` owns the
    //     command-vs-PaneInput emphasis recipe.
    let border_cell = |buf: &ratatui::buffer::Buffer| {
        let y = buf.area().height / 2;
        let cell = &buf[(0, y)];
        (cell.fg, cell.modifier, cell.symbol().to_string())
    };
    let (unsel_fg, unsel_mod, unsel_sym) = border_cell(&unselected);
    let (sel_fg, sel_mod, sel_sym) = border_cell(&selected);
    assert_ne!(
        (sel_fg, sel_mod, sel_sym.clone()),
        (unsel_fg, unsel_mod, unsel_sym.clone()),
        "selected card border must differ from the unselected card border"
    );
    assert_eq!(
        sel_fg,
        Color::Reset,
        "the selected card's border must be the terminal's own foreground, so it \
         contrasts on light and dark alike — not an absolute tint, not a \
         low-contrast status colour (issue #442), got {sel_fg:?}"
    );
    assert!(
        !sel_mod.contains(Modifier::DIM),
        "the selected card's border must not be DIM, got {sel_mod:?}"
    );
    assert_eq!(
        (unsel_sym.as_str(), sel_sym.as_str()),
        ("│", "┃"),
        "selection must also thicken the border: unselected `│`, selected `┃`"
    );

    // The `▸ ` selection prefix appears only on the selected card's title row.
    assert!(
        buffer_to_text(&selected).contains('▸'),
        "selected card must carry the `▸ ` selection prefix in its title"
    );
    assert!(
        !buffer_to_text(&unselected).contains('▸'),
        "unselected card must NOT carry the `▸ ` selection prefix"
    );

    // (c) The palette's one deliberate non-named exception, `palette::ROLE_NAME`,
    //     is the ONLY non-Reset, non-named-ANSI foreground allowed on a card —
    //     and in particular no cell foreground is an absolute `Color::Rgb(..)`.
    //     `placeholder_card` now carries a non-empty display name (see its doc
    //     comment) specifically so the identity row renders into this buffer.
    let role_name = dot_agent_deck::palette::ROLE_NAME;
    let named_ansi = [
        Color::Black,
        Color::Red,
        Color::Green,
        Color::Yellow,
        Color::Blue,
        Color::Magenta,
        Color::Cyan,
        Color::Gray,
        Color::DarkGray,
        Color::LightRed,
        Color::LightGreen,
        Color::LightYellow,
        Color::LightBlue,
        Color::LightMagenta,
        Color::LightCyan,
        Color::White,
    ];
    for (label, buf) in [("card", &unselected), ("card_selected", &selected)] {
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                let fg = buf[(x, y)].fg;
                if fg == Color::Reset || named_ansi.contains(&fg) {
                    continue;
                }
                assert_eq!(
                    fg,
                    role_name,
                    "surface `{label}` has a non-Reset, non-named-ANSI foreground \
                     {fg:?} at ({x},{y}) that is not palette::ROLE_NAME — the \
                     palette's WCAG rationale claims ROLE_NAME is the only \
                     deliberate exception:\n{}",
                    buffer_to_color_text(buf)
                );
            }
        }
    }
}

/// Scenario: Read `src/ui.rs` from disk and assert it contains none of the
/// forbidden absolute-background patterns — `bg(Color::Rgb`,
/// `bg(palette.terminal_bg)`, `bg(palette.selected_bg)`, `bg(palette.tab_bar_bg)`.
/// This source lint guards the `render_frame` canvas/tab-bar fills, which paint
/// the whole window and aren't cheaply reachable through a render seam. Under
/// the PRD #13 terminal-relative model none of these absolute fills may remain.
/// Reintroducing any of the four patterns in `src/ui.rs` would fail this lint.
#[spec("theme/guard/002")]
#[test]
fn guard_002_no_absolute_bg_in_source() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/ui.rs");
    let src = std::fs::read_to_string(path).expect("src/ui.rs should be readable");
    for forbidden in [
        "bg(Color::Rgb",
        "bg(palette.terminal_bg)",
        "bg(palette.selected_bg)",
        "bg(palette.tab_bar_bg)",
    ] {
        assert!(
            !src.contains(forbidden),
            "src/ui.rs still contains forbidden absolute-background pattern `{forbidden}` — \
             terminal-relative surfaces must use Color::Reset / Modifier::REVERSED"
        );
    }
}

/// Scenario: Render a single dashboard card for a live `AgentType::Pi` session
/// with NO display name (so the card's identity falls back to the plain
/// `<session-id>` form) into a `TestBackend` buffer, then assert the rendered
/// card surface shows the session id but carries no agent-type badge by
/// default — no `Pi` / `ClaudeCode` / `OpenCode` / `No agent` label text
/// anywhere, and no cell in Pi's registry `badge_color`. Fork-only: the
/// badge is removed from every card by default, so a Pi pane must render
/// exactly like any other agent type once un-badged, not fall back to
/// showing its type name. Does not assert the toggled-on state, where the
/// badge does render (`dashboard/agent-badge/001`).
#[spec("dashboard/pane/007")]
#[test]
fn pane_007_pi_card_omits_agent_type_badge() {
    // Fork-only badge removal: `render_session_card` no longer emits the
    // `(agent_type_text, badge_style)` segment for any agent type, so a
    // no-display-name Pi pane's identity falls back to the bare session id
    // (`orch-01`), with no `Pi` text and no registry badge colour anywhere
    // on the card. This mirrors the same-shape assertions in
    // `pane_008_codex_card_omits_agent_type_badge` but pins the Pi agent
    // type specifically, since Pi historically needed its own identity
    // check (PRD #201) to prove it renders as a first-class agent at all.
    //
    // A current activity time keeps the compact bottom-border elapsed value
    // small; this identity test does not assert its exact value.
    let now = chrono::Utc::now();
    let session = SessionState {
        session_id: "orch-01".to_string(),
        agent_type: AgentType::Pi,
        cwd: Some("/home/dev/workspace".to_string()),
        status: SessionStatus::Thinking,
        active_tool: None,
        started_at: now,
        last_activity: now,
        recent_events: VecDeque::new(),
        tool_count: 0,
        last_user_prompt: Some("orchestrate the release".to_string()),
        first_prompts: vec!["orchestrate the release".to_string()],
        pane_id: Some("pi-pane-1".to_string()),
        agent_id: Some("1".to_string()),
        // No friendly name → identity falls back to the bare session id;
        // no agent-type badge form exists to fall back to instead.
        display_name: None,
        pending_permission_tool: None,
        shell_synthetic_working: false,
        model: None,
        expects_agent_report: false,
    };
    let width: u16 = 80;
    let density = CardDensityKind::Normal;
    let height = density.rendered_height();
    let buffer = render_card_to_buffer(
        &session,
        None, // no display name
        Some(1),
        density,
        0,     // animation tick
        false, // not selected
        width,
        height,
    );
    let text = buffer_to_text(&buffer);

    // The bare session id still surfaces on the card's identity row, since
    // there is no display name to fall back to.
    assert!(
        text.contains("orch-01"),
        "a Pi pane's card must show the session id `orch-01` once the \
         agent-type badge is removed:\n{text}"
    );
    // The agent-type badge is gone entirely — no type label for Pi or any
    // other agent type leaks onto the card.
    for other in ["Pi", "ClaudeCode", "OpenCode", "Codex", "No agent"] {
        assert!(
            !text.contains(other),
            "a Pi pane's card must not show the agent-type label `{other}` \
             once the badge is removed:\n{text}"
        );
    }
    // ...and no cell should carry Pi's registry badge color either.
    let would_be_badge_color = dot_agent_deck::agent_registry::spec(&AgentType::Pi).badge_color;
    let has_badge_colored_cell = (0..buffer.area().height)
        .any(|y| (0..buffer.area().width).any(|x| buffer[(x, y)].fg == would_be_badge_color));
    assert!(
        !has_badge_colored_cell,
        "no cell should carry Pi's registry badge color {would_be_badge_color:?} \
         once the badge is removed:\n{}",
        buffer_to_color_text(&buffer)
    );
}

/// Scenario: Render a live Codex session with no friendly display name into a
/// color-aware card buffer. Fork-only rename/hide (never pushed upstream):
/// the agent-type badge is removed by default, so the card must show NO
/// `Codex` label text and NO cell anywhere on the card in Codex's registry
/// badge color. Repeats the check for named Claude Code / OpenCode / Pi /
/// Codex cards, where the friendly display name must still render. Does not
/// assert the toggled-on state, where the badge does render
/// (`dashboard/agent-badge/001`).
#[spec("dashboard/pane/008")]
#[test]
fn pane_008_codex_card_omits_agent_type_badge() {
    // `last_activity` is nudged 30s into the future of `now` rather than left
    // equal to it (issue #15): this fixture feeds both the immediate snapshot
    // below AND the later `pane_008_named_agent_badges` snapshot (via
    // `session.clone()` in the loop further down), so by the time the second
    // snapshot renders, real wall-clock time has already passed doing the
    // first snapshot's assertions and comparisons. The bottom border's
    // `Last:` field is computed from `Utc::now()` at render time
    // (`format_elapsed`, src/ui.rs), not at fixture build time, so seeding
    // `last_activity == now` raced that elapsed computation across a whole
    // second boundary under any scheduling delay (routine under parallel test
    // load) — intermittently rendering `Last: 1s` instead of the pinned
    // `Last: 0s`. A *past* offset (e.g. `now - 500ms`) would only shrink the
    // safety margin further and make the race worse, not fix it. Nudging
    // forward instead relies on `format_elapsed`'s existing clamp of a
    // negative elapsed delta to zero (`delta.num_seconds().max(0)`): the
    // render-time delta stays negative (clamped to `0s`) for any render
    // happening within 30s of this instant, comfortably covering both
    // snapshots in this test.
    let now = chrono::Utc::now();
    let session = SessionState {
        session_id: "wrapped-01".to_string(),
        agent_type: AgentType::Codex,
        cwd: Some("/home/dev/workspace".to_string()),
        status: SessionStatus::Thinking,
        active_tool: None,
        started_at: now,
        last_activity: now + chrono::Duration::seconds(30),
        recent_events: VecDeque::new(),
        tool_count: 0,
        last_user_prompt: Some("inspect the repository".to_string()),
        first_prompts: vec!["inspect the repository".to_string()],
        pane_id: Some("codex-pane-1".to_string()),
        agent_id: Some("1".to_string()),
        display_name: None,
        pending_permission_tool: None,
        shell_synthetic_working: false,
        model: None,
        expects_agent_report: false,
    };
    let width: u16 = 80;
    let density = CardDensityKind::Normal;
    let height = density.rendered_height();
    let buffer = render_card_to_buffer(&session, None, Some(1), density, 0, false, width, height);
    let text = buffer_to_text(&buffer);
    assert!(
        !text.contains("Codex"),
        "the agent-type badge is removed from cards; a wrapped Codex pane's \
         card must not render the `Codex` label anywhere:\n{text}"
    );

    let would_be_badge_color = dot_agent_deck::agent_registry::spec(&AgentType::Codex).badge_color;
    let has_badge_colored_cell = (0..buffer.area().height)
        .any(|y| (0..buffer.area().width).any(|x| buffer[(x, y)].fg == would_be_badge_color));
    assert!(
        !has_badge_colored_cell,
        "no cell should carry Codex's registry badge color {would_be_badge_color:?} \
         once the badge is removed:\n{}",
        buffer_to_color_text(&buffer)
    );

    insta::assert_snapshot!(buffer_to_color_text(&buffer));

    let mut named_badges = String::new();
    for (agent_type, friendly_name) in [
        (AgentType::ClaudeCode, "friendly-claude"),
        (AgentType::OpenCode, "friendly-opencode"),
        (AgentType::Pi, "friendly-pi"),
        (AgentType::Codex, "friendly-codex"),
    ] {
        let mut named = session.clone();
        named.agent_type = agent_type.clone();
        named.display_name = Some(friendly_name.to_string());
        let named_buffer = render_card_to_buffer(
            &named,
            Some(friendly_name),
            Some(1),
            density,
            0,
            false,
            width,
            height,
        );
        let label = dot_agent_deck::agent_registry::spec(&agent_type).label;
        let expected_color = dot_agent_deck::agent_registry::spec(&agent_type).badge_color;
        let text = buffer_to_text(&named_buffer);
        assert!(
            !text.contains(label),
            "the agent-type badge is removed from cards; a named {agent_type:?} \
             card must not render its registry `{label}` label anywhere, even \
             alongside the friendly name {friendly_name:?}:\n{text}"
        );
        assert!(
            text.contains(friendly_name),
            "a named {agent_type:?} card must still render its friendly display \
             name {friendly_name:?} once the badge is removed:\n{text}"
        );
        let has_badge_colored_cell = (0..named_buffer.area().height).any(|y| {
            (0..named_buffer.area().width).any(|x| named_buffer[(x, y)].fg == expected_color)
        });
        assert!(
            !has_badge_colored_cell,
            "no cell on the named {agent_type:?} card should carry registry \
             color {expected_color:?} once the badge is removed:\n{}",
            buffer_to_color_text(&named_buffer)
        );
        named_badges.push_str(&format!(
            "== {agent_type:?} / {friendly_name} ==\n{}",
            buffer_to_color_text(&named_buffer)
        ));
    }
    insta::assert_snapshot!("pane_008_named_agent_badges", named_badges);
}

/// Scenario: Render the same live Codex session card through
/// `render_card_for_mode_to_buffer` with the agent-type badge toggle off and
/// on, then repeat the on/off pair for all five shipped agent types with a
/// friendly display name. Off must show no type label and no cell in that
/// agent's registry `badge_color`; on must show `<Label> · <name>` and a cell
/// carrying both `badge_color` and `Modifier::BOLD`. Also pins D4: a
/// placeholder (`AgentType::None`) card shows no badge even when the toggle
/// is on — only its unrelated `● No agent` status text, never a doubled
/// occurrence from a restored identity segment. PRD fork#378: a session
/// carrying a known `model` renders `<Label> (<Model>) · <name>` when the
/// toggle is on, in the same registry `badge_color` + `Modifier::BOLD` cell,
/// and shows neither the label nor the model when the toggle is off; a
/// session with no model keeps the bare `<Label> · <name>` form.
/// Reviewer/auditor round 2 additionally pins: model-label normalization
/// strips a known vendor prefix (`claude-`, `gpt-`, `gemini-`, `llama-`,
/// `mistral-`) case-insensitively and passes an unrecognized id through
/// unchanged, with a value that is ONLY a vendor prefix falling back to its
/// raw form rather than collapsing to empty; and an empty or
/// whitespace-only model renders the bare `<Label> · <name>` form, never an
/// empty `()` or a whitespace-padded one.
#[spec("dashboard/agent-badge/001")]
#[test]
fn agent_badge_001_card_shows_registry_badge_only_when_enabled() {
    let now = chrono::Utc::now();
    let session = SessionState {
        session_id: "wrapped-01".to_string(),
        agent_type: AgentType::Codex,
        cwd: Some("/home/dev/workspace".to_string()),
        status: SessionStatus::Thinking,
        active_tool: None,
        started_at: now,
        last_activity: now,
        recent_events: VecDeque::new(),
        tool_count: 0,
        last_user_prompt: Some("inspect the repository".to_string()),
        first_prompts: vec!["inspect the repository".to_string()],
        pane_id: Some("codex-pane-1".to_string()),
        agent_id: Some("1".to_string()),
        display_name: None,
        pending_permission_tool: None,
        shell_synthetic_working: false,
        // PRD fork#378: no model known — the badge must keep the bare
        // `<Label> · <name>` form asserted below unchanged.
        model: None,
        expects_agent_report: false,
    };
    let width: u16 = 80;
    let density = CardDensityKind::Normal;
    let height = density.rendered_height();
    let codex_color = dot_agent_deck::agent_registry::spec(&AgentType::Codex).badge_color;

    let off = render_card_for_mode_to_buffer(
        &session,
        None,
        Some(1),
        density,
        0,
        false,
        UiMode::Normal,
        width,
        height,
        false,
    );
    let off_text = buffer_to_text(&off);
    assert!(
        !off_text.contains("Codex"),
        "with the badge toggle off (the default), a Codex card must not show \
         the `Codex` label:\n{off_text}"
    );
    let off_has_badge_cell = (0..off.area().height)
        .any(|y| (0..off.area().width).any(|x| off[(x, y)].fg == codex_color));
    assert!(
        !off_has_badge_cell,
        "no cell should carry Codex's registry badge color {codex_color:?} \
         while the toggle is off:\n{}",
        buffer_to_color_text(&off)
    );

    let on = render_card_for_mode_to_buffer(
        &session,
        None,
        Some(1),
        density,
        0,
        false,
        UiMode::Normal,
        width,
        height,
        true,
    );
    // PRD fork#405 M1: identity moved off the title row onto the first body
    // row, so the old whole-buffer `"Codex · wrapped-01"` needle is
    // row-scoped into two assertions — the badge in the title, the name on
    // the body row — which is what actually pins *placement* rather than
    // merely tolerating it.
    let on_title = row_text(&on, 0);
    let on_body0 = row_text(&on, 1);
    assert!(
        on_title.contains("Codex") && !on_title.contains("wrapped-01"),
        "with the badge toggle on, the title row must carry the `Codex` badge and NOT the \
         identity:\n{on_title}"
    );
    assert!(
        on_body0.contains("wrapped-01"),
        "with the badge toggle on, the first body row must carry the identity \
         `wrapped-01`:\n{on_body0}"
    );
    let on_has_bold_badge_cell = (0..on.area().height).any(|y| {
        (0..on.area().width)
            .any(|x| on[(x, y)].fg == codex_color && on[(x, y)].modifier.contains(Modifier::BOLD))
    });
    assert!(
        on_has_bold_badge_cell,
        "with the badge toggle on, a cell must carry Codex's registry badge \
         color {codex_color:?} AND Modifier::BOLD:\n{}",
        buffer_to_color_text(&on)
    );
    insta::assert_snapshot!("agent_badge_001_codex_on", buffer_to_color_text(&on));

    // PRD fork#378: the same session but with a known model. With the
    // toggle off, neither the label nor the model string may leak onto the
    // card; with the toggle on, the badge grows a bracketed model segment
    // inside the SAME registry-coloured, bold span — `Codex (Opus) ·
    // wrapped-01`, never the bare `Codex · wrapped-01` form once a model is
    // known.
    let mut session_with_model = session.clone();
    session_with_model.model = Some("Opus".to_string());

    let off_model = render_card_for_mode_to_buffer(
        &session_with_model,
        None,
        Some(1),
        density,
        0,
        false,
        UiMode::Normal,
        width,
        height,
        false,
    );
    let off_model_text = buffer_to_text(&off_model);
    assert!(
        !off_model_text.contains("Codex"),
        "with the badge toggle off, a Codex card with a known model must \
         still show no `Codex` label:\n{off_model_text}"
    );
    assert!(
        !off_model_text.contains("Opus"),
        "with the badge toggle off, a Codex card's known model must not \
         leak onto the card:\n{off_model_text}"
    );

    let on_model = render_card_for_mode_to_buffer(
        &session_with_model,
        None,
        Some(1),
        density,
        0,
        false,
        UiMode::Normal,
        width,
        height,
        true,
    );
    // PRD fork#405 M1: row-scoped, same split as the no-model case above —
    // the bracketed badge lives in the title, identity lives on the body row.
    let on_model_title = row_text(&on_model, 0);
    let on_model_body0 = row_text(&on_model, 1);
    assert!(
        on_model_title.contains("Codex (Opus)"),
        "with the badge toggle on, a Codex card with a known model must show \
         `Codex (Opus)` on the title row:\n{on_model_title}"
    );
    assert!(
        !on_model_title.contains("wrapped-01"),
        "the title row must never carry the identity, bracketed model or not:\n{on_model_title}"
    );
    assert!(
        on_model_body0.contains("wrapped-01"),
        "with the badge toggle on and a known model, the first body row must still carry the \
         identity `wrapped-01`:\n{on_model_body0}"
    );
    let on_model_has_bold_badge_cell = (0..on_model.area().height).any(|y| {
        (0..on_model.area().width).any(|x| {
            on_model[(x, y)].fg == codex_color && on_model[(x, y)].modifier.contains(Modifier::BOLD)
        })
    });
    assert!(
        on_model_has_bold_badge_cell,
        "with the badge toggle on and a known model, a cell must carry \
         Codex's registry badge color {codex_color:?} AND Modifier::BOLD:\n{}",
        buffer_to_color_text(&on_model)
    );

    // PRD fork#378 reviewer/auditor round 2, item 1: model-label
    // normalization. A compact heuristic — strip a leading vendor prefix
    // from a short, stable list, everything else passes through unchanged.
    // No per-model lookup table, since vendor prefixes are stable but model
    // ids churn constantly.
    // PRD fork#405 M1: returns the Buffer (not pre-flattened text) so
    // callers that need row-scoped placement checks (the empty-model loop
    // below) can get them; callers that only care about the title's
    // bracketed model text keep using `buffer_to_text` on the result, same
    // as before.
    let rendered_badge_with = |agent_type: AgentType, model: &str| -> ratatui::buffer::Buffer {
        let mut s = session.clone();
        s.agent_type = agent_type;
        s.model = Some(model.to_string());
        render_card_for_mode_to_buffer(
            &s,
            None,
            Some(1),
            density,
            0,
            false,
            UiMode::Normal,
            width,
            height,
            true,
        )
    };

    let text = buffer_to_text(&rendered_badge_with(
        AgentType::ClaudeCode,
        "claude-sonnet-5",
    ));
    assert!(
        text.contains("ClaudeCode (sonnet-5)"),
        "a `claude-` vendor prefix must be stripped from the displayed \
         model:\n{text}"
    );
    assert!(
        !text.contains("claude-sonnet-5"),
        "the raw, unstripped model id must not also appear:\n{text}"
    );

    let text = buffer_to_text(&rendered_badge_with(AgentType::Codex, "gpt-5.1-codex-mini"));
    assert!(
        text.contains("Codex (5.1-codex-mini)"),
        "a `gpt-` vendor prefix must be stripped from the displayed \
         model:\n{text}"
    );

    let text = buffer_to_text(&rendered_badge_with(AgentType::Pi, "unknown-model-x"));
    assert!(
        text.contains("Pi (unknown-model-x)"),
        "a model id matching no known vendor prefix must pass through \
         unchanged:\n{text}"
    );

    let text = buffer_to_text(&rendered_badge_with(AgentType::ClaudeCode, "CLAUDE-Opus-4"));
    assert!(
        text.contains("ClaudeCode (Opus-4)"),
        "vendor-prefix matching must be case-insensitive:\n{text}"
    );

    let text = buffer_to_text(&rendered_badge_with(AgentType::ClaudeCode, "claude-"));
    assert!(
        text.contains("ClaudeCode (claude-)"),
        "a model that is ONLY a vendor prefix must fall back to its raw \
         value rather than normalizing to empty:\n{text}"
    );
    assert!(
        !text.contains("ClaudeCode ()"),
        "a bare vendor-prefix model must never render an empty `()`:\n{text}"
    );

    // PRD fork#378 reviewer/auditor round 2, item 2 (F4): an empty or
    // whitespace-only model must render the bare `<Label>` badge with no
    // bracket. PRD fork#405 M1: the identity half of the old
    // `"ClaudeCode · wrapped-01"` needle is row-scoped — the bare badge
    // stays on the title row, the identity now lives on the body row.
    for empty_model in ["", "   "] {
        let buf = rendered_badge_with(AgentType::ClaudeCode, empty_model);
        let title = row_text(&buf, 0);
        let body0 = row_text(&buf, 1);
        assert!(
            title.contains("ClaudeCode") && !title.contains('('),
            "an empty/whitespace model {empty_model:?} must render the bare `ClaudeCode` \
             badge on the title row with no bracket:\n{title}"
        );
        assert!(
            body0.contains("wrapped-01"),
            "an empty/whitespace model {empty_model:?} must still carry the identity on the \
             first body row:\n{body0}"
        );
    }

    for (agent_type, friendly_name) in [
        (AgentType::ClaudeCode, "friendly-claude"),
        (AgentType::OpenCode, "friendly-opencode"),
        (AgentType::Pi, "friendly-pi"),
        (AgentType::Codex, "friendly-codex"),
        (AgentType::Devin, "friendly-devin"),
    ] {
        let mut named = session.clone();
        named.agent_type = agent_type.clone();
        named.display_name = Some(friendly_name.to_string());
        let label = dot_agent_deck::agent_registry::spec(&agent_type).label;
        let expected_color = dot_agent_deck::agent_registry::spec(&agent_type).badge_color;

        let off = render_card_for_mode_to_buffer(
            &named,
            Some(friendly_name),
            Some(1),
            density,
            0,
            false,
            UiMode::Normal,
            width,
            height,
            false,
        );
        let off_text = buffer_to_text(&off);
        assert!(
            !off_text.contains(label),
            "with the toggle off, a named {agent_type:?} card must not show \
             its registry `{label}` label:\n{off_text}"
        );

        let on = render_card_for_mode_to_buffer(
            &named,
            Some(friendly_name),
            Some(1),
            density,
            0,
            false,
            UiMode::Normal,
            width,
            height,
            true,
        );
        // PRD fork#405 M1: row-scoped — the badge label lives on the title
        // row, the friendly name lives on the first body row.
        let on_title = row_text(&on, 0);
        let on_body0 = row_text(&on, 1);
        assert!(
            on_title.contains(label) && !on_title.contains(friendly_name),
            "with the toggle on, a named {agent_type:?} card must show its `{label}` badge on \
             the title row and NOT the friendly name there:\n{on_title}"
        );
        assert!(
            on_body0.contains(friendly_name),
            "with the toggle on, a named {agent_type:?} card must show its friendly name \
             {friendly_name:?} on the first body row:\n{on_body0}"
        );
        let on_has_bold_badge_cell = (0..on.area().height).any(|y| {
            (0..on.area().width).any(|x| {
                on[(x, y)].fg == expected_color && on[(x, y)].modifier.contains(Modifier::BOLD)
            })
        });
        assert!(
            on_has_bold_badge_cell,
            "with the toggle on, a cell on the named {agent_type:?} card must \
             carry registry color {expected_color:?} AND Modifier::BOLD:\n{}",
            buffer_to_color_text(&on)
        );
    }

    // D4: a placeholder card (AgentType::None) must show no badge even when
    // the toggle is on. `spec(&None).label` is "No agent", which already
    // appears once as the unrelated status text — a verbatim restoration
    // would double it via a `No agent · <pane>` identity segment, so the
    // guard is an exact occurrence count, not a bare substring check.
    let placeholder = SessionState {
        session_id: String::new(),
        agent_type: AgentType::None,
        cwd: None,
        status: SessionStatus::Idle,
        active_tool: None,
        started_at: now,
        last_activity: now,
        recent_events: VecDeque::new(),
        tool_count: 0,
        last_user_prompt: None,
        first_prompts: Vec::new(),
        pane_id: Some("pane-x".to_string()),
        agent_id: None,
        display_name: None,
        pending_permission_tool: None,
        shell_synthetic_working: false,
        model: None,
        expects_agent_report: false,
    };
    let placeholder_on = render_card_for_mode_to_buffer(
        &placeholder,
        None,
        Some(1),
        density,
        0,
        false,
        UiMode::Normal,
        width,
        height,
        true,
    );
    let placeholder_text = buffer_to_text(&placeholder_on);
    assert_eq!(
        placeholder_text.matches("No agent").count(),
        1,
        "D4: a placeholder card must show `No agent` exactly once (its status \
         text) even with the badge toggle on — never a second, doubled \
         occurrence from a restored identity segment:\n{placeholder_text}"
    );
}

/// Scenario: PRD fork#378. Register a pane and apply a synthetic
/// `SessionStart` `AgentEvent` carrying `model: Some("Opus")` through the
/// real `AppState::apply_event` seam, then render the resulting session's
/// card with the badge toggle on and confirm it shows `ClaudeCode (Opus) ·
/// runtime-01`. Apply a second, later event carrying a DIFFERENT model
/// (`Some("Haiku")`) and confirm the rendered badge updates to `ClaudeCode
/// (Haiku) · runtime-01`, with no trace of the stale `Opus` value. Apply
/// a third event carrying no model at all (`None`) and confirm the badge
/// still reads `ClaudeCode (Haiku) · runtime-01` — most events don't
/// carry a model, and a `None` on one must not clear a previously-known one.
/// The session id is kept to 11 chars or fewer because the bare-id fallback
/// render in `src/ui.rs` hard-caps at 11 chars.
#[spec("dashboard/agent-badge/004")]
#[test]
fn agent_badge_004_model_updates_at_runtime_and_none_does_not_clear() {
    // PRD fork#378 reviewer/auditor round 2 (LOW 7 / Q4): SonarCloud's
    // `new_duplicated_lines_density` finding traced to this test's three
    // near-identical 15-field `AgentEvent` literals and 10-argument render
    // calls. Two local builders — mirroring `tests/rehydration.rs::live_011`'s
    // `hook_event`/`rollover_event`/`shell_event` pattern — factor out
    // everything this scenario holds fixed, leaving only what varies
    // (event type, elapsed offset, model) at each call site.

    /// A `runtime-01` / `pane-badge-model` `AgentEvent`, varying only the
    /// event type, the elapsed offset from `started`, and the model.
    fn event(
        started: chrono::DateTime<chrono::Utc>,
        offset_secs: i64,
        event_type: EventType,
        tool_name: Option<&str>,
        model: Option<&str>,
    ) -> AgentEvent {
        AgentEvent {
            session_id: "runtime-01".to_string(),
            agent_type: AgentType::ClaudeCode,
            event_type,
            tool_name: tool_name.map(str::to_string),
            tool_detail: None,
            cwd: None,
            timestamp: started + chrono::Duration::seconds(offset_secs),
            user_prompt: None,
            metadata: HashMap::new(),
            pane_id: Some("pane-badge-model".to_string()),
            agent_id: Some("agent-badge-model".to_string()),
            agent_version: None,
            schema_version: None,
            live_target: None,
            model: model.map(str::to_string),
        }
    }

    /// Render a session's card with the badge toggle on, at this scenario's
    /// fixed density/width/height.
    fn render(
        session: &SessionState,
        density: CardDensityKind,
        width: u16,
        height: u16,
    ) -> ratatui::buffer::Buffer {
        render_card_for_mode_to_buffer(
            session,
            None,
            Some(1),
            density,
            0,
            false,
            UiMode::Normal,
            width,
            height,
            true,
        )
    }

    let mut state = AppState::default();
    state.register_pane("pane-badge-model".to_string());
    let started = chrono::Utc::now();

    let width: u16 = 80;
    let density = CardDensityKind::Normal;
    let height = density.rendered_height();
    let claude_color = dot_agent_deck::agent_registry::spec(&AgentType::ClaudeCode).badge_color;

    state.apply_event(event(
        started,
        0,
        EventType::SessionStart,
        None,
        Some("Opus"),
    ));
    let session = state
        .sessions
        .get("runtime-01")
        .expect("the session exists after SessionStart")
        .clone();
    assert_eq!(
        session.model.as_deref(),
        Some("Opus"),
        "a SessionStart carrying model:Some(\"Opus\") must set SessionState.model"
    );
    let buffer = render(&session, density, width, height);
    // PRD fork#405 M1: row-scoped — the bracketed badge lives on the title
    // row, identity lives on the first body row.
    let title = row_text(&buffer, 0);
    let body0 = row_text(&buffer, 1);
    assert!(
        title.contains("ClaudeCode (Opus)"),
        "the badge must render the initial model on the title row:\n{title}"
    );
    assert!(
        body0.contains("runtime-01"),
        "identity must still render on the first body row:\n{body0}"
    );

    // A later event with a DIFFERENT model overwrites it.
    state.apply_event(event(started, 1, EventType::Thinking, None, Some("Haiku")));
    let session = state
        .sessions
        .get("runtime-01")
        .expect("the session still exists")
        .clone();
    assert_eq!(
        session.model.as_deref(),
        Some("Haiku"),
        "a later event carrying a DIFFERENT model must overwrite the known model"
    );
    let buffer = render(&session, density, width, height);
    let text = buffer_to_text(&buffer);
    let title = row_text(&buffer, 0);
    let body0 = row_text(&buffer, 1);
    assert!(
        title.contains("ClaudeCode (Haiku)"),
        "the badge must update to the new model on the title row:\n{title}"
    );
    assert!(
        body0.contains("runtime-01"),
        "identity must still render on the first body row:\n{body0}"
    );
    assert!(
        !text.contains("Opus"),
        "the stale model must not remain on the rendered badge:\n{text}"
    );

    // A later event carrying NO model must NOT clear the previously-known one.
    state.apply_event(event(started, 2, EventType::ToolStart, Some("Read"), None));
    let session = state
        .sessions
        .get("runtime-01")
        .expect("the session still exists")
        .clone();
    assert_eq!(
        session.model.as_deref(),
        Some("Haiku"),
        "an event carrying no model must NOT clear a previously-known model"
    );
    let buffer = render(&session, density, width, height);
    let title = row_text(&buffer, 0);
    let body0 = row_text(&buffer, 1);
    assert!(
        title.contains("ClaudeCode (Haiku)"),
        "the badge must keep showing the last-known model on the title row after a modelless \
         event:\n{title}"
    );
    assert!(
        body0.contains("runtime-01"),
        "identity must still render on the first body row:\n{body0}"
    );

    let has_bold_badge_cell = (0..buffer.area().height).any(|y| {
        (0..buffer.area().width).any(|x| {
            buffer[(x, y)].fg == claude_color && buffer[(x, y)].modifier.contains(Modifier::BOLD)
        })
    });
    assert!(
        has_bold_badge_cell,
        "the badge cell must carry ClaudeCode's registry badge color \
         {claude_color:?} AND Modifier::BOLD:\n{}",
        buffer_to_color_text(&buffer)
    );
}

/// Scenario: PRD fork#378 reviewer/auditor round 2 (MEDIUM 3 / F1 + F2),
/// premise rewritten by PRD fork#405 M1. Apply a `SessionStart` `AgentEvent`
/// whose model is a 300-character multi-byte (CJK) string followed by a
/// distinctive tail marker, through the real `AppState::apply_event` seam,
/// then render the card with the badge toggle on. Originally this test
/// existed because the badge (no clamp of its own) sat BEFORE the `· <id>`
/// identity segment in `truncate_styled_segments`'s shared left-to-right
/// title budget, so an unbounded model could consume the whole title and
/// push identity off it — that was the actual defect fork#378 fixed and
/// this test pinned. PRD fork#405 M1 moves identity off the title row
/// entirely, onto its own unconditional body row with its own width budget
/// (the full card inner width, not a slice of the title). Identity can no
/// longer be pushed off by ANY title content, unbounded model included — not
/// because it survives a truncation race, but because it is no longer in
/// that race at all. This test is rewritten to assert exactly that
/// structural immunity, rather than re-pointing the needle at a body row
/// and calling it done, which would keep the OLD assertion's shape
/// (identity "survives" competition) even though there is no longer any
/// competition to survive. The truncation cap itself — the raw tail marker
/// must not survive to the render — is retained and asserted on the title
/// row, since that half of fork#378's defect is unrelated to M1 and still
/// real. The 300 chars are all 3-byte UTF-8 characters, so any truncation
/// implemented as a byte-index slice rather than a char-boundary one panics
/// this test rather than merely failing an assertion.
#[spec("dashboard/agent-badge/005")]
#[test]
fn agent_badge_005_long_model_cannot_starve_the_identity_row() {
    let mut state = AppState::default();
    state.register_pane("pane-badge-long".to_string());
    let started = chrono::Utc::now();

    let long_model = format!("{}TAIL-MARKER-ZZZZ", "日".repeat(100));
    state.apply_event(AgentEvent {
        session_id: "long-id-01".to_string(),
        agent_type: AgentType::ClaudeCode,
        event_type: EventType::SessionStart,
        tool_name: None,
        tool_detail: None,
        cwd: None,
        timestamp: started,
        user_prompt: None,
        metadata: HashMap::new(),
        pane_id: Some("pane-badge-long".to_string()),
        agent_id: Some("agent-badge-long".to_string()),
        agent_version: None,
        schema_version: None,
        live_target: None,
        model: Some(long_model),
    });
    let session = state
        .sessions
        .get("long-id-01")
        .expect("the session exists after SessionStart")
        .clone();

    let width: u16 = 80;
    let density = CardDensityKind::Normal;
    let height = density.rendered_height();
    let buffer = render_card_for_mode_to_buffer(
        &session,
        None,
        Some(1),
        density,
        0,
        false,
        UiMode::Normal,
        width,
        height,
        true,
    );
    let text = buffer_to_text(&buffer);
    let title = row_text(&buffer, 0);
    let body0 = row_text(&buffer, 1);

    assert!(
        !text.contains("TAIL-MARKER-ZZZZ"),
        "an unbounded model must be truncated to a sane display cap — the \
         raw tail of a 300-char model must not survive to the render:\n{text}"
    );
    assert!(
        !title.contains("long-id-01"),
        "the title row must never carry identity at all, long model or not — it moved to the \
         body row (PRD fork#405 M1):\n{title}"
    );
    assert!(
        body0.contains("long-id-01"),
        "the identity row must show the full session id regardless of how long the model \
         badge is, since it has its own width budget and shares none with the title:\n{body0}"
    );
}

/// Scenario: PRD fork#378 reviewer/auditor round 2 (LOW / F5): apply a
/// `SessionStart` `AgentEvent` whose model embeds a Unicode `Cf` format
/// character (U+202E RIGHT-TO-LEFT OVERRIDE) through the real
/// `AppState::apply_event` seam, then render the card with the badge
/// toggle on. `src/terminal_sanitize.rs` exists for exactly this class —
/// the auditor confirmed ANSI/control (`Cc`) bytes are already filtered by
/// ratatui at two layers, so this does NOT re-test that; `Cf` chars pass
/// both ratatui filters unchanged and can visually reorder or conceal the
/// rest of the title row.
#[spec("dashboard/agent-badge/006")]
#[test]
fn agent_badge_006_model_with_format_chars_is_sanitized() {
    let mut state = AppState::default();
    state.register_pane("pane-badge-cf".to_string());
    let started = chrono::Utc::now();

    let hostile_model = "Sonnet\u{202e}-evil";
    state.apply_event(AgentEvent {
        session_id: "cf-id-01".to_string(),
        agent_type: AgentType::ClaudeCode,
        event_type: EventType::SessionStart,
        tool_name: None,
        tool_detail: None,
        cwd: None,
        timestamp: started,
        user_prompt: None,
        metadata: HashMap::new(),
        pane_id: Some("pane-badge-cf".to_string()),
        agent_id: Some("agent-badge-cf".to_string()),
        agent_version: None,
        schema_version: None,
        live_target: None,
        model: Some(hostile_model.to_string()),
    });
    let session = state
        .sessions
        .get("cf-id-01")
        .expect("the session exists after SessionStart")
        .clone();

    let width: u16 = 80;
    let density = CardDensityKind::Normal;
    let height = density.rendered_height();
    let buffer = render_card_for_mode_to_buffer(
        &session,
        None,
        Some(1),
        density,
        0,
        false,
        UiMode::Normal,
        width,
        height,
        true,
    );
    let text = buffer_to_text(&buffer);

    assert!(
        !text.contains('\u{202e}'),
        "a Cf format char (U+202E RIGHT-TO-LEFT OVERRIDE) in the model must \
         not reach the rendered title unsanitized:\n{text}"
    );
    let sanitized = dot_agent_deck::terminal_sanitize::sanitize_for_terminal_display(hostile_model);
    assert!(
        text.contains(&sanitized),
        "the model must be routed through the repo's terminal sanitizer \
         (src/terminal_sanitize.rs) before rendering:\n{text}"
    );
}

/// Scenario: Aggregate a busy mixed deck (14 Claude Code + 8 Codex sessions) and
/// render its stats bar at 60 columns — the width the bar actually gets, since it
/// draws into the last row of the left dashboard column whenever panes are open.
/// The bar must spend that width on the status counts and the `tools` total, with
/// no per-agent-type breakdown: the breakdown used to consume ~30 columns here and
/// silently clip the `tools` total off the right edge.
#[spec("dashboard/stats/001")]
#[test]
fn stats_001_narrow_bar_keeps_tools_total_and_omits_agent_breakdown() {
    let mut state = AppState::default();
    let panes = std::iter::repeat_n(AgentType::ClaudeCode, 14)
        .chain(std::iter::repeat_n(AgentType::Codex, 8))
        .enumerate();
    for (i, agent_type) in panes {
        let pane_id = format!("pane-{i:02}");
        state.register_pane(pane_id.clone());
        state.apply_event(AgentEvent {
            session_id: format!("mixed-{i:02}"),
            agent_type,
            event_type: EventType::SessionStart,
            tool_name: None,
            tool_detail: None,
            cwd: None,
            timestamp: chrono::Utc::now(),
            user_prompt: None,
            metadata: HashMap::new(),
            pane_id: Some(pane_id.clone()),
            agent_id: Some(format!("agent-{pane_id}")),
            agent_version: None,
            schema_version: None,
            live_target: None,
            model: None,
        });
    }

    let stats = state.aggregate_stats();
    assert_eq!(
        stats.active, 22,
        "all 22 mixed-agent sessions count as active"
    );

    let buffer = render_stats_bar_to_buffer(&stats, None, 60, 1);
    let rendered = buffer_to_text(&buffer);
    assert!(
        rendered.contains("22 active") && rendered.contains("tools"),
        "a narrow stats bar must still fit the active count AND the tools total:\n{rendered}"
    );
    assert!(
        !rendered.contains("ClaudeCode") && !rendered.contains("Codex"),
        "the stats bar must not spend its width on a per-agent-type breakdown \
         (every card already carries a registry-colored type badge):\n{rendered}"
    );
}

/// Scenario: Apply otherwise-identical live and history-only Codex session
/// events, then render their cards together through the dashboard `TestBackend`
/// seam. The history-only card must show a history marker and dim its numeric
/// input shortcut, while the live card keeps the normal writable affordance.
#[spec("dashboard/pane/009")]
#[test]
fn pane_009_history_only_card_marks_and_dims_input_affordance() {
    let session_from_event = |session_id: &str, pane_id: &str, writable: &str| {
        let event: AgentEvent = serde_json::from_value(serde_json::json!({
            "session_id": session_id,
            "agent_type": "codex",
            "event_type": "session_start",
            "timestamp": "2026-07-15T12:00:00Z",
            "pane_id": pane_id,
            "cwd": "/home/dev/workspace",
            "live_target": {
                "kind": if writable == "live" { "pty" } else { "process" },
                "writable": writable,
            }
        }))
        .expect("deserialize AgentEvent fixture");
        let mut state = AppState::default();
        state.apply_event(event);
        state
            .sessions
            .remove(session_id)
            .expect("SessionStart must create a dashboard session")
    };

    let live = session_from_event("live-01", "live-pane", "live");
    let history = session_from_event("resume-01", "resume-pane", "history-only");
    let width = 80;
    let density = CardDensityKind::Normal;
    let card_height = density.rendered_height();
    let buffer = render_dashboard_cards_to_buffer(
        &[(&live, None), (&history, None)],
        None,
        density,
        0,
        width,
    );
    let text = buffer_to_text(&buffer);
    let lines: Vec<&str> = text.lines().collect();
    let live_card = lines[..card_height as usize].join("\n").to_lowercase();
    let history_card = lines[card_height as usize..].join("\n").to_lowercase();
    let live_shortcut_dimmed = buffer[(2, 0)].modifier.contains(Modifier::DIM);
    let history_shortcut_dimmed = buffer[(2, card_height)].modifier.contains(Modifier::DIM);

    insta::assert_snapshot!(
        format!(
            "live_has_history_marker: {}\nhistory_has_history_marker: {}\nlive_shortcut_dimmed: {live_shortcut_dimmed}\nhistory_shortcut_dimmed: {history_shortcut_dimmed}",
            live_card.contains("history"),
            history_card.contains("history"),
        ),
        @r"
    live_has_history_marker: false
    history_has_history_marker: true
    live_shortcut_dimmed: false
    history_shortcut_dimmed: true"
    );
}

// ---------------------------------------------------------------------------
// PRD #155 — centralized color palette (Option A). Border encodes STATUS in
// BOTH deck cards and embedded panes; the dedicated `selected` (Magenta) and
// `focused` (Cyan) accent roles never reuse a status color. These tests read
// the resolved border color out of the rendered buffer (the observable
// end-state) so they survive the palette module's exact API — except
// `theme/guard/003`, which is a deliberate source lint.
// ---------------------------------------------------------------------------

/// Build a live (non-placeholder) session fixture carrying `status`. Used to
/// drive the deck-card border through `render_card_to_buffer` for every status
/// role. Its current activity time keeps the compact border value small; these
/// tests inspect only the border color, never the body.
fn palette_session(status: SessionStatus) -> SessionState {
    let now = chrono::Utc::now();
    SessionState {
        session_id: "sess-palette".to_string(),
        agent_type: AgentType::ClaudeCode,
        cwd: Some("/home/dev/example-project".to_string()),
        status,
        active_tool: None,
        started_at: now,
        last_activity: now,
        recent_events: VecDeque::new(),
        tool_count: 0,
        last_user_prompt: Some("do the thing".to_string()),
        first_prompts: vec!["do the thing".to_string()],
        pane_id: Some("pane-1".to_string()),
        agent_id: Some("1".to_string()),
        display_name: None,
        pending_permission_tool: None,
        shell_synthetic_working: false,
        model: None,
        expects_agent_report: false,
    }
}

/// Read the `(fg, modifier)` of a card/pane's left border at a mid-height row.
/// The `Borders::ALL` block paints the left edge (`│`, x=0) with the resolved
/// border style for every inner row, so a mid-height cell is the border color
/// regardless of title/content layout (same seam as `guard_001`).
fn border_style_at_mid(buffer: &ratatui::buffer::Buffer) -> (Color, Modifier) {
    let y = buffer.area().height / 2;
    let cell = &buffer[(0, y)];
    (cell.fg, cell.modifier)
}

/// Render a single deck card for a live agent in `status` (not selected, not
/// focused) and return its resolved border `(fg, modifier)`. The 80-cell width
/// leaves the border title ample room; height comes from the density tier so
/// geometry tracks the production layout module.
fn card_border_at_mid(status: SessionStatus) -> (Color, Modifier) {
    let session = palette_session(status);
    let width: u16 = 80;
    let density = CardDensityKind::Normal;
    let height = density.rendered_height();
    let buffer = render_card_to_buffer(
        &session,
        Some("example-agent"),
        Some(1),
        density,
        0,     // animation tick
        false, // not selected
        width,
        height,
    );
    border_style_at_mid(&buffer)
}

/// Render an embedded pane (`TerminalWidget`) and return its resolved border
/// `(fg, modifier)`. The outer 22×6 area gives a 20×4 inner area; the vt100
/// screen is sized to match so the 1:1 render contract holds (no size-mismatch
/// fallback). `status` is both carried in the pane title and threaded into the
/// widget via `TerminalWidget::with_status`, so the border resolves through the
/// centralized palette.
///
/// PRD #155 Option A: when the pane is neither selected nor focused, its border
/// encodes STATUS — the SAME role the deck card uses (`src/terminal_widget.rs`
/// resolves it from the centralized [`palette`]). With `status = None` the
/// widget keeps the legacy focus-only border (focused → Cyan, else dimmed).
///
/// Issue #88 follow-up: `input_active` is the `UiMode::PaneInput` bit. The Cyan
/// `focused` accent now requires it, so `focused=true, input_active=false`
/// (command mode) falls through to the status role and thickens the border
/// instead — read the glyph with [`border_glyph_at_mid`] to see that.
fn render_pane_border_buffer(
    status: Option<SessionStatus>,
    focused: bool,
    input_active: bool,
) -> ratatui::buffer::Buffer {
    let area = Rect {
        x: 0,
        y: 0,
        width: 22,
        height: 6,
    };
    // Inner area is 20×4; size the screen to match (no contract fallback).
    let parser = vt100::Parser::new(4, 20, 0);
    let parser = Arc::new(Mutex::new(parser));
    let title = match &status {
        Some(s) => format!("{s:?}"),
        None => "pane".to_string(),
    };
    let mut widget = TerminalWidget::new(parser, title, focused).with_input_active(input_active);
    if let Some(s) = status {
        widget = widget.with_status(s);
    }
    let mut buf = ratatui::buffer::Buffer::empty(area);
    widget.render(area, &mut buf);
    buf
}

/// Resolved `(fg, modifier)` of an embedded pane's border. See
/// [`render_pane_border_buffer`] for the fixture geometry.
fn pane_border_at_mid(
    status: Option<SessionStatus>,
    focused: bool,
    input_active: bool,
) -> (Color, Modifier) {
    border_style_at_mid(&render_pane_border_buffer(status, focused, input_active))
}

/// The border GLYPH at the same mid-height left-edge cell `border_style_at_mid`
/// samples. `BorderType::Plain` paints `│` there and `BorderType::Thick` paints
/// `┃`, so this is how a test tells the two border weights apart — the channel
/// that carries focus once colour is reserved for liveness (issue #88 follow-up).
fn border_glyph_at_mid(buffer: &ratatui::buffer::Buffer) -> String {
    let y = buffer.area().height / 2;
    buffer[(0, y)].symbol().to_string()
}

/// The six status roles in the centralized palette and the named-ANSI color each
/// must resolve to (PRD #155 locked plan): working=Green, thinking=Blue,
/// compacting=Blue (shares the thinking role), waiting=Yellow, error=Red,
/// idle=DarkGray. The single source of truth shared by the deck-card (T1) and
/// embedded-pane (T2) assertions.
fn status_role_colors() -> [(SessionStatus, Color); 6] {
    [
        (SessionStatus::Working, Color::Green),
        (SessionStatus::Thinking, Color::Blue),
        // Compacting is thinking-adjacent and shares the thinking/Blue role
        // (`palette::status_color` maps Compacting -> Blue) rather than
        // introducing a sixth status color — so it must render Blue in both the
        // deck card and the embedded pane, never an accent (Magenta/Cyan).
        (SessionStatus::Compacting, Color::Blue),
        (SessionStatus::WaitingForInput, Color::Yellow),
        (SessionStatus::Error, Color::Red),
        (SessionStatus::Idle, Color::DarkGray),
    ]
}

/// Scenario: Render a deck card for each of the six agent statuses
/// (working/thinking/compacting/waiting/error/idle), none selected or focused,
/// and assert the card's border color is the matching centralized status role —
/// working=Green, thinking=Blue, compacting=Blue (it shares the thinking role),
/// waiting=Yellow, error=Red, idle=DarkGray. Also assert each status border is a
/// status role and never an accent role (Magenta=selected, Cyan=focused), so a
/// status can never collide with selection/focus. This pins PRD #155 Option A:
/// the deck-card border encodes status via the centralized palette roles.
#[spec("theme/palette/001")]
#[test]
fn palette_001_deck_card_border_is_status_role() {
    for (status, role) in status_role_colors() {
        let (fg, _modifier) = card_border_at_mid(status.clone());
        assert_eq!(
            fg, role,
            "deck card border for {status:?} must use the centralized status role color \
             {role:?}, got {fg:?}"
        );
        // A status role must never collide with an accent role: Magenta is
        // `selected`, Cyan is `focused` (PRD #155 criterion #3). This catches a
        // status (notably Compacting) drifting onto an accent color.
        assert_ne!(
            fg,
            Color::Magenta,
            "status {status:?} border must not reuse the `selected` accent (Magenta)"
        );
        assert_ne!(
            fg,
            Color::Cyan,
            "status {status:?} border must not reuse the `focused` accent (Cyan)"
        );
    }
}

/// Scenario: For each of the six agent statuses
/// (working/thinking/compacting/waiting/error/idle), render the deck card AND an
/// embedded pane (neither selected nor focused) and assert the pane's border
/// color is the SAME as the deck card's for that status — and that both equal the
/// palette status role color. This is the consistency criterion: a given state
/// looks identical as a deck card and as an embedded pane, including compacting,
/// which shares the thinking/Blue role.
#[spec("theme/palette/002")]
#[test]
fn palette_002_pane_border_matches_deck_status_color() {
    for (status, role) in status_role_colors() {
        let card_fg = card_border_at_mid(status.clone()).0;
        let pane_fg = pane_border_at_mid(Some(status.clone()), false, true).0;
        assert_eq!(
            pane_fg, card_fg,
            "embedded pane border for {status:?} must match the deck card's status color \
             (deck={card_fg:?}, pane={pane_fg:?})"
        );
        assert_eq!(
            card_fg, role,
            "the shared status color for {status:?} must be the {role:?} palette role"
        );
    }
}

/// Scenario: Render a SELECTED deck card in **command mode** (`UiMode::Normal`)
/// for a Working agent and assert its border is the terminal's own foreground
/// (`Color::Reset`) rather than the working-status Green, carried together with
/// a thick `┃` glyph, `Modifier::BOLD` and the `▸ ` title marker — three cues,
/// none of which can be dimmed or camouflaged away. Also assert the border is
/// neither of the retired accents (Magenta) nor the focused-pane Cyan. This pins
/// the issue #442 rule that a selected card must be visible whatever its agent
/// is doing (`mode/deck/001` owns the PaneInput half of the recipe).
#[spec("theme/palette/003")]
#[test]
fn palette_003_selected_card_border_is_terminal_fg_thick_marker() {
    let session = palette_session(SessionStatus::Working);
    let width: u16 = 80;
    let density = CardDensityKind::Normal;
    let height = density.rendered_height();
    let buffer = render_card_for_mode_to_buffer(
        &session,
        Some("example-agent"),
        Some(1),
        density,
        0,    // animation tick
        true, // SELECTED
        UiMode::Normal,
        width,
        height,
        false,
    );
    let (fg, modifier) = border_style_at_mid(&buffer);
    assert_eq!(
        fg,
        Color::Reset,
        "a selected card's border must be the terminal's own foreground, which \
         contrasts on light and dark alike, got {fg:?}"
    );
    assert_ne!(
        fg,
        Color::Green,
        "a selected card must not rest on its status colour — for idle that would \
         be DarkGray, invisible on a dark background (issue #442)"
    );
    assert!(
        modifier.contains(Modifier::BOLD),
        "command-mode selection must be BOLD, got {modifier:?}"
    );
    assert!(
        !modifier.contains(Modifier::DIM),
        "selection must never DIM a card (issue #442), got {modifier:?}"
    );
    assert_eq!(
        buffer[(0, buffer.area().height / 2)].symbol(),
        "┃",
        "selection must also be carried by a thick border glyph"
    );
    assert_ne!(
        fg,
        Color::Magenta,
        "selection must no longer spend the retired Magenta accent"
    );
    assert_ne!(
        fg,
        Color::Cyan,
        "a card border must not reuse the focused-pane cyan"
    );
    assert!(
        buffer_to_text(&buffer).contains('▸'),
        "selected card must carry the `▸ ` selection title marker"
    );
}

/// Scenario: Render a FOCUSED, LIVE (`UiMode::PaneInput`) embedded pane and
/// assert its border is the dedicated `focused` accent role — Color::Cyan — and
/// that this color is distinct from every status role
/// (green/blue/yellow/red/dark-gray) and from the `selected` accent (magenta).
/// This pins the Option-A split that keeps focus on Cyan while selection moves
/// to Magenta, so status/selection/focus are provably distinct. Then render a
/// pane that is focused AND live AND carries a real `Working` status and assert
/// its border is still Cyan (the focused accent), NOT Green (the Working status
/// color) — locking the precedence invariant that focus OVERRIDES a present
/// status color while the pane is accepting input. (The command-mode half of
/// that precedence — focused but NOT live — is `theme/palette/005`.)
#[spec("theme/palette/004")]
#[test]
fn palette_004_focused_pane_border_is_cyan_distinct() {
    let (fg, _modifier) = pane_border_at_mid(None, true, true);
    assert_eq!(
        fg,
        Color::Cyan,
        "focused pane border must use the `focused` role (Color::Cyan), got {fg:?}"
    );
    for collide in [
        Color::Green,
        Color::Blue,
        Color::Yellow,
        Color::Red,
        Color::DarkGray,
        Color::Magenta,
    ] {
        assert_ne!(
            fg, collide,
            "the focused accent (cyan) must be distinct from the {collide:?} status/selection role"
        );
    }

    // PRECEDENCE: focus must win over a PRESENT status color. The case above
    // proves `focused -> Cyan` only when status is None; this constructs a pane
    // that is focused=true AND has a real `Working` status (whose own status
    // color is Green) and asserts the border still resolves to the focused
    // accent (Cyan), never the Working/Green status color — locking Option A's
    // "focused overrides status" rule in the unified border precedence.
    let (focused_with_status_fg, _modifier) =
        pane_border_at_mid(Some(SessionStatus::Working), true, true);
    assert_eq!(
        focused_with_status_fg,
        Color::Cyan,
        "a focused pane with a present `Working` status must still use the `focused` \
         accent (Color::Cyan), got {focused_with_status_fg:?}"
    );
    assert_ne!(
        focused_with_status_fg,
        Color::Green,
        "focus must OVERRIDE a present status: the border must not fall back to the \
         Working-status Green when the pane is focused"
    );
}

/// Scenario: Render the SAME focused embedded pane twice — once live
/// (`UiMode::PaneInput`, keystrokes reach it) and once in command mode
/// (`Ctrl+D` pressed, keys drive the deck) — and assert the two are visually
/// distinguishable. Live keeps the Cyan `focused` accent on a thin `│` border;
/// command mode drops the accent to the agent's own `Working`/Green status color
/// and thickens the border to `┃`, so colour reports whether keystrokes land
/// here while thickness still reports which pane is focused.
#[spec("theme/palette/005")]
#[test]
fn palette_005_command_mode_focused_pane_drops_cyan_accent() {
    let working = Some(SessionStatus::Working);

    // Live: unchanged from palette_004 — Cyan accent, thin border.
    let live = render_pane_border_buffer(working.clone(), true, true);
    let (live_fg, _) = border_style_at_mid(&live);
    assert_eq!(
        live_fg,
        Color::Cyan,
        "a focused pane that is ACCEPTING INPUT must keep the `focused` accent (Cyan), \
         got {live_fg:?}"
    );

    // Command mode: the accent is withheld, so the border reports what the
    // agent is DOING instead of falsely advertising "type here".
    let parked = render_pane_border_buffer(working.clone(), true, false);
    let (parked_fg, _) = border_style_at_mid(&parked);
    assert_eq!(
        parked_fg,
        Color::Green,
        "a focused pane in COMMAND MODE must fall through to its agent's status role \
         (Working=Green), got {parked_fg:?}"
    );
    assert_ne!(
        parked_fg,
        Color::Cyan,
        "command mode must NOT render the `focused` accent — that cyan border is the \
         signal that keystrokes reach the pane, and in command mode they do not"
    );

    // The two modes must be tellable apart at all — the whole point of the
    // change. A regression that reinstates the accent unconditionally trips here
    // even if the assertions above are somehow satisfied.
    assert_ne!(
        live_fg, parked_fg,
        "live and command mode must resolve to DIFFERENT border colors, both were {live_fg:?}"
    );

    // FOCUS SURVIVES: thickness takes over the job colour just gave up, so a
    // multi-pane mode tab still shows which pane `Ctrl+D` / `Enter` returns to.
    let live_glyph = border_glyph_at_mid(&live);
    let parked_glyph = border_glyph_at_mid(&parked);
    assert_eq!(
        live_glyph, "│",
        "a live focused pane keeps the thin (Plain) border, got {live_glyph:?}"
    );
    assert_eq!(
        parked_glyph, "┃",
        "a focused pane in command mode must thicken its border so focus stays legible \
         once the cyan accent is gone, got {parked_glyph:?}"
    );

    // An UNFOCUSED pane must not thicken in either mode — thickness means
    // "focused", so it has to stay exclusive to the focused pane.
    for input_active in [true, false] {
        let unfocused = render_pane_border_buffer(working.clone(), false, input_active);
        let glyph = border_glyph_at_mid(&unfocused);
        assert_eq!(
            glyph, "│",
            "an unfocused pane must keep the thin border (input_active={input_active}), \
             got {glyph:?}"
        );
    }
}

/// Scenario: For every agent status, and in BOTH command and PaneInput mode,
/// render the same deck card selected and unselected and compare the two
/// borders. A selected card must be visible whatever its agent is doing: its
/// border takes the terminal's OWN foreground (`Color::Reset`) — near-white on a
/// dark theme, near-black on a light one — thickens from `│` to `┃`, and never
/// carries `Modifier::DIM`. The control in the same loop is that an UNSELECTED
/// card is untouched and still reports its status role, so an idle agent keeps
/// receding. Guards issue #442 both ways: the selected card must not fade
/// (the original DIM-magenta report) and must not inherit the low-contrast
/// `palette::STATUS_IDLE` (DarkGray), which on a dark background left a selected
/// idle card as invisible as an unselected one however thick its border was.
#[spec("theme/palette/006")]
#[test]
fn palette_006_selection_is_visible_at_every_status() {
    let width: u16 = 80;
    let density = CardDensityKind::Normal;
    let height = density.rendered_height();

    for (status, role) in status_role_colors() {
        for mode in [UiMode::Normal, UiMode::PaneInput] {
            let session = palette_session(status.clone());
            let render = |selected: bool| {
                render_card_for_mode_to_buffer(
                    &session,
                    Some("example-agent"),
                    Some(1),
                    density,
                    0,
                    selected,
                    mode,
                    width,
                    height,
                    false,
                )
            };
            let unselected = render(false);
            let selected = render(true);
            let y = height / 2;
            let (unsel, sel) = (&unselected[(0, y)], &selected[(0, y)]);
            let context = format!("{status:?} in {mode:?}");

            // The reported symptom: a selected card whose border colour is the
            // status colour is only as visible as that status colour, and
            // `STATUS_IDLE` on a dark ground is barely visible at all. The
            // terminal's own foreground is the one colour guaranteed to contrast
            // with the terminal's own background, on either theme.
            assert_eq!(
                sel.fg,
                Color::Reset,
                "{context}: a selected card's border must use the terminal's own \
                 foreground so it contrasts on every theme, got {:?}",
                sel.fg
            );
            assert_ne!(
                sel.fg, role,
                "{context}: a selected border must not inherit the status colour — \
                 for idle that is DarkGray, which is invisible on a dark background \
                 no matter how thick the border is (issue #442)"
            );

            // CONTROL: selection changed, the resting state did not. An
            // unselected idle card must still recede exactly as before.
            assert_eq!(
                unsel.fg, role,
                "{context}: an UNSELECTED card must still report its status role"
            );
            assert_eq!(
                unsel.symbol(),
                "│",
                "{context}: an unselected card must keep the thin border"
            );

            assert_eq!(
                sel.symbol(),
                "┃",
                "{context}: selection must also thicken the border glyph"
            );
            assert!(
                !sel.modifier.contains(Modifier::DIM),
                "{context}: a selected border must never be DIM — that is what made \
                 it read as an idle card (issue #442), got {:?}",
                sel.style()
            );
        }
    }
}

/// Extract the source region of the top-level function whose signature contains
/// `signature` — from the signature to the start of the next top-level `fn` item.
/// The boundary is any column-0 `fn` / `pub fn` / `pub(crate) fn` (or any
/// `pub(...) fn`) declaration: requiring column 0 makes restricted-visibility
/// items like `pub(crate) fn` boundaries too (a missed `pub(crate) fn` between a
/// checked function and the next plain/`pub` fn would otherwise over-extend the
/// region), and it skips indented *nested* inner `fn`s so the region ends at this
/// function's sibling rather than terminating early at an inner helper.
/// Brace-counting is avoided on purpose: bodies here carry `format!` strings with
/// `{ }` placeholders that would skew a brace match. Panics with a clear message
/// if the anchor is missing so a rename surfaces as an explicit lint failure
/// rather than a silent skip.
fn fn_region<'a>(src: &'a str, signature: &str) -> &'a str {
    /// True if `s` begins (at column 0) with a top-level fn declaration:
    /// `fn `, `pub fn `, or any `pub(<vis>) fn ` (e.g. `pub(crate) fn `).
    fn is_top_level_fn_start(s: &str) -> bool {
        match s.strip_prefix("pub") {
            Some(after_pub) => {
                // `pub fn ...` (no visibility qualifier) or `pub(<vis>) fn ...`.
                let after_vis = match after_pub.strip_prefix('(') {
                    Some(inner) => match inner.find(')') {
                        Some(close) => &inner[close + 1..],
                        None => return false,
                    },
                    None => after_pub,
                };
                after_vis.starts_with(" fn ")
            }
            None => s.starts_with("fn "),
        }
    }

    let start = src
        .find(signature)
        .unwrap_or_else(|| panic!("source lint anchor `{signature}` not found"));
    let rest = &src[start..];
    // The first newline whose following line (column 0) starts a sibling fn ends
    // the region; that newline index is the region's exclusive upper bound.
    let next_fn = rest
        .match_indices('\n')
        .find(|&(i, _)| is_top_level_fn_start(&rest[i + 1..]))
        .map(|(i, _)| i)
        .unwrap_or(rest.len());
    &rest[..next_fn]
}

// Unit-guard for `fn_region` itself (not a `#[spec]` catalog entry): the real
// `guard_003` anchors never sit next to a `pub(crate) fn` or contain a nested
// inner fn, so these boundary paths would otherwise go unexercised. Pins that the
// extracted region (a) reaches content *after* an indented nested inner fn rather
// than terminating early, and (b) stops at a following `pub(crate) fn` sibling
// rather than over-extending past it.
#[test]
fn fn_region_handles_nested_and_restricted_visibility_boundaries() {
    let src = concat!(
        "fn checked() {\n",
        "    fn inner() {\n",
        "        let _ = Color::Magenta;\n", // nested-fn body — must stay INSIDE
        "    }\n",
        "    let _ = Color::Green;\n", // after the nested fn — region must reach it
        "}\n",
        "pub(crate) fn sibling() {\n",
        "    let _ = Color::Yellow;\n", // pub(crate) sibling — must be OUTSIDE
        "}\n",
        "fn last() {}\n",
    );
    let region = fn_region(src, "fn checked");
    assert!(
        region.contains("Color::Magenta"),
        "region must include a nested inner fn's body:\n{region}"
    );
    assert!(
        region.contains("Color::Green"),
        "region must reach content after a nested inner fn (not terminate early):\n{region}"
    );
    assert!(
        !region.contains("Color::Yellow"),
        "region must stop at the following `pub(crate) fn` sibling, not over-extend:\n{region}"
    );
}

/// Scenario: Source-lint the deck-card render path (`src/ui.rs`) and the
/// embedded-pane render path (`src/terminal_widget.rs`): both must reference the
/// centralized `palette`, the deck-card status mapping (`status_style`) and
/// border resolver (`render_session_card`) must carry no inline status/accent
/// `Color::Green/Blue/Yellow/Red/Cyan` literals, and — since PRD fork#405 M1
/// added the palette's one deliberate non-named exception, `palette::ROLE_NAME
/// = Color::Indexed(130)` — no inline `Color::Indexed` literal either: an
/// inline 256-cube index in the card path would be exactly as much of a
/// single-source-of-truth violation as an inline `Color::Green`, since the
/// palette is supposed to be the only place that constant is spelled out. The
/// pane path must carry no inline status `Color::Green/Blue/Yellow/Red`
/// literal, and the stats bar (`render_stats_bar`) must carry no inline status
/// `Color::Green/Blue/Yellow/Red` literal — its non-status `Cyan`
/// (active-count) and `LightMagenta` (mode-label) accents stay legal. This is
/// the M4 tightening: the palette is the single source of truth for every
/// status color across all render paths.
#[spec("theme/guard/003")]
#[test]
fn guard_003_render_paths_use_palette_roles() {
    let ui_path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/ui.rs");
    let pane_path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/terminal_widget.rs");
    let ui = std::fs::read_to_string(ui_path).expect("src/ui.rs should be readable");
    let pane =
        std::fs::read_to_string(pane_path).expect("src/terminal_widget.rs should be readable");

    // (1) Both render paths reference the centralized palette (single source of
    //     truth), rather than carrying their own inline literals.
    assert!(
        ui.contains("palette"),
        "src/ui.rs must reference the centralized `palette` in its render paths"
    );
    assert!(
        pane.contains("palette"),
        "src/terminal_widget.rs (pane render path) must reference the centralized `palette`"
    );

    // (2) The deck-card status->color mapping carries no inline status/accent
    //     literals — every status color comes from the palette.
    let status_style = fn_region(&ui, "fn status_style");
    for lit in [
        "Color::Green",
        "Color::Blue",
        "Color::Yellow",
        "Color::Red",
        "Color::Cyan",
    ] {
        assert!(
            !status_style.contains(lit),
            "status_style still hardcodes inline `{lit}` — status colors must come from the palette"
        );
    }

    // (3) The deck-card border resolver carries no inline accent/status literal
    //     (notably the selection accent, formerly `Color::Cyan`), and no inline
    //     `Color::Indexed` either — the palette now owns one non-named colour
    //     (`palette::ROLE_NAME`, PRD fork#405 M1), so an inline 256-cube index
    //     in this path would be exactly as much of a single-source-of-truth
    //     violation as an inline `Color::Green`.
    let card = fn_region(&ui, "fn render_session_card");
    for lit in [
        "Color::Cyan",
        "Color::Green",
        "Color::Blue",
        "Color::Yellow",
        "Color::Red",
        "Color::Magenta",
        "Color::Indexed",
    ] {
        assert!(
            !card.contains(lit),
            "render_session_card still hardcodes inline `{lit}` — the border must resolve through the palette"
        );
    }

    // (4) The embedded-pane render path carries no inline status literal — the
    //     pane border's status colors must come from the palette (the
    //     consistency criterion mirrored from the deck card).
    for lit in ["Color::Green", "Color::Blue", "Color::Yellow", "Color::Red"] {
        assert!(
            !pane.contains(lit),
            "src/terminal_widget.rs still hardcodes inline `{lit}` — pane status colors must come from the palette"
        );
    }

    // (5) The stats bar's per-status segments carry no inline STATUS literal —
    //     every status color (working/thinking/compacting/waiting/error) routes
    //     through `palette::status_color`. Only the four status roles are
    //     checked here: `render_stats_bar` legitimately keeps the non-status
    //     `Color::Cyan` (active-count header) and `Color::LightMagenta`
    //     (mode label) accents, which are NOT status roles and must stay.
    let stats = fn_region(&ui, "fn render_stats_bar");
    for lit in ["Color::Green", "Color::Blue", "Color::Yellow", "Color::Red"] {
        assert!(
            !stats.contains(lit),
            "render_stats_bar still hardcodes inline `{lit}` — status segment colors must come from the palette"
        );
    }
}

/// Scenario: Build three dashboard session cards and a
/// `Tab::Dashboard` whose `selected_session_id` points at the
/// **second** card (`sess-beta`), then derive the highlighted index
/// via `ui::sync_and_derive_selection` and render the three cards
/// stacked into a `TestBackend` buffer with `insta`. The derived
/// index is 1 (not 0), so the snapshot shows the `▸` selection
/// marker and highlighted border on the second card while the first
/// and third stay unselected — proving the dashboard highlight
/// follows the stable selected session id (PRD #83 M3) rather than
/// defaulting to card 0.
#[spec("dashboard/pane/005")]
#[test]
fn pane_005_highlight_follows_selected_session_id() {
    // PRD #83 catalog: dashboard/pane/005 — the dashboard card
    // highlight must follow the per-tab `selected_session_id` (the M3
    // fix), not snap back to the top card. We render three cards and
    // point the selection at the 2nd, so a regression that ignored the
    // stable id (highlighting card 0) would visibly diff the snapshot.
    //
    // All sessions share one current activity time, keeping their compact
    // bottom-border elapsed values identical in the snapshot. `last_activity`
    // is nudged 30s into the future of that shared instant rather than left
    // equal to it — see the identical rationale in `pane_004_card_title_row`
    // above (issue #15): the render-time `Last:` field races the wall clock
    // when seeded at exactly `now`, and `format_elapsed`'s clamp to zero for a
    // negative delta (src/ui.rs) makes a forward offset the safe fix.
    let now = chrono::Utc::now();
    let make = |sid: &str, pane: &str, name: &str, cwd: &str| SessionState {
        session_id: sid.to_string(),
        agent_type: AgentType::ClaudeCode,
        cwd: Some(cwd.to_string()),
        status: SessionStatus::Working,
        active_tool: Some(ActiveTool {
            name: "Read".to_string(),
            detail: Some("src/main.rs".to_string()),
        }),
        started_at: now,
        last_activity: now + chrono::Duration::seconds(30),
        recent_events: VecDeque::new(),
        tool_count: 3,
        last_user_prompt: Some("do the thing".to_string()),
        first_prompts: vec!["do the thing".to_string()],
        pane_id: Some(pane.to_string()),
        agent_id: Some(name.to_string()),
        display_name: None,
        pending_permission_tool: None,
        shell_synthetic_working: false,
        model: None,
        expects_agent_report: false,
    };
    let s1 = make("sess-alpha", "pane-1", "1", "/home/dev/alpha");
    let s2 = make("sess-beta", "pane-2", "2", "/home/dev/beta");
    let s3 = make("sess-gamma", "pane-3", "3", "/home/dev/gamma");

    // Render-order filtered list of (session_id, pane_id) pairs — the
    // shape `sync_and_derive_selection` resolves the selection against.
    let filtered: [(&str, Option<&str>); 3] = [
        ("sess-alpha", Some("pane-1")),
        ("sess-beta", Some("pane-2")),
        ("sess-gamma", Some("pane-3")),
    ];

    // Per-tab selection pinned to the 2nd card's session id. With no
    // focused pane, the derived index resolves purely from the stable
    // session id — and must be 1, not 0.
    let mut tab = Tab::Dashboard {
        selected_session_id: Some("sess-beta".to_string()),
    };
    let selected_index = sync_and_derive_selection(&mut tab, None, &filtered, None)
        .expect("dashboard derives an index");
    assert_eq!(
        selected_index, 1,
        "selection must follow the stable session id to the 2nd card"
    );

    let cards: [(&SessionState, Option<&str>); 3] = [
        (&s1, Some("example-alpha")),
        (&s2, Some("example-beta")),
        (&s3, Some("example-gamma")),
    ];
    let buffer = render_dashboard_cards_to_buffer(
        &cards,
        // PRD #113: the renderer now takes an active/inactive `Option<usize>`;
        // `Some(idx)` paints the highlight on that card.
        Some(selected_index),
        CardDensityKind::Normal,
        0, // animation tick
        80,
    );
    insta::assert_snapshot!(buffer_to_text(&buffer));
}

/// Scenario: A pane is spawned (tagged placeholder card) and then a legacy hook
/// reports on it without an `agent_id`. The dashboard must still draw exactly
/// ONE card for that pane — before issue #398 the untagged event minted a second
/// session and the deck rendered two cards for the one pane.
#[spec("dashboard/pane/010")]
#[test]
fn pane_010_untagged_event_keeps_one_card_on_the_pane() {
    // Issue #398. The bug lived in `AppState::apply_event`, but what the user
    // actually saw was the card grid, so this drives the REAL state transition
    // and renders whatever it produces — rather than handing the renderer a
    // card list built by the test, which would assert nothing about the defect.
    const PANE: &str = "2";

    let mut state = AppState::default();
    state.register_pane(PANE.to_string());
    // The pane as it exists right after a daemon spawn: one tagged placeholder.
    state.insert_placeholder_session(
        PANE.to_string(),
        Some("/home/dev/worker".to_string()),
        Some(AgentType::ClaudeCode),
        Some("agent-42".to_string()),
    );

    // A pre-F9 / un-envied hook reports on the same pane, naming no generation.
    state.apply_event(AgentEvent {
        session_id: "legacy-hook-session".to_string(),
        agent_type: AgentType::ClaudeCode,
        event_type: EventType::WaitingForInput,
        tool_name: None,
        tool_detail: None,
        cwd: None,
        timestamp: chrono::Utc::now(),
        user_prompt: None,
        metadata: HashMap::new(),
        pane_id: Some(PANE.to_string()),
        agent_id: None,
        agent_version: None,
        schema_version: None,
        live_target: None,
        model: None,
    });

    // The card list the dashboard would build for this pane.
    let on_pane: Vec<&SessionState> = state
        .sessions
        .values()
        .filter(|s| s.pane_id.as_deref() == Some(PANE))
        .collect();
    assert_eq!(
        on_pane.len(),
        1,
        "one pane must yield one card; {} sessions claim pane {PANE}",
        on_pane.len()
    );

    // And the surviving card carries the status the hook reported, so the fix
    // suppressed a duplicate rather than suppressing the update itself.
    assert_eq!(
        on_pane[0].status,
        SessionStatus::WaitingForInput,
        "the single card must show the status the untagged hook reported"
    );

    let cards: [(&SessionState, Option<&str>); 1] = [(on_pane[0], Some("worker"))];
    let buffer = render_dashboard_cards_to_buffer(&cards, Some(0), CardDensityKind::Normal, 0, 80);
    let rendered = buffer_to_text(&buffer);
    // The status badge is per-card, so counting it counts cards — and keeps the
    // assertion readable without committing another snapshot file.
    assert_eq!(
        rendered.matches("Needs Input").count(),
        1,
        "exactly one card should render for the pane:\n{rendered}"
    );
}

/// Extract row `y` of `buffer` as a plain string, one visual cell per
/// character. PRD fork#405 M1 moves the role name from the card's border
/// title (row 0) onto the card's first BODY row — the whole point of the
/// change is *placement*, so a whole-buffer `contains` would pass just as
/// happily with the name still in the title. Row-scoping is what actually
/// pins the new layout.
fn row_text(buffer: &ratatui::buffer::Buffer, y: u16) -> String {
    let area = buffer.area();
    (0..area.width)
        .map(|x| buffer[(x, y)].symbol().to_string())
        .collect()
}

/// The foreground color of `needle`'s first character within row `y` of
/// `buffer`. Panics if `needle` isn't found on that row, by design: callers
/// assert placement (the row contains the needle) before calling this, so a
/// panic here means that placement assertion should have already failed —
/// this makes a missing-row bug surface as a clear panic message rather than
/// silently reading the wrong cell.
///
/// ASCII rows only — see the wide-glyph caveat: `row_text` maps one buffer
/// CELL to one `char`/string, but ratatui stores a wide (2-cell) glyph's
/// symbol in cell `x` and an empty symbol in `x+1`. So `row[..byte_start]
/// .chars().count()` undercounts the column for any needle that appears
/// after a wide glyph on the row (each wide glyph consumes one cell of
/// column but only one `char` of the count). Every current caller matches
/// ASCII on a row with no wide glyphs, so this is latent, not live — but
/// this helper sits next to `agent_badge_005`, whose whole fixture is CJK,
/// so a future caller reaching for it there would get a wrong column.
fn row_needle_fg(buffer: &ratatui::buffer::Buffer, y: u16, needle: &str) -> Color {
    let row = row_text(buffer, y);
    let byte_start = row
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not found on row {y}: {row:?}"));
    let col = row[..byte_start].chars().count() as u16;
    buffer[(col, y)].fg
}

/// Scenario: PRD fork#405 M1. Render the same ClaudeCode/Opus session with
/// the agent-type badge toggle ON and OFF. Badge ON: the title row (row 0)
/// must carry `ClaudeCode (Opus)` and must NOT carry the display name; the
/// first body row (row 1) must carry the display name in
/// `palette::ROLE_NAME` (`Color::Indexed(130)`). Badge OFF (the default):
/// the title row must carry neither the badge nor the name, while the body
/// row must STILL carry the name in the same colour — the unconditional
/// half, and the single most likely thing to regress, so it is asserted
/// explicitly and independently of the badge-on case. Also asserts
/// `Color::Indexed(130)` is distinct from every status role colour and every
/// registry `badge_color`, and that a card with `display_name: None` falls
/// back to `id_display` on the same row. Snapshots the badge-on/badge-off
/// pair with `buffer_to_color_text` so a future colour or placement
/// regression shows up as a snapshot diff too. Finally, a card with BOTH
/// `display_name: None` and an empty `session_id` renders a blank identity
/// row (`Line::from("")`) that still occupies exactly one row — the `Dir:`
/// row directly below it lands on row 2 either way — pinning the height
/// guarantee the PRD names for the both-empty branch.
#[spec("dashboard/pane/012")]
#[test]
fn pane_012_role_name_on_its_own_body_row() {
    let role_name = dot_agent_deck::palette::ROLE_NAME;
    assert_eq!(
        role_name,
        Color::Indexed(130),
        "PRD fork#405 M1 chose Indexed(130) on measured contrast — see src/palette.rs"
    );

    let now = chrono::Utc::now();
    let session = SessionState {
        session_id: "sess-role-line-01".to_string(), // >11 chars: exercises truncation elsewhere, not this fixture's own fallback path
        agent_type: AgentType::ClaudeCode,
        cwd: Some("/home/dev/workspace".to_string()),
        status: SessionStatus::Working,
        active_tool: None,
        started_at: now,
        last_activity: now,
        recent_events: VecDeque::new(),
        tool_count: 0,
        last_user_prompt: Some("inspect the repository".to_string()),
        first_prompts: vec!["inspect the repository".to_string()],
        pane_id: Some("pane-role-1".to_string()),
        agent_id: Some("1".to_string()),
        display_name: Some("example-coder".to_string()),
        pending_permission_tool: None,
        shell_synthetic_working: false,
        model: Some("Opus".to_string()),
        expects_agent_report: false,
    };
    let width: u16 = 80;
    let density = CardDensityKind::Normal;
    // Height comes from the density tier itself (not a hardcoded number) so
    // this test's buffer geometry tracks `card_height()` automatically once
    // the coder updates it — the same convention `pane_004`/`pane_006` use.
    let height = density.rendered_height();

    // --- Badge ON ---------------------------------------------------------
    let on = render_card_for_mode_to_buffer(
        &session,
        Some("example-coder"),
        Some(1),
        density,
        0,
        false,
        UiMode::Normal,
        width,
        height,
        true, // show_agent_type_badge
    );
    let on_title = row_text(&on, 0);
    let on_body0 = row_text(&on, 1);
    assert!(
        on_title.contains("ClaudeCode (Opus)"),
        "badge ON: the title row must carry the type/model badge:\n{on_title}"
    );
    assert!(
        !on_title.contains("example-coder"),
        "badge ON: the title row must NOT carry the display name — identity moved to the body \
         row:\n{on_title}"
    );
    assert!(
        on_body0.contains("example-coder"),
        "badge ON: the first body row must carry the display name:\n{on_body0}"
    );
    assert_eq!(
        row_needle_fg(&on, 1, "example-coder"),
        role_name,
        "badge ON: the display name on the body row must render in palette::ROLE_NAME \
         (Color::Indexed(130))"
    );
    // PRD fork#405 delta-review (reviewer D4): F1's separating space between
    // the badge text and the border fill is a `Reset/Reset`-styled
    // whitespace run, which `buffer_to_color_text` elides on flush — so the
    // snapshots above cannot pin it; the only trace there is the fill
    // losing one dash. Assert the raw cell directly so a regression that
    // drops the space again fails loudly instead of as a one-character
    // snapshot diff.
    assert!(
        on_title.contains("ClaudeCode (Opus) \u{2500}"),
        "badge ON (live): the badge text must be followed by a separating space \
         before the border fill begins:\n{on_title}"
    );

    // --- Badge OFF (the default) ------------------------------------------
    // This is the unconditional half: the role row must NOT ride the same
    // Ctrl+M toggle the model/badge segment does.
    let off = render_card_for_mode_to_buffer(
        &session,
        Some("example-coder"),
        Some(1),
        density,
        0,
        false,
        UiMode::Normal,
        width,
        height,
        false, // show_agent_type_badge
    );
    let off_title = row_text(&off, 0);
    let off_body0 = row_text(&off, 1);
    assert!(
        !off_title.contains("ClaudeCode") && !off_title.contains("example-coder"),
        "badge OFF: the title row must carry neither the badge nor the name:\n{off_title}"
    );
    assert!(
        off_body0.contains("example-coder"),
        "badge OFF: the first body row must STILL carry the display name — this row is \
         unconditional, it does not ride the badge toggle:\n{off_body0}"
    );
    assert_eq!(
        row_needle_fg(&off, 1, "example-coder"),
        role_name,
        "badge OFF: the display name on the body row must still render in palette::ROLE_NAME \
         even though the title's badge is hidden"
    );

    insta::assert_snapshot!("pane_012_role_name_badge_on", buffer_to_color_text(&on));
    insta::assert_snapshot!("pane_012_role_name_badge_off", buffer_to_color_text(&off));

    // --- Colour distinctness ------------------------------------------------
    for (status, role_color) in status_role_colors() {
        assert_ne!(
            role_name, role_color,
            "palette::ROLE_NAME must be distinct from the {status:?} status role colour \
             {role_color:?}"
        );
    }
    for agent_type in [
        AgentType::ClaudeCode,
        AgentType::OpenCode,
        AgentType::Pi,
        AgentType::Codex,
        AgentType::Devin,
        AgentType::None,
    ] {
        let badge_color = dot_agent_deck::agent_registry::spec(&agent_type).badge_color;
        assert_ne!(
            role_name, badge_color,
            "palette::ROLE_NAME must be distinct from {agent_type:?}'s registry badge_color \
             {badge_color:?}"
        );
    }

    // --- id_display fallback -------------------------------------------------
    // `session_id` is already "sess-role-line-01" (17 chars) from the clone —
    // no reassignment needed, and it's already >11 chars, so id_display's
    // truncation to the first 11 ("sess-role-l") is exercised below.
    let mut unnamed = session.clone();
    unnamed.display_name = None;
    let unnamed_buf = render_card_for_mode_to_buffer(
        &unnamed,
        None,
        Some(1),
        density,
        0,
        false,
        UiMode::Normal,
        width,
        height,
        false,
    );
    let unnamed_body0 = row_text(&unnamed_buf, 1);
    assert!(
        unnamed_body0.contains("sess-role-l"), // first 11 chars of the session id
        "a card with display_name: None must fall back to id_display on the identity row:\n\
         {unnamed_body0}"
    );
    assert_eq!(
        row_needle_fg(&unnamed_buf, 1, "sess-role-l"),
        role_name,
        "the id_display fallback must still render in palette::ROLE_NAME"
    );

    // --- Empty identity: the height guarantee --------------------------------
    // PRD fork#405 M1 pushes the identity row UNCONDITIONALLY, emitting
    // `Line::from("")` when both `display_name` and `session_id` are empty, so
    // the emitted line count never diverges from what `card_height()` reserves.
    // Untested until now (auditor N2). Close to unreachable in production —
    // `insert_placeholder_session` always synthesizes `pane-{pane_id}` as the
    // session id — but the branch is real code with no other coverage.
    let mut empty_identity = session.clone();
    empty_identity.session_id = String::new();
    empty_identity.display_name = None;
    let empty_buf = render_card_for_mode_to_buffer(
        &empty_identity,
        None,
        Some(1),
        density,
        0,
        false,
        UiMode::Normal,
        width,
        height,
        false,
    );
    let empty_body0 = row_text(&empty_buf, 1);
    // `row_text` reads the WHOLE row including the card's own left/right
    // border cells, which are never whitespace — crop those off before
    // checking blankness, or this can never pass regardless of content.
    let empty_body0_inner: String = {
        let chars: Vec<char> = empty_body0.chars().collect();
        chars
            .get(1..chars.len().saturating_sub(1))
            .unwrap_or_default()
            .iter()
            .collect()
    };
    assert!(
        empty_body0_inner.trim().is_empty(),
        "the identity row must render blank (`Line::from(\"\")`) when both display_name and \
         session_id are empty:\n{empty_body0}"
    );
    let empty_dir_row = row_text(&empty_buf, 2);
    assert!(
        empty_dir_row.contains("Dir:"),
        "the blank identity row must still occupy exactly ONE row — with the height guarantee \
         intact, `Dir:` lands on row 2 whether or not identity has any text to show:\n\
         {empty_dir_row}"
    );
}

/// Build one event-rich session fixture that fills `render_session_card`
/// to capacity at every density tier: three user prompts (Spacious shows 3,
/// Normal/Compact show the latest 1) and three `ToolStart` events
/// (Normal/Spacious show 3, Compact shows 1). With this fixture the rendered
/// card carries its maximum content lines on every tier, so any rows left
/// blank inside the border are reserved-but-unused — exactly what
/// `dashboard/density/004` asserts must not happen.
///
/// `last_activity = now` keeps the compact bottom-border elapsed value small;
/// these tests inspect only blank/non-blank rows, so its exact value is
/// irrelevant.
fn filled_session() -> SessionState {
    let now = chrono::Utc::now();
    let mut events: VecDeque<AgentEvent> = VecDeque::new();
    for prompt in ["first prompt", "second prompt", "third prompt"] {
        events.push_back(AgentEvent {
            session_id: "sess-fill".to_string(),
            agent_type: AgentType::ClaudeCode,
            event_type: EventType::Thinking,
            tool_name: None,
            tool_detail: None,
            cwd: None,
            timestamp: now,
            user_prompt: Some(prompt.to_string()),
            metadata: HashMap::new(),
            pane_id: None,
            agent_id: None,
            agent_version: None,
            schema_version: None,
            live_target: None,
            model: None,
        });
    }
    for (name, detail) in [
        ("Read", "src/main.rs"),
        ("Edit", "src/ui.rs"),
        ("Bash", "cargo test"),
    ] {
        events.push_back(AgentEvent {
            session_id: "sess-fill".to_string(),
            agent_type: AgentType::ClaudeCode,
            event_type: EventType::ToolStart,
            tool_name: Some(name.to_string()),
            tool_detail: Some(detail.to_string()),
            cwd: None,
            timestamp: now,
            user_prompt: None,
            metadata: HashMap::new(),
            pane_id: None,
            agent_id: None,
            agent_version: None,
            schema_version: None,
            live_target: None,
            model: None,
        });
    }
    SessionState {
        session_id: "sess-fill".to_string(),
        agent_type: AgentType::ClaudeCode,
        cwd: Some("/home/dev/example-project".to_string()),
        status: SessionStatus::Working,
        active_tool: Some(ActiveTool {
            name: "Bash".to_string(),
            detail: Some("cargo test".to_string()),
        }),
        started_at: now,
        last_activity: now,
        recent_events: events,
        tool_count: 12,
        last_user_prompt: Some("third prompt".to_string()),
        first_prompts: vec!["first prompt".to_string()],
        pane_id: Some("pane-1".to_string()),
        agent_id: Some("1".to_string()),
        display_name: None,
        pending_permission_tool: None,
        shell_synthetic_working: false,
        model: None,
        expects_agent_report: false,
    }
}

/// Count inner card rows that are blank starting from the row directly above
/// the bottom border and scanning upward, stopping at the first row that
/// carries content. Inner columns only (`1..width-1`) so the left/right border
/// `│` glyphs are ignored. The result is the number of reserved-but-empty rows
/// the card holds below its real content — `0` means the reserved card height
/// equals the rendered content height. The mid-card blank separator line
/// (Normal/Spacious) is never counted because the scan stops at the tool line
/// rendered beneath it.
fn trailing_blank_inner_rows(buffer: &ratatui::buffer::Buffer) -> usize {
    let area = buffer.area();
    let (w, h) = (area.width, area.height);
    if h < 3 || w < 3 {
        return 0;
    }
    let mut count = 0usize;
    let mut y = h - 2; // last inner row (the one just above the bottom border)
    loop {
        let row_blank = (1..w - 1).all(|x| buffer[(x, y)].symbol().trim().is_empty());
        if row_blank {
            count += 1;
        } else {
            break;
        }
        if y == 1 {
            break;
        }
        y -= 1;
    }
    count
}

/// Scenario: Render a single fully-populated session card at each density tier
/// (Compact, Normal, Spacious) in a wide 80-column viewport sized to the tier's
/// own `rendered_height`, then assert the card has zero trailing blank rows —
/// the row directly above the bottom border carries rendered content on every
/// tier. This locks in PRD #147's content-derived `card_height`: the reserved
/// height equals the lines `render_session_card` actually emits (5/8/10),
/// so no row is reserved-but-empty. Before #147 the heights were hardcoded
/// larger than the content (Compact 7 rows for 3 content lines, Normal 9 for 6,
/// Spacious 11 for 8), which would trip this assertion as trailing blank rows.
#[spec("dashboard/density/004")]
#[test]
fn density_004_no_trailing_blank_rows() {
    // PRD #147 M2: reserved card height must equal rendered content height, so
    // a card shows no empty rows below its content at any tier. The fixture
    // fills every tier to capacity (3 prompts + 3 tools); we render at width 80
    // with the stats in the bottom border at the tier's declared height, then
    // count trailing blank inner rows — which must be 0.
    let session = filled_session();
    let width: u16 = 80;
    for (density, label) in [
        (CardDensityKind::Compact, "Compact"),
        (CardDensityKind::Normal, "Normal"),
        (CardDensityKind::Spacious, "Spacious"),
    ] {
        let height = density.rendered_height();
        let buffer = render_card_to_buffer(
            &session,
            Some("example-coder"),
            Some(1),
            density,
            0,     // animation tick
            false, // not selected
            width,
            height,
        );
        let trailing = trailing_blank_inner_rows(&buffer);
        assert_eq!(
            trailing,
            0,
            "{label} card reserves {trailing} blank row(s) below its content \
             (height={height}); reserved height must match rendered content:\n{}",
            buffer_to_text(&buffer)
        );
    }
}

/// Turn the fully-populated [`filled_session`] fixture into a long-idle one:
/// same 3 prompts and 3 tool calls, `Idle` status, and a `last_activity` an
/// hour in the past. This is exactly the state the removed ASCII-art overlay
/// used to `Clear` and paint over at Spacious density.
fn filled_idle_session() -> SessionState {
    SessionState {
        status: SessionStatus::Idle,
        active_tool: None,
        last_activity: chrono::Utc::now() - chrono::Duration::hours(1),
        display_name: Some("example-coder".to_string()),
        ..filled_session()
    }
}

/// Scenario: Render a long-idle, fully-populated session card at Spacious
/// density at the flash-on tick and again at the flash-off tick, and render the
/// same session at Normal density for comparison. The Spacious card must keep
/// all of its ordinary content — three prompt lines, three tool lines, the dir
/// line and the title — and carry an `Idle` badge whose leading dot is inked at
/// tick 0 and blank at tick 30, exactly as Normal renders it.
#[spec("dashboard/density/005")]
#[test]
fn density_005_spacious_idle_shows_flashing_dot_over_card_content() {
    // Issue #519: Spacious used to be the ONE density where an idle card could
    // stop showing card content — `render_session_card` ran `Clear` over the
    // inner area and painted generated ASCII-art frames on top. That branch is
    // gone, so all three densities now share the flashing-dot indicator. This
    // pins the surviving behavior at the density that used to diverge; without
    // it the only Spacious assertion left is `density_004`'s trailing-blank-row
    // count, which a re-introduced full-area overlay would still satisfy.
    let session = filled_idle_session();
    let width: u16 = 80;
    let density = CardDensityKind::Spacious;

    let render_at = |tick: u64, density: CardDensityKind| {
        buffer_to_text(&render_card_to_buffer(
            &session,
            Some("example-coder"),
            Some(1),
            density,
            tick,
            false, // not selected
            width,
            density.rendered_height(),
        ))
    };

    // `flash_dot` inks the dot for the first half of each 60-tick period and
    // blanks it for the second, so tick 0 and tick 30 straddle one flash.
    let flash_on = render_at(0, density);
    let flash_off = render_at(30, density);

    assert!(
        flash_on.contains("● Idle"),
        "Spacious idle card should ink the status dot at the flash-on tick:\n{flash_on}"
    );
    assert!(
        !flash_off.contains("● Idle") && flash_off.contains("Idle"),
        "Spacious idle card should blank the status dot at the flash-off tick, \
         keeping the Idle label:\n{flash_off}"
    );

    // The overlay used to Clear the inner area, so content presence — not just
    // the badge — is what distinguishes the fallback from a re-introduced art
    // pane. Spacious is the tier that shows the fixture at full capacity.
    for (label, rendered) in [("flash-on", &flash_on), ("flash-off", &flash_off)] {
        for needle in [
            "first prompt",
            "second prompt",
            "third prompt",
            "Read",
            "Edit",
            "Bash",
            "example-project",
            "example-coder",
        ] {
            assert!(
                rendered.contains(needle),
                "{label} Spacious idle card should still show {needle:?}:\n{rendered}"
            );
        }
    }

    // Same indicator as the densities that never had an art path, so the three
    // tiers cannot drift apart again.
    assert!(
        render_at(0, CardDensityKind::Normal).contains("● Idle"),
        "Normal density should render the same idle indicator as Spacious"
    );
    assert!(
        render_at(0, CardDensityKind::Compact).contains("● Idle"),
        "Compact density should render the same idle indicator as Spacious"
    );

    assert_eq!(
        flash_on.lines().count(),
        density.rendered_height() as usize,
        "the Spacious card should occupy exactly its declared height"
    );
    insta::assert_snapshot!(flash_on);
}

/// Replicate `choose_density`'s tier selection using the public
/// `rendered_height` seam: pick the largest tier whose stacked card rows fit
/// in `avail`, falling back to Compact. Mirrors `src/ui.rs::choose_density` so
/// the orchestration capacity test tracks the same selection the renderer uses
/// without depending on the private function.
fn pick_density(n_decks: usize, cols: usize, avail: u16) -> CardDensityKind {
    let rows = n_decks.div_ceil(cols.max(1)) as u16;
    for density in [
        CardDensityKind::Spacious,
        CardDensityKind::Normal,
        CardDensityKind::Compact,
    ] {
        if rows * density.rendered_height() <= avail {
            return density;
        }
    }
    CardDensityKind::Compact
}

/// Row heights dividing `total_available` across `total_rows` as evenly as
/// possible — mirrors `even_row_heights` (PRD fork#446 M1, `src/ui.rs`,
/// beside `fit_grid`) without depending on the private function, the same
/// mirroring `pick_density` above does for `choose_density`. The first
/// `total_available % total_rows` rows get one extra row over the floor of
/// `total_available / total_rows`. `None` only when `total_available` can't
/// give every row at least 1.
fn even_row_heights_mirror(total_rows: usize, total_available: u16) -> Option<Vec<u16>> {
    if (total_available as usize) < total_rows {
        return None;
    }
    let total_rows_u16 = total_rows as u16;
    let base = total_available / total_rows_u16;
    let remainder = (total_available % total_rows_u16) as usize;
    Some(
        (0..total_rows)
            .map(|i| if i < remainder { base + 1 } else { base })
            .collect(),
    )
}

/// Scenario: Model the single-column orchestration deck card area (the ~34%
/// width column → one grid column) at several card-area heights, and compute
/// the renderer's own capacity (`visible_rows = available / card_height`) for
/// 7 decks; assert all 7 fit without scrolling and that the 7th deck actually
/// renders in the visible slice, at every height tried — not pinned to one
/// assumed terminal size. This locks in PRD #147: with the content-derived
/// Compact height of 6 (PRD fork#405 M1 added the unconditional role-name
/// row, moving Compact height 5 → 6), 48 / 6 = 8 rows fit so all 7 decks
/// show. Before #147 the hardcoded Compact height of 7 fit only 48 / 7 = 6
/// rows, dropping the 7th deck to a scroll — the regression this test guards
/// against.
///
/// A much larger deck count (20, at AVAILABLE=48) used to be this test's
/// "over-correction guard" — scrolling had to still engage past Compact's
/// capacity. PRD fork#446 (even card division) inverts that on purpose: no
/// card is ever hidden, so at 20 decks the column instead divides evenly
/// (`even_row_heights`) and every deck still renders, degraded to a shorter
/// M2 card tier rather than dropped to a scroll.
#[spec("orchestration/layout/001")]
#[test]
fn layout_001_seven_decks_fit_single_column() {
    // PRD #147 M2: on the orchestration tab decks stack in one column and
    // density bottoms out at Compact. The renderer fits
    // `visible_rows = available_for_density / card_height` rows (src/ui.rs
    // ~7861); the rest scroll. We mirror that math through the public
    // `rendered_height` seam, then confirm it with a real card render.
    const COLS: usize = 1;
    const DECKS: usize = 7;

    // Generalised beyond a single terminal size (PRD fork#446): 42 is the
    // exact 7*6 boundary, 48 is the size the test used to pin alone, 54 and
    // 90 give slack on top. The property must hold at all of them, not just
    // the one AVAILABLE=48 fork#405 M1 left as the tightest-possible margin.
    for available in [42u16, 48, 54, 90] {
        let density = pick_density(DECKS, COLS, available);
        let card_height = density.rendered_height();
        let visible_rows = (available / card_height).max(1) as usize;

        // (1) Capacity: all 7 decks must fit in the single column with no scrolling.
        assert!(
            visible_rows >= DECKS,
            "all {DECKS} orchestration decks must fit without scrolling at \
             card-area height {available} (density={density:?}, card_height={card_height}); \
             only {visible_rows} fit"
        );

        // (2) Buffer proof: the orchestration renderer draws only the first
        //     `visible_rows` of the deck rows (src/ui.rs ~7871); render that visible
        //     slice as real cards and assert the 7th deck shows. If the cards were
        //     too tall to all fit, the 7th would be scrolled off and absent.
        let visible = visible_rows.min(DECKS);
        let session = filled_session();
        let names: Vec<String> = (1..=visible).map(|i| format!("deck-{i}")).collect();
        let cards: Vec<(&SessionState, Option<&str>)> =
            names.iter().map(|n| (&session, Some(n.as_str()))).collect();
        // PRD #113: `selected` is `Option<usize>`; this capacity test highlights
        // nothing, so pass `None` (the old out-of-range `usize::MAX` sentinel).
        let buffer = render_dashboard_cards_to_buffer(&cards, None, density, 0, 64);
        let text = buffer_to_text(&buffer);
        assert!(
            text.contains("deck-7"),
            "the 7th orchestration deck must render in the visible card area \
             at card-area height {available} (rendered {visible} of {DECKS} decks):\n{text}"
        );
    }

    // (3) INVERTED ON PURPOSE (PRD fork#446 — see the "An existing guard is
    //     inverted on purpose" section of prds/fork-446-even-card-division.md):
    //     a deck count well past Compact's old capacity must no longer engage
    //     scrolling. At 20 decks and AVAILABLE=48 (48 / 20 = 2, 48 % 20 = 8),
    //     even division gives the first 8 rows 3 rows and the remaining 12
    //     rows 2 rows, summing to exactly 48 — every deck renders, each at
    //     M2's shortest bordered tier (>= 2 rows), none dropped to a scroll.
    const MANY: usize = 20;
    const AVAILABLE_MANY: u16 = 48;
    let heights = even_row_heights_mirror(MANY, AVAILABLE_MANY)
        .expect("48 rows for 20 decks must divide — at least 1 row per deck");
    assert_eq!(heights.len(), MANY);
    assert_eq!(
        heights.iter().sum::<u16>(),
        AVAILABLE_MANY,
        "row heights {heights:?} must sum to exactly {AVAILABLE_MANY} — no blank tail"
    );

    let session = filled_session();
    let names: Vec<String> = (1..=MANY).map(|i| format!("deck-{i}")).collect();
    let mut rendered_all = String::new();
    for (i, height) in heights.iter().enumerate() {
        let card_number = if i < 9 { Some((i + 1) as u8) } else { None };
        let buffer = render_card_to_buffer(
            &session,
            Some(names[i].as_str()),
            card_number,
            CardDensityKind::Compact,
            0,
            false,
            64,
            *height,
        );
        rendered_all.push_str(&buffer_to_text(&buffer));
        rendered_all.push('\n');
    }
    for name in &names {
        assert!(
            rendered_all.contains(name.as_str()),
            "deck {name} must render — no orchestration deck is ever hidden \
             once the column divides evenly (heights={heights:?}):\n{rendered_all}"
        );
    }
}

/// The longest prefix of `s` that fits in `max` bytes without splitting a
/// character, computed by walking characters rather than by slicing bytes —
/// deliberately a different algorithm from the production truncator, so the
/// expectation is an independent statement of what a char-boundary cut is
/// rather than a restatement of the code under test.
///
/// Deliberately duplicated in `tests/mouse_dispatch.rs` rather than shared: see
/// the note there.
fn char_boundary_prefix(s: &str, max: usize) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if out.len() + ch.len_utf8() > max {
            break;
        }
        out.push(ch);
    }
    out
}

/// Expand `s` the way a ratatui buffer holds it: a double-width character
/// occupies two cells, and `buffer_to_text` joins the second (padding) cell in
/// as a space. Without this a sweep over wide characters would compare the
/// rendered title against a string the renderer never produces.
fn as_rendered_cells(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        out.push(ch);
        for _ in 1..ch.to_string().width().max(1) {
            out.push(' ');
        }
    }
    out
}

/// Scenario: A hook posts a session whose id is nine `α` characters, so byte 11
/// — where the card title cuts the id — falls inside a character. Render that
/// card between two healthy ones: the deck must draw all three cards with the
/// Unicode id cut on a character boundary, instead of panicking the render loop
/// and taking every other agent's card down with it. Repeated for 2-, 3- and
/// 4-byte characters at every offset that puts byte 11 mid-character.
#[spec("dashboard/pane/011")]
#[test]
fn pane_011_multibyte_session_id_renders_the_whole_deck() {
    // Issue #574. `&session.session_id[..11]` is a BYTE index, and `str`
    // indexing panics when it lands inside a character. The id is
    // producer-controlled — `apply_event` keys the session map on whatever the
    // hook socket sent, with no length, charset or boundary check anywhere in
    // between — so this drives the real state transition first and renders
    // whatever it produces, rather than hand-building the card list.
    //
    // The blast radius is the point: the slice runs inside the render loop, so
    // one bad id killed EVERY frame. That is why the assertion is made on a
    // three-card deck and not on the poisoned card alone.
    const POISONED_ID: &str = "ααααααααα"; // 9 chars x 2 bytes = 18; byte 11 is mid-char
    assert!(
        !POISONED_ID.is_char_boundary(11),
        "the fixture only reproduces the defect if byte 11 splits a character"
    );

    let mut state = AppState::default();
    state.register_pane("2".to_string());
    state.apply_event(AgentEvent {
        session_id: POISONED_ID.to_string(),
        agent_type: AgentType::ClaudeCode,
        event_type: EventType::WaitingForInput,
        tool_name: None,
        tool_detail: None,
        cwd: Some("/home/dev/worker".to_string()),
        timestamp: chrono::Utc::now(),
        user_prompt: None,
        metadata: HashMap::new(),
        pane_id: Some("2".to_string()),
        agent_id: None,
        agent_version: None,
        schema_version: None,
        live_target: None,
        model: None,
    });
    let poisoned = state
        .sessions
        .get(POISONED_ID)
        .expect("apply_event keys the session map on the producer's id verbatim")
        .clone();

    // Two ordinary neighbours, so a panic in the middle card is visible as the
    // loss of cards that have nothing wrong with them.
    let mut healthy_a = poisoned.clone();
    healthy_a.session_id = "sess-alpha".to_string();
    healthy_a.pane_id = Some("1".to_string());
    let mut healthy_b = poisoned.clone();
    healthy_b.session_id = "sess-gamma".to_string();
    healthy_b.pane_id = Some("3".to_string());

    // No display name on the poisoned card: the id is what the title renders
    // only when the card has no friendlier label.
    let cards: [(&SessionState, Option<&str>); 3] = [
        (&healthy_a, Some("example-alpha")),
        (&poisoned, None),
        (&healthy_b, Some("example-gamma")),
    ];
    let rendered = buffer_to_text(&render_dashboard_cards_to_buffer(
        &cards,
        Some(0),
        CardDensityKind::Normal,
        0,
        80,
    ));

    // One status badge per card, so counting badges counts surviving cards.
    assert_eq!(
        rendered.matches("Needs Input").count(),
        3,
        "one bad session id must not cost the other agents their cards:\n{rendered}"
    );
    let expected = format!(
        "{}…",
        as_rendered_cells(&char_boundary_prefix(POISONED_ID, 11))
    );
    assert!(
        rendered.contains(&expected),
        "the title must cut the id on a character boundary and mark the cut, expected `{expected}`:\n{rendered}"
    );
    assert!(
        rendered.contains("example-alpha") && rendered.contains("example-gamma"),
        "both neighbouring cards must still render their titles:\n{rendered}"
    );

    // Property sweep: every character width, at every ASCII offset that moves
    // the cut inside a character. `session_id` is arbitrary bytes from the
    // socket, so no single fixture proves the render is boundary-safe.
    for filler in ['α', 'あ', '𝄞', '😀'] {
        for pad in 0..4usize {
            let id: String = "x".repeat(pad) + &filler.to_string().repeat(12);
            let mut session = poisoned.clone();
            session.session_id = id.clone();
            let card = buffer_to_text(&render_card_to_buffer(
                &session,
                None,
                Some(1),
                CardDensityKind::Normal,
                0,
                false,
                80,
                CardDensityKind::Normal.rendered_height(),
            ));
            let want = format!("{}…", as_rendered_cells(&char_boundary_prefix(&id, 11)));
            assert!(
                card.contains(&want),
                "id `{id}` must render as `{want}`:\n{card}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// dashboard/grid — the card grid's joint column/density layout (issue #588)
// ---------------------------------------------------------------------------

/// The seven roles of the orchestration that reported issue #588, in the order
/// its `.dot-agent-deck.toml` listed them. Positions 6 and 7 — `researcher` and
/// `documenter` — are the two that were never painted.
const REPORTED_ROLES: [&str; 7] = [
    "orchestrator",
    "developer",
    "tester",
    "reviewer",
    "releaser",
    "researcher",
    "documenter",
];

/// One live role card. `last_activity` is nudged 30s into the future for the
/// reason `pane_004_card_title_row` documents: `Last:` is computed from
/// `Utc::now()` at render time, so an equal timestamp sits on the `0s`/`1s`
/// boundary and tips snapshots under parallel test load.
fn role_session(index: usize, role: &str) -> SessionState {
    let now = chrono::Utc::now();
    SessionState {
        session_id: format!("sess-role-{index:02}"),
        agent_type: AgentType::ClaudeCode,
        cwd: Some("/home/dev/dot-agent-deck".to_string()),
        status: SessionStatus::Idle,
        active_tool: None,
        started_at: now,
        last_activity: now + chrono::Duration::seconds(30),
        recent_events: VecDeque::new(),
        tool_count: 0,
        last_user_prompt: None,
        first_prompts: Vec::new(),
        pane_id: Some(format!("pane-role-{index:02}")),
        agent_id: Some(format!("agent-role-{index:02}")),
        display_name: Some(role.to_string()),
        shell_synthetic_working: false,
        pending_permission_tool: None,
        model: None,
        expects_agent_report: false,
    }
}

fn reported_role_sessions() -> Vec<SessionState> {
    REPORTED_ROLES
        .iter()
        .enumerate()
        .map(|(i, role)| role_session(i, role))
        .collect()
}

fn as_cards(sessions: &[SessionState]) -> Vec<(&SessionState, Option<&str>)> {
    sessions
        .iter()
        .map(|s| (s, s.display_name.as_deref()))
        .collect()
}

/// How many cards the buffer actually shows: each card contributes exactly one
/// top-left corner — `┌`, or `┏` when it is selected and drawn thick.
///
/// Counted from the drawn cells rather than asked of the layout code, so every
/// assertion built on it is a statement about what reached the screen.
fn drawn_cards(rendered: &str) -> usize {
    rendered
        .lines()
        .map(|line| line.matches('┌').count() + line.matches('┏').count())
        .sum()
}

/// How many card columns the buffer shows, from the widest card row. Same
/// corner-counting witness as [`drawn_cards`], per row instead of in total.
fn drawn_columns(rendered: &str) -> usize {
    rendered
        .lines()
        .map(|line| line.matches('┌').count() + line.matches('┏').count())
        .max()
        .unwrap_or(0)
}

/// Scenario: Render the reported seven-role orchestration into a 90x27 deck —
/// too short for seven single-column cards at any density — and assert every one
/// of the seven role names is on screen. A 90x60 control deck, which already
/// fitted them before the fix, renders the same seven.
#[spec("dashboard/grid/001")]
#[test]
fn grid_001_short_deck_still_paints_every_role() {
    let sessions = reported_role_sessions();
    let cards = as_cards(&sessions);

    // The reported geometry. 27 rows less the title and stats-bar rows leaves
    // 25 for cards; seven single-column Compact cards need 7*5 = 35, so the old
    // width-only column choice sliced to the five rows that fit and painted
    // `orchestrator … releaser`, dropping `researcher` and `documenter`. Two
    // columns need only 7.div_ceil(2) * 5 = 20 rows, and 90 columns is two cards
    // of 45 — comfortably past MIN_CARD_W.
    let (buffer, _) = render_card_grid_to_buffer(&cards, Some(0), 0, 90, 27);
    let rendered = buffer_to_text(&buffer);

    for role in REPORTED_ROLES {
        assert!(
            rendered.contains(role),
            "every role of a seven-role orchestration must be painted on a 90x27 deck; \
             `{role}` is missing:\n{rendered}"
        );
    }
    assert_eq!(
        drawn_cards(&rendered),
        REPORTED_ROLES.len(),
        "no card may be sliced off when a layout exists that fits them all:\n{rendered}"
    );
    assert_eq!(
        drawn_columns(&rendered),
        2,
        "fitting seven cards in 25 rows takes a second column:\n{rendered}"
    );
    insta::assert_snapshot!(rendered);

    // Control: the same seven roles on a deck tall enough that one column always
    // fitted them. If this one failed too, the fixture — not the height — would
    // be what the assertion above is really measuring.
    let (tall_buffer, _) = render_card_grid_to_buffer(&cards, Some(0), 0, 90, 60);
    let tall = buffer_to_text(&tall_buffer);
    for role in REPORTED_ROLES {
        assert!(
            tall.contains(role),
            "control: a tall deck must still paint `{role}`:\n{tall}"
        );
    }
    assert_eq!(
        drawn_columns(&tall),
        1,
        "control: a deck that already fits one column must keep one column — \
         widening is only ever spent on completeness:\n{tall}"
    );
    assert_eq!(drawn_cards(&tall), REPORTED_ROLES.len());
}

/// Scenario: Sweep deck geometries from one column to an escalated two, and for
/// each compare `UiState::columns` — the value left/right card navigation reads
/// — against the number of card columns actually drawn in the buffer. Selection
/// movement and the rendered grid must never disagree about the column count.
#[spec("dashboard/grid/002")]
#[test]
fn grid_002_nav_columns_match_the_drawn_grid() {
    let sessions = reported_role_sessions();

    // (cards, width, height, what the case exercises)
    let cases: [(usize, u16, u16, &str); 6] = [
        (3, 90, 40, "narrow deck, everything fits in one column"),
        (
            7,
            90,
            60,
            "narrow deck, tall enough for seven single-column cards",
        ),
        (
            7,
            90,
            27,
            "narrow deck escalated to two columns to fit all seven",
        ),
        (7, 120, 27, "wide deck already at two columns"),
        (7, 200, 20, "three columns from width alone"),
        (
            7,
            90,
            12,
            "nothing fits — the grid stays sliced at one column",
        ),
    ];

    for (count, width, height, what) in cases {
        let cards = as_cards(&sessions[..count]);
        let (buffer, probe) = render_card_grid_to_buffer(&cards, Some(0), 0, width, height);
        let rendered = buffer_to_text(&buffer);
        assert_eq!(
            probe.nav_columns,
            drawn_columns(&rendered),
            "{what}: card navigation reads {} columns while {} were drawn at {width}x{height} \
             — arrow-key movement would step somewhere the user is not looking:\n{rendered}",
            probe.nav_columns,
            drawn_columns(&rendered),
        );
    }
}

/// Scenario: Render seven roles into a 90x12 deck, too small for them at any
/// column count the width allows, and assert the two cards that do fit are
/// accompanied by a `(↓5)` marker in the title. A hidden role must never be
/// silently indistinguishable from a role that failed to start.
#[spec("dashboard/grid/003")]
#[test]
fn grid_003_unavoidable_overflow_is_signalled() {
    let sessions = reported_role_sessions();
    let cards = as_cards(&sessions);

    // PRD fork#446 M1: before this feature, a layout that didn't fit at the
    // joint-fit-seeking density simply truncated to a scrolled window at the
    // PREFERRED column count. Now the render path first tries widening to
    // `max_columns_for_width` and dividing the available height evenly across
    // rows (`even_row_heights`) — so genuine, unavoidable overflow now
    // requires height so small that even ONE row per (widened) row-of-cards
    // doesn't fit: `even_row_heights` returns `None` only when
    // `available_height < total_rows`. At width 90, `max_columns_for_width`
    // is 2, so 7 cards need `7.div_ceil(2) = 4` row-slots; height 5 leaves
    // `available_for_cards = 3 < 4`, which is still genuinely unavoidable —
    // and the widened 2-column count is what the old scroll/truncate
    // fallback then renders at.
    let (buffer, _) = render_card_grid_to_buffer(&cards, Some(0), 0, 90, 5);
    let rendered = buffer_to_text(&buffer);
    let title = rendered.lines().next().expect("a rendered title row");

    assert_eq!(
        drawn_cards(&rendered),
        2,
        "fixture must genuinely overflow — five of the seven cards unpainted — \
         or this test proves nothing:\n{rendered}"
    );
    assert!(
        title.contains("(↓5)"),
        "a sliced grid must count the cards it is not showing in its title:\n{title}"
    );
    assert!(
        title.contains("7 session(s)"),
        "the title must still name the full role count:\n{title}"
    );
    assert_eq!(
        drawn_columns(&rendered),
        2,
        "PRD fork#446: the joint fit-seeking failing no longer keeps the \
         preferred column count as-is — it widens to `max_columns_for_width` \
         first (attempting even row division), and that widened count is \
         what the truncating fallback renders at even when the division \
         itself still can't make everything fit:\n{rendered}"
    );
    insta::assert_snapshot!(rendered);

    // Scrolled to the bottom: the marker must flip to counting what is above.
    let (scrolled, _) = render_card_grid_to_buffer(&cards, Some(6), 0, 90, 5);
    let scrolled_title = buffer_to_text(&scrolled)
        .lines()
        .next()
        .expect("a rendered title row")
        .to_string();
    assert!(
        scrolled_title.contains("(\u{2191}6)"),
        "selecting the last card (index 6) at the widened 2-column layout \
         scrolls six cards above the window (row 3 of 4, one card per \
         visible row), and the title must say so:\n{scrolled_title}"
    );
}
