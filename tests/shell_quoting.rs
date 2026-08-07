//! Unit + behavioural coverage for `common::sh_quote` (issue #57).
//!
//! Several test fixtures build small `/bin/sh` programs by interpolating paths
//! that derive from developer-controlled roots (`TMPDIR` via
//! `tempfile::tempdir()`, the cargo target dir via `env!("CARGO_BIN_EXE_…")`).
//! Interpolated raw into a double-quoted `"{path}"`, a `"`, `$`, backtick, `\`
//! or newline terminates or expands the string — and PR #54 MEASURED that the
//! result does not fail loudly: the quotes rebalanced into valid shell and the
//! embedded `$(…)` executed while the fixture still reported success.
//!
//! `sh_quote` is the fix for the sites where the value must genuinely appear in
//! the script's text (the environment route is preferred where it fits — see
//! `common::write_login_shell_pinning_dir`). Fast tier on purpose: it is pure
//! string work plus one tiny `/bin/sh` invocation, so it guards every consumer
//! without paying for the L2 harness.

mod common;

use common::sh_quote;

/// The exact rendering matters: a consumer splices the result straight into
/// script source, so a change here is a change to every generated script.
#[test]
fn sh_quote_wraps_plain_values_in_single_quotes() {
    assert_eq!(sh_quote("/tmp/plain"), "'/tmp/plain'");
    assert_eq!(sh_quote(""), "''");
}

/// The one character single-quoting cannot contain: close, emit an escaped
/// quote outside any quoting, reopen.
#[test]
fn sh_quote_escapes_embedded_single_quotes() {
    assert_eq!(sh_quote("it's"), r#"'it'\''s'"#);
    assert_eq!(sh_quote("'"), r#"''\'''"#);
    assert_eq!(sh_quote("''"), r#"''\'''\'''"#);
}

/// Everything else is inert inside `'…'` and must pass through byte-for-byte —
/// no doubling, no backslashes added.
#[test]
fn sh_quote_passes_other_metacharacters_through_verbatim() {
    for raw in [
        r#"say "hi""#,
        "$HOME",
        "$(touch pwned)",
        "`touch pwned`",
        r"back\slash",
        "two\nlines",
        "a;b|c&d",
        "*?[]{}",
        "#comment",
        "tab\there",
    ] {
        assert_eq!(
            sh_quote(raw),
            format!("'{raw}'"),
            "inside single quotes POSIX sh expands nothing, so `{raw}` must be \
             wrapped and otherwise left alone"
        );
    }
}

/// A leading `-` must not be readable as a flag: the result is always fully
/// quoted, so it arrives as one ordinary word.
#[test]
fn sh_quote_neutralizes_a_leading_dash() {
    assert_eq!(sh_quote("-rf"), "'-rf'");
    assert!(sh_quote("--force").starts_with('\''));
}

/// The property that actually matters, proven against a real `/bin/sh` rather
/// than by inspecting the string: a quoted value reaches the script as ONE
/// literal word, byte-for-byte, and none of it is executed.
///
/// Reintroducing the raw `"{value}"` interpolation fails this immediately — the
/// embedded `"` rebalances the quoting and the `$(…)` runs.
#[cfg(unix)]
#[test]
fn sh_quoted_values_reach_a_real_shell_verbatim_and_unexecuted() {
    let scratch = tempfile::tempdir().expect("scratch tempdir");

    // Every character the double-quoted-interpolation bug is sensitive to, plus
    // a single quote so the `'\''` path is exercised end to end.
    let nasty = r#"val "$(touch pwned)" `touch pwned` \x it's $HOME"#;

    let out = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("printf '%s' {}", sh_quote(nasty)))
        .current_dir(scratch.path())
        .output()
        .expect("run /bin/sh probe");

    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        nasty,
        "an sh_quote'd value must survive a real shell verbatim.\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !scratch.path().join("pwned").exists(),
        "a `pwned` marker means the `$(…)` inside the value was parsed as shell \
         syntax and EXECUTED — the value is not being quoted"
    );
}
