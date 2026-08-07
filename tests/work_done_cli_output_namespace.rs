//! upstream #331 — the CLI-side half of the work-done output-path collision.
//!
//! `handle_work_done` (`src/state.rs`) writes its per-role report
//! unconditionally to a path derived from role name + cwd, with no existence
//! check. Issue #331's own proposed fix is narrower than the fix required for
//! the daemon-side clobber (see `delegate_prompt_injection.rs`'s
//! `delegate_023_existing_report_survives_and_collision_is_announced`, which
//! pins the general case), but it is still worth having on its own: a
//! `dot-agent-deck work-done --task-file <path>` invocation whose `--path`
//! resolves into the daemon's own `.dot-agent-deck/work-done-*.md`
//! namespace is a literal, mechanically-preventable setup for exactly that
//! clobber — the CLI can refuse it client-side, before ever reading the file
//! or contacting the daemon.
//!
//! Real subprocess technique (not an in-process `handle_work_done` call):
//! this behavior lives in the CLI argument-handling arm in `src/main.rs`, so
//! the only way to observe it is to run the actual compiled binary. A real
//! daemon is spawned so the two cases are told apart by EXIT STATUS alone,
//! not by wording: if the CLI does not refuse client-side, the reachable
//! daemon accepts the message and the process exits 0; a real refusal exits
//! non-zero before ever touching the socket.

mod common;

use std::process::{Command, Output};

use dot_agent_deck::agent_pty::DOT_AGENT_DECK_PANE_ID;
use spec::spec;

/// Run `dot-agent-deck work-done --task-file <relative_task_file>` from
/// `cwd`, wired at the real daemon's socket so a non-refusing CLI would
/// succeed. Env is intentionally minimal (PATH, a fresh HOME, the pane id,
/// and the daemon socket) so nothing outside the check under test can
/// influence the outcome.
#[cfg(unix)]
fn run_work_done_task_file(
    daemon: &common::DaemonProc,
    cwd: &std::path::Path,
    home: &std::path::Path,
    relative_task_file: &str,
) -> Output {
    let bin = env!("CARGO_BIN_EXE_dot-agent-deck");
    let mut cmd = Command::new(bin);
    cmd.arg("work-done");
    cmd.arg("--task-file").arg(relative_task_file);
    cmd.current_dir(cwd);
    cmd.env_clear();
    if let Ok(path) = std::env::var("PATH") {
        cmd.env("PATH", path);
    }
    cmd.env("HOME", home);
    cmd.env(DOT_AGENT_DECK_PANE_ID, "cli-refusal-test-pane");
    cmd.env("DOT_AGENT_DECK_SOCKET", &daemon.hook_socket);
    cmd.output()
        .expect("run `dot-agent-deck work-done` subprocess")
}

/// Scenario: Start a real daemon, then run `dot-agent-deck work-done
/// --task-file .dot-agent-deck/work-done-coder.md` from a cwd where that
/// exact file already exists — a `--task-file` argument that resolves
/// straight into the daemon's own output namespace. The CLI must refuse and
/// exit non-zero WITHOUT ever reaching the (reachable) daemon; a second,
/// otherwise-identical call whose `--task-file` points outside that
/// namespace must still succeed, proving the refusal is scoped rather than a
/// blanket rejection of `--task-file`.
#[cfg(unix)]
#[spec("orchestration/delegate/024")]
#[test]
fn delegate_024_work_done_refuses_task_file_inside_own_output_namespace() {
    let daemon = common::spawn_daemon_serve(None, "0");
    let cwd = common::race_safe_tempdir();
    let home = cwd.path().join("home");
    std::fs::create_dir_all(&home).expect("create fresh HOME for the CLI subprocess");
    let dot_dir = cwd.path().join(".dot-agent-deck");
    std::fs::create_dir_all(&dot_dir).expect("create .dot-agent-deck");

    // A file sitting exactly where the daemon's own work-done output lives
    // today (role-keyed: `work-done-<role>.md`).
    std::fs::write(dot_dir.join("work-done-coder.md"), "some report content")
        .expect("seed collision file");

    let refused = run_work_done_task_file(
        &daemon,
        cwd.path(),
        &home,
        ".dot-agent-deck/work-done-coder.md",
    );
    assert!(
        !refused.status.success(),
        "`work-done --task-file` pointing INSIDE the daemon's own \
         `.dot-agent-deck/work-done-*.md` output namespace exited successfully instead of \
         being refused — the daemon IS reachable in this test, so a non-refusing CLI \
         forwards the message and exits 0. This --task-file was set up to be overwritten \
         by the daemon's own next work-done write.\nstdout: {:?}\nstderr: {:?}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr),
    );

    // Control: an otherwise-identical call whose --task-file points OUTSIDE
    // the daemon's own namespace must still be accepted.
    std::fs::write(
        cwd.path().join("worker-report.md"),
        "an ordinary worker report",
    )
    .expect("seed non-collision file");
    let allowed = run_work_done_task_file(&daemon, cwd.path(), &home, "worker-report.md");
    assert!(
        allowed.status.success(),
        "a --task-file OUTSIDE the daemon's own output namespace must still be accepted; \
         the refusal must be scoped to the collision path, not a blanket rejection of \
         --task-file.\nstdout: {:?}\nstderr: {:?}",
        String::from_utf8_lossy(&allowed.stdout),
        String::from_utf8_lossy(&allowed.stderr),
    );
}

/// Scenario: Start a real daemon, then seed a real file matching the
/// `work-done-*.md` pattern INSIDE `.dot-agent-deck`, and a SYMLINK at a
/// path outside the namespace (a name that does not itself match
/// `work-done-*.md`) pointing at that real file. Run `dot-agent-deck
/// work-done --task-file` against the symlink's own (outside) path. The CLI
/// must still refuse, because the argument RESOLVES into the namespace, not
/// because of what it is lexically named; a second, otherwise-identical call
/// with no symlink involved at all must still succeed.
#[cfg(unix)]
#[spec("orchestration/delegate/026")]
#[test]
fn delegate_026_work_done_refuses_file_symlink_resolving_into_output_namespace() {
    let daemon = common::spawn_daemon_serve(None, "0");
    let cwd = common::race_safe_tempdir();
    let home = cwd.path().join("home");
    std::fs::create_dir_all(&home).expect("create fresh HOME for the CLI subprocess");
    let dot_dir = cwd.path().join(".dot-agent-deck");
    std::fs::create_dir_all(&dot_dir).expect("create .dot-agent-deck");

    // The real file sits INSIDE the namespace, matching the work-done-*.md
    // glob the daemon's own output uses.
    let real_target = dot_dir.join("work-done-coder.md");
    std::fs::write(&real_target, "some report content").expect("seed real namespace file");

    // The --task-file argument's OWN path/name lies OUTSIDE the namespace
    // (so a lexical-only check sees nothing to refuse), but it is a symlink
    // whose target RESOLVES straight into it.
    let symlink_path = cwd.path().join("external-report.md");
    std::os::unix::fs::symlink(&real_target, &symlink_path)
        .expect("create file symlink into the output namespace");

    let bypassed = run_work_done_task_file(&daemon, cwd.path(), &home, "external-report.md");
    assert!(
        !bypassed.status.success(),
        "`work-done --task-file external-report.md`, a symlink whose target resolves \
         inside the daemon's own `.dot-agent-deck/work-done-*.md` output namespace, \
         exited successfully instead of being refused — a check on the SUPPLIED path's \
         own lexical name/parent does not see through the symlink, so this file is read \
         by the CLI and then silently overwritten by the daemon's own next write to the \
         same target.\nstdout: {:?}\nstderr: {:?}",
        String::from_utf8_lossy(&bypassed.stdout),
        String::from_utf8_lossy(&bypassed.stderr),
    );

    // Control: a legitimate --task-file outside the namespace, with no
    // symlink involved at all, must still be accepted.
    std::fs::write(
        cwd.path().join("worker-report.md"),
        "an ordinary worker report",
    )
    .expect("seed non-collision file");
    let allowed = run_work_done_task_file(&daemon, cwd.path(), &home, "worker-report.md");
    assert!(
        allowed.status.success(),
        "a plain --task-file OUTSIDE the daemon's own output namespace, with no symlink \
         involved, must still be accepted; the refusal must be scoped to the collision \
         path, not a blanket rejection of --task-file.\nstdout: {:?}\nstderr: {:?}",
        String::from_utf8_lossy(&allowed.stdout),
        String::from_utf8_lossy(&allowed.stderr),
    );
}

/// Scenario: Start a real daemon, then seed a real file matching
/// `work-done-*.md` INSIDE `.dot-agent-deck`, and an INTERMEDIATE DIRECTORY
/// SYMLINK ("alias") that aliases straight into `.dot-agent-deck` itself.
/// Run `dot-agent-deck work-done --task-file alias/work-done-coder.md` — the
/// filename segment already matches the glob, but the PARENT segment is
/// "alias", not ".dot-agent-deck", so a lexical-only check on that parent
/// segment sees nothing to refuse even though the path resolves straight
/// into the namespace. The CLI must still refuse; a second, otherwise-
/// identical call with no symlink involved at all must still succeed.
#[cfg(unix)]
#[spec("orchestration/delegate/027")]
#[test]
fn delegate_027_work_done_refuses_task_file_through_aliased_output_dir() {
    let daemon = common::spawn_daemon_serve(None, "0");
    let cwd = common::race_safe_tempdir();
    let home = cwd.path().join("home");
    std::fs::create_dir_all(&home).expect("create fresh HOME for the CLI subprocess");
    let dot_dir = cwd.path().join(".dot-agent-deck");
    std::fs::create_dir_all(&dot_dir).expect("create .dot-agent-deck");
    std::fs::write(dot_dir.join("work-done-coder.md"), "some report content")
        .expect("seed real namespace file");

    // An intermediate directory symlink that ALIASES straight into
    // `.dot-agent-deck`.
    let alias_dir = cwd.path().join("alias");
    std::os::unix::fs::symlink(&dot_dir, &alias_dir)
        .expect("create intermediate directory symlink aliasing into .dot-agent-deck");

    let bypassed = run_work_done_task_file(&daemon, cwd.path(), &home, "alias/work-done-coder.md");
    assert!(
        !bypassed.status.success(),
        "`work-done --task-file alias/work-done-coder.md`, reached through an \
         intermediate directory symlink that aliases into `.dot-agent-deck`, exited \
         successfully instead of being refused — a check on the SUPPLIED path's own \
         literal parent segment name ('alias') does not see that it resolves into the \
         namespace, so this file is read by the CLI and then silently overwritten by \
         the daemon's own next write to the same target.\nstdout: {:?}\nstderr: {:?}",
        String::from_utf8_lossy(&bypassed.stdout),
        String::from_utf8_lossy(&bypassed.stderr),
    );

    // Control: a legitimate --task-file outside the namespace, reached
    // through no symlink at all, must still be accepted.
    std::fs::write(
        cwd.path().join("worker-report.md"),
        "an ordinary worker report",
    )
    .expect("seed non-collision file");
    let allowed = run_work_done_task_file(&daemon, cwd.path(), &home, "worker-report.md");
    assert!(
        allowed.status.success(),
        "a plain --task-file OUTSIDE the daemon's own output namespace, reached through \
         no symlink, must still be accepted; the refusal must be scoped to the \
         collision path, not a blanket rejection of --task-file.\nstdout: {:?}\nstderr: \
         {:?}",
        String::from_utf8_lossy(&allowed.stdout),
        String::from_utf8_lossy(&allowed.stderr),
    );
}

/// Scenario: A worker's pane cwd (where the daemon actually writes its
/// output, per `pane_cwd_map` at `StartAgent`) has its own `.dot-agent-deck`
/// directory holding the real `work-done-coder.md`. The worker then `cd`s
/// into a CHILD directory that carries a second, unrelated
/// `.dot-agent-deck` directory of its own before invoking the CLI — so the
/// pane's real output file and the CLI process's own cwd disagree. From that
/// child cwd, `work-done --task-file` naming the pane cwd's real output file
/// — first as an absolute path, then as a relative `../` path — must still
/// be refused; a harmless `work-done-*.md` inside the CHILD's own decoy
/// `.dot-agent-deck` is refused too (an accepted false positive); a plain
/// file outside any `.dot-agent-deck` is still accepted.
#[cfg(unix)]
#[spec("orchestration/delegate/028")]
#[test]
fn delegate_028_work_done_refuses_task_file_despite_cwd_drift() {
    let daemon = common::spawn_daemon_serve(None, "0");
    let pane_cwd = common::race_safe_tempdir();
    let home = pane_cwd.path().join("home");
    std::fs::create_dir_all(&home).expect("create fresh HOME for the CLI subprocess");

    // The pane's own namespace — where the daemon's real output for this
    // pane lives, per `pane_cwd_map` captured at `StartAgent`.
    let pane_dot_dir = pane_cwd.path().join(".dot-agent-deck");
    std::fs::create_dir_all(&pane_dot_dir).expect("create pane cwd's .dot-agent-deck");
    let real_target = pane_dot_dir.join("work-done-coder.md");
    std::fs::write(&real_target, "some report content").expect("seed real namespace file");

    // The CLI's OWN cwd: a child directory the worker `cd`d into before
    // invoking the CLI, carrying a second, unrelated `.dot-agent-deck` of
    // its own. This is what makes a cwd-anchored check diverge: comparing
    // against `current_dir()/.dot-agent-deck` resolves to THIS directory,
    // not the pane's real output directory.
    let child_cwd = pane_cwd.path().join("subdir");
    let child_dot_dir = child_cwd.join(".dot-agent-deck");
    std::fs::create_dir_all(&child_dot_dir).expect("create child cwd's own .dot-agent-deck");

    // 1) Absolute path from the child cwd, naming the pane cwd's real
    // output file.
    let absolute = real_target
        .to_str()
        .expect("real_target path is valid UTF-8")
        .to_string();
    let refused_absolute = run_work_done_task_file(&daemon, &child_cwd, &home, &absolute);
    assert!(
        !refused_absolute.status.success(),
        "`work-done --task-file <absolute path>` naming the PANE cwd's real work-done \
         output, invoked from a different (child) cwd, exited successfully instead of \
         being refused — a check anchored to the CLI process's OWN cwd cannot see this, \
         since the pane's real output directory is a different .dot-agent-deck than the \
         one under the CLI's cwd.\nstdout: {:?}\nstderr: {:?}",
        String::from_utf8_lossy(&refused_absolute.stdout),
        String::from_utf8_lossy(&refused_absolute.stderr),
    );

    // 2) Same target, via a relative `../` path from the child cwd.
    let refused_relative = run_work_done_task_file(
        &daemon,
        &child_cwd,
        &home,
        "../.dot-agent-deck/work-done-coder.md",
    );
    assert!(
        !refused_relative.status.success(),
        "`work-done --task-file ../.dot-agent-deck/work-done-coder.md`, naming the PANE \
         cwd's real output via a relative path from a child cwd, exited successfully \
         instead of being refused.\nstdout: {:?}\nstderr: {:?}",
        String::from_utf8_lossy(&refused_relative.stdout),
        String::from_utf8_lossy(&refused_relative.stderr),
    );

    // 3) A harmless decoy: a `work-done-*.md` inside the CHILD's own
    // unrelated `.dot-agent-deck` — not the pane's real output at all.
    // Under the fixed rule (anchored to nothing but the literal
    // `.dot-agent-deck/work-done-*.md` shape, anywhere on disk) this is
    // refused too — a deliberately accepted false positive, since a decoy
    // refusal is harmless while a missed real file destroys a report.
    std::fs::write(
        child_dot_dir.join("work-done-decoy.md"),
        "unrelated decoy report",
    )
    .expect("seed decoy file in the child cwd's own .dot-agent-deck");
    let refused_decoy = run_work_done_task_file(
        &daemon,
        &child_cwd,
        &home,
        ".dot-agent-deck/work-done-decoy.md",
    );
    assert!(
        !refused_decoy.status.success(),
        "`work-done --task-file .dot-agent-deck/work-done-decoy.md`, a harmless decoy \
         under the CLI's own (non-pane) cwd, was expected to be refused too under the \
         fixed rule (an accepted false positive) but exited successfully instead.\n\
         stdout: {:?}\nstderr: {:?}",
        String::from_utf8_lossy(&refused_decoy.stdout),
        String::from_utf8_lossy(&refused_decoy.stderr),
    );

    // Control: a plain file outside any `.dot-agent-deck` must still be
    // accepted.
    std::fs::write(
        child_cwd.join("worker-report.md"),
        "an ordinary worker report",
    )
    .expect("seed non-collision file");
    let allowed = run_work_done_task_file(&daemon, &child_cwd, &home, "worker-report.md");
    assert!(
        allowed.status.success(),
        "a plain --task-file outside any .dot-agent-deck must still be accepted; the \
         refusal must be scoped to the work-done-*.md namespace shape, not a blanket \
         rejection of --task-file.\nstdout: {:?}\nstderr: {:?}",
        String::from_utf8_lossy(&allowed.stdout),
        String::from_utf8_lossy(&allowed.stderr),
    );
}
