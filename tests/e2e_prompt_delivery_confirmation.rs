#![cfg(all(feature = "e2e", unix))]

//! L2 regressions for spawn-time prompt confirmation. The real scenario
//! repeats the reported three-dispatch Claude Code startup race with
//! interactive Haiku agents.
//!
//! Fork #194/#341 retired this file's synthetic swallowed-seed round-trip
//! scenario (`scheduler/dispatch/014`, formerly here): `MAX_PAYLOAD_SUBMISSIONS
//! = 1` (`src/prompt_delivery.rs`) means every attempt past the first is a
//! submit-only probe, so a launcher that genuinely consumes attempt 1 no
//! longer gets a bounded replacement payload to read as a resubmission — the
//! property that scenario asserted no longer holds in production. Recovering
//! that case is deferred to fork issue #343. `dispatch_015` below exercises
//! the identical mechanism against a real Claude Code agent and is expected to
//! regress the same way; that loss was already priced in by
//! `MAX_PAYLOAD_SUBMISSIONS`'s own doc comment, since `dispatch_015` self-skips
//! in CI for lack of credentials and so never turns the board red.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Output};
use std::time::Duration;

use common::TuiDeck;
use dot_agent_deck::event::{SESSION_START_ORIGIN_METADATA_KEY, WRAPPER_FORK_SESSION_START_ORIGIN};
use spec::spec;

const REAL_AGENT_COMMAND: &str = "claude --model claude-haiku-4-5-20251001 --allowedTools Bash";

struct SiblingWorktreeGuards(Vec<PathBuf>);

impl Drop for SiblingWorktreeGuards {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

fn path_with_binary_dir() -> String {
    let bin = env!("CARGO_BIN_EXE_dot-agent-deck");
    let bindir = Path::new(bin).parent().expect("binary path has a parent");
    format!(
        "{}:{}",
        bindir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

fn commit_fixture_repo(dir: &Path) {
    let run = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git available");
        assert!(out.status.success(), "git {args:?} failed: {out:?}");
    };
    run(&["config", "user.email", "deck-test@example.com"]);
    run(&["config", "user.name", "Deck Test"]);
    run(&["add", "-A"]);
    run(&["commit", "-qm", "fixture baseline"]);
}

fn dispatch_worktree_of(deck: &TuiDeck, name: &str) -> PathBuf {
    deck.workdir()
        .parent()
        .expect("fixture dir has a parent")
        .join(format!(
            "{}-dispatch-{name}",
            deck.workdir()
                .file_name()
                .expect("fixture dir has a name")
                .to_string_lossy()
        ))
}

fn open_cat_caller_pane(deck: &TuiDeck) -> String {
    deck.send_keys(b"\x0e");
    deck.send_keys(b" ");
    deck.wait_for_string("New Agent");
    deck.send_keys(b"\t");
    deck.send_keys(b"caller");
    deck.send_keys(b"\t");
    deck.send_keys(&[0x7f; 128]);
    deck.send_keys(b"cat");
    let (col, row) = deck
        .find_in_grid("[Submit]")
        .expect("new-pane form should render Submit");
    deck.click(col, row);
    deck.wait_for_absence("[Submit]");

    let find_caller = || {
        common::agent_records_on(deck.attach_socket_path())
            .into_iter()
            .find_map(|record| record.pane_id_env.filter(|_| record.cwd.is_some()))
    };
    assert!(
        common::wait_until(Duration::from_secs(60), || find_caller().is_some()),
        "no registered caller pane appeared; records={:?}\ngrid:\n{}",
        common::agent_records_on(deck.attach_socket_path()),
        deck.snapshot_grid()
    );
    find_caller().expect("caller checked above")
}

fn start_dispatch(deck: &TuiDeck, caller_pane: &str, name: &str, prompt: &str) -> Child {
    std::process::Command::new(env!("CARGO_BIN_EXE_dot-agent-deck"))
        .args(["dispatch", name, "--task", prompt, "--single"])
        .env("DOT_AGENT_DECK_SOCKET", deck.hook_socket_path())
        .env("DOT_AGENT_DECK_PANE_ID", caller_pane)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("start dispatch {name}: {error}"))
}

fn dispatch_concurrently(deck: &TuiDeck, caller_pane: &str, cases: &[(&str, &str)]) -> Vec<Output> {
    let children: Vec<Child> = cases
        .iter()
        .map(|(name, prompt)| start_dispatch(deck, caller_pane, name, prompt))
        .collect();
    children
        .into_iter()
        .map(|child| child.wait_with_output().expect("wait for dispatch CLI"))
        .collect()
}

fn confirmed_prompt(deck: &TuiDeck, name: &str) -> Option<String> {
    let display_name = format!("dispatch-{name}");
    common::agent_records_on(deck.attach_socket_path())
        .into_iter()
        .find(|record| record.display_name.as_deref() == Some(display_name.as_str()))
        .and_then(|record| record.live)
        .and_then(|live| live.last_user_prompt)
}

fn prompt_attempt_log(deck: &TuiDeck, name: &str) -> String {
    std::fs::read_to_string(dispatch_worktree_of(deck, name).join("prompt-attempts.log"))
        .unwrap_or_else(|_| "<no attempt log>".to_string())
}

fn swallowed_submission_count(deck: &TuiDeck, name: &str, prompt: &str) -> usize {
    prompt_attempt_log(deck, name)
        .lines()
        .filter(|line| *line == format!("swallowed|{prompt}"))
        .count()
}

fn dispatch_pane_id(deck: &TuiDeck, name: &str) -> Option<String> {
    let display_name = format!("dispatch-{name}");
    common::agent_records_on(deck.attach_socket_path())
        .into_iter()
        .find(|record| record.display_name.as_deref() == Some(display_name.as_str()))
        .and_then(|record| record.pane_id_env)
}

fn pane_delivery_log_lines<'a>(log: &'a str, pane_id: &str) -> Vec<&'a str> {
    log.lines()
        .filter(|line| line.contains(pane_id))
        .filter(|line| {
            line.contains("prompt written to pane; provisional")
                || line.contains("prompt delivery unconfirmed; re-submitting")
                || line.contains("prompt delivery confirmed by the agent")
                || line.contains("prompt delivery unconfirmed at the deadline; abandoning")
        })
        .collect()
}

fn payload_write_attempt(line: &str) -> Option<u32> {
    if !line.contains("prompt written to pane; provisional") {
        return None;
    }
    let (_, suffix) = line.split_once("attempt=")?;
    let end = suffix
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(suffix.len());
    suffix[..end].parse().ok()
}

fn delivery_diagnostics(deck: &TuiDeck, cases: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (name, prompt) in cases {
        let attempts = prompt_attempt_log(deck, name);
        let first_submission_swallowed = attempts
            .lines()
            .any(|line| line == format!("swallowed|{prompt}"));
        out.push_str(&format!(
            "\n{name}: expected={prompt:?}, confirmed_exact={:?}, first_submission_swallowed={first_submission_swallowed}, attempt_log={attempts:?}",
            confirmed_prompt(deck, name)
        ));
    }
    out
}

/// Every delivery-lifecycle line the daemon logged, verbatim and in order.
///
/// Issue #664: `scheduler/dispatch/015`'s failure used to read only as
/// `confirmed_exact=None`, which is indistinguishable between "the retry path
/// is broken" (the regression the test exists to catch) and "the daemon
/// ABANDONED this delivery because nothing confirmed it inside the 60 s
/// production `AUTOMATIC_PROMPT_DEADLINE`" (a starved machine, or budget spent
/// somewhere it could not be recovered from).
/// Those need different responses and the panic could not tell them apart, so
/// the lines that name the difference — `abandoning`, `not re-submitting`, and
/// the per-attempt trail leading to them, each carrying its own `delivery_id`
/// and attempt count — are printed with the assertion instead of having to be
/// reconstructed afterwards.
fn delivery_log_evidence(log: &str) -> String {
    const MARKERS: [&str; 5] = [
        "prompt written to pane; provisional",
        "prompt delivery unconfirmed; re-submitting",
        "prompt delivery confirmed by the agent",
        "prompt delivery unconfirmed at the deadline; abandoning",
        "prompt delivery stopped without confirmation",
    ];
    let lines: Vec<&str> = log
        .lines()
        .filter(|line| MARKERS.iter().any(|marker| line.contains(marker)))
        .collect();
    if lines.is_empty() {
        "<no delivery lifecycle lines in the deck log>".to_string()
    } else {
        lines.join("\n")
    }
}

/// How long the deck's readiness gate waits for a `SessionStart` before writing
/// the prompt anyway, pinned short so the fallback path is reached in seconds
/// rather than the production 30 s.
const READINESS_GATE_MS: u64 = 3_000;

fn write_default_command_config(command: &str) -> tempfile::TempDir {
    let dir = common::harness_tempdir().expect("config tempdir");
    let escaped = command.replace('\\', "\\\\").replace('"', "\\\"");
    std::fs::write(
        dir.path().join("config.toml"),
        format!("default_command = \"{escaped}\"\n"),
    )
    .expect("write dispatch config");
    dir
}

fn trust_paths_for_worktrees(deck: &TuiDeck, names: &[&str]) -> Vec<String> {
    let mut paths: Vec<String> = names
        .iter()
        .map(|name| {
            dispatch_worktree_of(deck, name)
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    if let Ok(parent) = deck
        .workdir()
        .parent()
        .expect("fixture dir has a parent")
        .canonicalize()
    {
        let stem = deck
            .workdir()
            .file_name()
            .expect("fixture dir has a name")
            .to_string_lossy();
        for name in names {
            let canonical_shape = parent
                .join(format!("{stem}-dispatch-{name}"))
                .to_string_lossy()
                .into_owned();
            if !paths.contains(&canonical_shape) {
                paths.push(canonical_shape);
            }
        }
    }
    paths
}

fn write_bootstrap_swallowing_real_claude(workdir: &Path) -> PathBuf {
    let wrapper = workdir.join("bootstrap-swallowing-real-claude.sh");
    let binary = shell_quote(env!("CARGO_BIN_EXE_dot-agent-deck"));
    let body = format!(
        "#!/bin/sh\n\
         printf '{{\"hook_event_name\":\"SessionStart\",\"session_id\":\"bootstrap-%s\",\"metadata\":{{\"{SESSION_START_ORIGIN_METADATA_KEY}\":\"{WRAPPER_FORK_SESSION_START_ORIGIN}\"}}}}' \"$DOT_AGENT_DECK_PANE_ID\" | {binary} hook --agent claude-code >/dev/null 2>&1 || exit 97\n\
         IFS= read -r swallowed || exit 98\n\
         printf 'swallowed|%s\\n' \"$swallowed\" >> prompt-attempts.log\n\
         printf '{{\"hook_event_name\":\"PreToolUse\",\"session_id\":\"bootstrap-%s\",\"tool_name\":\"Bootstrap\"}}' \"$DOT_AGENT_DECK_PANE_ID\" | {binary} hook --agent claude-code >/dev/null 2>&1 || exit 99\n\
         IFS= read -r swallowed || exit 100\n\
         printf 'swallowed|%s\\n' \"$swallowed\" >> prompt-attempts.log\n\
         exec {REAL_AGENT_COMMAND}\n"
    );
    std::fs::write(&wrapper, body).expect("write real-Claude bootstrap launcher");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))
        .expect("chmod real-Claude bootstrap launcher");
    wrapper
}

/// Scenario: Launch three real interactive Haiku dispatches through bootstrap launchers that declare a wrapper handoff, consume both payload attempts, then exec Claude. After each native Claude start, a later attempt must recover the exact sentinel-bearing seed, confirm it through UserPromptSubmit, and avoid deadline abandonment; failures print per-pane attempt and delivery evidence.
#[spec("scheduler/dispatch/015")]
#[test]
fn dispatch_015_three_real_claude_seeds_are_genuinely_confirmed() {
    skip_unless!(common::check_claude_available());

    let staging = common::harness_tempdir().expect("real-Claude bootstrap staging dir");
    let launcher = write_bootstrap_swallowing_real_claude(staging.path());
    let config = write_default_command_config(&launcher.to_string_lossy());
    let log_name = "prompt-delivery.log";
    let deck = TuiDeck::builder()
        .with_env(
            "DOT_AGENT_DECK_CONFIG",
            config.path().join("config.toml").to_string_lossy(),
        )
        // Issue #664: without a log the failure cannot say WHY a pane never
        // confirmed. `dispatch/014` above has always captured this; /015 —
        // the one whose panes race a real 60 s deadline — did not, so its
        // abandonment was invisible. See [`delivery_log_evidence`].
        .with_env("DOT_AGENT_DECK_LOG", log_name)
        // Issue #664: this scenario can NEVER satisfy the readiness gate before
        // the write, so leaving it at the production 30 s spent half the
        // delivery budget on a wait with no possible outcome. The gate skips a
        // `wrapper_fork`-origin `SessionStart` and holds out for the agent's
        // NATIVE one (`state::wait_for_session_start`), but the bootstrap
        // launcher only `exec`s Claude after the write it is blocked reading —
        // so Claude cannot emit that native event until the gate has already
        // given up. Measured: the gate timed out at 30.1 s and the whole
        // delivery was abandoned 29.9 s later, the two halves of one 60 s
        // `AUTOMATIC_PROMPT_DEADLINE` captured before the wait. Pinning it here
        // — exactly as `dispatch/014` does, and to the same constant — returns
        // that half to the retry window the real agent actually gets, which is
        // what production spends it on when a native `SessionStart` releases
        // the gate in milliseconds. It changes no deadline and no assertion.
        .with_env(
            "DOT_AGENT_DECK_SESSION_START_WAIT_MS",
            READINESS_GATE_MS.to_string(),
        )
        .with_env("PATH", path_with_binary_dir())
        .with_imported_claude_credentials()
        .launch_with_fixture("minimal");
    deck.wait_for_string("No active sessions");

    let cases = [
        (
            "real-seed-alpha",
            "Use Bash to verify seed-confirm-alpha-7f31.txt exists in the current directory then print its exact filename and wait",
        ),
        (
            "real-seed-beta",
            "Use Bash to verify seed-confirm-beta-8c42.txt exists in the current directory then print its exact filename and wait",
        ),
        (
            "real-seed-gamma",
            "Use Bash to verify seed-confirm-gamma-9d53.txt exists in the current directory then print its exact filename and wait",
        ),
    ];
    for (_, prompt) in &cases {
        let sentinel = prompt
            .split_whitespace()
            .find(|word| word.starts_with("seed-confirm-") && word.ends_with(".txt"))
            .expect("prompt carries a sentinel filename");
        std::fs::write(
            deck.workdir().join(sentinel),
            "dispatch seed confirmation\n",
        )
        .expect("write real-agent sentinel");
    }
    commit_fixture_repo(deck.workdir());

    let names: Vec<&str> = cases.iter().map(|(name, _)| *name).collect();
    let trust_paths = trust_paths_for_worktrees(&deck, &names);
    common::seed_claude_trust_in_home(deck.home_dir(), &trust_paths)
        .expect("seed Claude onboarding and project trust");
    let caller_pane = open_cat_caller_pane(&deck);
    let worktrees: Vec<PathBuf> = names
        .iter()
        .map(|name| dispatch_worktree_of(&deck, name))
        .collect();
    let _guards = SiblingWorktreeGuards(worktrees);

    let outputs = dispatch_concurrently(&deck, &caller_pane, &cases);
    let failed_commands: Vec<String> = cases
        .iter()
        .zip(&outputs)
        .filter(|(_, output)| !output.status.success())
        .map(|((name, _), output)| {
            format!(
                "{name}: status={} stdout={:?} stderr={:?}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        })
        .collect();
    assert!(
        failed_commands.is_empty(),
        "real dispatch commands failed: {failed_commands:#?}{}\nFinal grid:\n{}",
        delivery_diagnostics(&deck, &cases),
        deck.snapshot_grid()
    );

    let all_confirmed = common::wait_until(Duration::from_secs(150), || {
        cases
            .iter()
            .all(|(name, prompt)| confirmed_prompt(&deck, name).as_deref() == Some(*prompt))
    });
    let all_two_payload_attempts_swallowed = cases
        .iter()
        .all(|(name, prompt)| swallowed_submission_count(&deck, name, prompt) == 2);
    let log = std::fs::read_to_string(deck.workdir().join(log_name)).unwrap_or_default();
    let pane_log_evidence: Vec<(&str, Option<String>, Vec<&str>)> = cases
        .iter()
        .map(|(name, _)| {
            let pane_id = dispatch_pane_id(&deck, name);
            let lines = pane_id
                .as_deref()
                .map(|pane_id| pane_delivery_log_lines(&log, pane_id))
                .unwrap_or_default();
            (*name, pane_id, lines)
        })
        .collect();
    let all_post_boot_payloads_written = pane_log_evidence.iter().all(|(_, _, lines)| {
        lines
            .iter()
            .filter_map(|line| payload_write_attempt(line))
            .any(|attempt| attempt > 2)
    });
    let none_abandoned = pane_log_evidence.iter().all(|(_, _, lines)| {
        !lines
            .iter()
            .any(|line| line.contains("prompt delivery unconfirmed at the deadline; abandoning"))
    });
    assert!(
        all_two_payload_attempts_swallowed
            && all_confirmed
            && all_post_boot_payloads_written
            && none_abandoned,
        "every bootstrap launcher must swallow both payload attempts, then every real interactive Claude pane must receive the seed payload on an attempt after attempt 2 and genuinely submit it without deadline abandonment; a healthy Idle pane with no matching UserPromptSubmit is an undelivered seed. all_two_payload_attempts_swallowed={all_two_payload_attempts_swallowed}, all_confirmed={all_confirmed}, all_post_boot_payloads_written={all_post_boot_payloads_written}, none_abandoned={none_abandoned}, pane_log_evidence={pane_log_evidence:?}{}\nDelivery log:\n{}\nFinal grid:\n{}",
        delivery_diagnostics(&deck, &cases),
        delivery_log_evidence(&log),
        deck.snapshot_grid()
    );
}
