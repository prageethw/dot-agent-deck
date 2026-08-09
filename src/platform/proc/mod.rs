//! Process lifecycle: agent teardown + daemon-stop termination + orphan
//! watchdog (PRD #42 M1, lifted from `agent_pty.rs`, `build_version_handshake.rs`,
//! and `daemon.rs`).
//!
//! Unix uses POSIX signals: `killpg(SIGTERM/SIGKILL)` to tear down an agent's
//! whole process group, `kill(SIGTERM/SIGKILL)` to stop the daemon by PID, and
//! `getppid` for the (test-only) orphan watchdog. Windows (PRD #163 M3) has no
//! signal analogue and splits the two jobs apart:
//!
//! - **Agent teardown** — each agent is adopted into a **Job Object** at spawn
//!   ([`AgentProcessGroup`]) and the whole descendant tree is reaped with
//!   `TerminateJobObject`, the unconditional backstop that stands in for
//!   `killpg(SIGKILL)`. The SIGTERM grace window maps to a best-effort
//!   `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, …)` — **explicitly best-effort**
//!   per the PRD, see the windows backend for why.
//! - **Daemon stop** — the graceful half is not a signal at all but the existing,
//!   cross-platform `KIND_SHUTDOWN`/ACK protocol frame; only the force escalation
//!   is platform code (`TerminateProcess`). Which of the two a platform uses is
//!   declared by [`GRACEFUL_STOP_DELIVERY`] so the shared caller
//!   ([`crate::build_version_handshake::terminate_daemon_graceful`]) never
//!   branches on `cfg` and the wire format stays identical everywhere. Because
//!   that escalation names a *pid* long after the target was probed,
//!   [`pin_process`] lets the caller hold the target's identity across the whole
//!   sequence — a real handle on Windows, a documented no-op on Unix.
//!
//! Note: peer-credential PID *discovery* (`SO_PEERCRED`) lives in
//! [`crate::platform::peercred`]; this module only owns kill/teardown.
//!
//! PRD #386 M1/M2 adds a second, read-only concern alongside teardown: the
//! **descendant-process scan** the shell-activity signal is built on. Its
//! cross-platform half — [`ProcessInfo`], [`descendants`] and the structural
//! [`descendant_shell_activity`] discriminator — lives in `scan.rs` and
//! compiles everywhere; only [`process_table`], the act of sampling the
//! machine, is platform code (`ps` on Unix, an unconditional `None` on Windows,
//! matching [`foreground_pgid`]'s existing contract).

mod scan;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

pub use scan::{
    CLAUDE_BASH_TOOL_SHAPE, MEASURED_SHELL_TOOL_SHAPES, ProcessInfo, ShellToolShape,
    descendant_shell_activity, descendants,
};

/// Result of delivering the daemon-stop graceful signal to a PID via
/// [`terminate_pid`].
///
/// The distinction matters to
/// [`crate::build_version_handshake::terminate_daemon_graceful`]: `Delivered`
/// means the signal reached a live process that may still be shutting down, so
/// the caller must poll for it to disappear (and possibly escalate to
/// [`force_kill_pid`]); `AlreadyGone` means the target PID no longer existed
/// (`ESRCH` on Unix, `ERROR_INVALID_PARAMETER` from `OpenProcess` — or an
/// already-signalled exit code — on Windows), so there is nothing to wait for
/// and the caller can report `Stopped` immediately. This mirrors `main`, where an
/// `ESRCH` from the `SIGTERM` `kill(2)` short-circuited straight to
/// `Ok(TerminateOutcome::Stopped)` rather than entering the poll/escalate loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminateSignal {
    /// The signal was delivered to a live process; it may still be dying, so
    /// the caller must poll for the process to disappear.
    Delivered,
    /// The target process was already gone when signalled (`ESRCH`) — an
    /// already-gone success that short-circuits the poll/escalate loop.
    AlreadyGone,
}

/// PRD #163 M3 — how the **graceful** half of `daemon stop` reaches the daemon
/// on this platform. The force half is always platform code
/// ([`force_kill_pid`]: `SIGKILL` / `TerminateProcess`); this enum is only about
/// the polite first ask.
///
/// It exists so [`crate::build_version_handshake::terminate_daemon_graceful`] —
/// which owns the shared *graceful → poll → force* escalation state machine, and
/// is the single path both `daemon stop` and the build-mismatch prompt go
/// through — can pick the right first step without a `cfg` branch of its own and
/// without any per-platform wire format. The platform decision lives here, at
/// the platform seam; the protocol stays identical on every platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GracefulStopDelivery {
    /// Unix: an out-of-band `SIGTERM` delivered by [`terminate_pid`].
    ///
    /// Load-bearing property: **zero protocol bytes are exchanged**, so
    /// `daemon stop` works against *any* daemon version — including the
    /// v0.24.x daemon that predates `KIND_SHUTDOWN` and motivated PRD #103.
    Signal,
    /// Windows: there is no `SIGTERM`. The graceful request is the existing
    /// `KIND_SHUTDOWN`/ACK frame (identical wire on every platform), sent by
    /// the shared caller *before* [`terminate_pid`], which then only classifies
    /// the target as still-alive vs already-gone. `TerminateProcess` remains the
    /// escalation when the daemon does not go away within the grace window.
    ///
    /// The trade-off this makes explicit: unlike [`Signal`](Self::Signal) the
    /// graceful step now needs a daemon that speaks `PROTOCOL_VERSION` ≥ 2.
    /// That is free on Windows — the Windows daemon is unblocked *by* #163, so
    /// no older Windows daemon exists — and a daemon that does not answer just
    /// falls through to the force escalation, exactly like a Unix daemon that
    /// ignores `SIGTERM`.
    ShutdownProtocol,
}

/// Unix delivers the graceful stop as a signal — see
/// [`GracefulStopDelivery::Signal`] for the zero-protocol-bytes property this
/// pins.
#[cfg(unix)]
pub const GRACEFUL_STOP_DELIVERY: GracefulStopDelivery = GracefulStopDelivery::Signal;

/// Windows has no `SIGTERM`, so the graceful stop rides the shared
/// `KIND_SHUTDOWN`/ACK protocol — see [`GracefulStopDelivery::ShutdownProtocol`].
#[cfg(windows)]
pub const GRACEFUL_STOP_DELIVERY: GracefulStopDelivery = GracefulStopDelivery::ShutdownProtocol;

/// Guard a `u32` PID before naming it in a Win32 lifecycle call
/// (`OpenProcess` for the daemon-stop path, `GenerateConsoleCtrlEvent`'s
/// process-group id for the agent grace window). The Windows counterpart of the
/// Unix `checked_signal_pid` / `pid_to_pgid` guards, and it exists for the same
/// reason: **0 is not "no process", it is a wildcard.**
///
/// - `GenerateConsoleCtrlEvent(_, 0)` is documented as "signal every process
///   that shares the caller's console" — the exact `killpg(0, …)` hazard, which
///   for a console-hosted registry would mean signalling the TUI itself.
/// - `OpenProcess(_, _, 0)` names the System Idle Process, never a daemon.
///
/// Unlike the Unix guard there is no overflow arm: a Windows PID *is* a `u32`
/// (`DWORD`), so no value other than 0 changes the call's meaning.
///
/// Compiled on every platform — it is pure data, so the rule stays unit-testable
/// on Linux CI where the `#[cfg(windows)]` callers are absent (the same shape
/// [`crate::platform::lock::spawn_mutex_name`] uses).
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn checked_target_pid(pid: u32) -> std::io::Result<u32> {
    if pid == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "pid is 0; refusing the Win32 call (0 is a wildcard: it broadcasts to every process \
             sharing the console / names the System Idle Process)",
        ));
    }
    Ok(pid)
}

/// Fork issue #136 finding 2: the process-wide cap on outstanding detached
/// reap threads (see [`detach_reap_or_fallback_sync`]) both platforms'
/// `terminate_child_with_grace_and_detached_reap_forcing_group_backstop` fall
/// back to a synchronous `wait()` at.
///
/// "One thread per timeout" is not a bound — a single `create_worktree_sync`
/// attempt can reach this path up to three times (the branch probe, the add
/// that still runs even when the probe already timed out, and the best-effort
/// cleanup after a timed-out add all share [`crate::issue_dispatch_run::run_status_sync`]),
/// and a user retrying after the visible failure repeats it. Each blocked
/// `wait()` — a process wedged in uninterruptible kernel I/O never returns
/// until that I/O completes — holds a native OS thread and its stack
/// (typically 2-8MB) indefinitely, so an *unbounded* count of these is exactly
/// the thread-exhaustion state that this fix's finding 1 half depends on
/// avoiding (a `spawn` failing is what turns finding 1's dropped-`Result` bug
/// into a real zombie).
///
/// 8 is chosen as comfortably more than one attempt's worst case (3) — so a
/// single stuck attempt does not immediately force a second, independent
/// attempt onto the synchronous fallback — while still keeping the worst-case
/// resident cost (8 native threads' worth of stack) small and bounded no
/// matter how many timeouts accumulate. This is a small, infrequent,
/// user-triggered path (`git worktree add`/`remove` past a 30s timeout), not a
/// hot one, so there is no throughput reason to pick anything larger.
pub(crate) const MAX_OUTSTANDING_DETACHED_REAPS: usize = 8;

/// The live count of detached reap threads currently blocked in `wait()`
/// against [`MAX_OUTSTANDING_DETACHED_REAPS`]. Incremented by
/// [`try_reserve_detached_reap_slot`] and released by
/// [`release_detached_reap_slot`] once a reap completes (or is abandoned
/// because the thread never started — see [`detach_reap_or_fallback_sync`]).
static OUTSTANDING_DETACHED_REAPS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Try to reserve one of `cap` outstanding detached-reap slots against
/// `counter`. `true` means the slot is reserved and the caller owns
/// releasing it (via [`release_detached_reap_slot`]) once its reap
/// completes; `false` means the cap is saturated and the caller must fall
/// back to a synchronous reap instead of spawning a detached thread.
///
/// Takes `counter` and `cap` as parameters, rather than reaching for
/// [`OUTSTANDING_DETACHED_REAPS`] / [`MAX_OUTSTANDING_DETACHED_REAPS`]
/// directly, so `orchestration/worktree/012` can force the cap low and drive
/// this exact reservation policy against its own private counter — the
/// production mechanism, not a reimplementation of it — without needing a
/// process-wide state reset between tests.
pub(crate) fn try_reserve_detached_reap_slot(
    counter: &std::sync::atomic::AtomicUsize,
    cap: usize,
) -> bool {
    use std::sync::atomic::Ordering;
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            (current < cap).then_some(current + 1)
        })
        .is_ok()
}

/// Release a slot reserved by [`try_reserve_detached_reap_slot`]. Called once
/// a detached reap thread's `wait()` returns, or immediately if the thread
/// that would have released it never started at all (spawn failure).
fn release_detached_reap_slot(counter: &'static std::sync::atomic::AtomicUsize) {
    counter.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
}

/// Fork issue #136: reap `child` on a short-lived detached thread, subject to
/// the `counter`/`cap` bound from [`try_reserve_detached_reap_slot`] — the
/// single mechanism both platforms' `terminate_child_with_grace_and_detached_reap_forcing_group_backstop`
/// end their SIGKILL phase with, closing findings 1 and 2 together:
///
/// - **Cap saturated:** fall back to a synchronous `wait()` on the calling
///   thread rather than spawning past the bound.
/// - **Spawn fails:** finding 1 — the old code discarded `Builder::spawn`'s
///   `Result` with `let _ =`, so a failed spawn dropped the closure that
///   *owned* `child`, and `Child` does not reap on `Drop`. Here `child` is
///   handed to the new thread over an (unbounded, non-blocking) channel
///   rather than moved directly into its closure, so a failed spawn — the
///   closure and its `Receiver` half are simply never created/dropped
///   unrun — leaves `child` untouched in this function's own scope,
///   recoverable for the same synchronous fallback as the at-cap case.
///
/// Either fallback is logged (`caller_context` names which call site and
/// phase this came from) so a saturated cap or a failing spawn is visible in
/// the logs rather than silently changing latency.
///
/// Why a bounded synchronous fallback rather than a queue or a shared worker
/// pool: a single worker thread stalls every *other* outstanding reap the
/// moment one child blocks forever — exactly the failure mode this exists to
/// defend against. The synchronous fallback's cost is small in practice: the
/// `wait()` normally returns immediately after the SIGKILL already delivered
/// by the caller, so blocking here requires both uninterruptible I/O *and*
/// saturation at the same time. Trading a rare, brief block for a guaranteed
/// reap and a hard thread bound is the right trade.
pub(crate) fn detach_reap_or_fallback_sync(
    child: Box<dyn portable_pty::Child + Send + Sync>,
    thread_name: &'static str,
    caller_context: &'static str,
) {
    detach_reap_or_fallback_sync_with_cap(
        child,
        &OUTSTANDING_DETACHED_REAPS,
        MAX_OUTSTANDING_DETACHED_REAPS,
        thread_name,
        caller_context,
    );
}

/// [`detach_reap_or_fallback_sync`], parameterized on `counter`/`cap` so
/// `orchestration/worktree/012` can exercise the real reservation/fallback
/// policy with the cap forced low, against a counter private to the test —
/// see [`try_reserve_detached_reap_slot`]'s doc comment for why.
pub(crate) fn detach_reap_or_fallback_sync_with_cap(
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    counter: &'static std::sync::atomic::AtomicUsize,
    cap: usize,
    thread_name: &'static str,
    caller_context: &'static str,
) {
    if !try_reserve_detached_reap_slot(counter, cap) {
        tracing::warn!(
            context = caller_context,
            cap,
            "outstanding detached-reap cap saturated; reaping synchronously on the calling \
             thread instead of spawning past the bound"
        );
        let _ = child.wait();
        return;
    }

    let (tx, rx) = std::sync::mpsc::channel::<Box<dyn portable_pty::Child + Send + Sync>>();
    let spawn_result = std::thread::Builder::new()
        .name(thread_name.into())
        .spawn(move || {
            if let Ok(mut child) = rx.recv() {
                let _ = child.wait();
            }
            release_detached_reap_slot(counter);
        });

    let Ok(_join_handle) = spawn_result else {
        // Fork issue #136 finding 1: `child` was never moved into the failed
        // spawn's closure (only `rx` was), so it is still owned right here —
        // nothing to recover, nothing dropped unreaped.
        release_detached_reap_slot(counter);
        tracing::warn!(
            context = caller_context,
            "failed to spawn a detached reap thread; reaping synchronously on the calling \
             thread instead"
        );
        let _ = child.wait();
        return;
    };

    // The thread exists and is blocked on `rx.recv()`; handing off `child`
    // cannot fail unless that thread already died before receiving (e.g. an
    // immediate panic), in which case `send` returns it back to us rather
    // than silently discarding it.
    if let Err(unsent) = tx.send(child) {
        // The thread died (e.g. panicked) before reaching `rx.recv()`, so it
        // never reached its own `release_detached_reap_slot` call either —
        // release the slot here instead of leaking it permanently.
        release_detached_reap_slot(counter);
        tracing::warn!(
            context = caller_context,
            "detached reap thread was spawned but exited before it could receive the child; \
             reaping synchronously on the calling thread instead"
        );
        let mut child = unsent.0;
        let _ = child.wait();
    }
}

#[cfg(unix)]
pub use unix::{
    AgentProcessGroup, PinnedProcess, current_ppid, force_kill_child_and_wait, force_kill_pid,
    foreground_pgid, pin_process, process_table, process_table_async, send_sigterm_to_child_group,
    spawn_in_new_process_group,
    terminate_child_with_grace_and_detached_reap_forcing_group_backstop,
    terminate_child_with_grace_and_wait,
    terminate_child_with_grace_and_wait_forcing_group_backstop, terminate_pid,
};
#[cfg(windows)]
pub use windows::{
    AgentProcessGroup, PinnedProcess, current_ppid, force_kill_child_and_wait, force_kill_pid,
    foreground_pgid, pin_process, process_table, process_table_async, send_sigterm_to_child_group,
    terminate_child_with_grace_and_detached_reap_forcing_group_backstop,
    terminate_child_with_grace_and_wait,
    terminate_child_with_grace_and_wait_forcing_group_backstop, terminate_pid,
};

/// A [`portable_pty::Child`] stand-in over a real [`std::process::Child`], so the
/// teardown state machines in both backends can be driven against **real**
/// processes without a PTY (PRD #163 review).
///
/// Lives here, not in a backend, because both the shared cross-platform test
/// below and the Windows backend's descendant-leak test need it. Only the three
/// methods the teardown path actually calls do anything; `clone_killer` is not on
/// that path and returns a no-op killer rather than pretending to duplicate the
/// handle.
///
/// Promoted out of `#[cfg(test)]` for fork issue #133:
/// [`crate::issue_dispatch_run::run_status_sync`] spawns a plain
/// `std::process::Child` (not a pty) and, on timeout, needs the same
/// `&mut Box<dyn portable_pty::Child + Send + Sync>` shape
/// [`terminate_child_with_grace_and_wait`] takes — so this wrapper is now a
/// production dependency, not only a test one. Visibility change only; the
/// `NoopKiller` caveat above still holds (nothing on the teardown path clones
/// a killer).
pub(crate) mod test_child {
    /// Wraps a real OS child. `Debug` is required by the `portable_pty` traits.
    #[derive(Debug)]
    pub(crate) struct StdChild(pub(crate) std::process::Child);

    fn to_pty_status(status: std::process::ExitStatus) -> portable_pty::ExitStatus {
        portable_pty::ExitStatus::with_exit_code(status.code().unwrap_or(0) as u32)
    }

    /// Stand-in for the killer handle `clone_killer` is contractually required to
    /// produce. Nothing in the teardown path clones a killer, so it is never used.
    #[derive(Debug)]
    struct NoopKiller;

    impl portable_pty::ChildKiller for NoopKiller {
        fn kill(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            Box::new(NoopKiller)
        }
    }

    impl portable_pty::ChildKiller for StdChild {
        fn kill(&mut self) -> std::io::Result<()> {
            self.0.kill()
        }
        fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            Box::new(NoopKiller)
        }
    }

    impl portable_pty::Child for StdChild {
        fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
            Ok(self.0.try_wait()?.map(to_pty_status))
        }
        fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
            self.0.wait().map(to_pty_status)
        }
        fn process_id(&self) -> Option<u32> {
            Some(self.0.id())
        }
        #[cfg(windows)]
        fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spec::spec;

    /// PRD #163 M3 — the platform seam `daemon stop` is built on. On Unix the
    /// graceful step MUST stay a signal: that is the property that lets
    /// `daemon stop` kill a daemon predating every protocol surface (PRD #103's
    /// whole motivation). A regression to `ShutdownProtocol` here would silently
    /// add a `KIND_SHUTDOWN` round-trip to the Unix path and break that.
    #[test]
    fn graceful_stop_delivery_matches_the_platform_mechanism() {
        #[cfg(unix)]
        assert_eq!(GRACEFUL_STOP_DELIVERY, GracefulStopDelivery::Signal);
        #[cfg(windows)]
        assert_eq!(
            GRACEFUL_STOP_DELIVERY,
            GracefulStopDelivery::ShutdownProtocol
        );
    }

    /// The Win32 pid guard: everything but 0 passes through untouched (a Windows
    /// PID is a full `u32`, so there is no overflow arm to check).
    #[test]
    fn checked_target_pid_accepts_every_nonzero_pid() {
        assert_eq!(checked_target_pid(1).unwrap(), 1);
        assert_eq!(checked_target_pid(12345).unwrap(), 12345);
        // Above i32::MAX — legal on Windows, unlike the Unix `pid_t` guards.
        assert_eq!(checked_target_pid(u32::MAX).unwrap(), u32::MAX);
    }

    /// 0 is a wildcard on both Win32 calls this guards, so it must be refused
    /// with `InvalidInput` rather than passed through — the Windows analogue of
    /// the `kill(0, …)` / `killpg(0, …)` broadcast hazard.
    #[test]
    fn checked_target_pid_rejects_zero_pid() {
        let err = checked_target_pid(0).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        let msg = err.to_string();
        assert!(msg.contains("wildcard"), "{msg:?}");
    }

    /// PRD #163 review — the teardown contract both backends owe the caller for
    /// the case `close_agent` hits whenever the user closes a pane whose agent
    /// quit on its own: a child that exits **inside** the grace window must be
    /// torn down promptly rather than costing the whole window, and must not panic
    /// on a process there is nothing left to signal.
    ///
    /// Un-gated on purpose — it runs on the Linux and the `windows-latest`
    /// `cargo nextest run` jobs, driving each backend's real state machine against
    /// a real OS process. The Windows-specific half of the same fix (the job is
    /// still terminated, so descendants of an exited child are reaped instead of
    /// leaking) needs a second process in the job and lives in that backend's
    /// tests.
    #[test]
    fn terminating_a_child_that_exits_during_the_grace_window_is_prompt() {
        // A process that exits immediately, on either platform. Deliberately NOT
        // reaped here: production reaps through the teardown call itself, so the
        // pid must still be the child's (a zombie on Unix, a live process object
        // on Windows) when the state machine runs.
        let program = if cfg!(windows) { "cmd" } else { "true" };
        let args: &[&str] = if cfg!(windows) {
            &["/C", "exit", "0"]
        } else {
            &[]
        };
        let spawned = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn a short-lived helper");

        let group = AgentProcessGroup::adopt(Some(spawned.id()));
        let mut child: Box<dyn portable_pty::Child + Send + Sync> =
            Box::new(super::test_child::StdChild(spawned));

        let grace = std::time::Duration::from_secs(3);
        let started = std::time::Instant::now();
        terminate_child_with_grace_and_wait(&mut child, grace, &group);
        assert!(
            started.elapsed() < grace,
            "an already-exited child must not cost the full grace window (took {:?})",
            started.elapsed()
        );
    }

    /// Fork issue #133 — PR #123 bounded `git worktree add` at 30s on the TUI's
    /// synchronous path and kills the child on expiry, but `child.kill()` reaps
    /// only the direct `git` process; a hook grandchild (`post-checkout`
    /// especially) is not in that kill's blast radius and can outlive the
    /// bound. The fix escalates to a process-group kill instead, which only
    /// works if the child leads its own group — `killpg` on a pid that is NOT
    /// a group leader targets the wrong group entirely. Unlike an agent
    /// spawned through `portable-pty` (already `setsid`'d for pty allocation),
    /// a plain `std::process::Command` child inherits the deck's own group, so
    /// `unix::spawn_in_new_process_group` has to make it a group leader at
    /// spawn time.
    ///
    /// Scenario: a child spawned through `spawn_in_new_process_group` is a
    /// process-group leader — `getpgid(pid) == pid` — while a plainly-spawned
    /// `std::process::Command` child is not. The plain-spawn side is asserted
    /// too, by contrast, so the leader assertion cannot pass vacuously (e.g.
    /// if the test process itself happened to already be a group leader).
    #[cfg(unix)]
    #[spec("orchestration/worktree/008")]
    #[test]
    fn worktree_008_spawn_in_new_process_group_is_its_own_leader() {
        let mut grouped_command = std::process::Command::new("sleep");
        grouped_command.arg("30");
        let (mut grouped_child, _group) =
            super::unix::spawn_in_new_process_group(&mut grouped_command)
                .expect("spawn a group-leader child");
        let grouped_pid = grouped_child.id() as libc::pid_t;
        // SAFETY: `getpgid(2)` is async-signal-safe and has no side effects; it
        // only reports the target's process-group id.
        let grouped_pgid = unsafe { libc::getpgid(grouped_pid) };

        let mut plain_child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn a plainly-spawned child");
        let plain_pid = plain_child.id() as libc::pid_t;
        // SAFETY: same as above.
        let plain_pgid = unsafe { libc::getpgid(plain_pid) };

        // Cleanup before asserting, so a failed assertion never leaks a
        // 30-second `sleep`.
        let _ = grouped_child.kill();
        let _ = grouped_child.wait();
        let _ = plain_child.kill();
        let _ = plain_child.wait();

        assert_eq!(
            grouped_pgid, grouped_pid,
            "a child spawned via spawn_in_new_process_group must lead its own process group \
             (getpgid must equal its own pid), or a subsequent killpg on that pid targets the \
             wrong group"
        );
        assert_ne!(
            plain_pgid, plain_pid,
            "a plainly-spawned std::process::Command child must inherit the caller's process \
             group rather than lead its own — this contrast is what keeps the assertion above \
             from passing vacuously"
        );
    }

    /// Fork issue #133 — the mechanism the fix actually buys: killing the
    /// whole process group, not just the tracked child, reaches a grandchild
    /// the child forked before it died. A real 30s-hook integration test would
    /// be slow and flaky (same call as `orchestration/worktree/007` on the
    /// previous PR), so this stands in with a cheap shell one-liner that forks
    /// a backgrounded sibling process before termination ever runs.
    ///
    /// Scenario: a child spawned through `spawn_in_new_process_group` runs
    /// `sh -c 'sleep 300 & sleep 300'`, which forks a backgrounded grandchild
    /// `sleep` before anything terminates it. After
    /// `terminate_child_with_grace_and_wait` returns, both the direct child's
    /// pid and the discovered grandchild's pid are confirmed gone (`kill(pid,
    /// 0)` reporting `ESRCH` for each, via a bounded poll rather than a single
    /// point-in-time read) — proving the group kill reached the grandchild a
    /// single-pid kill would have orphaned.
    #[cfg(unix)]
    #[spec("orchestration/worktree/009")]
    #[test]
    fn worktree_009_terminate_with_grace_kills_the_whole_group_including_grandchildren() {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "sleep 300 & sleep 300"]);
        let (child, group) = super::unix::spawn_in_new_process_group(&mut command)
            .expect("spawn a group-leader shell child");
        let child_pid = child.id() as libc::pid_t;

        // Discover the backgrounded grandchild through the repo's own
        // descendant scan. Bounded polling, not a bare sleep: the fork behind
        // `&` is near-instant but not synchronous with `spawn()` returning.
        let discover_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let grandchild_pid = loop {
            if let Some(table) = process_table()
                && let Some(descendant) = descendants(&table, child_pid as i32).first()
            {
                break descendant.pid as libc::pid_t;
            }
            assert!(
                std::time::Instant::now() < discover_deadline,
                "the backgrounded `sleep` never showed up as a descendant of pid {child_pid} \
                 within the discovery window"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        };

        let mut boxed_child: Box<dyn portable_pty::Child + Send + Sync> =
            Box::new(super::test_child::StdChild(child));
        terminate_child_with_grace_and_wait(
            &mut boxed_child,
            std::time::Duration::from_millis(200),
            &group,
        );

        // SAFETY: signal 0 sends nothing; it only probes whether `pid` still
        // exists (ESRCH means it is gone).
        let alive = |pid: libc::pid_t| -> bool { unsafe { libc::kill(pid, 0) == 0 } };
        let confirm_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let child_alive = alive(child_pid);
            let grandchild_alive = alive(grandchild_pid);
            if !child_alive && !grandchild_alive {
                break;
            }
            if std::time::Instant::now() >= confirm_deadline {
                panic!(
                    "expected the process-group kill to reap both the direct child (pid \
                     {child_pid}) and its grandchild (pid {grandchild_pid}); still alive: {}",
                    match (child_alive, grandchild_alive) {
                        (true, true) => "both",
                        (true, false) => "the direct child",
                        (false, true) => "the grandchild",
                        (false, false) => unreachable!(),
                    }
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// Fork issue #133 P1 (found independently by both the reviewer and the
    /// auditor on PR #134): `terminate_child_with_grace_and_wait` returns as
    /// soon as `try_wait` shows the *direct* child reaped, skipping phase 3's
    /// SIGKILL entirely — the exact orphan fork issue #133 exists to kill,
    /// since `git` exits promptly on SIGTERM while a `post-checkout` hook can
    /// trap or ignore it. `009` cannot see this: `sleep` dies on SIGTERM the
    /// same as everything else in that scenario, so the direct child and its
    /// grandchild always die together regardless of whether phase 3 ever
    /// runs. This test constructs the shape `009` cannot: a leader that dies
    /// promptly on SIGTERM alongside a same-group descendant that ignores it.
    ///
    /// Scenario: spawn `sh -c '(trap "" TERM; exec sleep 300) & exec sleep
    /// 300'` as a process-group leader. The backgrounded subshell sets `trap
    /// "" TERM` (SIG_IGN) for itself before tail-`exec`ing into `sleep`,
    /// which inherits that ignored disposition across `exec` per POSIX (an
    /// ignored signal's disposition survives exec; only a *caught* signal
    /// resets to default) — making it SIGTERM-resistant. The outer script
    /// never touches TERM's disposition and tail-`exec`s into its own
    /// `sleep`, which keeps the default disposition and so dies on SIGTERM
    /// like an ordinary cooperative process. After discovering the
    /// backgrounded descendant through the repo's own descendant scan (same
    /// mechanism as `009`, not inferred from the leader's exit),
    /// `terminate_child_with_grace_and_wait_forcing_group_backstop` runs with
    /// a 200ms grace window; both the leader and the resistant descendant are
    /// only reaped/killed after that window elapses (fork #143: the forcing
    /// branch no longer polls for an early exit, so it never reaps the
    /// leader before sending the group SIGKILL), and both are confirmed gone
    /// afterward via a bounded `kill(pid, 0)` poll.
    #[cfg(unix)]
    #[spec("orchestration/worktree/010")]
    #[test]
    fn worktree_010_forcing_backstop_kills_a_sigterm_resistant_descendant_after_a_cooperative_leader_exits()
     {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "(trap \"\" TERM; exec sleep 300) & exec sleep 300"]);
        let (child, group) = super::unix::spawn_in_new_process_group(&mut command)
            .expect("spawn a group-leader shell child");
        let child_pid = child.id() as libc::pid_t;

        // Discover the backgrounded, SIGTERM-resistant descendant the same
        // way `009` does: bounded polling against the repo's own descendant
        // scan, not a bare sleep or an assumption inferred from the parent's
        // exit.
        let discover_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let descendant_pid = loop {
            if let Some(table) = process_table()
                && let Some(descendant) = descendants(&table, child_pid as i32).first()
            {
                break descendant.pid as libc::pid_t;
            }
            assert!(
                std::time::Instant::now() < discover_deadline,
                "the backgrounded SIGTERM-resistant shell never showed up as a descendant of \
                 pid {child_pid} within the discovery window"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        };

        let mut boxed_child: Box<dyn portable_pty::Child + Send + Sync> =
            Box::new(super::test_child::StdChild(child));
        super::terminate_child_with_grace_and_wait_forcing_group_backstop(
            &mut boxed_child,
            std::time::Duration::from_millis(200),
            &group,
        );

        // SAFETY: signal 0 sends nothing; it only probes whether `pid` still
        // exists (ESRCH means it is gone).
        let alive = |pid: libc::pid_t| -> bool { unsafe { libc::kill(pid, 0) == 0 } };
        let confirm_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let leader_alive = alive(child_pid);
            let descendant_alive = alive(descendant_pid);
            if !leader_alive && !descendant_alive {
                break;
            }
            if std::time::Instant::now() >= confirm_deadline {
                panic!(
                    "expected the forcing backstop to reap both the cooperative leader (pid \
                     {child_pid}) and the SIGTERM-resistant descendant (pid {descendant_pid}); \
                     still alive: {}",
                    match (leader_alive, descendant_alive) {
                        (true, true) => "both",
                        (true, false) => "the leader",
                        (false, true) => "the descendant",
                        (false, false) => unreachable!(),
                    }
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// Fork issue #136: `terminate_child_with_grace_and_detached_reap_forcing_group_backstop`
    /// must return promptly even when the underlying `wait()` cannot complete
    /// immediately, instead of blocking the caller for however long the reap
    /// takes — that is the property the fix exists to guarantee. A genuinely
    /// unkillable process (one wedged in uninterruptible kernel I/O, e.g. a
    /// stuck NFS mount) cannot be constructed in a test, so this pins the same
    /// observable shape with a pure-Rust `portable_pty::Child` stand-in whose
    /// `wait()` deliberately blocks far longer than the grace window — the
    /// call must still return quickly, and the reap must still complete on
    /// its own afterward. This asserts on that OBSERVABLE behavior (return
    /// latency, eventual completion of the stand-in's `wait()`), not on the
    /// implementation detail of a thread existing.
    ///
    /// Scenario: a fake child that never reports exited via `try_wait` and
    /// whose `wait()` sleeps 1s before completing is torn down with a 50ms
    /// grace window. The call is expected to return in well under a second —
    /// comfortably less than what blocking on the fake's `wait()` would cost
    /// — and the fake's `wait()` is separately confirmed (via a bounded poll
    /// on a shared flag it sets when done) to have run to completion shortly
    /// after, proving the reap was not simply dropped.
    #[cfg(unix)]
    #[spec("orchestration/worktree/011")]
    #[test]
    fn worktree_011_detached_reap_forcing_backstop_returns_before_a_slow_wait_completes() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        #[derive(Debug)]
        struct SlowReapChild {
            reaped: Arc<AtomicBool>,
            reap_delay: std::time::Duration,
        }

        impl portable_pty::ChildKiller for SlowReapChild {
            fn kill(&mut self) -> std::io::Result<()> {
                // No real process backs this stand-in — SIGTERM/SIGKILL are
                // both no-ops here, matching a process that has already been
                // killed but is stuck in kernel I/O and has not yet been
                // reaped.
                Ok(())
            }
            fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
                Box::new(SlowReapChild {
                    reaped: self.reaped.clone(),
                    reap_delay: self.reap_delay,
                })
            }
        }

        impl portable_pty::Child for SlowReapChild {
            fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
                // Never reports exited via polling, so the caller always runs
                // phase 3 and reaches the final reap.
                Ok(None)
            }
            fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
                std::thread::sleep(self.reap_delay);
                self.reaped.store(true, Ordering::SeqCst);
                Ok(portable_pty::ExitStatus::with_exit_code(137))
            }
            fn process_id(&self) -> Option<u32> {
                // No real pid to report; this exercises
                // `signal_child_pgroup_or_fallback`'s `child.kill()` fallback
                // path rather than a real `killpg`.
                None
            }
        }

        let reaped = Arc::new(AtomicBool::new(false));
        let reap_delay = std::time::Duration::from_secs(1);
        let child: Box<dyn portable_pty::Child + Send + Sync> = Box::new(SlowReapChild {
            reaped: reaped.clone(),
            reap_delay,
        });
        let grace = std::time::Duration::from_millis(50);
        let group = AgentProcessGroup;

        let started = std::time::Instant::now();
        super::terminate_child_with_grace_and_detached_reap_forcing_group_backstop(
            child, grace, &group,
        );
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "the call should return once the grace window and signalling are done, without \
             waiting for the slow reap to finish (reap takes {reap_delay:?}, call took \
             {elapsed:?})"
        );
        assert!(
            !reaped.load(Ordering::SeqCst),
            "the reap should not have completed synchronously within the call — otherwise this \
             test is not exercising the detached path at all"
        );

        let confirm_deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !reaped.load(Ordering::SeqCst) {
            assert!(
                std::time::Instant::now() < confirm_deadline,
                "the detached reap never completed — the child was signalled but never reaped, \
                 which would reintroduce the zombie leak the detached thread exists to avoid"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// Fork issue #143: the forcing teardown paths must never derive a
    /// process-group id from a child that has already been reaped —
    /// `try_wait`'s underlying `waitpid(WNOHANG)` reaps as a side effect of
    /// merely checking exit status, and a reaped pid can be recycled by the
    /// kernel before the group signal goes out. The trigger is a
    /// sub-millisecond race needing pid-space wraparound, so it cannot be
    /// forced deterministically; what this pins instead is the ordering
    /// property that makes the race structurally impossible: a reap
    /// (`try_wait` or `wait`) never happens before the phase-3 group signal.
    ///
    /// Scenario: a fake `portable_pty::Child` with no real pid
    /// (`process_id() -> None`, which routes every signal through the
    /// observable `ChildKiller::kill` fallback instead of a real `killpg`,
    /// so this needs no OS process at all) records every `try_wait`/`wait`/
    /// `kill` call it receives, in call order, into a shared log.
    /// `terminate_child_with_grace_and_wait_forcing_group_backstop` runs
    /// against it with a short grace window; the log is then asserted to
    /// contain no `try_wait` calls at all (the forcing path must not poll —
    /// polling is exactly what reaps early) and exactly two `kill` calls
    /// (phase 1's SIGTERM, then phase 3's forcing SIGKILL), both preceding
    /// the single `wait` call that performs the actual reap.
    #[cfg(unix)]
    #[spec("orchestration/worktree/013")]
    #[test]
    fn worktree_013_forcing_backstop_never_reaps_before_sending_the_group_signal() {
        use std::sync::{Arc, Mutex};

        #[derive(Debug)]
        struct OrderingProbeChild {
            log: Arc<Mutex<Vec<&'static str>>>,
        }

        impl portable_pty::ChildKiller for OrderingProbeChild {
            fn kill(&mut self) -> std::io::Result<()> {
                self.log.lock().unwrap().push("kill");
                Ok(())
            }
            fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
                Box::new(OrderingProbeChild {
                    log: self.log.clone(),
                })
            }
        }

        impl portable_pty::Child for OrderingProbeChild {
            fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
                self.log.lock().unwrap().push("try_wait");
                Ok(None)
            }
            fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
                self.log.lock().unwrap().push("wait");
                Ok(portable_pty::ExitStatus::with_exit_code(0))
            }
            fn process_id(&self) -> Option<u32> {
                // No real pid — forces `signal_child_pgroup_or_fallback`
                // through the observable `ChildKiller::kill` fallback rather
                // than a real `killpg`, so this test needs no OS process.
                None
            }
        }

        let log = Arc::new(Mutex::new(Vec::new()));
        let mut boxed_child: Box<dyn portable_pty::Child + Send + Sync> =
            Box::new(OrderingProbeChild { log: log.clone() });
        let group = AgentProcessGroup;

        super::terminate_child_with_grace_and_wait_forcing_group_backstop(
            &mut boxed_child,
            std::time::Duration::from_millis(20),
            &group,
        );

        let calls = log.lock().unwrap().clone();
        assert!(
            !calls.contains(&"try_wait"),
            "the forcing backstop must never poll `try_wait` — doing so would reap the direct \
             child before phase 3 can safely derive a pgid from it; calls were: {calls:?}"
        );
        let last_kill = calls.iter().rposition(|c| *c == "kill");
        let first_wait = calls.iter().position(|c| *c == "wait");
        match (last_kill, first_wait) {
            (Some(k), Some(w)) => assert!(
                k < w,
                "the reap (`wait`) must happen strictly after the last group signal (`kill`), \
                 never before it; calls were: {calls:?}"
            ),
            _ => panic!("expected both a `kill` call and a `wait` call; calls were: {calls:?}"),
        }
        assert_eq!(
            calls.iter().filter(|c| **c == "kill").count(),
            2,
            "expected exactly two signals (SIGTERM then the forcing SIGKILL); calls were: \
             {calls:?}"
        );
    }

    /// Fork issue #136 finding 2 (both the reviewer and the auditor, and the
    /// auditor's correction of a justification the spec had accepted):
    /// "one thread per timeout" was never a bound on its own — a single
    /// stuck `create_worktree_sync` attempt can reach the detached-reap tail
    /// up to three times, and a retrying user repeats it — so the outstanding
    /// count must itself be capped, with a synchronous fallback that still
    /// reaps rather than ever dropping the child (finding 1).
    ///
    /// This drives [`super::detach_reap_or_fallback_sync_with_cap`] directly
    /// — the exact function both platforms' detached-reap backstops funnel
    /// into, parameterized on `counter`/`cap` rather than reimplemented —
    /// with the cap forced to 1 and a counter private to this test, so
    /// nothing here depends on (or asserts) the real process-wide cap value
    /// or interferes with any other test's use of it.
    ///
    /// Scenario: two fake children, each reporting never-exited via
    /// `try_wait` and recording completion via their own `AtomicBool` when
    /// their `wait()` returns (child A after 500ms, child B after 150ms).
    /// With the cap forced to 1, child A is handed to the detached path
    /// first — it takes the only slot, so the call returns promptly, well
    /// before A's own `wait()` could have finished. Child B is handed off
    /// immediately afterward, while A's slot is still held: the cap is
    /// saturated, so B's call takes the synchronous fallback branch instead
    /// — it must therefore both take at least as long as B's own `wait()`
    /// AND have B's completion flag already set the moment it returns,
    /// proving the fallback genuinely reaps inline rather than dropping B or
    /// racing its own return against the reap. A bounded poll afterward
    /// confirms A's detached reap also eventually completes — the honest
    /// property this test exists for: a reap beyond the cap still completes,
    /// on either path.
    #[cfg(unix)]
    #[spec("orchestration/worktree/012")]
    #[test]
    fn worktree_012_outstanding_reap_cap_falls_back_to_a_synchronous_reap_that_still_completes() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        #[derive(Debug)]
        struct SlowReapChild {
            reaped: Arc<AtomicBool>,
            reap_delay: std::time::Duration,
        }

        impl portable_pty::ChildKiller for SlowReapChild {
            fn kill(&mut self) -> std::io::Result<()> {
                Ok(())
            }
            fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
                Box::new(SlowReapChild {
                    reaped: self.reaped.clone(),
                    reap_delay: self.reap_delay,
                })
            }
        }

        impl portable_pty::Child for SlowReapChild {
            fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
                Ok(None)
            }
            fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
                std::thread::sleep(self.reap_delay);
                self.reaped.store(true, Ordering::SeqCst);
                Ok(portable_pty::ExitStatus::with_exit_code(137))
            }
            fn process_id(&self) -> Option<u32> {
                None
            }
        }

        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        const CAP: usize = 1;

        let a_reaped = Arc::new(AtomicBool::new(false));
        let a: Box<dyn portable_pty::Child + Send + Sync> = Box::new(SlowReapChild {
            reaped: a_reaped.clone(),
            reap_delay: std::time::Duration::from_millis(500),
        });
        let b_reaped = Arc::new(AtomicBool::new(false));
        let b_reap_delay = std::time::Duration::from_millis(150);
        let b: Box<dyn portable_pty::Child + Send + Sync> = Box::new(SlowReapChild {
            reaped: b_reaped.clone(),
            reap_delay: b_reap_delay,
        });

        // A takes the only slot and goes detached: returns promptly, well
        // under its own 500ms reap delay.
        let a_started = std::time::Instant::now();
        super::detach_reap_or_fallback_sync_with_cap(
            a,
            &COUNTER,
            CAP,
            "worktree-012-reap-a",
            "orchestration-worktree-012-test-a",
        );
        let a_elapsed = a_started.elapsed();
        assert!(
            a_elapsed < std::time::Duration::from_millis(300),
            "the first call should take the free slot and return via the detached path, well \
             before its own 500ms reap could finish (call took {a_elapsed:?})"
        );
        assert!(
            !a_reaped.load(Ordering::SeqCst),
            "A's reap should not have completed synchronously within the call — otherwise the \
             cap forced no detached path at all and the rest of this test cannot distinguish \
             the two branches"
        );

        // B is handed off immediately after, while A's slot is still held —
        // the cap is saturated, so B must take the synchronous fallback.
        let b_started = std::time::Instant::now();
        super::detach_reap_or_fallback_sync_with_cap(
            b,
            &COUNTER,
            CAP,
            "worktree-012-reap-b",
            "orchestration-worktree-012-test-b",
        );
        let b_elapsed = b_started.elapsed();
        assert!(
            b_elapsed >= b_reap_delay.saturating_sub(std::time::Duration::from_millis(30)),
            "with the cap saturated by A, B's call must fall back to a synchronous wait and so \
             cost at least B's own reap delay ({b_reap_delay:?}), not return early (call took \
             {b_elapsed:?})"
        );
        assert!(
            b_reaped.load(Ordering::SeqCst),
            "B's reap must already be complete the moment the saturated-cap call returns — the \
             synchronous fallback must reap inline, not merely schedule the reap and return \
             before it finishes"
        );

        // A's detached reap — which the cap allowed to proceed — must still
        // complete on its own; the cap must never turn into a second way to
        // drop a child.
        let confirm_deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !a_reaped.load(Ordering::SeqCst) {
            assert!(
                std::time::Instant::now() < confirm_deadline,
                "A's detached reap never completed — it was allowed past the cap but then \
                 dropped, which is exactly the zombie leak finding 1 exists to prevent"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}
