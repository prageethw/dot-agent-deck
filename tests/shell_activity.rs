//! PRD #386 M1 — the process-table primitive the descendant-scan
//! shell-activity signal is built on: `process_table()` enumerates every
//! process on the machine (Unix; `None` on Windows), and `descendants()`
//! walks that table from a root pid, cycle-safely.
//!
//! Per the PRD's Test Plan, this proves nothing about the feature working in
//! a real pane — that is the explicit lesson of PRD #370, whose one
//! end-to-end test was a correct mechanism test wired to nothing real. This
//! file exists so a later failure in the mechanism localises; M3/M4/M6
//! (later milestones) carry the burden of proving the signal actually fires.
//!
//! RED until M1 lands: `dot_agent_deck::platform::proc::{ProcessInfo,
//! process_table, descendants}` do not exist yet, so this test binary fails
//! to compile. That compile-level RED is the point — coder implements to
//! this file's intended API.

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

use dot_agent_deck::platform::proc::{ProcessInfo, descendants, process_table};

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
/// controlling terminal, session-leader true, and its full argv containing
/// the marker.
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
    let table = vec![
        ProcessInfo {
            pid: 2,
            ppid: 1,
            has_controlling_tty: false,
            session_leader: false,
            argv: "cycle-entry".to_string(),
        },
        ProcessInfo {
            pid: 1,
            ppid: 2,
            has_controlling_tty: false,
            session_leader: false,
            argv: "cycle-back-to-root".to_string(),
        },
        ProcessInfo {
            pid: 3,
            ppid: 2,
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
