//! Shared terminal-display sanitization (issue #232, extending the
//! precedent in `src/keybindings.rs`'s former private `sanitize_for_terminal`).
//!
//! Escapes, rather than drops, every char that could forge or hide content
//! once printed to a live terminal: Unicode category `Cc` controls
//! (`char::is_control()` — C0 including ESC/CR/LF/TAB, and C1 including DEL
//! and U+009B) plus every codepoint in Unicode general category `Cf`
//! (format characters — bidi embedding/override/isolate controls, LRM/RLM,
//! the Arabic letter mark, zero-width space/non-joiner/joiner, the BOM,
//! WORD JOINER, SOFT HYPHEN, the Mongolian vowel separator, the invisible
//! math operators, and the tag-format block, among others) that can reorder
//! or hide surrounding text without any control byte at all. `Cf` is
//! classified by category via `regex`'s `\p{Cf}` Unicode property support
//! (already a project dependency), not by an enumerated list of codepoints —
//! issue #232 round 2 found the prior hand-picked list silently missing
//! WORD JOINER and others it claimed to cover, the same "list of the ones we
//! thought of" shape that made issue #174 harmful: it needs updating every
//! time Unicode adds a character, and nothing makes that omission visible.
//! No exceptions are carved out of `Cf` here. Escaping (not dropping)
//! matters equally: silently stripping a hostile char makes two differently
//! named, hostile paths/strings render identically, which is exactly what an
//! operator deciding what to delete must not see.
//!
//! **Injective within valid UTF-8.** [`sanitize_for_terminal_display`]'s
//! escaping (issue #232 round 2, gap 4) also escapes a literal backslash, so
//! two distinct inputs never render identically: without that, a real LF and
//! the two printable bytes `\` + `n` both rendered as the two-char sequence
//! `\n`. More generally, `\\` is reserved for a literal backslash, and every
//! hostile char's `escape_default` spelling begins with a single backslash
//! followed by a non-backslash character — so escaping `\` itself keeps the
//! codeword stream uniquely decodable for valid UTF-8. This guarantee stops at
//! the UTF-8 boundary, though: [`sanitize_path_for_terminal_display`] goes
//! through `Path::to_string_lossy()`, which collapses every invalid UTF-8
//! byte sequence to `U+FFFD` before this module ever sees it, so two
//! distinct non-UTF-8 paths can still render identically. That is a
//! pre-existing, different-cause defect (byte-level, not char-level) tracked
//! as fork #276 — this module cannot restore information `to_string_lossy()`
//! already discarded.
//!
//! **Display-only.** The output is for a terminal to *look at*, never a
//! value to act on: it must never feed a subprocess argument, a `--json`
//! field, or anything else that must round-trip to the original bytes. The
//! escaped spelling denotes the original content: it does not reconstitute
//! it, and is not shell-copyable back into the real value.

use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

/// Compiled once: matches any single char in Unicode general category `Cf`
/// (format characters). See the module doc for why category classification
/// replaced an enumerated codepoint list.
fn cf_category_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\p{Cf}").expect("static Cf category regex compiles"))
}

/// True for a char this module escapes before printing to a terminal. See
/// the module doc for exactly which Unicode categories this covers and why.
fn is_hostile_terminal_char(c: char) -> bool {
    if c.is_control() {
        return true;
    }
    let mut buf = [0u8; 4];
    cf_category_regex().is_match(c.encode_utf8(&mut buf))
}

/// Escape hostile terminal chars in `s` (see the module doc), leaving every
/// other char — including printable non-ASCII Unicode — untouched. Also
/// escapes a literal backslash so the escaped output stays injective (see
/// the module doc's "Injective within valid UTF-8" section) — otherwise a
/// real control char and its own `\...` escape spelling render identically.
pub fn sanitize_for_terminal_display(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '\\' {
            out.push_str("\\\\");
        } else if is_hostile_terminal_char(c) {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    out
}

/// [`sanitize_for_terminal_display`] for a `Path`, going through
/// `to_string_lossy()` first (the same lossy UTF-8 rendering already used
/// for display elsewhere) — display-only, never a path to act on. See the
/// module doc: this inherits `to_string_lossy()`'s loss of injectivity for
/// invalid UTF-8 (fork #276), which this function does not attempt to fix.
pub fn sanitize_path_for_terminal_display(path: &Path) -> String {
    sanitize_for_terminal_display(&path.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scenario: issue #232 round 2, gap 2. `U+2060` WORD JOINER is Unicode
    /// category `Cf` (zero-width, valid in a Unix path component) but was
    /// missing from the old enumerated exception list, so it passed through
    /// unescaped and could conceal content in every sanitized human field.
    /// After classifying by category instead of by list, it must be
    /// escaped, while an ordinary printable ASCII neighbor survives
    /// unchanged.
    #[test]
    fn escapes_word_joiner_u2060() {
        let out = sanitize_for_terminal_display("keep\u{2060}-remove");
        assert!(
            !out.contains('\u{2060}'),
            "raw WORD JOINER (U+2060) must not appear verbatim, got {out:?}"
        );
        let escaped: String = '\u{2060}'.escape_default().collect();
        assert!(
            out.contains(&escaped),
            "expected the escaped spelling {escaped:?} for WORD JOINER, got {out:?}"
        );
        assert!(
            out.contains("keep") && out.contains("-remove"),
            "printable ASCII around the hostile char must survive unchanged, got {out:?}"
        );
    }

    /// Scenario: issue #232 round 2, gap 4. Before escaping a literal
    /// backslash, a real LF and the two printable bytes `\` + `n` both
    /// rendered as the identical two-char sequence `\n` -- this output
    /// decides what gets deleted, so two distinct inputs must never render
    /// identically. Pins that the two former collision cases (LF vs literal
    /// `\n`, and ESC vs literal `\u{1b}`) now render differently.
    #[test]
    fn escaping_a_literal_backslash_breaks_the_former_collisions() {
        let real_lf = sanitize_for_terminal_display("\n");
        let literal_backslash_n = sanitize_for_terminal_display("\\n");
        assert_ne!(
            real_lf, literal_backslash_n,
            "a real LF and the two printable chars '\\' + 'n' must no longer render \
             identically; got real_lf={real_lf:?}, literal={literal_backslash_n:?}"
        );

        let real_esc = sanitize_for_terminal_display("\u{1b}");
        let literal_backslash_u_1b = sanitize_for_terminal_display("\\u{1b}");
        assert_ne!(
            real_esc, literal_backslash_u_1b,
            "a real ESC and the literal text '\\u{{1b}}' must no longer render identically; \
             got real_esc={real_esc:?}, literal={literal_backslash_u_1b:?}"
        );
    }

    /// A plain backslash with no adjacent hostile content still round-trips
    /// to a doubled backslash, and ordinary printable text around it is
    /// untouched -- pins the injective escaping in isolation from the
    /// collision-pair scenarios above.
    #[test]
    fn escapes_plain_backslash() {
        let out = sanitize_for_terminal_display(r"a\b");
        assert_eq!(out, r"a\\b");
    }
}
