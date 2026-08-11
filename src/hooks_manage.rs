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

fn read_settings(path: &PathBuf) -> Value {
    match std::fs::read_to_string(path) {
        Ok(contents) => serde_json::from_str(&contents).unwrap_or_else(|_| json!({})),
        Err(_) => json!({}),
    }
}

fn write_settings(path: &PathBuf, settings: &Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let contents = serde_json::to_string_pretty(settings)?;
    std::fs::write(path, contents)
}

/// Build a rule object in the new hooks format:
/// `{ "hooks": [{"type": "command", "command": "..."}] }`
/// For Notification, adds a matcher for permission_prompt.
fn make_rule(binary_path: &str, hook_type: &str) -> Value {
    let command_obj = json!({
        "type": "command",
        "command": format!("{binary_path} hook")
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
        let expected = make_rule(binary_path, hook_type);

        let already_current = rules.iter().any(|rule| rule == &expected);
        let before = rules.len();

        // Normalize down to a single fresh rule, but only for THIS binary —
        // leave rules belonging to a genuinely different deck binary alone
        // (fork issue #229 part B).
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
/// them. Reporting the actual count (fork issue #229 part C) is what makes
/// "matched nothing" distinguishable from "removed some" — the prior message
/// ("No dot-agent-deck hooks found to remove.") was correct-sounding and wrong
/// in exactly the failure case the issue is about: it printed on every run once
/// the matcher went blind, whether or not anything was actually there.
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

// --- fork issue #229: binary-agnostic rule identification ---
//
// The old matcher looked for the literal substring "dot-agent-deck" in a rule's
// command, which blinds it the moment the binary runs under any other filename
// (this fork's `worker-agent-deck`, CLAUDE.md rule 21, or any other renamed
// build) — the rule it just wrote becomes invisible to the tool that wrote it.
// The replacement identifies a rule by its STRUCTURE — an executable path
// followed by exactly the single argument `hook` — paired with a check on the
// executable token itself, mirroring codex_hooks_manage.rs's
// `command_is_deck_owned` (`:132-137`): matching the suffix alone would sweep
// up an unrelated `mytool hook` or `git hook` (test 005), so the exact-shape
// check is paired with recognising the executable, never used alone.

/// The compiled crate's own binary name (`"dot-agent-deck"`— upstream and this
/// fork alike; CLAUDE.md rule 21 keeps `Cargo.toml` unchanged). Every hook rule
/// written by any version of this project before a renamed install existed
/// carries exactly this as its executable's basename.
const DEFAULT_BINARY_NAME: &str = env!("CARGO_PKG_NAME");

/// The fragment of [`DEFAULT_BINARY_NAME`] that survives a rename in practice —
/// this fork's `worker-agent-deck` drops the `dot-` prefix and keeps the rest.
/// A basename CONTAINING it reads as "still identifiably a deck binary": narrow
/// enough to exclude an unrelated command (`git`, `mytool`) or a genuinely
/// distinct binary that just happens to look deck-ish (`other-deck-name`, fork
/// issue #229 test 003), while still covering a deck binary renamed like this
/// fork's own.
fn deck_name_fragment() -> &'static str {
    DEFAULT_BINARY_NAME
        .strip_prefix("dot-")
        .unwrap_or(DEFAULT_BINARY_NAME)
}

/// Whether `basename` identifies SOME deck binary, generically — without
/// reference to any specific binary currently installing. This is the only
/// identification available to uninstall, which has no installing binary to
/// compare against.
fn basename_identifies_deck_binary(basename: &str) -> bool {
    basename.contains(deck_name_fragment())
}

/// Whether `existing` (an executable path already recorded in some rule) and
/// `installing` (the binary currently running `install`) are the SAME binary,
/// so that a rule for `existing` should be replaced rather than left alongside
/// a fresh rule for `installing`.
///
/// Symlinks are resolved first — the real-world case this exists for: a
/// `dot-agent-deck` symlink pointing at a renamed `worker-agent-deck` collapses
/// to one rule. Every path here can fail to resolve (most are test fixtures
/// that were never written to disk — `canonicalize` errors on all of them), so
/// resolution failure falls back to a literal string comparison; this never
/// panics or unwraps on it.
///
/// The LEGACY ARM is separate and narrower than either: a rule recorded under
/// the bare historical default name is treated as this same deployment's own
/// prior self ONLY when the binary currently installing is itself recognisably
/// a deck binary ([`basename_identifies_deck_binary`]) — not any
/// differently-named rule that merely happens to also look deck-ish. That
/// scoping is what keeps a legacy `dot-agent-deck` rule from being silently
/// absorbed by an unrelated `other-deck-name` install (fork issue #229 test
/// 003) while still letting it be replaced by a genuine renamed reinstall
/// (test 006).
fn executables_match(existing: &str, installing: &str) -> bool {
    if let (Ok(existing_real), Ok(installing_real)) = (
        Path::new(existing).canonicalize(),
        Path::new(installing).canonicalize(),
    ) {
        return existing_real == installing_real;
    }
    if existing == installing {
        return true;
    }
    let existing_base = Path::new(existing).file_name().and_then(|n| n.to_str());
    let installing_base = Path::new(installing).file_name().and_then(|n| n.to_str());
    existing_base == Some(DEFAULT_BINARY_NAME)
        && installing_base.is_some_and(basename_identifies_deck_binary)
}

/// Parse `command` as `<executable> hook` — nothing more, nothing less — and
/// return the executable token, or `None` if the command doesn't have exactly
/// that shape. The exact-suffix requirement is what keeps this from sweeping up
/// an unrelated command that merely ends in the word "hook" (`mytool hook`,
/// `/usr/bin/git hook` — test 005) or a user hook that merely mentions the
/// deck's name as an argument (test 004): neither has `hook` as its ONLY
/// remaining token.
fn command_hook_executable(command: &str) -> Option<&str> {
    let mut parts = command.split_whitespace();
    let exe = parts.next()?;
    if parts.next() != Some("hook") || parts.next().is_some() {
        return None;
    }
    Some(exe)
}

/// Whether `command` is a deck-owned hook command, generically — no specific
/// installing binary to compare against.
fn command_is_ours(command: &str) -> bool {
    command_hook_executable(command)
        .and_then(|exe| Path::new(exe).file_name())
        .and_then(|n| n.to_str())
        .is_some_and(basename_identifies_deck_binary)
}

/// Whether `command` is a deck-owned hook command for the SPECIFIC binary
/// currently installing.
fn command_matches_binary(command: &str, binary_path: &str) -> bool {
    command_hook_executable(command).is_some_and(|exe| executables_match(exe, binary_path))
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

/// Whether `rule` (in either JSON shape) is a deck-owned rule, generically.
fn rule_is_ours(rule: &Value) -> bool {
    rule_commands(rule).any(command_is_ours)
}

/// Whether `rule` is a deck-owned rule for the same binary as `binary_path`.
fn rule_matches_binary(rule: &Value, binary_path: &str) -> bool {
    rule_commands(rule).any(|cmd| command_matches_binary(cmd, binary_path))
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

    let mut settings = read_settings(&path);
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
    let mut settings = read_settings(path);
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
    let mut settings = read_settings(&path);

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
    let mut settings = read_settings(&path);

    let outcome = uninstall_impl(&mut settings);

    if let Err(e) = write_settings(&path, &settings) {
        eprintln!("Error writing {}: {e}", path.display());
        return;
    }

    if outcome.rules_removed == 0 {
        println!("No dot-agent-deck hooks matched this binary; nothing removed.");
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
    let mut settings = read_settings(path);
    install_impl(&mut settings, binary_path);
    write_settings(path, &settings).expect("failed to write settings");
}

pub fn uninstall_from(path: &PathBuf) {
    let mut settings = read_settings(path);
    uninstall_impl(&mut settings);
    write_settings(path, &settings).expect("failed to write settings");
}
