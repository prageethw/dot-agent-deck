//! PRD #386 M1/M2 — the process-table primitive and the structural
//! discriminator the descendant-scan shell-activity signal is built on:
//! `process_table()` enumerates every process on the machine (Unix; `None`
//! on Windows) including each process's POSIX session id (`ProcessInfo`),
//! `descendants()` walks that table from a root pid cycle-safely, and
//! `descendant_shell_activity()` classifies a table as busy/idle by
//! `getsid(descendant) != getsid(root_pid)` — the structural test that
//! replaced this PRD's original argv-match condition 3 (see the PRD's Work
//! Log, 2026-08-06).
//!
//! Per the PRD's Test Plan, M1 proves nothing about the feature working in a
//! real pane — that is the explicit lesson of PRD #370, whose one
//! end-to-end test was a correct mechanism test wired to nothing real. M2
//! proves the classification is correct against the measured process shapes,
//! including the CI trap where the agent itself has no controlling
//! terminal. M3/M4/M6 (later milestones) carry the burden of proving the
//! signal actually fires in a real pane.
//!
//! RED until M1/M2 land: `dot_agent_deck::platform::proc::{ProcessInfo,
//! process_table, descendants, descendant_shell_activity}` do not exist yet
//! (or, for `ProcessInfo`, exist without a `session_id` field), so this test
//! binary fails to compile. That compile-level RED is the point — coder
//! implements to this file's intended API.

#[cfg(unix)]
use std::ffi::CString;
#[cfg(unix)]
use std::io::Read as _;
#[cfg(unix)]
use std::os::unix::io::FromRawFd as _;
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
#[cfg(unix)]
use std::process::{Child, Command, Stdio};
#[cfg(unix)]
use std::time::{Duration, Instant};

use dot_agent_deck::platform::proc::{
    ProcessInfo, descendant_shell_activity, descendants, process_table,
};

use spec::spec;

// ---------------------------------------------------------------------------
// Real-process half (Unix only — spawns and setsid()'s a genuine grandchild).
// ---------------------------------------------------------------------------

/// Owns the two real processes `spawn_detached_grandchild` creates and kills
/// both on drop, so a panic mid-assertion still cleans up rather than
/// leaking a process into the runner's tree. `target_pid` is learned from
/// `mid`'s own `pre_exec` fork over a pipe — a `fork()` return value is only
/// visible inside the two forked branches, never to this (the original)
/// process.
#[cfg(unix)]
struct SpawnedGrandchild {
    mid: Child,
    target_pid: i32,
}

#[cfg(unix)]
impl Drop for SpawnedGrandchild {
    fn drop(&mut self) {
        // SAFETY: `target_pid` is a real pid this process learned over the
        // pipe; SIGKILL on an already-exited pid just returns ESRCH, which is
        // fine to ignore for cleanup purposes.
        unsafe {
            libc::kill(self.target_pid, libc::SIGKILL);
        }
        let _ = self.mid.kill();
        let _ = self.mid.wait();
    }
}

/// Spawns a real `mid` process directly from the test (so `mid`'s ppid is
/// the test binary's own pid — the "test-owned ancestor"), and has `mid`
/// itself `fork()` a second real process inside its own `pre_exec` — before
/// `mid` execs anything — so the second process (`target`) is a genuine
/// grandchild of the test process with a real `ppid` chain, not a synthetic
/// relationship. `target` immediately `setsid()`s (detaching from any
/// controlling terminal and becoming its own session leader) and then
/// `execv`s into a marker-tagged `/bin/sleep 20` — the same shape a Claude
/// Bash-tool child has in a real pane (on pipes, via setsid, argv carries an
/// identifiable marker). `target`'s real pid is handed back to this process
/// over a pipe written inside `mid`'s `pre_exec`, synchronously, before
/// `mid` itself execs its own long-lived placeholder (`sleep 20`) so the
/// `ppid` chain stays valid for the whole assertion window. 20s is long
/// enough for the assertion but short enough that a test process that dies
/// mid-run does not leave orphans behind indefinitely.
#[cfg(unix)]
fn spawn_detached_grandchild(marker: &str) -> SpawnedGrandchild {
    let program = CString::new("/bin/sleep").expect("no NUL in \"/bin/sleep\"");
    let argv_strings: Vec<CString> = vec![
        CString::new(marker.to_string()).expect("marker must not contain an interior NUL"),
        CString::new("20").expect("static literal has no interior NUL"),
    ];

    let mut pipe_fds = [0i32; 2];
    // SAFETY: `pipe(2)` fills both fds on success. Used purely to hand
    // `target`'s real pid back to this process, since a `fork()` return
    // value is invisible outside the two forked branches.
    let rc = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
    assert_eq!(rc, 0, "pipe() failed: {}", std::io::Error::last_os_error());
    let [read_fd, write_fd] = pipe_fds;

    let mut mid = Command::new("/bin/sleep");
    mid.arg("20");
    mid.stdin(Stdio::null());
    mid.stdout(Stdio::null());
    mid.stderr(Stdio::null());

    // SAFETY: runs inside the forked `mid` child, strictly before `mid`
    // execs `sleep 20`. Only async-signal-safe libc calls are used in the
    // `target` (grandchild) branch (`fork`, `setsid`, `close`, `execv`,
    // `_exit`) — the same discipline `child_pre_exec` in `src/wrap.rs` uses
    // for its single-fork case. Building `argv_ptrs` (a `Vec`) in the
    // grandchild branch is a pragmatic, widely-used exception to strict
    // async-signal-safety (the branch is single-threaded at fork time, so
    // there is no other thread that could be holding the allocator lock).
    unsafe {
        mid.pre_exec(move || {
            match libc::fork() {
                -1 => Err(std::io::Error::last_os_error()),
                0 => {
                    // `target` branch: never returns to Rust's own
                    // post-`pre_exec` exec logic — either `execv` replaces
                    // this process image, or `_exit` on failure.
                    libc::close(read_fd);
                    libc::close(write_fd);
                    libc::setsid();
                    let mut argv_ptrs: Vec<*const libc::c_char> =
                        argv_strings.iter().map(|s| s.as_ptr()).collect();
                    argv_ptrs.push(std::ptr::null());
                    libc::execv(program.as_ptr(), argv_ptrs.as_ptr());
                    libc::_exit(127);
                }
                child_pid => {
                    // `mid` branch: hand `target`'s real pid back over the
                    // pipe, then fall through (`Ok(())`) so `mid` execs its
                    // own placeholder and stays alive as `target`'s real
                    // parent for the rest of the test.
                    libc::close(read_fd);
                    let bytes = child_pid.to_ne_bytes();
                    libc::write(write_fd, bytes.as_ptr() as *const libc::c_void, bytes.len());
                    libc::close(write_fd);
                    Ok(())
                }
            }
        });
    }

    let mid_child = mid.spawn().expect("spawn mid (the test-owned ancestor)");
    // SAFETY: this process's own copy of the pipe — the write end is only
    // ever used from inside the forked child above.
    unsafe {
        libc::close(write_fd);
    }
    // SAFETY: `read_fd` was just filled by `pipe(2)` above and is owned
    // exclusively by this process from this point on.
    let mut reader = unsafe { std::fs::File::from_raw_fd(read_fd) };
    let mut buf = [0u8; 4];
    reader
        .read_exact(&mut buf)
        .expect("read target's real pid back from the pipe");
    let target_pid = i32::from_ne_bytes(buf);

    SpawnedGrandchild {
        mid: mid_child,
        target_pid,
    }
}

/// Scenario: the test spawns `mid` directly (a real child of the test
/// process), and `mid` itself forks a second real process (`target`),
/// `setsid()`s it, and execs it as a marker-tagged `/bin/sleep 20` — so
/// `target` is a genuine grandchild of the test process, on pipes, detached
/// from any controlling terminal, and its own session leader: the same
/// process shape a real Claude Bash-tool child has in a real pane. Calls the
/// not-yet-written `process_table()` + `descendants()` primitives and
/// asserts `target` is found as a descendant of the test's own pid, with no
/// controlling terminal, session-leader true, its full argv containing the
/// marker, and — the amended assertion — a `session_id` that differs from
/// the test process's own (read independently via `libc::getsid(0)`), the
/// real-process proof that `getsid` reads what condition 3 of the
/// discriminator assumes it reads.
#[cfg(unix)]
#[spec("status/shell-activity/001")]
#[test]
fn shell_activity_001_finds_a_real_detached_grandchild_as_a_descendant() {
    let marker = format!("shell-activity-001-target-{}", std::process::id());
    let spawned = spawn_detached_grandchild(&marker);
    let root_pid = std::process::id() as i32;

    // Poll: `mid`'s pre_exec fork/setsid/exec races this thread, so give the
    // OS a moment to make `target` observable in the process table.
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut found: Option<ProcessInfo> = None;
    while found.is_none() && Instant::now() < deadline {
        let table = process_table().expect("process_table() must enumerate on unix");
        found = descendants(&table, root_pid)
            .into_iter()
            .find(|p| p.pid == spawned.target_pid)
            .cloned();
        if found.is_none() {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    let target = found.unwrap_or_else(|| {
        panic!(
            "target pid {} never appeared as a descendant of test pid {root_pid} within 2s",
            spawned.target_pid
        )
    });

    assert!(
        !target.has_controlling_tty,
        "a setsid()'d grandchild must report no controlling terminal: {target:?}"
    );
    assert!(
        target.session_leader,
        "a process that just called setsid() is its own session leader: {target:?}"
    );
    assert!(
        target.argv.contains(&marker),
        "descendant argv must be the full command line — marker {marker:?} not found in {:?}",
        target.argv
    );

    // SAFETY: `getsid(2)` with pid 0 means "the caller's own session id"; it
    // is async-signal-safe and has no side effects. This is an independent
    // oracle (not routed through `process_table()`) proving `target.session_id`
    // — the field the discriminator's condition 3 is built on — actually
    // reflects what `getsid` reports, rather than merely differing from some
    // other field by coincidence.
    let own_sid = unsafe { libc::getsid(0) };
    assert_ne!(
        target.session_id, own_sid,
        "a setsid()'d grandchild must be its own session leader: its session id must differ \
         from the test process's own (target={target:?}, own_sid={own_sid})"
    );
}

// ---------------------------------------------------------------------------
// Cycle-safety half (any platform — pure data, no real processes, no `ps`).
// ---------------------------------------------------------------------------

/// Scenario: constructs a synthetic table by hand where pid 2 is a normal
/// child of root pid 1, pid 3 is a normal child of pid 2, but the table ALSO
/// claims pid 1's `ppid` is 2 — a `ppid` cycle looping straight back into
/// the branch the walk just descended, exactly the shape the PRD says a
/// non-atomically sampled `ps` table can produce after PID reuse. Calls
/// `descendants()` on a background thread with a bounded join timeout and
/// asserts it returns (rather than looping forever) with exactly `{2, 3}` —
/// each reachable descendant reported once, and the root itself never
/// re-reported despite the cycle linking back to it.
#[spec("status/shell-activity/002")]
#[test]
fn shell_activity_002_descendant_walk_terminates_on_a_ppid_cycle() {
    // `session_id` is irrelevant to cycle-termination — every row shares one
    // arbitrary value so the walk's own logic (ppid-following, cycle
    // detection) is what's under test, not the discriminator.
    const IRRELEVANT_SID: i32 = 1000;
    let table = vec![
        ProcessInfo {
            pid: 2,
            ppid: 1,
            session_id: IRRELEVANT_SID,
            has_controlling_tty: false,
            session_leader: false,
            argv: "cycle-entry".to_string(),
        },
        ProcessInfo {
            pid: 1,
            ppid: 2,
            session_id: IRRELEVANT_SID,
            has_controlling_tty: false,
            session_leader: false,
            argv: "cycle-back-to-root".to_string(),
        },
        ProcessInfo {
            pid: 3,
            ppid: 2,
            session_id: IRRELEVANT_SID,
            has_controlling_tty: false,
            session_leader: false,
            argv: "past-the-cycle".to_string(),
        },
    ];

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let pids: Vec<i32> = descendants(&table, 1).into_iter().map(|p| p.pid).collect();
        let _ = tx.send(pids);
    });

    let mut pids = rx.recv_timeout(std::time::Duration::from_secs(2)).expect(
        "descendants() must terminate on a synthetic ppid cycle instead of looping forever",
    );
    pids.sort_unstable();
    assert_eq!(
        pids,
        vec![2, 3],
        "the walk must report each reachable descendant exactly once and must not \
         re-report the root (pid 1) even though the cycle links back to it"
    );
}

// ---------------------------------------------------------------------------
// M2 — the structural discriminator (any platform — pure data, no real
// processes, no `ps`). Fixture rows are the measured tables from
// `.dot-agent-deck/386-argv-notes.md` §4a / the PRD's own reproduction of it:
// a live agent pane's `getsid(2)` table and its `ps -Ao pid,ppid,pgid,tty,stat,args`
// table, both captured 2026-08-06 against Claude Code 2.1.220. The two
// captures were taken at different instants and so carry different pids for
// the Bash-tool subtree (61296 in the `ps` capture, 63120 in the `getsid`
// capture); this fixture uses the `getsid` capture's pid (63120) as the row
// identity and attributes to it the no-controlling-tty / session-leader
// shape the PRD states is true of "every Bash-tool child measured" (not
// specific to either single capture).
//
// Session id caveat, reported rather than invented: the `getsid` table
// names an explicit session id for only three of the five confounders
// (context7, engram, caffeinate — all `sid=51698`, the agent's own). It does
// not list `task-master` or `pysemgrep`. Their session id here (also
// `51698`) is derived, not separately `getsid`-measured: the `ps` table
// shows `pgid=51698` for both, on the same `ttys014` as the three confirmed
// rows, and none of the five ever appears with a `pgid` that isn't also a
// confirmed session id elsewhere in the same tables — so `pgid` and `sid`
// coincide throughout this tree (true for the pane leader and for the three
// confirmed confounders alike). Flagged here, and in the work-done report,
// rather than silently presented as a direct `getsid` reading.
// ---------------------------------------------------------------------------

const AGENT_PID: i32 = 51757;
const AGENT_SID: i32 = 51698;
const BASH_TOOL_SID: i32 = 63118;

/// The agent's own row plus its five measured long-lived children
/// (`context7`, `task-master`, `engram`, `pysemgrep`, `caffeinate`), all in
/// the agent's own session on the pane's tty. `flip_ctty_to_none` simulates
/// the CI trap: every row (the agent included) reporting no controlling
/// terminal, exactly the container shape `.dot-agent-deck/386-argv-notes.md`
/// §5 measured (`tty_nr = 0` for PID 1 under `docker run` without `-t`).
fn backbone_rows(flip_ctty_to_none: bool) -> Vec<ProcessInfo> {
    let has_ctty = !flip_ctty_to_none;
    let mut rows = vec![ProcessInfo {
        pid: AGENT_PID,
        // Parent is the pane leader (devbox), not itself a descendant of
        // AGENT_PID and so never reachable by the walk — included only so
        // `descendant_shell_activity` can look up the agent's own session id
        // from the same table it classifies against.
        ppid: 51698,
        session_id: AGENT_SID,
        has_controlling_tty: has_ctty,
        session_leader: false,
        argv: "claude --model opus".to_string(),
    }];
    for (pid, argv) in [
        (51787, "npm exec @upstash/context7-mcp"),
        (51788, "npm exec task-master-ai"),
        (51789, "engram mcp --tools=agent"),
        (51807, "pysemgrep mcp"),
        (60798, "caffeinate -i -t 300"),
    ] {
        rows.push(ProcessInfo {
            pid,
            ppid: AGENT_PID,
            session_id: AGENT_SID,
            has_controlling_tty: has_ctty,
            session_leader: false,
            argv: argv.to_string(),
        });
    }
    rows
}

/// The measured Bash-tool descendant: `setsid`-detached into its own
/// session (`BASH_TOOL_SID`, differing from `AGENT_SID`), no controlling
/// terminal, its own session leader — already true in the CI-trap case, so
/// unlike [`backbone_rows`] this has no `flip_ctty_to_none` parameter.
fn bash_tool_descendant_row() -> ProcessInfo {
    ProcessInfo {
        pid: 63120,
        ppid: AGENT_PID,
        session_id: BASH_TOOL_SID,
        has_controlling_tty: false,
        session_leader: true,
        argv: "/bin/zsh -c source …/shell-snapshots/snapshot-zsh-… && eval …".to_string(),
    }
}

/// Scenario: builds three pairs of fixture tables from the measured
/// `getsid`/`ps` captures in `.dot-agent-deck/386-argv-notes.md` and the
/// PRD — Table A (backbone plus the Bash-tool descendant), Table B (backbone
/// only), and a CI-trap variant of both where every row, the agent included,
/// reports no controlling terminal. Calls the not-yet-written
/// `descendant_shell_activity()` with the argv cross-check disabled
/// (`shapes: &[]`) throughout, and asserts Table A classifies busy, Table B
/// classifies idle, and the CI-trap variant of each classifies identically
/// to its non-trap counterpart — pinning both that the session-id test alone
/// excludes every measured confounder and that the exclusion survives the
/// container shape where a bare no-controlling-terminal test would collapse.
#[spec("status/shell-activity/003")]
#[test]
fn shell_activity_003_session_id_discriminator_classifies_the_measured_confounders() {
    // Table A: backbone + the measured Bash-tool descendant -> busy.
    let mut table_a = backbone_rows(false);
    table_a.push(bash_tool_descendant_row());
    assert_eq!(
        descendant_shell_activity(&table_a, AGENT_PID, &[]),
        Some(true),
        "a descendant in a different POSIX session than the agent must classify the pane as busy"
    );

    // Table B: backbone only, argv cross-check disabled -> idle. This is
    // what pins the claim the whole design rests on: the session-id test
    // alone, with no argv help, already excludes every measured confounder.
    let table_b = backbone_rows(false);
    assert_eq!(
        descendant_shell_activity(&table_b, AGENT_PID, &[]),
        Some(false),
        "every measured confounder (context7, task-master, engram, pysemgrep, caffeinate) shares \
         the agent's own session id; the session-id test alone, with the argv cross-check \
         disabled, must exclude all five"
    );

    // CI trap: same two tables, every row (agent included) now reports no
    // controlling terminal. Classification must be unchanged — the
    // discriminator compares session ids, and must never fall back to a
    // bare no-ctty test that would collapse here.
    let mut table_a_ci_trap = backbone_rows(true);
    table_a_ci_trap.push(bash_tool_descendant_row());
    assert_eq!(
        descendant_shell_activity(&table_a_ci_trap, AGENT_PID, &[]),
        Some(true),
        "classification of the busy table must not change when every process, including the \
         agent, has no controlling terminal (the CI/container shape)"
    );

    let table_b_ci_trap = backbone_rows(true);
    assert_eq!(
        descendant_shell_activity(&table_b_ci_trap, AGENT_PID, &[]),
        Some(false),
        "classification of the confounder-only table must not change under the same CI-trap \
         condition — this is the direct regression test for a bare no-controlling-terminal \
         fallback"
    );
}

// ---------------------------------------------------------------------------
// Windows contract half — `windows-latest` runs `cargo nextest run` (the
// fast tier, no `--features e2e`) in CI, so this genuinely executes there
// even though it cannot be run from this macOS worktree.
// ---------------------------------------------------------------------------

/// Scenario: on Windows there is no process-enumeration backend (PRD #386
/// scopes native Windows process walking out, matching the existing
/// `foreground_pgid`'s `None` contract). Calls the not-yet-written
/// `process_table()` and asserts it reports `None` rather than a table, so
/// an accidental future Windows implementation is caught here rather than
/// shipping unverified.
#[cfg(windows)]
#[spec("status/shell-activity/001")]
#[test]
fn shell_activity_001_process_table_is_none_on_windows() {
    assert_eq!(
        process_table(),
        None,
        "process_table() must be None on Windows, matching foreground_pgid's existing contract"
    );
}
