//! Shared terminal-display sanitization (issue #232, extending the
//! precedent in `src/keybindings.rs`'s former private `sanitize_for_terminal`).
//!
//! Escapes, rather than drops, every char that could forge or hide content
//! once printed to a live terminal: Unicode category `Cc` controls
//! (`char::is_control()` — C0 including ESC/CR/LF/TAB, and C1 including DEL
//! and U+009B) plus a fixed set of bidi/format (`Cf`) controls that can
//! reorder or hide surrounding text without any control byte at all — the
//! embedding/override/isolate controls, LRM/RLM, the Arabic letter mark, the
//! zero-width space/non-joiner/joiner, and the BOM. Escaping (not dropping)
//! matters equally: silently stripping a hostile char makes two differently
//! named, hostile paths/strings render identically, which is exactly what an
//! operator deciding what to delete must not see.
//!
//! **Display-only.** The output is for a terminal to *look at*, never a
//! value to act on: it must never feed a subprocess argument, a `--json`
//! field, or anything else that must round-trip to the original bytes. The
//! escaped spelling denotes the original content: it does not reconstitute
//! it, and is not shell-copyable back into the real value.

use std::path::Path;

/// True for a char this module escapes before printing to a terminal. See
/// the module doc for exactly which Unicode categories and codepoints this
/// covers and why.
fn is_hostile_terminal_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{200e}'
                | '\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
                | '\u{061c}'
                | '\u{200b}'..='\u{200d}'
                | '\u{feff}'
        )
}

/// Escape hostile terminal chars in `s` (see the module doc), leaving every
/// other char — including printable non-ASCII Unicode — untouched.
pub fn sanitize_for_terminal_display(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if is_hostile_terminal_char(c) {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    out
}

/// [`sanitize_for_terminal_display`] for a `Path`, going through
/// `to_string_lossy()` first (the same lossy UTF-8 rendering already used
/// for display elsewhere) — display-only, never a path to act on.
pub fn sanitize_path_for_terminal_display(path: &Path) -> String {
    sanitize_for_terminal_display(&path.to_string_lossy())
}
