//! Sanitizers for producer-supplied text that ends up rendered into the live
//! terminal.
//!
//! Several strings on this project's wire are written by a peer we do not
//! control — hook events arrive on a socket any agent on the deck can post to,
//! and `list_agents` records are echoed by a daemon that may be older or
//! malformed. Every one of those strings is eventually drawn into a ratatui
//! cell or written to the pre-alt-screen terminal, so a raw ESC, a NUL or a
//! Unicode bidi override in one of them can repaint, reorder or hide text the
//! user is relying on to make a decision.
//!
//! Two policies live here, and which one is right depends on where the value
//! lands. [`strip_control_and_bidi`] **strips**, because its values land in
//! width-constrained regions (a dashboard card title gets whatever columns the
//! status badge leaves it) and `\x1b` expanding to four visible characters
//! would spend that budget on the attacker's behalf.
//! [`escape_control_and_bidi`] **escapes**, because a diagnostic line wants to
//! *show* that the producer sent something peculiar rather than quietly hide
//! it, and has a whole scrollback line to spend saying so — see
//! `keybindings::sanitize_for_terminal`, which does the same for a warning
//! printed once to stderr, and `remote_doctor::render`, which routes every
//! producer-controlled field of its report through the escaping form (PRD
//! #345 audit). Pick stripping for a value that has to stay readable inside a
//! fixed box, escaping for one whose whole purpose is to be inspected.
//!
//! This is now the single implementation of the control+bidi filter.
//! [`crate::build_version_handshake`] carried a byte-identical copy until issue
//! #670 and delegates here instead. [`crate::daemon_client`] still has its own
//! `strip_control_chars`, which drops control characters but NOT bidi — that is
//! a different seam (the `list_agents` wire boundary, on records the daemon has
//! already validated) with its own audit, and folding it in here is deliberately
//! not part of #670. Reach for this module rather than writing a third copy: it
//! is the divergence between those two that let the bidi half be present on one
//! untrusted path and absent on another.

use crate::agent_pty::DISPLAY_NAME_MAX_LEN;
use crate::prompt_delivery::truncate_on_char_boundary;
use regex::Regex;
use std::sync::OnceLock;

/// Compiled once: matches any single char in Unicode general category `Cf`
/// (format characters). See [`is_bidi_format_char`] for why category
/// classification, not an enumerated codepoint list, is what this module
/// checks against.
fn cf_category_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\p{Cf}").expect("static Cf category regex compiles"))
}

/// Returns `true` for a Unicode format-character (general category `Cf`) —
/// bidirectional formatting/override controls, zero-width space/non-joiner/
/// joiner, the BOM, WORD JOINER, SOFT HYPHEN, the tag-format block, and more.
///
/// [`char::is_control`] does **not** catch these — they are category `Cf`,
/// not `Cc` — but a terminal honours them, so a `U+202E`
/// (RIGHT-TO-LEFT OVERRIDE) planted in an untrusted string visually reverses
/// the characters after it, and an invisible one (`U+2060` WORD JOINER,
/// `U+200B` ZWSP, the `U+E0000..=U+E007F` tag block used for invisible-text
/// smuggling) can hide or spoof content with no visible trace at all.
/// Classified by category via `regex`'s `\p{Cf}` Unicode property support
/// (the same mechanism [`crate::terminal_sanitize`] uses), not by an
/// enumerated list of codepoints: fork issue #232 round 2 found a prior
/// hand-picked list here silently missing WORD JOINER and others it claimed
/// to cover — the same "list of the ones we thought of" shape that misses
/// whatever nobody thought of, and never updates itself as Unicode adds more.
pub fn is_bidi_format_char(c: char) -> bool {
    let mut buf = [0u8; 4];
    cf_category_regex().is_match(c.encode_utf8(&mut buf))
}

/// Drop every character from `s` that could perturb or spoof the terminal it is
/// rendered into:
///
/// - C0 control bytes (`< 0x20`) and `0x7F` (DEL),
/// - C1 controls (`0x80..=0x9F`, including U+0085 NEL) — both covered by
///   [`char::is_control`],
/// - the bidi formatting / override codepoints [`is_bidi_format_char`] names.
///
/// `keep_newlines` retains `\n` for callers whose own output is multi-line and
/// whose line structure is theirs rather than the producer's. Pass `false` for
/// any value that has to stay on one line — an embedded newline in a card title
/// or a prompt name is an extra line the producer did not get to ask for.
pub fn strip_control_and_bidi(s: &str, keep_newlines: bool) -> String {
    s.chars()
        .filter(|&c| {
            if keep_newlines && c == '\n' {
                return true;
            }
            !(c.is_control() || is_bidi_format_char(c))
        })
        .collect()
}

/// Escape, rather than drop, every character [`strip_control_and_bidi`] would
/// remove: C0/C1 controls (ESC, NUL, CR, LF, DEL and friends) become their
/// `\n` / `\u{1b}` source form, and the bidi formatting / override
/// codepoints become `\u{202e}`-style escapes. Everything else — accents,
/// CJK, emoji, punctuation — passes through byte for byte.
///
/// Use this on producer-controlled text written to a **diagnostic**, where the
/// reader is trying to work out what a peer actually sent. Silently stripping
/// there would hide the very evidence the line exists to carry, and the
/// diagnostic's own conclusions are what an attacker wants to repaint: PRD
/// #345's `remote doctor` quotes ssh's stderr, the remote's registry entry and
/// the listen specs `ssh -G` resolved, every one of which a hostile remote or
/// a planted `~/.ssh/config` can fill with CSI/OSC sequences that clear the
/// screen, retitle the terminal or forge a hyperlink.
///
/// The result contains no control characters, so applying this twice is a
/// no-op on the second pass — callers may escape at the boundary where the
/// value enters an error type *and* again at the render seam without producing
/// `\\u{1b}`.
pub fn escape_control_and_bidi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_control() {
            out.extend(c.escape_default());
        } else if is_bidi_format_char(c) {
            out.extend(c.escape_unicode());
        } else {
            out.push(c);
        }
    }
    out
}

/// Sanitize a producer-supplied display name into something safe to store and
/// render, or `None` when nothing usable survives.
///
/// Strips control and bidi characters, trims surrounding whitespace, then —
/// only when the result is over [`DISPLAY_NAME_MAX_LEN`] bytes — clamps it on
/// a character boundary via [`truncate_on_char_boundary`], leaving room for
/// the trailing `…` that truncator appends so the **final** output never
/// exceeds [`DISPLAY_NAME_MAX_LEN`] bytes either way. That marker matches the
/// one `ui::render_session_card` already puts on a shortened
/// `session_id`. `None` means "no usable name": the caller should leave
/// whatever name it already had in place rather than store an empty title.
///
/// The byte ceiling is deliberately the daemon's own
/// [`DISPLAY_NAME_MAX_LEN`], so a name that reaches a card through the hook
/// socket cannot be longer than one that reaches it through
/// `agent_pty::is_valid_display_name` on the attach socket — that function
/// enforces the cap strictly (`<= DISPLAY_NAME_MAX_LEN`), which is why the
/// ellipsis has to come out of the same budget rather than being added on
/// top of it. The two differ in what they do about a bad value — the daemon
/// **rejects** the whole name, this **repairs** it — because the daemon is
/// validating a request a user just typed and can retype, while this is
/// scrubbing a field on an event whose other contents we still want to apply.
///
/// Note the clamp counts BYTES, not display columns. That matches every other
/// length bound in this project and is not the render budget: the title's own
/// fit is decided later by `ui::truncate_styled_segments`, whose char-vs-column
/// accounting is issue #357.
pub fn sanitize_display_name(raw: &str) -> Option<String> {
    let stripped = strip_control_and_bidi(raw, false);
    let trimmed = stripped.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() <= DISPLAY_NAME_MAX_LEN {
        return Some(trimmed.to_string());
    }
    // Truncate to leave room for the ellipsis truncate_on_char_boundary is
    // about to append, so the total never exceeds DISPLAY_NAME_MAX_LEN.
    let budget = DISPLAY_NAME_MAX_LEN.saturating_sub("…".len());
    Some(truncate_on_char_boundary(trimmed, budget))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_control_and_bidi_removes_escapes_del_c1_and_overrides() {
        // One string carrying every class at once: ESC, NUL, CR/LF, DEL, a C1
        // control that only `char::is_control` catches, and an RLO.
        let dirty = "ze\x1b[31mta\0-li\u{202e}ve\x7f-\u{0085}77\r\n";
        assert_eq!(strip_control_and_bidi(dirty, false), "ze[31mta-live-77");

        // `keep_newlines` is the ONLY exemption, and it is opt-in.
        assert_eq!(strip_control_and_bidi("a\nb", false), "ab");
        assert_eq!(strip_control_and_bidi("a\n\u{202e}b\x1b", true), "a\nb");

        // Text outside the ASCII range is untouched — names are UTF-8 and
        // legitimately contain accents, CJK and emoji.
        assert_eq!(
            strip_control_and_bidi("café-日本-🎉", false),
            "café-日本-🎉"
        );
    }

    #[test]
    fn strip_control_and_bidi_covers_every_bidi_codepoint() {
        // Enumerated rather than spot-checked: a range typo in
        // `is_bidi_format_char` would otherwise leave one override live, and one
        // is all a spoof needs.
        for c in ['\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}']
            .into_iter()
            .chain(['\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}'])
            .chain(['\u{200E}', '\u{200F}', '\u{061C}'])
        {
            assert!(
                is_bidi_format_char(c),
                "U+{:04X} must be recognised",
                c as u32
            );
            let input = format!("a{c}b");
            assert_eq!(
                strip_control_and_bidi(&input, false),
                "ab",
                "U+{:04X} survived the filter",
                c as u32
            );
        }
    }

    #[test]
    fn strip_control_and_bidi_covers_the_cf_category_gaps_an_enumerated_list_missed() {
        // These are all general category `Cf` but outside any bidi-specific
        // range — an enumerated bidi codepoint list (this module's own prior
        // shape, and fork issue #232 round 2's original gap) misses every one
        // of them. U+2060 WORD JOINER is the exact codepoint that issue named.
        for c in [
            '\u{2060}',  // WORD JOINER
            '\u{200B}',  // ZERO WIDTH SPACE
            '\u{200C}',  // ZERO WIDTH NON-JOINER
            '\u{200D}',  // ZERO WIDTH JOINER
            '\u{FEFF}',  // ZERO WIDTH NO-BREAK SPACE / BOM
            '\u{00AD}',  // SOFT HYPHEN
            '\u{E0001}', // tag block (invisible-text smuggling)
        ] {
            assert!(
                is_bidi_format_char(c),
                "U+{:04X} is category Cf and must be recognised",
                c as u32
            );
            let input = format!("a{c}b");
            assert_eq!(
                strip_control_and_bidi(&input, false),
                "ab",
                "U+{:04X} survived the filter",
                c as u32
            );
        }
    }

    #[test]
    fn escape_control_and_bidi_shows_what_was_sent_instead_of_hiding_it() {
        // The same fixture the stripping test uses, so the two policies are
        // directly comparable: every class survives as visible evidence.
        let dirty = "ze\x1b[31mta\0-li\u{202e}ve\x7f-\u{0085}77\r\n";
        let escaped = escape_control_and_bidi(dirty);
        assert_eq!(
            escaped,
            "ze\\u{1b}[31mta\\u{0}-li\\u{202e}ve\\u{7f}-\\u{85}77\\r\\n"
        );
        // Nothing a terminal acts on may survive — that is the whole point.
        assert!(
            !escaped
                .chars()
                .any(|c| c.is_control() || is_bidi_format_char(c)),
            "escaped output still carries a live control/bidi character: {escaped:?}"
        );

        // Ordinary text is untouched, so a report stays readable.
        assert_eq!(escape_control_and_bidi("café-日本-🎉"), "café-日本-🎉");
        assert_eq!(
            escape_control_and_bidi("port 1080 is bound"),
            "port 1080 is bound"
        );
    }

    #[test]
    fn escape_control_and_bidi_is_idempotent() {
        // Callers escape at the error boundary AND again at the render seam;
        // a second pass must not turn `\u{1b}` into `\\u{1b}`.
        for raw in ["\x1b]0;pwn\x07", "a\u{202e}b", "plain", "back\\slash"] {
            let once = escape_control_and_bidi(raw);
            assert_eq!(
                escape_control_and_bidi(&once),
                once,
                "escaping {raw:?} twice changed the result"
            );
        }
    }

    #[test]
    fn sanitize_display_name_scrubs_trims_and_drops_empties() {
        assert_eq!(
            sanitize_display_name("fix-auth-bug"),
            Some("fix-auth-bug".to_string())
        );
        assert_eq!(
            sanitize_display_name("  \x1b]0;pwn\x07 dispatch-670 \n"),
            Some("]0;pwn dispatch-670".to_string())
        );

        // Nothing usable survives → `None`, so the caller keeps the name it had
        // instead of blanking a card title. This is the case the old
        // `.filter(|n| !n.is_empty())` guard covered for the literal empty
        // string only.
        assert_eq!(sanitize_display_name(""), None);
        assert_eq!(sanitize_display_name("   "), None);
        assert_eq!(sanitize_display_name("\x1b\u{202e}\0\x7f"), None);
    }

    #[test]
    fn sanitize_display_name_clamps_on_a_char_boundary() {
        // ASCII: exactly at the ceiling passes through untouched, one over is
        // cut (leaving room for the ellipsis) and marked.
        let at_cap = "a".repeat(DISPLAY_NAME_MAX_LEN);
        assert_eq!(sanitize_display_name(&at_cap), Some(at_cap.clone()));
        let over = "a".repeat(DISPLAY_NAME_MAX_LEN + 1);
        let clamped = sanitize_display_name(&over).expect("a long name is repaired, not dropped");
        assert_eq!(
            clamped.len(),
            DISPLAY_NAME_MAX_LEN,
            "clamped + ellipsis must fill exactly the cap, not overshoot it"
        );
        assert_eq!(
            clamped,
            format!("{}…", "a".repeat(DISPLAY_NAME_MAX_LEN - "…".len()))
        );

        // Multi-byte: the cut must snap DOWN to a boundary rather than split a
        // character. Swept across widths and offsets because the surviving byte
        // count depends on where the ceiling lands inside the fixture, and a
        // single fixture that happens to align proves nothing.
        for filler in ['α', 'あ', '𝄞', '😀'] {
            for pad in 0..4usize {
                let raw = "x".repeat(pad) + &filler.to_string().repeat(DISPLAY_NAME_MAX_LEN);
                let out = sanitize_display_name(&raw).expect("non-empty input yields a name");
                assert!(
                    out.len() <= DISPLAY_NAME_MAX_LEN,
                    "the FINAL output (including the ellipsis) must never exceed the cap: {} bytes for filler {filler:?} pad {pad}",
                    out.len()
                );
                let body = out.strip_suffix('…').expect("an over-long name is marked");
                assert!(
                    body.ends_with(filler),
                    "the cut split a character for filler {filler:?} pad {pad}: {body:?}"
                );
            }
        }
    }

    #[test]
    fn sanitize_display_name_strips_word_joiner_u2060() {
        // Auditor F1: this seam previously classified by an enumerated bidi
        // codepoint list rather than Unicode category `Cf`, silently
        // reintroducing the exact gap fork issue #232 round 2 closed on the
        // `terminal_sanitize` path — U+2060 WORD JOINER (and its invisible
        // siblings) reached a card's `display_name` unsanitized. There is no
        // render-seam sanitizer behind this ingest path, so this is the only
        // guard.
        let hostile = "evil\u{2060}\u{200B}name";
        let out = sanitize_display_name(hostile).expect("visible content survives sanitization");
        assert!(
            !out.contains('\u{2060}') && !out.contains('\u{200B}'),
            "WORD JOINER / ZWSP must not reach the stored display_name, got {out:?}"
        );
        assert_eq!(out, "evilname");
    }

    #[test]
    fn sanitize_display_name_clamps_after_stripping_not_before() {
        // A name padded to well over the ceiling with control bytes is a REAL
        // name once scrubbed. Clamping first would cut it to 128 bytes of
        // padding and then scrub that to nothing, losing a name that fits.
        let padded = format!("{}{}", "\x1b".repeat(400), "nightly-sweep");
        assert_eq!(
            sanitize_display_name(&padded),
            Some("nightly-sweep".to_string())
        );
    }
}
