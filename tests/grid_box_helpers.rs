//! Fast-tier guards for the harness's shared box-drawing grid helpers
//! (`common::BORDER_WEIGHTS` and the predicates built on it).
//!
//! The helpers guarded here exist because a LOOSER form of each shipped first
//! and was satisfied by the wrong thing:
//!
//! - `label_in_box_top_border` replaced
//!   `grid.lines().any(|l| l.contains('┌') && l.contains(label))`, which an
//!   AGENT could satisfy — on an Orchestration tab the sidebar's deck cards and
//!   the focused role's live terminal share rows, so a row holding a UUID-titled
//!   card's top-left corner also holds whatever that terminal printed to its
//!   right, including every role name.
//! - `orchestration_pane_column` replaced a whole-grid search whose row-join
//!   spliced sidebar card text BETWEEN adjacent wrapped pane rows, breaking any
//!   needle that straddled the wrap — the root cause of `idle_worker_011` going
//!   red (issue #460).
//! - `card_identity_row` replaced `find_in_grid(<role>)` — "the first row with
//!   that text on it" — as the way to say WHERE a role's card is. The focused
//!   role's embedded pane is titled with its role name on a row above every
//!   card, so that lookup returned the pane header whenever the role being
//!   looked for was the focused one, and `session/restore/008` read a correctly
//!   ordered deck as reversed about one run in three (issue #809).
//!
//! `label_in_box_body_row` and `contains_word_token` are guarded here as well;
//! the tests for each say what they replaced.
//!
//! They live in the FAST tier, not beside their e2e callers, deliberately. Pure
//! string logic in a `tests/e2e_*.rs` file is gated `#![cfg(feature = "e2e")]`
//! and so runs only in the pre-PR tier a human triggers by hand (CLAUDE.md rule
//! 5) — the one guard against a regression here would fire nowhere in CI.
//! Nothing in this file spawns a PTY, a binary or a daemon, so there is no
//! reason for it to be gated at all (review of #465, S5).

mod common;

/// Every fixture below is a real rectangle: each row's right-hand glyph sits at
/// the same column as the top row's. `label_in_box_top_border` reads only the
/// top row and would not notice, but these constants read as documentation of a
/// rendered card, and a column-exact parser (`try_first_card` in
/// `tests/e2e_card_layout.rs`) requires alignment — so a fixture the renderer
/// could never produce would hand its next reader a confusing failure. Asserted
/// rather than merely eyeballed because two of them were silently 3 columns
/// adrift when written (review of #465, N1).
fn assert_is_rectangle(name: &str, grid: &str) {
    /// Column every fixture card below closes at.
    const RIGHT_EDGE: usize = 31;

    for (index, line) in grid.lines().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let opening = chars[0];
        let weight = common::BORDER_WEIGHTS
            .iter()
            .find(|w| [w.top_left, w.vertical, w.bottom_left].contains(&opening))
            .unwrap_or_else(|| {
                panic!("{name} row {index} does not open with a box glyph ({opening:?}):\n{grid}")
            });
        let closing = *chars.get(RIGHT_EDGE).unwrap_or_else(|| {
            panic!(
                "{name} row {index} is only {} scalars wide, so it cannot close its box at \
                 column {RIGHT_EDGE}:\n{grid}",
                chars.len()
            )
        });
        assert!(
            [weight.top_right, weight.vertical, weight.bottom_right].contains(&closing),
            "{name} row {index} closes at column {RIGHT_EDGE} with {closing:?}, which is not a \
             right-hand glyph of the weight it opened with ({opening:?}):\n{grid}"
        );
    }
}

/// Scenario: Run the shared card-title predicate against three hand-built grids
/// that reproduce what an Orchestration tab actually paints. The adversarial one
/// puts every role name in the live agent pane to the right of two UUID-titled
/// sidebar cards, and must NOT match `coder`; the plain, thick and double-weight
/// single-card grids each carry `coder` inside their own top border, and must.
#[test]
fn card_title_predicate_rejects_labels_outside_the_card_span() {
    const CODER_LABEL: &str = "ClaudeCode · coder";

    // The historical false pass: UUID-titled sidebar cards have a top-left
    // corner on rows where the adjacent live pane prints every role. The role
    // text is present on each row, but outside both cards' right edge.
    const FALSE_PASS_GRID: &str = "\
┌─ ClaudeCode · 6134822e-f2 ─┐  ┃ ClaudeCode · orchestrator ClaudeCode · coder ClaudeCode · reviewer
│ waiting                    │  ┃
└────────────────────────────┘  ┃
┏━ ClaudeCode · c15a2be1-77 ━┓  ┃ ClaudeCode · orchestrator ClaudeCode · coder ClaudeCode · reviewer
┃ working                    ┃  ┃
┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛  ┃";
    assert!(
        !common::label_in_box_top_border(FALSE_PASS_GRID, CODER_LABEL),
        "a role label in the agent pane must not make a UUID-titled sidebar card match\n\
         Adversarial grid:\n{FALSE_PASS_GRID}"
    );
    // The two cards' OWN titles still match, so the rejection above is the
    // span crop doing its job rather than the predicate failing outright.
    assert!(
        common::label_in_box_top_border(FALSE_PASS_GRID, "ClaudeCode · 6134822e-f2"),
        "the plain card's own title must still match:\n{FALSE_PASS_GRID}"
    );
    assert!(
        common::label_in_box_top_border(FALSE_PASS_GRID, "ClaudeCode · c15a2be1-77"),
        "the thick card's own title must still match:\n{FALSE_PASS_GRID}"
    );

    const PLAIN_CARD_GRID: &str = "\
┌─ ClaudeCode · coder ─────────┐  ┃ live agent output
│                              │  ┃
└──────────────────────────────┘  ┃";
    const THICK_CARD_GRID: &str = "\
┏━ ClaudeCode · coder ━━━━━━━━━┓  ┃ live agent output
┃                              ┃  ┃
┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛  ┃";
    // `BorderType::Double` is unreachable in `src/` today. Covered anyway
    // because `common::BORDER_WEIGHTS` deliberately lists it as forward
    // tolerance, and an untested table row is how the plain-vs-thick split that
    // caused #460 went unnoticed.
    const DOUBLE_CARD_GRID: &str = "\
╔═ ClaudeCode · coder ═════════╗  ┃ live agent output
║                              ║  ┃
╚══════════════════════════════╝  ┃";

    for (name, grid) in [
        ("PLAIN_CARD_GRID", PLAIN_CARD_GRID),
        ("THICK_CARD_GRID", THICK_CARD_GRID),
        ("DOUBLE_CARD_GRID", DOUBLE_CARD_GRID),
    ] {
        assert_is_rectangle(name, grid);
        assert!(
            common::label_in_box_top_border(grid, CODER_LABEL),
            "{name} carries {CODER_LABEL:?} inside its own top border:\n{grid}"
        );
    }
}

/// Scenario: Run the shared pane-column crop against three hand-built
/// Orchestration-tab grids. On an expanded grid of each border weight it finds
/// the `┌orchestrator` column and crops the sidebar away, so a needle that wraps
/// across two pane rows matches once squeezed — while the SAME needle does not
/// match the uncropped grid, because the row-join splices sidebar card text
/// between the two halves. On a grid whose panes are all collapsed it returns
/// `None` rather than a wrong column.
#[test]
fn orchestration_pane_crop_drops_the_sidebar_and_reports_a_missing_anchor() {
    // The wrap trap in miniature: the needle's two halves are on consecutive
    // PANE rows, and each of those rows also carries a sidebar card whose text
    // lands between them once the rows are joined.
    const NEEDLE: &str = "daemon report, not a message from a person";
    let expanded = |tl: char, v: char| {
        format!(
            "\
┌─ 1 ClaudeCode · orch… ─┐ {tl}orchestrator──────────────
│ working                │ {v} daemon report, not a
┌─ 2 ClaudeCode · worker ┐ {v} message from a person
│ idle                   │ {v}
"
        )
    };

    for weight in common::BORDER_WEIGHTS {
        let grid = expanded(weight.top_left, weight.vertical);
        let edge = common::orchestration_pane_left_edge(&grid).unwrap_or_else(|| {
            panic!(
                "no pane-column anchor found for the {:?} weight:\n{grid}",
                weight.top_left
            )
        });
        assert_eq!(
            edge, 27,
            "the {:?}-weight anchor is at column {edge}, not the sidebar boundary 27:\n{grid}",
            weight.top_left
        );

        let pane =
            common::orchestration_pane_column(&grid).expect("a located edge always yields a crop");
        assert!(
            !pane.contains("ClaudeCode"),
            "the crop must drop the sidebar's card text entirely:\n{pane}"
        );
        assert!(
            common::squeeze_wrapped_text(&pane).contains(&common::squeeze_wrapped_text(NEEDLE)),
            "the wrapped needle must be contiguous once the CROPPED rows are squeezed and \
             joined:\n{pane}"
        );
        // The regression this crop exists for: without it the sidebar's own text
        // is spliced between the needle's two halves, so the whole-grid search
        // misses a needle that is fully on screen.
        assert!(
            !common::squeeze_wrapped_text(&grid).contains(&common::squeeze_wrapped_text(NEEDLE)),
            "the UNCROPPED grid must still miss the needle — otherwise this fixture no longer \
             reproduces the splice that made the crop necessary:\n{grid}"
        );
    }

    // Every pane collapsed: a collapsed `Stacked` pane draws `Borders::TOP` with
    // a padded title and NO corner glyph, so there is no anchor to crop on. The
    // helper must say so rather than guess a column — the whole point of S4's
    // split failure reporting is that this state is NOT "the needle is absent".
    const COLLAPSED_GRID: &str = "\
┌─ 1 ClaudeCode · orch… ─┐ orchestrator ─────────────
│ working                │ worker ───────────────────
└────────────────────────┘";
    assert!(
        common::orchestration_pane_left_edge(COLLAPSED_GRID).is_none(),
        "a grid whose orchestrator pane is COLLAPSED has no corner anchor, so the edge must be \
         None rather than a wrong column:\n{COLLAPSED_GRID}"
    );
    assert!(
        common::orchestration_pane_column(COLLAPSED_GRID).is_none(),
        "no anchor means no crop:\n{COLLAPSED_GRID}"
    );
}

/// Scenario: Run `label_in_box_body_row` against a two-card row where the
/// SECOND card carries the searched label. Before the fix (review finding)
/// this helper used `position()`, which always returns the FIRST vertical
/// glyph in the whole body row regardless of which card's top-left corner is
/// being checked — so it only ever cropped the leftmost card's own body span,
/// and a label sitting in the second (or third) card of a multi-column row
/// could never be found. The fixed version mirrors
/// `label_in_box_top_border`: it iterates EVERY top-left corner on the row
/// and starts each crop's vertical search at that corner's own column.
#[test]
fn body_row_predicate_finds_a_label_in_the_second_card_of_a_row() {
    const TWO_CARD_GRID: &str = "\
┌─ alpha ─┐┌─ bravo ─┐
│ alpha   ││ bravo   │";
    assert!(
        common::label_in_box_body_row(TWO_CARD_GRID, "alpha"),
        "the FIRST card's own body label must still match:\n{TWO_CARD_GRID}"
    );
    assert!(
        common::label_in_box_body_row(TWO_CARD_GRID, "bravo"),
        "a label in the SECOND card's body row must match — this is the exact case the \
         `position()`-based implementation could never find:\n{TWO_CARD_GRID}"
    );
    assert!(
        !common::label_in_box_body_row(TWO_CARD_GRID, "charlie"),
        "a label present in neither card must not match:\n{TWO_CARD_GRID}"
    );
}

/// Scenario: Run `label_in_box_body_row` against two fixtures, each built to
/// fail under exactly one prior/alternate implementation rather than pass
/// against all of them the way a same-weight two-card grid would. The first
/// nests a PLAIN-weight vertical inside a THICK card's own body span, so a
/// weight-BLIND crop (one that stops at the nearest vertical glyph of any
/// weight) truncates the label — only a search scoped to the corner's own
/// weight reaches it. The second puts three cards, PLAIN/THICK/PLAIN, on one
/// row and searches the THIRD card's label, which the pre-fix `position()`
/// form (review finding: it always returned the FIRST vertical of a given
/// weight in the WHOLE row regardless of which corner was being checked) can
/// never reach, because it only ever finds the first PLAIN-weight span.
/// Verified against both the pre-fix `position()` form and a corner-scoped
/// but weight-blind form by executing each against these fixtures, not by
/// tracing (review finding: the prior fixture in this slot passed identically
/// against the current implementation, the pre-fix `position()` form, and a
/// weight-blind form, so it guarded nothing — D7 was not actually closed).
#[test]
fn body_row_predicate_scopes_the_search_to_each_cards_own_border_weight() {
    // Discriminates weight-scoped from weight-blind: the nested `│` sits
    // INSIDE the outer THICK card's own body span, so a crop that stops at
    // any vertical glyph (not just the corner's own weight) truncates
    // "bravo" before it is reached.
    const NESTED_WEIGHT_GRID: &str = "\
┏━ outer ━━━┓
┃ a │ bravo ┃";
    assert!(
        common::label_in_box_body_row(NESTED_WEIGHT_GRID, "bravo"),
        "the label beyond a NESTED plain-weight vertical must still match under the \
         outer card's own THICK weight — a weight-blind crop would stop at the nested \
         `│` and cut it off:\n{NESTED_WEIGHT_GRID}"
    );

    // Discriminates the fix from the pre-fix `position()` form: three cards,
    // PLAIN/THICK/PLAIN, and the label sought is in the THIRD card. `position()`
    // only ever finds the first PLAIN-weight vertical in the whole row, so it
    // can only ever crop the FIRST plain card's span and never reaches this one.
    const THREE_CARD_GRID: &str = "\
┌─ a ─┐┏━ b ━┓┌─ c ─┐
│  a  │┃  b  ┃│  c  │";
    assert!(
        common::label_in_box_body_row(THREE_CARD_GRID, "c"),
        "the THIRD card's own body label must match — the pre-fix `position()` form \
         can only ever find the first PLAIN-weight span in the row:\n{THREE_CARD_GRID}"
    );
    assert!(
        !common::label_in_box_body_row(THREE_CARD_GRID, "zzz"),
        "a label present in no card must not match:\n{THREE_CARD_GRID}"
    );
}

/// Scenario: Run the shared word-boundary predicate (`common::contains_word_token`,
/// PRD fork#405 review round — consolidated out of three byte-identical e2e-local
/// copies) against the three cases those e2e callers actually depend on: a role
/// name must not match as a substring of a longer, hyphenated identifier
/// (`worker` inside this deck's own `worker-deck` chrome title), must still match
/// when abutted by a card's own border glyph rather than whitespace (`│worker`,
/// the PRD fork#405 M1 body-row shape with no leading pad), and must not match a
/// different role name that happens to share a prefix (`beta` inside
/// `beta-agent`).
#[test]
fn word_token_predicate_respects_identifier_boundaries() {
    assert!(
        !common::contains_word_token("worker-deck — 1/2 session(s)", "worker"),
        "`worker` must not match as a substring of the hyphenated `worker-deck` chrome title"
    );
    assert!(
        common::contains_word_token("│worker", "worker"),
        "`worker` must match when abutted by a card border glyph instead of whitespace"
    );
    assert!(
        !common::contains_word_token("│beta-agent│", "beta"),
        "`beta` must not match as a substring of the longer identifier `beta-agent`"
    );
}

/// Build the grid an Orchestration tab paints for `cards` (top to bottom), with
/// the embedded pane of `focused` drawn in the pane column to their right.
///
/// The shape is the one `session/restore/008` failed on in CI (issue #809), at
/// a reduced width: a tab bar on row 0; on row 1 the deck's own chrome line
/// with the focused pane's header — `┌<role>───…┐` — beside it, ABOVE every
/// card; then one three-row card per role, exactly as the product titles it:
/// the ` N ` shortcut badge on the left of the top border and the `● No agent`
/// status on the right, the role name on the first body row with no pad, and a
/// bottom border. The focused role's card is the selected one, so it is drawn
/// in the thick weight and its badge reads ` ▸ N `. Built rather than pasted
/// so every row is a real rectangle by construction and the same deck can be
/// re-drawn in a different order or with a different role focused.
fn orchestration_tab_grid(cards: &[&str], focused: &str) -> String {
    orchestration_tab_grid_with_pane_output(cards, focused, "")
}

/// [`orchestration_tab_grid`] with `pane_output` as the first line the focused
/// pane's agent has printed — the pane's first content row, which sits on grid
/// row 2, above every card's identity row.
fn orchestration_tab_grid_with_pane_output(
    cards: &[&str],
    focused: &str,
    pane_output: &str,
) -> String {
    const CARD_INNER: usize = 26;
    const PANE_INNER: usize = 20;
    /// What every card's title carries on its right before any agent has
    /// announced itself — and the reason a role may not be located by "the
    /// title does not mention it": this one mentions `agent`.
    const STATUS: &str = " ● No agent ";
    let plain = common::BORDER_WEIGHTS[0];
    let thick = common::BORDER_WEIGHTS[1];
    let pad = |text: &str, width: usize, fill: char| -> String {
        let len = text.chars().count();
        assert!(
            len <= width,
            "{text:?} does not fit a {width}-column fixture span"
        );
        let mut padded = text.to_string();
        padded.extend(std::iter::repeat_n(fill, width - len));
        padded
    };

    let mut sidebar = vec![pad(
        &format!(" worker-deck — {} session(s)", cards.len()),
        CARD_INNER + 2,
        ' ',
    )];
    for (index, name) in cards.iter().enumerate() {
        let selected = *name == focused;
        let weight = if selected { thick } else { plain };
        let badge = format!(" {}{} ", if selected { "▸ " } else { "" }, index + 1);
        sidebar.push(format!(
            "{}{}{STATUS}{}",
            weight.top_left,
            pad(
                &badge,
                CARD_INNER - STATUS.chars().count(),
                weight.horizontal
            ),
            weight.top_right
        ));
        sidebar.push(format!(
            "{}{}{}",
            weight.vertical,
            pad(name, CARD_INNER, ' '),
            weight.vertical
        ));
        sidebar.push(format!(
            "{}{}{}",
            weight.bottom_left,
            pad("", CARD_INNER, weight.horizontal),
            weight.bottom_right
        ));
    }

    let last = sidebar.len() - 1;
    let mut rows = vec![" Dashboard │ tdd-cycle [×]".to_string()];
    rows.extend(sidebar.iter().enumerate().map(|(row, left)| {
        let pane = if row == 0 {
            format!(
                "{}{}{}",
                plain.top_left,
                pad(focused, PANE_INNER, plain.horizontal),
                plain.top_right
            )
        } else if row == last {
            format!(
                "{}{}{}",
                plain.bottom_left,
                pad("", PANE_INNER, plain.horizontal),
                plain.bottom_right
            )
        } else {
            let content = if row == 1 { pane_output } else { "" };
            format!(
                "{}{}{}",
                plain.vertical,
                pad(content, PANE_INNER, ' '),
                plain.vertical
            )
        };
        format!("{left}{pane}")
    }));
    rows.join("\n")
}

/// Scenario: Draw the Orchestration tab `session/restore/008` failed on in CI —
/// cards `orchestrator`, `coder`, `reviewer` in saved order, with `reviewer`
/// the focused role so its embedded pane's header names it on row 1, above
/// every card. The first-row-with-that-text lookup the test used to rely on
/// must report row 1 for `reviewer` (the defect, reproduced), while
/// `common::card_identity_row` must report each role's own card row, for every
/// choice of focused role.
#[test]
fn card_identity_row_reads_the_card_and_never_the_focused_pane_header() {
    const ROLES: [&str; 3] = ["orchestrator", "coder", "reviewer"];

    let grid = orchestration_tab_grid(&ROLES, "reviewer");
    let first_row_with_text = |needle: &str| grid.lines().position(|line| line.contains(needle));
    assert_eq!(
        first_row_with_text("reviewer"),
        Some(1),
        "this fixture must reproduce the failure it guards: the focused pane's header names \
         `reviewer` on row 1, above every card, which is what a whole-grid text lookup \
         finds first:\n{grid}"
    );
    assert_eq!(
        first_row_with_text("coder"),
        Some(6),
        "…while `coder` is first seen on its own card, so the old lookup read the correctly \
         ordered deck as `reviewer` (row 1) before `coder` (row 6):\n{grid}"
    );

    // Whichever role is focused — and so whichever name the pane header
    // carries, in whichever border weight its card is drawn — every role's row
    // is its own card's identity row.
    for focused in ROLES {
        let grid = orchestration_tab_grid(&ROLES, focused);
        for (index, role) in ROLES.iter().enumerate() {
            assert_eq!(
                common::card_identity_row(&grid, role),
                Some(3 + 3 * index),
                "with `{focused}` focused, `{role}` must be located on its own card's first \
                 body row, not on the pane header:\n{grid}"
            );
        }
    }
}

/// Scenario: Draw the same Orchestration tab with the `coder` and `reviewer`
/// cards swapped — the deck a broken restore would paint — once for each
/// choice of focused role. `common::card_identity_row` must put `reviewer`
/// above `coder` every time, so an ordering assertion built on it still fails
/// when the order really is wrong, and is not merely immune to the pane header.
#[test]
fn card_identity_row_still_reports_a_genuinely_reversed_deck_as_reversed() {
    const REVERSED: [&str; 3] = ["orchestrator", "reviewer", "coder"];

    for focused in REVERSED {
        let grid = orchestration_tab_grid(&REVERSED, focused);
        let coder = common::card_identity_row(&grid, "coder");
        let reviewer = common::card_identity_row(&grid, "reviewer");
        assert_eq!(
            (reviewer, coder),
            (Some(6), Some(9)),
            "with `{focused}` focused, a deck drawn reviewer-before-coder must be READ \
             reviewer-before-coder — otherwise the saved-order assertion in \
             `session/restore/008` could no longer fail:\n{grid}"
        );
    }
}

/// Scenario: Draw the Orchestration tab with the focused pane's agent having
/// printed a line that opens with a role name — first `reviewer`'s pane
/// printing `coder: on it`, then every pairing of focused role and named role.
/// That line sits on grid row 2, inside a real box, above every card, so a
/// locator that only rules out "the box titled with this role" returns it.
/// `common::card_identity_row` must return the role's card row every time,
/// because a pane's title carries no card badge.
#[test]
fn card_identity_row_ignores_pane_output_that_opens_with_a_role_name() {
    const ROLES: [&str; 3] = ["orchestrator", "coder", "reviewer"];

    // The review finding's own shape: the pane belongs to a DIFFERENT role
    // than the one its output names, so its title does not give it away.
    let grid = orchestration_tab_grid_with_pane_output(&ROLES, "reviewer", "coder: on it");
    assert_eq!(
        grid.lines().position(|line| line.contains("coder")),
        Some(2),
        "this fixture must put `coder` on the pane's first content row, above the coder \
         card:\n{grid}"
    );
    assert_eq!(
        common::card_identity_row(&grid, "coder"),
        Some(6),
        "`coder` must be located on its CARD; row 2 is the `reviewer` pane's own output, \
         and a box with no card badge in its title is not a card:\n{grid}"
    );

    for focused in ROLES {
        for (index, role) in ROLES.iter().enumerate() {
            let grid =
                orchestration_tab_grid_with_pane_output(&ROLES, focused, &format!("{role}: on it"));
            assert_eq!(
                common::card_identity_row(&grid, role),
                Some(3 + 3 * index),
                "with `{focused}` focused and its pane printing `{role}: on it`, `{role}` \
                 must still be located on its own card:\n{grid}"
            );
        }
    }
}

/// Scenario: Draw an Orchestration tab one of whose roles is named `agent`,
/// while every card's title carries the `● No agent` status the product paints
/// before an agent announces itself. A locator that tells a card from a pane by
/// "the title does not mention the role" rejects that role's own card;
/// `common::card_identity_row` must find it, for every choice of focused role.
#[test]
fn card_identity_row_finds_a_card_whose_own_title_mentions_the_role() {
    const ROLES: [&str; 3] = ["orchestrator", "agent", "reviewer"];

    for focused in ROLES {
        let grid = orchestration_tab_grid(&ROLES, focused);
        let title_row = grid.lines().nth(5).expect("the second card's top border");
        assert!(
            title_row.contains("No agent"),
            "this fixture must put the role's name in its own card's title, as the status \
             text does:\n{grid}"
        );
        assert_eq!(
            common::card_identity_row(&grid, "agent"),
            Some(6),
            "with `{focused}` focused, the `agent` card must be found even though its own \
             title reads `No agent`:\n{grid}"
        );
    }
}

/// Scenario: Draw two cards side by side on one row, `coder` on the left and
/// `reviewer` on the right. `common::card_identity_row` must find both and
/// report the SAME row for each — it reports a row, not a position, so it can
/// order cards only within one column. This pins the documented limit, so a
/// caller who needs a multi-column order is told by a test rather than by a
/// false failure.
#[test]
fn card_identity_row_reports_one_row_for_cards_drawn_side_by_side() {
    const SIDE_BY_SIDE: &str = "\
┌ 1 ─────┐┌ 2 ─────┐
│coder   ││reviewer│
└────────┘└────────┘";
    assert_eq!(
        common::card_identity_row(SIDE_BY_SIDE, "coder"),
        Some(1),
        "the left-hand card must be found:\n{SIDE_BY_SIDE}"
    );
    assert_eq!(
        common::card_identity_row(SIDE_BY_SIDE, "reviewer"),
        Some(1),
        "the right-hand card must be found too, on the same row — which is exactly why a \
         row alone cannot order cards drawn side by side:\n{SIDE_BY_SIDE}"
    );
}

/// Scenario: Run `common::card_identity_row` against the boxes that are NOT a
/// card for the role asked about: a deck whose focused pane header names a role
/// that has no card at all, a pane box titled with the role whose first content
/// row is agent output opening with that same name, a card named `coder-2`
/// when `coder` is asked for, two boxes whose title carries no card badge, a
/// body row that has some other glyph under the top border's right corner, and
/// one that ends before that column. Each must yield `None`, while the
/// `coder-2` card is still found under its own full name.
#[test]
fn card_identity_row_rejects_every_box_that_is_not_that_roles_card() {
    // The pane header names `reviewer`, but no card does.
    let header_only = orchestration_tab_grid(&["orchestrator", "coder"], "reviewer");
    assert!(
        header_only.contains("┌reviewer"),
        "the fixture must actually draw the `reviewer` pane header:\n{header_only}"
    );
    assert_eq!(
        common::card_identity_row(&header_only, "reviewer"),
        None,
        "a role named ONLY by the focused pane's header has no card, so there is no row to \
         report:\n{header_only}"
    );

    // A pane is a box too, and its first content row belongs to the agent —
    // here a shell prompt that happens to open with the role name.
    const PANE_ECHOING_ITS_ROLE: &str = "\
┌reviewer──────────┐
│reviewer $        │
└──────────────────┘";
    assert_eq!(
        common::card_identity_row(PANE_ECHOING_ITS_ROLE, "reviewer"),
        None,
        "a box titled with a display name is an embedded pane; its content opening with \
         the role name does not make it a card:\n{PANE_ECHOING_ITS_ROLE}"
    );

    // Token boundary: `coder` is not the card named `coder-2`.
    let suffixed = orchestration_tab_grid(&["orchestrator", "coder-2", "reviewer"], "reviewer");
    assert_eq!(
        common::card_identity_row(&suffixed, "coder"),
        None,
        "`coder` must not match the longer role name `coder-2`:\n{suffixed}"
    );
    assert_eq!(
        common::card_identity_row(&suffixed, "coder-2"),
        Some(6),
        "…and that card must still be found under its own full name:\n{suffixed}"
    );

    // No card badge: a perfectly good rectangle whose first body row opens with
    // the role name, but whose title is bare border, or text with no number.
    const UNTITLED_BOX: &str = "\
┌──────────────────┐
│coder             │
└──────────────────┘";
    const UNNUMBERED_BOX: &str = "\
┌ notes ───────────┐
│coder             │
└──────────────────┘";
    for (name, boxed) in [("untitled", UNTITLED_BOX), ("unnumbered", UNNUMBERED_BOX)] {
        assert_eq!(
            common::card_identity_row(boxed, "coder"),
            None,
            "the {name} box opens with no ` N ` card badge, so it is not a card whatever \
             its first row says:\n{boxed}"
        );
    }

    // Not a rectangle. The top border closes at column 11; the row below has
    // its right-hand vertical at column 9, two columns short, and a SPACE at
    // column 11 — so it is rejected for the glyph under the corner, not for
    // being too short to have one. Spelled with `concat!` so the two trailing
    // spaces that make the row long enough cannot be trimmed away unnoticed.
    const RAGGED: &str = concat!("┌ 1 ───────┐\n", "│coder   │", "  ");
    assert_eq!(
        RAGGED.lines().nth(1).map(|body| body.chars().count()),
        Some(12),
        "the ragged body row must reach column 11, or this fixture exercises the \
         too-short case below instead of the wrong-glyph one:\n{RAGGED}"
    );
    assert_eq!(
        common::card_identity_row(RAGGED, "coder"),
        None,
        "a body row with something other than the box's vertical under the top border's \
         right corner is not that box's body row:\n{RAGGED}"
    );

    // The same border over a row that simply ENDS before column 11 — a frame
    // torn mid-row. It must be `None`, and it must not panic on the read.
    const TRUNCATED: &str = "\
┌ 1 ───────┐
│coder   │";
    assert_eq!(
        common::card_identity_row(TRUNCATED, "coder"),
        None,
        "a body row that ends before the top border's right corner is not that box's body \
         row:\n{TRUNCATED}"
    );
}
