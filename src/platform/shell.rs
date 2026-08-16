//! Default-shell and command-wrap policy (PRD #42 M1, lifted from
//! `agent_pty.rs`).
//!
//! A multi-word agent command is run through the platform shell rather than
//! exec'd directly. Unix uses `$SHELL -c …` (falling back to `/bin/sh`);
//! Windows uses `%COMSPEC% /C …` (falling back to `cmd.exe`).

/// Whether a `command` must be run through `<shell> <flag>` rather than exec'd
/// directly. A command with whitespace is a shell command line (pipes, `;`,
/// redirections, multiple words); a single bare word is exec'd directly.
///
/// Platform-independent: the predicate is the same on Unix and Windows; only
/// the *shell* used for the wrap (see [`default_shell`] / [`shell_command_flag`])
/// differs.
pub fn command_needs_shell_wrap(command: &str) -> bool {
    command.contains(char::is_whitespace)
}

/// Resolve the shell used for the `-c`/`/C` wrap of a multi-word command and
/// for the no-command fallback.
///
/// A caller may pin the shell with `shell_override` (PRD #127 M2.1: the
/// scheduler injects `SHELL` to run an explicit multi-word command under a
/// deterministic `/bin/sh -c` while reserving the daemon's own `$SHELL` for the
/// omitted-command fallback). When unset:
/// - Unix: the process `$SHELL`, then `/bin/sh`.
/// - Windows: the process `%COMSPEC%`, then `cmd.exe`.
pub fn default_shell(shell_override: Option<&str>) -> String {
    if let Some(s) = shell_override {
        return s.to_string();
    }
    #[cfg(unix)]
    {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    }
    #[cfg(windows)]
    {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
    }
}

/// Shell for a command line the **deck itself pinned** rather than one the user
/// configured — the watch runner's fixed `clear`-and-rerun invocation, and the
/// scheduler's deliberate "run this multi-word command under a predictable
/// interpreter, not the daemon's `$SHELL`" override (PRD #127 C2).
///
/// `posix_shell` is returned verbatim on Unix, so each call site keeps the exact
/// shell it pins today (`/bin/sh` for the scheduler's `SHELL` override, `sh` for
/// the watch runner). Windows has no deterministic POSIX shell to pin, so it
/// resolves the same interpreter [`default_shell`] would: `%COMSPEC%`, else
/// `cmd.exe`.
///
/// Exists so those call sites stop carrying their own `#[cfg]` pair — the
/// platform choice stays here at the seam (PRD #163 M1).
pub fn fixed_command_shell(posix_shell: &str) -> String {
    #[cfg(unix)]
    {
        posix_shell.to_string()
    }
    #[cfg(windows)]
    {
        let _ = posix_shell;
        default_shell(None)
    }
}

/// Flag passed to [`default_shell`] to run a command string: `-c` on Unix,
/// `/C` on Windows (`cmd.exe`).
pub fn shell_command_flag() -> &'static str {
    #[cfg(unix)]
    {
        "-c"
    }
    #[cfg(windows)]
    {
        "/C"
    }
}

/// Quote `arg` as a single word of the POSIX shell [`default_shell`]/
/// [`shell_command_flag`] would run — for a caller that assembles a command
/// *line* (rather than an argv vector) that is later parsed by an outer
/// shell, e.g. `dot-agent-deck watch --interval N <command>` written into a
/// pane's shell (issue #157: the executable path went in bare and a watch
/// pane silently failed to start once the deck was installed at a path
/// containing a space).
///
/// **POSIX-only.** This implements single-quoting per POSIX shell grammar
/// and nothing else — it is not, and must not be treated as, a general
/// shell-quoting contract for other interpreters. `cmd.exe` has no
/// equivalent single-quote construct, so there is no Windows arm of this
/// function; a caller that needs a `cmd.exe`-safe command line must build it
/// itself with [`escape_cmd_exe_program`] / [`quote_cmd_exe_arg`] below
/// (issue #157 finding A1, audit-verified — a `{:?}` Debug-escaped
/// double-quoted string is *not* `cmd.exe`-safe either, since `cmd.exe`
/// treats a literal backslash before an embedded `"` as not escaping it, so
/// the quote still closes the word and everything after it is parsed as a
/// fresh command line by the outer shell; fork issue #283).
///
/// Single-quotes `arg` unconditionally, closing/escaping/reopening an
/// embedded `'` as `'\''` (the standard POSIX technique) — nothing else is
/// special between single quotes, so this preserves whitespace, double
/// quotes, `$`, backticks, backslashes, real newlines, `;`, `&`, `|`,
/// redirection, glob characters and parentheses.
#[cfg(unix)]
pub fn quote_shell_arg(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', r"'\''"))
}

/// Escape `token` for the leading (program-name) position of a `cmd.exe`
/// command line by caret-escaping whitespace and every `cmd.exe`
/// metacharacter, rather than wrapping it in `"…"`. `cmd.exe` locates the
/// program at the head of a command line by scanning up to the first
/// unescaped space — so a bare path containing a space (issue #157's
/// original regression) is split and never found unless that space is
/// protected. Caret-escaping the space (`^ `), rather than quoting the whole
/// token, is deliberate, not merely an alternative notation: `"` is not a
/// legal character in a Windows path, so a program-name token this function
/// produces never contains one, and never needs to. That matters because
/// `cmd.exe /C <string>` applies a SEPARATE, one-time preprocessing pass to
/// its `string` argument before normal parsing ever sees it (`cmd /?`'s
/// documented `/C` quoting rules): quotes are preserved as typed only if
/// the ENTIRE argument contains exactly two quote characters naming a
/// single existing executable with nothing special between them; otherwise,
/// if the argument's first character happens to be a quote, `cmd.exe`
/// unconditionally strips that leading quote AND the LAST quote character
/// anywhere in the argument — not necessarily the same pair — corrupting a
/// correctly `"…"`-quoted program token the moment [`quote_cmd_exe_arg`]'s
/// own necessary quoting of the trailing command pushes the total past two.
/// A caller can't know in advance whether the returned line will be handed
/// to `cmd.exe /C` (this quirk) or typed into an already-running
/// interactive `cmd.exe` (no such quirk — see `watch_invocation`'s Windows
/// arm, whose output is typed into a pane's shell either way), so the fix
/// has to work under both: never let the token start with `"` at all, and
/// this preprocessing pass — which activates ONLY "if the first character
/// is a quote character" — never triggers, in either context. Caret-
/// escaping the token's OWN metacharacters (`&|<>()^%!,;=`) alongside
/// whitespace is the same defense [`quote_cmd_exe_arg`] applies to the
/// command position, just without the quote-wrap that position needs and
/// this one must avoid.
///
/// **Caveat — `%` and `!` are not actually neutralized, only incidentally
/// inert.** See [`quote_cmd_exe_arg`]'s doc comment for the mechanism and
/// its failure cases (delayed expansion for `!`, batch-file rules for
/// `%`); the same caveat applies here since this function shares the same
/// caret-escape strategy. Neither case lets a value break out of the
/// program-name position — both operate a parse phase later than the
/// metacharacter scan this function defends — so it is a content-mangling
/// risk, not an injection, but it is deliberately not left silently
/// implied to be resolved (fork issue #283).
///
/// **Caveat — a raw control character cannot be caret-escaped away
/// either**, for the reasons [`sanitize_cmd_exe_control_chars`]'s doc
/// comment gives; this function replaces every one with a space before
/// doing anything else (fork issue #423 review finding B1's sibling: a
/// control character in an executable path, reachable only via a
/// self-controlled `std::env::current_exe()`, not user config).
#[cfg(windows)]
pub fn escape_cmd_exe_program(token: &str) -> String {
    let token = sanitize_cmd_exe_control_chars(token);
    let mut result = String::with_capacity(token.len());
    for c in token.chars() {
        if matches!(
            c,
            ' ' | '"' | '^' | '&' | '|' | '<' | '>' | '(' | ')' | ',' | ';' | '=' | '%' | '!'
        ) {
            result.push('^');
        }
        result.push(c);
    }
    result
}

/// Quote `arg` as a single `cmd.exe` command-line ARGUMENT (as opposed to
/// [`escape_cmd_exe_program`]'s leading program-name position, which
/// deliberately avoids literal quotes — see its doc comment) so that it
/// survives BOTH of the two separate parsers that see the same text: the
/// interpreting `cmd.exe`'s own metacharacter scan, and the CRT/
/// `CommandLineToArgvW`-compatible `argv` parsing the launched child process
/// applies to recover its own arguments. Fork issue #283.
///
/// This is genuinely two escaping passes, not one — matching the treatment
/// at <https://qntm.org/cmd> (the definitive public writeup of this
/// problem), which is also the algorithm Node.js's `child_process` uses to
/// shell out through `cmd.exe` safely:
///
/// 1. **CRT round-trip quoting** ([`quote_shell_arg`]'s Windows analogue):
///    wrap `arg` in `"…"`, doubling any run of backslashes that immediately
///    precedes either an embedded `"` or the end of the string, and
///    escaping each embedded `"` with one extra backslash. In isolation
///    this is exactly what `std::process::Command` does to its own Windows
///    arguments, and it guarantees a child that parses its command line via
///    `CommandLineToArgvW` recovers `arg` byte for byte as a single `argv`
///    element — but it does nothing about `cmd.exe`'s OWN, separate scan of
///    the same text.
/// 2. **Caret-escaping every `cmd.exe` metacharacter** in step 1's output,
///    including the quotes step 1 just added. `cmd.exe` decides whether it
///    is "inside quotes" purely by counting bare `"` characters as it scans
///    — it does not honor a preceding backslash the way the CRT does, which
///    is exactly issue #157 finding A1's mechanism: a `\"` produced by step
///    1's escaping is still a bare `"` byte to `cmd.exe`, so it still flips
///    `cmd.exe`'s quote parity and exposes whatever follows (e.g. ` &
///    calc.exe & `) as an unquoted, separately-interpreted command.
///    Caret-escaping every metacharacter — quotes included — means
///    `cmd.exe`'s scan never sees a bare special character at all, so it
///    never leaves "quoted" mode and never treats anything as a command
///    separator. `cmd.exe` strips the carets as it forwards the line to the
///    launched process, so the child receives exactly step 1's output.
///
/// **Caveat — `%` and `!` are in the caret set but are NOT actually
/// neutralized, only incidentally inert.** Issue #283 requires this to be
/// stated plainly rather than implied away by the "never sees a bare
/// special character" claim above:
/// - **`%`**: percent expansion is `cmd.exe`'s phase-1 parse, which runs
///   *before* phase 2 strips carets — `^` cannot suppress it. `^%VAR^%`
///   happens to come out right only because the inserted caret corrupts
///   the variable name, the lookup fails, and an undefined reference is
///   left verbatim under command-line rules; under batch-file rules an
///   undefined reference is *deleted* instead, so the same input mangles.
/// - **`!`**: delayed expansion (`cmd /V:ON`, or the
///   `HKCU\Software\Microsoft\Command Processor\DelayedExpansion`
///   registry default) is `cmd.exe`'s phase 3, after caret-stripping. A
///   single `^!` reduces to a bare `!` before phase 3 runs, so `!VAR!`
///   still expands; the canonical escape there is `^^!`, which this
///   function does not emit.
///
/// Both are content-mangling/disclosure risks, not injection — the
/// expansion happens after the metacharacter scan this function defends,
/// so it cannot introduce a new command — but neither is actually closed
/// by quoting the way every other character in the caret set is.
///
/// **Caveat — a raw control character is replaced with a space before
/// either pass runs, not caret-escaped.** No caret encoding neutralizes
/// one: on the persistent-watch-pane delivery path the line is typed
/// into an already-running interactive `cmd.exe`, and the console's
/// cooked-mode line editor consumes several control bytes as *editing
/// commands* — a newline submits the line, `ESC` clears the whole input
/// buffer, `BS`/`DEL` delete backwards, `ETX` cancels it — before
/// `cmd.exe`'s own escaping grammar is ever consulted; and on the `/C
/// <string>` delivery path `cmd.exe` treats an embedded raw newline as a
/// statement separator the same way it would inside a batch file. See
/// [`sanitize_cmd_exe_control_chars`] (fork issue #423 review findings
/// B1 and C1).
#[cfg(windows)]
pub fn quote_cmd_exe_arg(arg: &str) -> String {
    caret_escape_cmd_metachars(&crt_argv_quote(&sanitize_cmd_exe_control_chars(arg)))
}

/// Step 1 of [`quote_cmd_exe_arg`]: the standard CRT/`CommandLineToArgvW`
/// round-trip quoting algorithm (Microsoft-documented; identical to what
/// `std::process::Command` applies to its own Windows arguments).
#[cfg(windows)]
fn crt_argv_quote(arg: &str) -> String {
    if !arg.is_empty()
        && !arg
            .chars()
            .any(|c| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '"'))
    {
        return arg.to_string();
    }

    let chars: Vec<char> = arg.chars().collect();
    let mut result = String::with_capacity(chars.len() + 2);
    result.push('"');

    let mut i = 0;
    while i < chars.len() {
        let mut num_backslashes = 0;
        while i < chars.len() && chars[i] == '\\' {
            num_backslashes += 1;
            i += 1;
        }

        if i == chars.len() {
            // Trailing backslash run right before the closing quote this
            // function is about to add: double it so the CRT parser sees an
            // even run (literal backslashes) followed by the real delimiter.
            result.extend(std::iter::repeat_n('\\', num_backslashes * 2));
        } else if chars[i] == '"' {
            // Backslash run immediately before an embedded quote: double it,
            // then escape the quote itself with one more backslash so the
            // CRT parser treats it as a literal `"` rather than a delimiter.
            result.extend(std::iter::repeat_n('\\', num_backslashes * 2 + 1));
            result.push('"');
            i += 1;
        } else {
            // A backslash run not immediately before a quote is not special
            // to the CRT parser — pass it through unchanged.
            result.extend(std::iter::repeat_n('\\', num_backslashes));
            result.push(chars[i]);
            i += 1;
        }
    }

    result.push('"');
    result
}

/// Step 2 of [`quote_cmd_exe_arg`]: caret-escape every character `cmd.exe`
/// treats specially during its own command-line scan, so that scan never
/// sees an unescaped metacharacter — including the quotes [`crt_argv_quote`]
/// added, which is what stops finding A1's quote-parity trick from working.
///
/// No control character appears in this set: caret-escaping one would not
/// help even in principle (see [`sanitize_cmd_exe_control_chars`]'s doc
/// comment — none of them are `cmd.exe` metacharacters this function's own
/// scan defends against), and this function is private to the module with
/// exactly one caller, [`quote_cmd_exe_arg`], which already sanitizes
/// control characters out before this ever runs (fork issue #423 review
/// finding C4 — the prior `\r`/`\n` entries here were dead code for the
/// same reason and are removed rather than kept as inert "defense in
/// depth").
#[cfg(windows)]
fn caret_escape_cmd_metachars(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '"' | '^' | '&' | '|' | '<' | '>' | '(' | ')' | ',' | '%' | '!'
        ) {
            result.push('^');
        }
        result.push(c);
    }
    result
}

/// Replace every control character in `s` with a single space before either
/// [`escape_cmd_exe_program`] or [`quote_cmd_exe_arg`] processes it.
///
/// Caret-escaping cannot neutralize a raw control character the way it
/// neutralizes every other `cmd.exe` metacharacter, because none of them
/// are ever reached by `cmd.exe`'s OWN scan in the first place — fixed
/// originally for `\r`/`\n` alone (fork issue #423 review finding B1),
/// then widened to the whole class once review finding C1 established
/// that a real `\r`/`\n` was never the only member of it:
///
/// - On the persistent-watch-pane delivery path (see `watch_invocation`'s
///   doc comment in `mode_manager.rs`), the line is typed
///   character-by-character into an already-running interactive
///   `cmd.exe`. The Windows console's cooked-mode line editor
///   (`ENABLE_LINE_INPUT`) consumes several bytes as *editing commands*
///   while reading that input, before `cmd.exe`'s own grammar is ever
///   consulted: a newline submits the current line exactly as if the user
///   had pressed Enter; `ESC` (0x1B) clears the ENTIRE input buffer typed
///   so far, discarding the program token, `watch --interval N`, and the
///   opening `^"` along with it, so only what follows the `ESC` byte is
///   left for `cmd.exe` to parse — e.g. `"\x1bcalc.exe x"` reduces to
///   `calc.exe x^"`, i.e. arbitrary program execution, not merely content
///   mangling; `BS`/`DEL` (0x08/0x7F) delete backwards; `ETX` (0x03,
///   Ctrl-C) cancels the line outright and starts a fresh prompt.
/// - On the `%COMSPEC% /C <string>` delivery path, `cmd.exe` treats an
///   embedded raw newline inside its `string` argument as a statement
///   separator, the same way it treats one inside a batch file.
///
/// Either way, a scheme that only defends against `cmd.exe`'s
/// METACHARACTER scan cannot close this off — the character has to be
/// gone before either quoting pass ever sees it.
///
/// Replacing with a space, rather than stripping or rejecting outright, is
/// the least-surprising per-operand choice: the value then reads exactly
/// as if the caller had used a space there in the first place; `main`'s
/// old `{:?}`-Debug-quoted format also mangled a control character's
/// content while keeping the line intact (via a visible escape sequence
/// like `\n` or `\u{1b}` rather than a space, so this is not
/// byte-identical to `main`, but it is the same class of "content
/// mangled, line count preserved" behavior); and stripping outright risks
/// silently joining two words (`"foo\nbar"` -> `"foobar"`) that a space
/// keeps apart. (Fork issue #423 review finding C2 considered rejecting
/// the value outright instead — the strictly safer design, since it
/// removes even the "two harmless lines merge into one harmful command"
/// counterexample the auditor raised — but that changes user-visible
/// behavior for callers beyond this fix's scope, so it was left as
/// substitution; see the finding for the tradeoff.)
#[cfg(windows)]
fn sanitize_cmd_exe_control_chars(s: &str) -> String {
    s.replace(char::is_control, " ")
}

#[cfg(all(windows, test))]
mod windows_quoting_tests {
    use super::*;

    #[test]
    fn escape_cmd_exe_program_leaves_a_safe_path_unchanged() {
        assert_eq!(
            escape_cmd_exe_program(r"C:\deck\dot-agent-deck.exe"),
            r"C:\deck\dot-agent-deck.exe"
        );
    }

    /// Caret-escapes each space rather than wrapping the whole token in
    /// `"…"` — see the function's doc comment for why a literal-quote-based
    /// wrap here is unsafe once combined with `quote_cmd_exe_arg`'s own
    /// necessary quoting of the trailing command.
    #[test]
    fn escape_cmd_exe_program_never_produces_a_leading_quote() {
        let escaped = escape_cmd_exe_program(r"C:\Program Files\My Deck\dot-agent-deck.exe");
        assert_eq!(escaped, r"C:\Program^ Files\My^ Deck\dot-agent-deck.exe");
        assert!(!escaped.starts_with('"'));
    }

    #[test]
    fn quote_cmd_exe_arg_leaves_a_word_with_no_special_characters_unquoted() {
        assert_eq!(quote_cmd_exe_arg("dev"), "dev");
    }

    #[test]
    fn quote_cmd_exe_arg_quotes_and_caret_escapes_a_spaced_argument() {
        assert_eq!(quote_cmd_exe_arg("npm run dev"), r#"^"npm run dev^""#);
    }

    /// The finding A1 shape: every `cmd.exe` metacharacter this function
    /// promises to neutralize — `"`, `&`, `>` — must come out caret-escaped,
    /// with no bare occurrence left for `cmd.exe`'s own scan to see. Checked
    /// by walking every character rather than searching for a fixed
    /// substring, since a *correctly* caret-escaped `&` followed by a space
    /// (`^& `) still contains the two-byte substring `"& "` — a naive
    /// `contains` check would flag correct output as broken.
    #[test]
    fn quote_cmd_exe_arg_neutralizes_every_cmd_exe_metacharacter() {
        let quoted = quote_cmd_exe_arg(r#"echo ok" & type nul > injected.marker & rem ""#);
        for bare in ['"', '&', '>'] {
            let mut prev = None;
            for c in quoted.chars() {
                if c == bare {
                    assert_eq!(
                        prev,
                        Some('^'),
                        "every `{bare}` in {quoted:?} must be immediately preceded by `^`"
                    );
                }
                prev = Some(c);
            }
        }
    }

    /// A run of backslashes immediately before the closing quote must be
    /// doubled so the CRT parser sees a literal backslash run, not an
    /// escape of the delimiter itself.
    #[test]
    fn quote_cmd_exe_arg_doubles_a_trailing_backslash_run_before_the_closing_quote() {
        let quoted = quote_cmd_exe_arg(r"C:\Program Files\");
        // Two literal backslashes (doubled from one), then the caret-escaped
        // closing quote.
        assert_eq!(quoted, "^\"C:\\Program Files\\\\^\"");
    }

    /// Fork issue #423 review finding B1: a real LF must never survive into
    /// the emitted line — `quote_cmd_exe_arg` replaces it with a space
    /// before either quoting pass runs, rather than caret-escaping it,
    /// since neither `cmd.exe`'s own scan nor a typed-PTY submit can be
    /// stopped by a caret. The real-`cmd.exe` behavioral proof is
    /// `watch_invocation_neutralizes_control_characters_through_real_cmd_exe`
    /// in `mode_manager.rs`; this is the fast, deterministic companion pin.
    #[test]
    fn quote_cmd_exe_arg_replaces_a_real_newline_with_a_space() {
        let quoted = quote_cmd_exe_arg("line one\nline two");
        assert!(!quoted.contains('\n') && !quoted.contains('\r'));
        assert_eq!(quoted, r#"^"line one line two^""#);
    }

    /// Same fix, the `\r\n` shape — each of the two bytes is independently
    /// replaced with a space, so the pair becomes two spaces rather than
    /// one.
    #[test]
    fn quote_cmd_exe_arg_replaces_a_crlf_pair_with_two_spaces() {
        let quoted = quote_cmd_exe_arg("line one\r\nline two");
        assert!(!quoted.contains('\n') && !quoted.contains('\r'));
        assert_eq!(quoted, r#"^"line one  line two^""#);
    }

    /// The program-name position carries the same hazard (a `current_exe()`
    /// path containing a raw newline, low-reachability but still a
    /// documented sibling of B1) and the same fix.
    #[test]
    fn escape_cmd_exe_program_replaces_a_real_newline_with_a_space() {
        let escaped = escape_cmd_exe_program("C:\\a\nb\\deck.exe");
        assert!(!escaped.contains('\n') && !escaped.contains('\r'));
        assert_eq!(escaped, "C:\\a^ b\\deck.exe");
    }

    /// N2 / S4: `;` and `=` are `cmd.exe` command-line delimiters exactly
    /// like `,` (already escaped) and are legal in NTFS path components, so
    /// leaving them bare would split the program token the same way an
    /// unescaped space did in #157.
    #[test]
    fn escape_cmd_exe_program_escapes_semicolon_and_equals() {
        assert_eq!(
            escape_cmd_exe_program("C:\\a;b\\deck.exe"),
            "C:\\a^;b\\deck.exe"
        );
        assert_eq!(
            escape_cmd_exe_program("C:\\a=b\\deck.exe"),
            "C:\\a^=b\\deck.exe"
        );
    }

    /// Fork issue #423 review finding C1: `sanitize_cmd_exe_control_chars`
    /// widened from CR/LF specifically (finding B1) to control characters
    /// generally, because the Windows console's cooked-mode line editor
    /// consumes `ESC` (clears the whole input buffer), `BS`/`DEL` (deletes
    /// backwards) and `ETX` (cancels the line) as editing commands before
    /// `cmd.exe`'s own grammar is ever consulted — the same "the character
    /// has to be gone before either quoting pass ever sees it" argument B1
    /// established, generalized to the whole class. This is the fast,
    /// deterministic string-level pin; the real-`cmd.exe` proof — which can
    /// only exercise the `/C <string>` delivery path, not the
    /// interactive-PTY console line editor a real Windows console applies —
    /// is `watch_invocation_neutralizes_control_characters_through_real_cmd_exe`
    /// in `mode_manager.rs`.
    #[test]
    fn quote_cmd_exe_arg_replaces_every_control_character_with_a_space() {
        let quoted = quote_cmd_exe_arg("a\x1bb\x08c\x03d\te");
        assert!(
            !quoted.chars().any(|c| c.is_control()),
            "a control character survived quoting: {quoted:?}"
        );
    }
}
