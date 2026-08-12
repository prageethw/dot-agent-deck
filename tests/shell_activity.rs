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
use std::sync::Arc;
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
use dot_agent_deck::agent_pty::{AgentPtyRegistry, DOT_AGENT_DECK_PANE_ID, SpawnOptions};
#[cfg(unix)]
use dot_agent_deck::platform::proc::{CLAUDE_BASH_TOOL_SHAPE, PS_SAMPLE_BUDGET};
use dot_agent_deck::platform::proc::{
    ProcessInfo, descendant_shell_activity, descendants, process_table,
};
#[cfg(windows)]
use dot_agent_deck::platform::proc::{ProcessTableOutcome, process_table_async};

use spec::spec;

/// Fork issue #160: every deadline below that polls [`process_table`] must
/// leave real headroom beyond `unix.rs`'s real [`PS_SAMPLE_BUDGET`] (now
/// `pub`, so this derives from the actual constant rather than a hand-kept
/// mirror of its value — fork issue #160 item 2, closing the drift risk
/// mechanically instead of by comment discipline). A deadline equal to the
/// budget gives a single timed-out sample zero room for a retry, which is
/// exactly what made `shell_activity_001`/`_004` fail intermittently in CI
/// under machine load: one `ps` call consumed the whole polling window and
/// there was no time left to try again.
///
/// `#[cfg(unix)]`: only `Duration`/`Instant` are imported under that gate
/// above, and `process_table()` only ever returns real data on Unix — it is
/// an unconditional `None` on Windows — so these polling tests are
/// `#[cfg(unix)]` entirely; an ungated use of `Duration` here fails to
/// compile on Windows.
#[cfg(unix)]
const PROCESS_TABLE_POLL_WINDOW: Duration = Duration::from_secs(PS_SAMPLE_BUDGET.as_secs() * 4);

/// Fork issue #160 F2: the trailing `sleep` in `shell_activity_004`'s script,
/// after the detached target is killed, keeps the pane's own shell (and so
/// its registry entry) alive long enough for the closing falling-edge loop to
/// find it. **Invariant: script window > poll window.** Once the shell exits,
/// `shell_activity_candidates` filters it out (no live child pid to read) and
/// `busy_for_pane` returns `None` forever — no amount of extra deadline can
/// observe a pane that has already left the table. Derived from
/// [`PROCESS_TABLE_POLL_WINDOW`] with real margin rather than hard-coded
/// separately, so the invariant cannot silently stop holding.
#[cfg(unix)]
const SCRIPT_TRAILING_WINDOW_SECS: u64 = PROCESS_TABLE_POLL_WINDOW.as_secs() + 7;

/// Fork issue #160 A5: backstops the pane script's `until [ -f … ]` poll loop
/// (0.05s granularity — 20 iterations/sec) so an orphaned shell cannot spin
/// forever. `TriggerFile::drop` only runs on unwind; it does **not** run on
/// nextest's `terminate-after` SIGKILL, on Ctrl-C, or on an OOM kill, so
/// without a backstop a shell orphaned on one of those paths would poll at
/// 20 Hz indefinitely, outliving the test process. Derived as `4 *
/// PROCESS_TABLE_POLL_WINDOW` (32s at the current 8s window) rather than a
/// bare literal, so it moves if that constant ever does. It cannot fire
/// during a legitimate run: the falling-edge loop that must observe the idle
/// reading before `trigger.release()` is ever called bounds its own wait to
/// `PROCESS_TABLE_POLL_WINDOW` — a single poll window, a quarter of this
/// backstop — so by the time `release()` runs there is still 3x
/// `PROCESS_TABLE_POLL_WINDOW` of headroom left on this loop's own budget.
#[cfg(unix)]
const TRIGGER_POLL_ITERATION_BOUND: u64 = PROCESS_TABLE_POLL_WINDOW.as_secs() * 4 * 20;

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
    //
    // Fork issue #160: a `None` sample (the budget exceeded, or `ps` failed)
    // is not evidence the target is absent — it is no data at all — so it
    // must keep polling within the window rather than panic on the first
    // miss, which is what `process_table().expect(...)` used to do here.
    let deadline = Instant::now() + PROCESS_TABLE_POLL_WINDOW;
    let mut found: Option<ProcessInfo> = None;
    while found.is_none() && Instant::now() < deadline {
        if let Some(table) = process_table() {
            found = descendants(&table, root_pid)
                .into_iter()
                .find(|p| p.pid == spawned.target_pid)
                .cloned();
        }
        if found.is_none() {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    let target = found.unwrap_or_else(|| {
        panic!(
            "target pid {} never appeared as a descendant of test pid {root_pid} within {:?}",
            spawned.target_pid, PROCESS_TABLE_POLL_WINDOW
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
// M3 — `AgentPtyRegistry::shell_foreground_busy_snapshot` (the public seam
// over `RunningAgent::shell_foreground_busy`) wired onto the descendant scan.
// Unix only — spawns a real PTY pane and a real `setsid()`'d, `ps`-visible
// child; there is no process table to scan on Windows.
// ---------------------------------------------------------------------------

/// RAII guard for the detached target's real pid, so a panic between
/// discovering it and the explicit kill below still reaps it rather than
/// leaking a 30s `sleep` into the runner's process tree — the same pattern
/// `SpawnedGrandchild` uses in `status/shell-activity/001`. A second SIGKILL
/// after the process has already exited just returns ESRCH, which this
/// ignores.
#[cfg(unix)]
struct KillOnDrop(Option<i32>);

#[cfg(unix)]
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            // SAFETY: `pid` is a real pid this process learned from its own
            // `process_table()` sample; SIGKILL on an already-exited pid is a
            // no-op ESRCH.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }
}

/// RAII guard for the trigger file that gates `shell_activity_004`'s script
/// (fork issue #160 item 1): the pane's script blocks in a POSIX
/// `until [ -f … ]` poll loop until this file exists, so the idle window
/// before the detached target launches is bounded only by
/// [`TRIGGER_POLL_ITERATION_BOUND`] (fork issue #160 A5) rather than a fixed
/// `sleep` gambling that its duration outlasts one exhausted
/// [`PS_SAMPLE_BUDGET`] sample. `release` is only called after the test has
/// *actually observed* the idle reading, so no timing constant below that
/// backstop is load-bearing for F1 any more.
///
/// Fork issue #160 A6: the trigger lives inside its own `tempfile::TempDir`
/// rather than a pid-derived name under the shared `env::temp_dir()` — a
/// unique, freshly created, `0700` directory has no predictable path for a
/// local attacker to plant a file or symlink at ahead of time (CWE-377 /
/// CWE-59), which removes the pid-derivation and the defensive pre-removal
/// the old scheme needed along with the hazard itself. `TempDir`'s own
/// `Drop` — which runs on unwind, exactly as the removed manual `Drop` did;
/// this workspace sets no `panic = "abort"` profile — recursively removes
/// the directory and the trigger file inside it together, so a failed run
/// leaves nothing behind.
#[cfg(unix)]
struct TriggerFile {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
}

#[cfg(unix)]
impl TriggerFile {
    fn new() -> Self {
        let dir =
            tempfile::TempDir::new().unwrap_or_else(|e| panic!("create trigger tempdir: {e}"));
        let path = dir.path().join("trigger");
        Self { _dir: dir, path }
    }

    /// Releases the pane's script by creating the file it polls for.
    /// Level-triggered (file existence, not a one-shot signal): if this runs
    /// before the shell has even reached its `until` loop, the file already
    /// exists by the time the shell gets there, so there is no lost-wakeup
    /// race (fork #160 round-3 verification, Q2).
    fn release(&self) {
        std::fs::write(&self.path, b"")
            .unwrap_or_else(|e| panic!("write trigger file {:?}: {e}", self.path));
    }
}

/// Scenario: spawns a real PTY pane running `/bin/sh` through
/// `AgentPtyRegistry::spawn_agent` (the same registry path a real agent pane
/// uses), tagged `agent_type: Some(AgentType::ClaudeCode)` so the shape
/// catalog actually reaches the scan (`shell_tool_shape_key` filters by
/// agent kind before the classifier ever sees a shape), whose script blocks
/// on [`TriggerFile`] before it spawns a `python3` child that calls
/// `os.setsid()` and `execv`s into `/bin/sleep` — a genuine `setsid`-detached,
/// marker-tagged, Bash-tool-argv-shaped process on pipes, off the pane's PTY
/// entirely, exactly the topology `status/shell-activity/386-argv-notes.md`
/// measured for a real Claude Bash-tool child and the one #370's own test
/// never exercised. Polls `shell_foreground_busy_snapshot(&[CLAUDE_BASH_TOOL_SHAPE])`
/// and asserts the pane reads idle before the detached child appears, busy
/// while it lives — independently confirmed via `process_table()` +
/// `descendants()` that the found descendant has no controlling terminal, is
/// its own session leader, and carries a session id different from the
/// pane's own shell — and idle again once the child is killed.
#[cfg(unix)]
#[spec("status/shell-activity/004")]
#[test]
fn shell_activity_004_shell_foreground_busy_flips_for_a_real_detached_pipe_child() {
    const PANE_ID: &str = "shell-activity-004-pane";
    let marker = format!("shell-activity-004-target-{}", std::process::id());
    // Must be declared before `registry` below: drop order is reverse
    // declaration order, so declaring `trigger` first makes `registry` drop
    // (and SIGKILL the pane's process group via `shutdown_all`) before the
    // trigger file is removed — reversed, the trigger would disappear first
    // and a shell still polling `until [ -f … ]` would keep spinning until
    // `TRIGGER_POLL_ITERATION_BOUND` (fork issue #160 A5, PR #206 round-3
    // verification).
    let trigger = TriggerFile::new();
    // Fork issue #160 F1: the script blocks in a POSIX `until [ -f … ]` poll
    // loop (0.05s granularity, negligible next to the process-table budget)
    // until `trigger`'s path exists, so the idle window before the detached
    // child launches is bounded only by `TRIGGER_POLL_ITERATION_BOUND` (fork
    // issue #160 A5) — the test below only calls `trigger.release()` once it
    // has actually observed the pane reading idle, rather than racing a
    // fixed-duration sleep against one exhausted `PS_SAMPLE_BUDGET` sample.
    // The python3 one-liner setsid()'s itself (detaching from the pane's
    // controlling terminal and becoming its own session leader, exactly as
    // Claude Code's Bash-tool child does) and execv's into `/bin/sleep 30`
    // with an argv crafted to carry the measured Bash-tool shape
    // (`shell-snapshots/snapshot-` and `&& eval `) so the argv cross-check is
    // exercised against a real process, not just a fixture string. 30s is a
    // generous backstop bound in case the test panics before the explicit
    // kill below runs; `KillOnDrop` and the explicit kill both aim to end it
    // long before that. Fork issue #160 F2: the trailing `sleep
    // {SCRIPT_TRAILING_WINDOW_SECS}` keeps the pane's own shell alive (and so
    // its registry entry) past `PROCESS_TABLE_POLL_WINDOW` following the
    // detached child's death — without real margin over that window the
    // shell can exit, and so leave the snapshot, before the closing
    // falling-edge loop's deadline, which would report `None` forever rather
    // than the `Some(false)` being asserted.
    let command = format!(
        "i=0; until [ -f '{trigger_path}' ] || [ \"$i\" -ge {TRIGGER_POLL_ITERATION_BOUND} ]; \
         do sleep 0.05; i=$((i + 1)); done; \
         python3 -c \"import os; os.setsid(); \
         os.execv('/bin/sleep', ['shell-snapshots/snapshot- && eval {marker}', '30'])\"; \
         sleep {SCRIPT_TRAILING_WINDOW_SECS}",
        trigger_path = trigger.path.display(),
    );

    let registry = Arc::new(AgentPtyRegistry::new());
    let id = registry
        .spawn_agent(SpawnOptions {
            command: Some(&command),
            env: vec![
                (DOT_AGENT_DECK_PANE_ID.to_string(), PANE_ID.to_string()),
                // Pins the `-c` wrap shell so the script's syntax is
                // predictable across developer machines regardless of login
                // shell; consumed by the wrap decision only, never exported
                // into the child's own environment.
                ("SHELL".to_string(), "/bin/sh".to_string()),
            ],
            // Load-bearing for the argv cross-check claim below:
            // `shell_tool_shape_key` selects `CLAUDE_BASH_TOOL_SHAPE` only
            // for `AgentType::ClaudeCode`, and `&[]` for `None` — so without
            // this the `&[CLAUDE_BASH_TOOL_SHAPE]` passed to
            // `shell_foreground_busy_snapshot` below is filtered out before
            // the scan ever sees it, and the crafted argv is never actually
            // checked.
            agent_type: Some(dot_agent_deck::event::AgentType::ClaudeCode),
            ..SpawnOptions::default()
        })
        .expect("spawn should succeed");
    let shell_pid = registry
        .child_pid(&id)
        .expect("spawned agent must expose a pid") as i32;

    let mut target_guard = KillOnDrop(None);

    let busy_for_pane = |registry: &AgentPtyRegistry| -> Option<bool> {
        registry
            .shell_foreground_busy_snapshot(&[CLAUDE_BASH_TOOL_SHAPE])
            .unwrap_or_default()
            .into_iter()
            .find(|(pane_id, _)| pane_id == PANE_ID)
            .map(|(_, busy)| busy)
    };

    // Falling edge before the rising edge: the script is blocked on
    // `trigger`, which is not yet released, so the detached child cannot
    // possibly have appeared — this is no longer a race against a state that
    // might stop being true while the loop waits (fork issue #160 F1); it
    // only needs to tolerate a transient process-table sample failure.
    //
    // Fork issue #160: `busy_for_pane` returns `None` when the underlying
    // sample fails (budget exceeded under load) — `shell_foreground_busy_snapshot`
    // calls the SYNCHRONOUS `process_table()` (`agent_pty.rs`), not
    // `process_table_async`; the async form has no caller in this test file.
    // A failure to observe the idle reading within the deadline below still
    // fails the test with the message on the `assert_eq!`, rather than
    // hanging silently — the loop's own deadline is the timeout, and the
    // assertion after it is what turns a timeout into a clear failure.
    let deadline = Instant::now() + PROCESS_TABLE_POLL_WINDOW;
    let mut state = busy_for_pane(&registry);
    while state != Some(false) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        state = busy_for_pane(&registry);
    }
    assert_eq!(
        state,
        Some(false),
        "must observe the pane reading idle (Some(false)) within {PROCESS_TABLE_POLL_WINDOW:?} \
         before releasing the trigger — got {state:?}, meaning either every process-table \
         sample failed within the deadline (None) or the pane read busy (Some(true)) despite \
         the detached target not having been released yet"
    );

    // Fork issue #160 F1: only now — with the idle reading actually
    // observed, not merely assumed to hold for some guessed duration —
    // release the script to launch the detached target. The window before
    // this point was unbounded, so no timing constant was load-bearing for
    // it.
    trigger.release();

    // Rising edge: the detached, setsid()'d, Bash-tool-shaped child appears.
    // Fork issue #160 N1: needs headroom for the script's `until` poll
    // granularity (0.05s) + python3/execv startup AND at least one retried
    // process-table sample, so this window adds a margin on top of
    // `PROCESS_TABLE_POLL_WINDOW` rather than reusing it as-is. There is no
    // fixed-duration leading sleep any more (item 1 above removed it), so
    // unlike the pre-fix `+ 2s` this margin has nothing left to silently
    // drift out of sync with — it is sized for the short post-release
    // startup cost alone.
    let deadline = Instant::now() + PROCESS_TABLE_POLL_WINDOW + Duration::from_secs(2);
    let mut state = busy_for_pane(&registry);
    while state != Some(true) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        state = busy_for_pane(&registry);
    }
    assert_eq!(
        state,
        Some(true),
        "a real detached-on-pipes descendant, off the pane's PTY entirely, in its own POSIX \
         session, carrying the Bash-tool argv shape, must read busy — this is exactly the #370 \
         defect: the old tcgetpgrp body compares pgids on the pane's own PTY and can never \
         observe a child that never touches it"
    );

    // Independent oracle: confirm this is genuinely the topology #370 could
    // never see, not just that the snapshot happened to say `true`.
    //
    // Fork issue #160 F5: this used to be a single one-shot `.expect()`,
    // justified by "`state == Some(true)` already proved a sample succeeded
    // moments ago" — sound only if sample failures are independent. They are
    // not: they are load-caused, and this same PR's own new docs
    // (`docs/develop/shell-activity-signal.md`) say the condition can
    // "persist for as long as the contention does". A success at t and a
    // failure at t+20ms are entirely compatible under sustained contention,
    // so this gets the same bounded retry the polled loops above use rather
    // than trusting a moments-ago success.
    let deadline = Instant::now() + PROCESS_TABLE_POLL_WINDOW;
    let mut table = process_table();
    while table.is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        table = process_table();
    }
    let table = table.unwrap_or_else(|| {
        panic!(
            "process_table() produced no sample within {PROCESS_TABLE_POLL_WINDOW:?}, even \
             though a shell-activity sample succeeded moments ago"
        )
    });
    let own_row = table
        .iter()
        .find(|p| p.pid == shell_pid)
        .expect("the pane's own shell must appear in its own process table sample");
    let target = descendants(&table, shell_pid)
        .into_iter()
        .find(|p| p.argv.contains(&marker))
        .unwrap_or_else(|| {
            panic!(
                "detached target carrying marker {marker:?} not found among descendants of \
                 shell pid {shell_pid}"
            )
        })
        .clone();
    assert!(
        !target.has_controlling_tty,
        "the detached child must have no controlling terminal — it never touches the pane's \
         PTY: {target:?}"
    );
    assert!(
        target.session_leader,
        "a setsid()'d child must be its own session leader: {target:?}"
    );
    assert_ne!(
        target.session_id, own_row.session_id,
        "the detached child must be in a different POSIX session than the pane's own shell — \
         the load-bearing condition the whole discriminator rests on: {target:?}"
    );
    target_guard.0 = Some(target.pid);

    // Falling edge: kill the detached child and confirm the signal clears. A
    // test that only asserted the rising edge would pass against an
    // implementation that never clears.
    // SAFETY: `target.pid` was just read from this process's own
    // `process_table()` sample.
    unsafe {
        libc::kill(target.pid, libc::SIGKILL);
    }
    let deadline = Instant::now() + PROCESS_TABLE_POLL_WINDOW;
    let mut state = busy_for_pane(&registry);
    while state != Some(false) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        state = busy_for_pane(&registry);
    }
    assert_eq!(
        state,
        Some(false),
        "once the detached descendant exits the pane must read idle again — a test that only \
         asserts the rising edge would pass against an implementation that never clears"
    );

    registry.close_agent(&id).unwrap();
    let _ = std::fs::remove_file(&ready_marker);
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
/// shipping unverified. Fork issue #160 F9: also asserts the async form
/// reports `ProcessTableOutcome::Unsupported` — until this test, that
/// contract had zero coverage, so a future Windows backend that started
/// returning `Failed` instead would silently turn the daemon's
/// silent-continue branch into a per-tick warning storm with nothing here to
/// catch it.
//
// Written as a sync `#[test]` driving an explicit multi-thread runtime
// rather than `#[tokio::test]`: the linkage-check (PRD #77 Decision 17) ties
// each `#[spec(...)]` to the next plain `fn` definition and does not
// recognize a `#[tokio::test] async fn` — see `tests/rehydration.rs`'s
// `live_007` for the same pattern.
#[cfg(windows)]
#[spec("status/shell-activity/001")]
#[test]
fn shell_activity_001_process_table_is_none_on_windows() {
    assert_eq!(
        process_table(),
        None,
        "process_table() must be None on Windows, matching foreground_pgid's existing contract"
    );
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build multi-thread runtime");
    assert_eq!(
        rt.block_on(process_table_async()),
        Err(ProcessTableOutcome::Unsupported),
        "process_table_async() must report Unsupported on Windows — permanent for the \
         process's life, distinct from a transient Failed sample"
    );
}
