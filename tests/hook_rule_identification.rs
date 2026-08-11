//! Fork issue #229 — Claude Code hook rules are identified by an
//! `.contains("dot-agent-deck")` substring check
//! ([`dot_agent_deck::hooks_manage`]), so a deck binary installed under any
//! other filename cannot see the rule it just wrote. Repeated auto-installs
//! then accumulate one rule per startup instead of replacing the prior one.
//!
//! These tests exercise `install_to` / `uninstall_from` — the same
//! explicit-settings-path seam `codex_hooks_safety.rs` uses for the sibling
//! Codex matcher — against a `tempfile` settings.json fixture. No `$HOME`
//! manipulation, no spawned processes.

use std::path::{Path, PathBuf};

use dot_agent_deck::hooks_manage::{install_to, uninstall_from};
use serde_json::{Value, json};

fn settings_path() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create settings dir");
    let path = dir.path().join("settings.json");
    (dir, path)
}

fn write_settings(path: &Path, value: &Value) {
    std::fs::write(
        path,
        serde_json::to_vec_pretty(value).expect("serialize settings fixture"),
    )
    .expect("write settings fixture");
}

fn read_settings(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).expect("read settings fixture"))
        .expect("parse settings fixture")
}

/// A rule in the current `{"hooks": [{"type": "command", "command": ...}]}` shape.
fn user_rule(command: &str) -> Value {
    json!({
        "hooks": [{"type": "command", "command": command}]
    })
}

/// A rule in the legacy flat `{"command": ...}` shape `rule_contains_dot_agent_deck`
/// still has a matching arm for.
fn old_format_rule(command: &str) -> Value {
    json!({"command": command})
}

/// Every command string carried by rules under `hook_type`, from either the
/// current nested shape or the legacy flat shape.
fn rule_commands(settings: &Value, hook_type: &str) -> Vec<String> {
    settings
        .get("hooks")
        .and_then(|h| h.get(hook_type))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .flat_map(|rule| {
            let nested = rule
                .get("hooks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|hook| hook.get("command").and_then(Value::as_str))
                .map(str::to_string);
            let flat = rule
                .get("command")
                .and_then(Value::as_str)
                .map(str::to_string);
            nested.chain(flat)
        })
        .collect()
}

/// Total rule count across every event type, so a headline assertion does not
/// need to know the private `HOOK_TYPES` list.
fn total_rule_count(settings: &Value) -> usize {
    settings
        .get("hooks")
        .and_then(Value::as_object)
        .map(|hooks| {
            hooks
                .values()
                .filter_map(Value::as_array)
                .map(Vec::len)
                .sum()
        })
        .unwrap_or(0)
}

/// Scenario: Install deck hooks three times in a row under the same renamed binary path (`/opt/tools/worker-agent-deck`, never installed as plain `dot-agent-deck`). Each event type must end up with exactly one deck rule, matching the count after a single install — not one appended per install.
#[test]
fn hook_rule_identification_001_repeated_install_renamed_binary_stays_single_rule() {
    let (_dir, path) = settings_path();
    let binary = "/opt/tools/worker-agent-deck";

    install_to(&path, binary);
    let after_one = total_rule_count(&read_settings(&path));

    install_to(&path, binary);
    install_to(&path, binary);
    let after_three = total_rule_count(&read_settings(&path));

    assert_eq!(
        after_three, after_one,
        "repeated install under a renamed binary must not accumulate rules: \
         after 1 install = {after_one}, after 3 installs = {after_three}"
    );

    let pre_tool_use = rule_commands(&read_settings(&path), "PreToolUse");
    assert_eq!(
        pre_tool_use,
        vec![format!("{binary} hook")],
        "PreToolUse must hold exactly one rule for the renamed binary; got {pre_tool_use:?}"
    );
}

/// Scenario: Install deck hooks under a renamed binary path, then uninstall. No deck rule should remain, but the current substring predicate cannot recognise hooks written under an unfamiliar binary name.
#[test]
fn hook_rule_identification_002_uninstall_removes_rules_written_under_renamed_binary() {
    let (_dir, path) = settings_path();
    let binary = "/opt/tools/worker-agent-deck";

    install_to(&path, binary);
    uninstall_from(&path);

    let settings = read_settings(&path);
    assert_eq!(
        total_rule_count(&settings),
        0,
        "uninstall must remove every rule written under a renamed binary; settings={settings:?}"
    );
}

/// Scenario: Install deck hooks from two genuinely different binary paths, then reinstall the first. Each distinct binary must keep its own rule throughout — the second install must not wipe the first's, and reinstalling the first must not add a third rule.
#[test]
fn hook_rule_identification_003_distinct_binaries_each_keep_their_own_rule() {
    let (_dir, path) = settings_path();

    install_to(&path, "/a/dot-agent-deck");
    install_to(&path, "/b/other-deck-name");

    let after_two = rule_commands(&read_settings(&path), "PreToolUse");
    assert_eq!(
        after_two.len(),
        2,
        "two genuinely different deck binaries must each keep their own rule; got {after_two:?}"
    );

    install_to(&path, "/a/dot-agent-deck");
    let after_reinstall = rule_commands(&read_settings(&path), "PreToolUse");
    assert_eq!(
        after_reinstall.len(),
        2,
        "re-installing an already-known binary must replace its own rule, not add a third; got {after_reinstall:?}"
    );
}

/// Scenario: A user-authored hook whose command merely mentions dot-agent-deck as an argument (an audit-wrapper watching for it) must never be treated as deck-owned, across both install and uninstall.
#[test]
fn hook_rule_identification_004_user_hook_mentioning_name_is_never_deleted() {
    let (_dir, path) = settings_path();
    let user_command = "/usr/local/bin/audit-wrapper --watch dot-agent-deck";
    write_settings(
        &path,
        &json!({
            "hooks": {
                "PreToolUse": [user_rule(user_command)]
            }
        }),
    );

    install_to(&path, "/opt/tools/worker-agent-deck");
    let after_install = rule_commands(&read_settings(&path), "PreToolUse");
    assert!(
        after_install.contains(&user_command.to_string()),
        "a user hook that merely mentions dot-agent-deck must survive install; got {after_install:?}"
    );

    uninstall_from(&path);
    let after_uninstall = rule_commands(&read_settings(&path), "PreToolUse");
    assert!(
        after_uninstall.contains(&user_command.to_string()),
        "a user hook that merely mentions dot-agent-deck must survive uninstall; got {after_uninstall:?}"
    );
}

/// Scenario: Unrelated user commands that happen to end in the literal word "hook" — never written by the deck — must never be mistaken for deck rules by install or uninstall. This guards against the specific hazard a naive command-suffix match would introduce.
#[test]
fn hook_rule_identification_005_unrelated_command_ending_in_hook_is_never_deleted() {
    let (_dir, path) = settings_path();
    let unrelated = ["mytool hook", "/usr/bin/git hook"];
    write_settings(
        &path,
        &json!({
            "hooks": {
                "PreToolUse": unrelated.iter().map(|c| user_rule(c)).collect::<Vec<_>>()
            }
        }),
    );

    install_to(&path, "/opt/tools/worker-agent-deck");
    let after_install = rule_commands(&read_settings(&path), "PreToolUse");
    for command in unrelated {
        assert!(
            after_install.contains(&command.to_string()),
            "an unrelated command ending in \"hook\" must survive install; got {after_install:?}"
        );
    }

    uninstall_from(&path);
    let after_uninstall = rule_commands(&read_settings(&path), "PreToolUse");
    for command in unrelated {
        assert!(
            after_uninstall.contains(&command.to_string()),
            "an unrelated command ending in \"hook\" must survive uninstall; got {after_uninstall:?}"
        );
    }
}

/// Scenario: A hook rule written by an older, differently-named deck install (the plain `dot-agent-deck` binary name) must still be recognised and replaced by a fresh install running under a new renamed binary, rather than left orphaned alongside it.
#[test]
fn hook_rule_identification_006_legacy_rule_is_recognised_and_replaced() {
    let (_dir, path) = settings_path();
    let legacy_command = "/usr/local/bin/dot-agent-deck hook";
    write_settings(
        &path,
        &json!({
            "hooks": {
                "PreToolUse": [user_rule(legacy_command)]
            }
        }),
    );

    install_to(&path, "/opt/tools/worker-agent-deck");

    let rules = rule_commands(&read_settings(&path), "PreToolUse");
    assert_eq!(
        rules,
        vec!["/opt/tools/worker-agent-deck hook".to_string()],
        "a legacy rule from a differently-named prior install must be replaced by the \
         fresh rule, not left orphaned; got {rules:?}"
    );
}

/// Scenario: A deck rule written in the legacy flat `{"command": ...}` shape (predating the current `{"hooks": [...]}` wrapper) must still be recognised and removed by uninstall.
#[test]
fn hook_rule_identification_007_old_flat_format_rule_is_recognised() {
    let (_dir, path) = settings_path();
    let legacy_command = "/usr/local/bin/dot-agent-deck hook";
    write_settings(
        &path,
        &json!({
            "hooks": {
                "PreToolUse": [old_format_rule(legacy_command)]
            }
        }),
    );

    uninstall_from(&path);

    let remaining = read_settings(&path)["hooks"]["PreToolUse"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        remaining.is_empty(),
        "an old flat-format deck rule must be recognised and removed by uninstall; got {remaining:?}"
    );
}
