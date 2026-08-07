//! Unit + behavioural coverage for the fixture path-safety helpers
//! (`common::sh_quote`, `common::sh_quote_path`) — issue #57.
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
//! `common::write_login_shell_pinning_dir`). A path also has to SURVIVE the
//! trip: on Unix a pathname is a byte string, so `to_string_lossy()` can hand a
//! fixture a U+FFFD pathname that does not exist — the same silent-wrong-target
//! failure in a different coat — which is why the path-taking helpers fail
//! closed and this file pins that contract too.
//!
//! Fast tier on purpose: pure string work plus a few tiny `/bin/sh`
//! invocations, so it guards every consumer without paying for the L2 harness.

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

/// A value beginning with `-` is quoted like any other: it reaches the consumer
/// as ONE literal word.
///
/// Deliberately NOT asserted: that the consumer stops treating it as an option.
/// Quoting drives shell word-splitting, not the receiving command's argv
/// parsing — `rm '-rf' dir` still hands `rm` the flag. The mechanism for that is
/// `--` before untrusted operands at the call site, and no fixture here needs
/// it (every call site is a command name or a redirection target).
#[test]
fn sh_quote_wraps_a_leading_dash_as_one_literal_word() {
    assert_eq!(sh_quote("-rf"), "'-rf'");
    assert_eq!(sh_quote("--force"), "'--force'");

    // The property, proven against a real `/bin/sh`: exactly one positional
    // parameter, holding the value byte-for-byte.
    #[cfg(unix)]
    {
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!(
                "set -- {}; printf '%s|%s' \"$#\" \"$1\"",
                sh_quote("-rf --force x")
            ))
            .output()
            .expect("run /bin/sh probe");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "1|-rf --force x",
            "an sh_quote'd value must reach the script as ONE word.\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
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

/// Build a `Path` whose bytes are a valid Unix pathname but NOT valid UTF-8.
///
/// `0x80` is a continuation byte with no lead byte, so `to_str()` returns
/// `None` and `to_string_lossy()` substitutes U+FFFD — a different pathname.
#[cfg(unix)]
fn non_utf8_path(parent: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::ffi::OsStrExt;
    parent.join(std::ffi::OsStr::from_bytes(b"sh\x80im"))
}

/// Run `f`, expecting it to panic, and return the panic message.
///
/// The default hook is muted for the duration so an EXPECTED panic does not
/// print a scary backtrace inside a passing test.
fn panic_message(f: impl FnOnce() + std::panic::UnwindSafe) -> String {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let payload = std::panic::catch_unwind(f);
    std::panic::set_hook(previous);

    let payload = payload.expect_err("the call must panic, not return");
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_else(|| "<non-string panic payload>".to_string())
}

/// A non-UTF-8 path must FAIL CLOSED, not be rewritten.
///
/// On Unix a pathname is a byte string, so a valid path need not be valid
/// UTF-8. `to_string_lossy()` turns the invalid bytes into U+FFFD, which is a
/// DIFFERENT and normally non-existent pathname — the generated script would
/// then write to, or resolve from, somewhere else while every assertion built
/// on the same lossy string still passed. That is the identical "passes while
/// testing nothing" failure mode as issue #57 itself, so the contract is a loud
/// panic naming the path.
#[cfg(unix)]
#[test]
fn sh_quote_path_rejects_a_non_utf8_path_loudly() {
    let scratch = tempfile::tempdir().expect("scratch tempdir");
    let bad = non_utf8_path(scratch.path());

    // The hazard, spelled out: `to_str()` refuses this path while
    // `to_string_lossy()` happily returns a DIFFERENT one that quoting accepts.
    assert!(
        bad.to_str().is_none() && !bad.to_string_lossy().is_empty(),
        "the fixture path must actually be non-UTF-8 for this test to mean \
         anything: {bad:?}"
    );

    let msg = panic_message(|| {
        let _ = common::sh_quote_path(&bad);
    });
    assert!(
        msg.contains("not valid UTF-8"),
        "a non-UTF-8 path must panic, not be quoted lossily. Got: {msg}"
    );
    assert!(
        msg.contains(scratch.path().to_str().expect("scratch dir is UTF-8")),
        "the panic must name the offending path so it is diagnosable. Got: {msg}"
    );
}

/// Same contract at the other fixture entry point: the login-shell pinner must
/// reject a non-UTF-8 keep dir BEFORE it writes or probes anything.
///
/// This is the path the auditor traced on PR #59: with a non-UTF-8 `TMPDIR` the
/// shim tree lands below it, the login shell prepends a U+FFFD directory that
/// does not exist, `manager_010`'s `claude` neutralizer becomes unreachable, and
/// the inherited PATH's real Homebrew `claude` runs instead — while
/// `assert_login_shell_keeps_dir_first` compares the captured PATH against the
/// same lossy string it supplied and passes.
#[cfg(unix)]
#[test]
fn login_shell_pinner_rejects_a_non_utf8_keep_dir_before_writing_anything() {
    let scratch = tempfile::tempdir().expect("scratch tempdir");
    let keep = non_utf8_path(scratch.path());

    let msg = panic_message(|| {
        let _ = common::write_login_shell_pinning_dir(scratch.path(), &keep);
    });
    assert!(
        msg.contains("not valid UTF-8"),
        "a non-UTF-8 keep dir must panic, not silently pin a U+FFFD directory. \
         Got: {msg}"
    );
    assert!(
        !scratch.path().join("login-shell.sh").exists(),
        "the check must fail closed BEFORE the fixture writes its login shell — \
         nothing may be generated or spawned from a path that was rewritten"
    );
}
