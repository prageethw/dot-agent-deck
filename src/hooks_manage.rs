use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

const HOOK_TYPES: &[&str] = &[
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Notification",
    "Stop",
    "PreCompact",
    "SubagentStart",
    "SubagentStop",
];

/// Claude Code's user settings file, in the location Claude itself uses:
/// `~/.claude/settings.json`.
///
/// PRD #163 M1: resolved through the platform seam rather than a raw `$HOME`
/// read, so that on Windows — where `$HOME` is normally unset — this finds
/// `%USERPROFILE%\.claude` instead of missing it entirely.
///
/// PRD #163 review: the seam function is
/// [`crate::platform::paths::home_dir_with_tmp_fallback`], *not* `home_dir`,
/// because the raw read this replaced fell back to `/tmp` when `$HOME` was unset.
/// Unix behavior is therefore byte-for-byte what it was — including the
/// `/tmp/.claude/settings.json` an unset `$HOME` resolves to.
fn settings_path() -> PathBuf {
    crate::platform::paths::home_dir_with_tmp_fallback()
        .join(".claude")
        .join("settings.json")
}

/// Read `path` the LENIENT way: any read or parse failure collapses to an empty
/// config. This is the pre-fix behavior, kept ONLY for [`uninstall_impl`] callers
/// — a config the deck cannot parse is treated as having nothing to uninstall.
/// Deliberately not fixed here (out of scope, "uninstall over malformed
/// settings" is its own follow-up): uninstall never WRITES fabricated content
/// over a file it couldn't read, it only fails to find rules to remove from it,
/// so the blast radius is smaller than install's — but it is not zero, and
/// widening this fix to uninstall is left for that follow-up.
fn read_settings_lenient(path: &Path) -> Value {
    match std::fs::read_to_string(path) {
        Ok(contents) => serde_json::from_str(&contents).unwrap_or_else(|_| json!({})),
        Err(_) => json!({}),
    }
}

/// Read `path` the STRICT way used by every install path. Only a MISSING file
/// (`ErrorKind::NotFound`) means empty — mirroring
/// `codex_hooks_manage::install_to`'s contract (`:290-316`). Malformed JSON is
/// backed up next to the original (`<path>.bak`, leaving the original bytes on
/// disk untouched) and returned as an `Err` so every install caller skips the
/// write instead of silently collapsing the user's settings to `{}` — the old
/// behavior here mapped ANY parse error to an empty config, so a settings.json
/// invalidated by a single trailing comma came back with `model`, `env`, and
/// every `permissions` entry destroyed, while the run reported success.
fn load_settings_or_refuse(path: &Path) -> io::Result<Value> {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(value) => Ok(value),
            Err(parse_err) => {
                let backup = path.with_extension("json.bak");
                let _ = std::fs::write(&backup, &bytes);
                Err(io::Error::new(
                    ErrorKind::InvalidData,
                    format!(
                        "{} is not valid JSON (left unchanged, original preserved at {}): \
                         {parse_err}",
                        path.display(),
                        backup.display()
                    ),
                ))
            }
        },
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(json!({})),
        Err(_) => Ok(json!({})),
    }
}

fn write_settings(path: &PathBuf, settings: &Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let contents = serde_json::to_string_pretty(settings)?;
    std::fs::write(path, contents)
}

/// The fixed command signature that identifies a deck-authored rule, mirroring
/// `codex_hooks_manage::HOOK_COMMAND_SUFFIX` (`:81`) and
/// `devin_hooks_manage::HOOK_COMMAND_SUFFIX` (`:70`). `--agent` defaults to
/// `CliAgent::ClaudeCode` (`src/main.rs:60-64`), so `<path> hook --agent
/// claude-code` is a valid invocation, behaviourally identical to the bare
/// `<path> hook` this used to write — writing the explicit form is what lets a
/// rule be identified by this SUFFIX alone, regardless of the executable path
/// (or its basename) preceding it.
const HOOK_COMMAND_SUFFIX: &str = "hook --agent claude-code";

/// The compiled crate's own binary name (`"dot-agent-deck"` — upstream and every
/// fork alike; the crate name itself is never renamed). Every hook rule written
/// before this fix carries exactly this as its executable's basename in the
/// LEGACY `<path> hook` shape — see [`is_legacy_deck_rule`].
const DEFAULT_BINARY_NAME: &str = env!("CARGO_PKG_NAME");

/// Build a rule object in the new hooks format:
/// `{ "hooks": [{"type": "command", "command": "..."}] }`
/// For Notification, adds a matcher for permission_prompt.
fn make_rule(binary_path: &str, hook_type: &str) -> Value {
    let command = format!(
        "{} {HOOK_COMMAND_SUFFIX}",
        shell_quote_if_needed(binary_path)
    );
    let command_obj = json!({
        "type": "command",
        "command": command
    });

    if hook_type == "Notification" {
        json!({
            "matcher": "permission_prompt",
            "hooks": [command_obj]
        })
    } else {
        json!({
            "hooks": [command_obj]
        })
    }
}

/// Single-quote `path` for a POSIX shell only when it contains a character
/// outside a conservative safe set; otherwise return it unchanged. Mirrors
/// `devin_hooks_manage::shell_quote_if_needed` (`:232-245`, tested by
/// `install_quotes_a_binary_path_with_spaces`, `:726-730`) — a binary path
/// containing whitespace (e.g. `/Applications/My Deck/dot-agent-deck`) written
/// unquoted splits into extra shell tokens and the command no longer parses to
/// the intended argv.
fn shell_quote_if_needed(path: &str) -> String {
    fn is_safe(b: u8) -> bool {
        b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'/' | b'.' | b'_' | b'-' | b'+' | b'=' | b':' | b'@' | b'%' | b','
            )
    }
    if !path.is_empty() && path.bytes().all(is_safe) {
        path.to_string()
    } else {
        format!("'{}'", path.replace('\'', r"'\''"))
    }
}

/// Undo [`shell_quote_if_needed`]: strip a single-quoted wrapper and unescape
/// `'\''` back to `'`, or return `exe` unchanged if it was never quoted.
fn unquote_if_needed(exe: &str) -> std::borrow::Cow<'_, str> {
    match exe.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        Some(inner) => std::borrow::Cow::Owned(inner.replace(r"'\''", "'")),
        None => std::borrow::Cow::Borrowed(exe),
    }
}

/// Ensure `settings["hooks"]` is an object and return a mutable reference to it.
fn ensure_hooks_object(settings: &mut Value) -> &mut serde_json::Map<String, Value> {
    let obj = settings
        .as_object_mut()
        .expect("settings must be an object");
    if !obj.contains_key("hooks") || !obj["hooks"].is_object() {
        obj.insert("hooks".into(), json!({}));
    }
    obj.get_mut("hooks").unwrap().as_object_mut().unwrap()
}

/// Ensure `hooks_obj[hook_type]` is an array and return a mutable reference.
fn ensure_hook_array<'a>(
    hooks_obj: &'a mut serde_json::Map<String, Value>,
    hook_type: &str,
) -> &'a mut Vec<Value> {
    if !hooks_obj.contains_key(hook_type) || !hooks_obj[hook_type].is_array() {
        hooks_obj.insert(hook_type.into(), json!([]));
    }
    hooks_obj
        .get_mut(hook_type)
        .unwrap()
        .as_array_mut()
        .unwrap()
}

fn install_impl(settings: &mut Value, binary_path: &str) -> (Vec<&'static str>, Vec<&'static str>) {
    let hooks_obj = ensure_hooks_object(settings);

    // Clean up deck entries for hook types no longer in HOOK_TYPES. These are
    // gone from HOOK_TYPES entirely, so any deck rule there is stale regardless
    // of which binary wrote it — use the generic, binary-agnostic predicate.
    let all_keys: Vec<String> = hooks_obj.keys().cloned().collect();
    for key in all_keys {
        if !HOOK_TYPES.contains(&key.as_str()) {
            if let Some(arr) = hooks_obj.get_mut(&key).and_then(|v| v.as_array_mut()) {
                arr.retain(|rule| !rule_is_ours(rule));
            }
            // Remove the key entirely if the array is now empty
            if hooks_obj
                .get(&key)
                .and_then(|v| v.as_array())
                .is_some_and(|a| a.is_empty())
            {
                hooks_obj.remove(&key);
            }
        }
    }

    let mut installed = Vec::new();
    let mut skipped = Vec::new();

    for &hook_type in HOOK_TYPES {
        let rules = ensure_hook_array(hooks_obj, hook_type);

        // Prune STALE deck-owned rules sharing the installing binary's own
        // basename — the shape N worktree builds actually take: every
        // `target/debug/dot-agent-deck` is a distinct real path with the SAME
        // basename, so a rebuilt or removed worktree leaves a dead rule with
        // that basename behind, and a fresh install from a surviving worktree
        // is the natural point to drop it. Scoped narrowly two ways: (1) only
        // rules ALREADY identified as deck-owned by `rule_is_ours` — never a
        // general "delete anything pointing at a missing path" sweep, which
        // would delete a user's own hooks for tools that simply are not
        // installed right now (test 014's coexisting `nonexistent-tool` rule);
        // (2) only rules whose basename matches the CURRENTLY installing
        // binary's basename — a genuinely different-looking deck binary
        // installed under a fictional/not-yet-real path (as most of this
        // file's fixtures are) must not be swept up just because it happens
        // not to exist on disk (test 003 pins this: installing `/b/…` must
        // never prune `/a/…`'s unrelated rule).
        rules.retain(|rule| !rule_is_dead_deck_rule(rule, binary_path));

        let expected = make_rule(binary_path, hook_type);

        let already_current = rules.iter().any(|rule| rule == &expected);
        let before = rules.len();

        // Normalize down to a single fresh rule, but only for THIS binary —
        // leave rules belonging to a genuinely different deck binary alone —
        // except a LEGACY rule under the historical default name, which always
        // migrates to whichever binary is currently installing.
        rules.retain(|rule| !rule_matches_binary(rule, binary_path));
        let removed = before - rules.len();
        rules.push(expected);

        if already_current && removed == 1 {
            skipped.push(hook_type);
        } else {
            installed.push(hook_type);
        }
    }

    (installed, skipped)
}

/// Outcome of [`uninstall_impl`]: which hook types had at least one deck rule
/// removed, and the total number of individual rules removed across all of
/// them. Reporting the actual count is what makes "matched nothing"
/// distinguishable from "removed some" — a message that always reads
/// "No dot-agent-deck hooks found to remove." is correct-sounding but silently
/// wrong the moment the matcher goes blind: it prints on every run whether or
/// not anything was actually there.
struct UninstallOutcome {
    hook_types: Vec<&'static str>,
    rules_removed: usize,
}

fn uninstall_impl(settings: &mut Value) -> UninstallOutcome {
    let hooks = match settings.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        Some(h) => h,
        None => {
            return UninstallOutcome {
                hook_types: Vec::new(),
                rules_removed: 0,
            };
        }
    };

    let mut hook_types = Vec::new();
    let mut rules_removed = 0;

    for &hook_type in HOOK_TYPES {
        if let Some(arr) = hooks.get_mut(hook_type).and_then(|v| v.as_array_mut()) {
            let before = arr.len();
            arr.retain(|rule| !rule_is_ours(rule));
            let removed = before - arr.len();
            if removed > 0 {
                hook_types.push(hook_type);
                rules_removed += removed;
            }
        }
    }

    UninstallOutcome {
        hook_types,
        rules_removed,
    }
}

// --- Identify deck rules by command SUFFIX, not by basename ---
//
// The old matcher looked for the literal substring "dot-agent-deck" in a rule's
// command, which blinds it the moment the binary runs under any other filename
// (this fork's `worker-agent-deck`, or any other renamed build) — the rule it
// just wrote becomes invisible to the tool that wrote it. A second revision
// relocated the hardcoding into a basename FRAGMENT check instead, which failed
// destructively on a crate rename (a crate named `dot-x` derives fragment `"x"`,
// and uninstall then deletes unrelated user hooks) and could only migrate a
// legacy rule when the INSTALLING binary's own name looked deck-ish, which made
// a coexisting fork/upstream install silently delete each other's rules.
//
// The replacement identifies a rule by its command's exact SUFFIX,
// [`HOOK_COMMAND_SUFFIX`] — mirroring `codex_hooks_manage::command_is_deck_owned`
// (`:132-137`) and `devin_hooks_manage::command_is_deck_owned` (`:199-204`). No
// basename check is layered on top: the suffix `"hook --agent claude-code"` is
// specific enough on its own that an unrelated `mytool hook` or `git hook` (test
// 005) does not end with it, and a user hook that merely mentions the deck's
// name as an argument (test 004) does not either.
//
// The one basename check that remains is narrower and different in kind: a
// LEGACY rule (written before this fix, in the bare `<path> hook` shape with no
// `--agent` suffix) is recognised only when its own executable's basename is
// EXACTLY [`DEFAULT_BINARY_NAME`] — never a fragment, and never compared against
// the installing binary's name. See [`is_legacy_deck_rule`].

/// Parse `command` as `<executable> hook --agent claude-code` in the CURRENT
/// format, recovering the executable by parsing from the RIGHT
/// (`strip_suffix`), not by counting whitespace-split tokens — so a quoted (or,
/// historically, unquoted) executable path containing spaces still round-trips
/// (test 008). The returned token may still be shell-quoted; pass it through
/// [`unquote_if_needed`] before comparing it as a path.
fn current_format_executable(command: &str) -> Option<&str> {
    let exe = command.trim_end().strip_suffix(HOOK_COMMAND_SUFFIX)?;
    let exe = exe.strip_suffix(' ')?;
    if exe.is_empty() { None } else { Some(exe) }
}

/// Parse `command` as the LEGACY, pre-fix `<executable> hook` shape — no
/// `--agent` suffix, never quoted — recovering the executable the same
/// parse-from-the-right way as [`current_format_executable`], so a historical
/// unquoted spaced path (test 009) is still recoverable even though counting
/// whitespace-split tokens could not locate its executable.
fn legacy_format_executable(command: &str) -> Option<&str> {
    let exe = command.trim_end().strip_suffix("hook")?;
    let exe = exe.strip_suffix(' ')?;
    if exe.is_empty() { None } else { Some(exe) }
}

/// Whether `command` is a LEGACY deck rule: the bare `<executable> hook` shape,
/// where `executable`'s basename is EXACTLY [`DEFAULT_BINARY_NAME`] — the
/// historical default every rule was written under before this fix existed.
/// Scoped to an exact basename match (never a fragment, never the installing
/// binary's own name) so a user tool whose basename merely contains "deck"
/// (test 012) or ends in the literal word "hook" (test 005) is never swept up.
fn is_legacy_deck_rule(command: &str) -> bool {
    legacy_format_executable(command)
        .and_then(|exe| Path::new(exe).file_name())
        .and_then(|n| n.to_str())
        == Some(DEFAULT_BINARY_NAME)
}

/// Whether `existing` and `installing` (both already unquoted) name the SAME
/// binary, so a rule for `existing` should be replaced rather than left
/// alongside a fresh rule for `installing`. Symlinks are resolved first — the
/// real-world case this exists for: a `dot-agent-deck` symlink pointing at a
/// renamed `worker-agent-deck` collapses to one rule. Every path here can fail
/// to resolve (most callers are test fixtures never written to disk), so
/// resolution failure falls back to a literal string comparison; this never
/// panics or unwraps on it.
fn executables_match(existing: &str, installing: &str) -> bool {
    if let (Ok(existing_real), Ok(installing_real)) = (
        Path::new(existing).canonicalize(),
        Path::new(installing).canonicalize(),
    ) {
        return existing_real == installing_real;
    }
    existing == installing
}

/// Every command string a rule carries, from either JSON shape: the current
/// nested `{"hooks": [{"command": ...}]}` or the legacy flat
/// `{"command": ...}`.
fn rule_commands(rule: &Value) -> impl Iterator<Item = &str> {
    let nested = rule
        .get("hooks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|hook| hook.get("command").and_then(Value::as_str));
    let flat = rule.get("command").and_then(Value::as_str).into_iter();
    nested.chain(flat)
}

/// Whether `command` is a deck-owned hook command, generically — no specific
/// installing binary to compare against. True for any CURRENT-format command
/// (any executable, by the suffix alone) or any LEGACY-format command whose
/// executable is exactly [`DEFAULT_BINARY_NAME`].
fn command_is_ours(command: &str) -> bool {
    current_format_executable(command).is_some() || is_legacy_deck_rule(command)
}

/// Whether `command` is a deck-owned hook command that should be treated as
/// belonging to the SPECIFIC binary currently installing: either a
/// current-format command whose executable matches `binary_path`
/// ([`executables_match`]), or a legacy rule — which always migrates to
/// whichever binary is installing now, regardless of that binary's own name —
/// migration is keyed off the legacy RULE, never the installer.
fn command_matches_binary(command: &str, binary_path: &str) -> bool {
    if let Some(exe) = current_format_executable(command) {
        return executables_match(&unquote_if_needed(exe), binary_path);
    }
    is_legacy_deck_rule(command)
}

/// Whether `rule` (in either JSON shape) is a deck-owned rule, generically.
fn rule_is_ours(rule: &Value) -> bool {
    rule_commands(rule).any(command_is_ours)
}

/// Whether `rule` is a deck-owned rule for the same binary as `binary_path`.
fn rule_matches_binary(rule: &Value, binary_path: &str) -> bool {
    rule_commands(rule).any(|cmd| command_matches_binary(cmd, binary_path))
}

/// Whether `rule` is a deck-owned rule sharing `binary_path`'s own basename
/// whose executable no longer resolves on disk. `owned_command_executable`
/// returns `None` for any command that is not deck-owned by either shape, so
/// this can never prune a user's own hook — only a rule the deck itself would
/// recognise as its own, and only when it looks like a stale sibling of the
/// binary currently installing (same basename, different — now-dead — path).
/// A deck rule for a genuinely different-looking binary is left to
/// [`rule_matches_binary`]/[`is_legacy_deck_rule`] instead, since most of this
/// file's own fixtures are fictional paths that were never on disk to begin
/// with and must not be swept up just because they don't exist.
fn rule_is_dead_deck_rule(rule: &Value, binary_path: &str) -> bool {
    let installing_basename = Path::new(binary_path).file_name();
    rule_commands(rule).any(|cmd| {
        owned_command_executable(cmd).is_some_and(|exe| {
            Path::new(&exe).file_name() == installing_basename && !Path::new(&exe).exists()
        })
    })
}

/// The literal, unquoted executable path a deck-owned command names, or `None`
/// if `command` is not deck-owned by either shape. Used only to check whether
/// that binary still exists on disk.
fn owned_command_executable(command: &str) -> Option<String> {
    if let Some(exe) = current_format_executable(command) {
        return Some(unquote_if_needed(exe).into_owned());
    }
    if is_legacy_deck_rule(command) {
        return legacy_format_executable(command).map(str::to_string);
    }
    None
}

/// Silently install hooks if Claude Code is detected.
/// Intended for dashboard startup — never prints to stdout.
pub fn auto_install() {
    let path = settings_path();
    if path.parent().is_none_or(|p| !p.exists()) {
        return;
    }

    let binary_path = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "dot-agent-deck".into());

    let mut settings = match load_settings_or_refuse(&path) {
        Ok(settings) => settings,
        Err(e) => {
            tracing::warn!("auto-install: {e}");
            return;
        }
    };
    let (installed, _skipped) = install_impl(&mut settings, &binary_path);

    if installed.is_empty() {
        return;
    }

    if let Err(e) = write_settings(&path, &settings) {
        tracing::warn!("auto-install: failed to write Claude Code hooks: {e}");
        return;
    }

    tracing::info!("auto-installed Claude Code hooks: {}", installed.join(", "));
}

/// Auto-install to a custom settings path (for testing).
pub fn auto_install_to(path: &PathBuf) {
    if path.parent().is_none_or(|p| !p.exists()) {
        return;
    }

    let binary_path = "dot-agent-deck".to_string();
    let mut settings = match load_settings_or_refuse(path) {
        Ok(settings) => settings,
        Err(e) => {
            tracing::warn!("auto-install: {e}");
            return;
        }
    };
    let (installed, _skipped) = install_impl(&mut settings, &binary_path);

    if installed.is_empty() {
        return;
    }

    write_settings(path, &settings).expect("failed to write settings");
}

pub fn install() {
    let binary_path = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "dot-agent-deck".into());

    let path = settings_path();
    let mut settings = match load_settings_or_refuse(&path) {
        Ok(settings) => settings,
        Err(e) => {
            eprintln!("Error: {e}");
            return;
        }
    };

    let (installed, skipped) = install_impl(&mut settings, &binary_path);

    if let Err(e) = write_settings(&path, &settings) {
        eprintln!("Error writing {}: {e}", path.display());
        return;
    }

    if !installed.is_empty() {
        println!("Installed hooks: {}", installed.join(", "));
    }
    if !skipped.is_empty() {
        println!("Already installed (skipped): {}", skipped.join(", "));
    }
    println!("Settings file: {}", path.display());
}

pub fn uninstall() {
    let path = settings_path();
    let mut settings = read_settings_lenient(&path);

    let outcome = uninstall_impl(&mut settings);

    if let Err(e) = write_settings(&path, &settings) {
        eprintln!("Error writing {}: {e}", path.display());
        return;
    }

    if outcome.rules_removed == 0 {
        println!("No dot-agent-deck hooks found to remove.");
    } else {
        println!(
            "Removed {} hook rule{}: {}",
            outcome.rules_removed,
            if outcome.rules_removed == 1 { "" } else { "s" },
            outcome.hook_types.join(", ")
        );
    }
    println!("Settings file: {}", path.display());
}

// --- Testable versions that accept a custom path ---

pub fn install_to(path: &PathBuf, binary_path: &str) {
    let mut settings = match load_settings_or_refuse(path) {
        Ok(settings) => settings,
        Err(e) => {
            tracing::warn!("install: {e}");
            return;
        }
    };
    install_impl(&mut settings, binary_path);
    write_settings(path, &settings).expect("failed to write settings");
}

pub fn uninstall_from(path: &PathBuf) {
    let mut settings = read_settings_lenient(path);
    uninstall_impl(&mut settings);
    write_settings(path, &settings).expect("failed to write settings");
}
