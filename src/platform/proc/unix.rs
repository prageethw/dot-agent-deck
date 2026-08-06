//! Unix process lifecycle: `killpg`/`kill` signal teardown + `getppid`.
//! Behavior-preserving lift of the signal helpers from `agent_pty.rs`, the
//! daemon-stop kill from `build_version_handshake.rs`, and `current_ppid` from
//! `daemon.rs`.

use std::time::Duration;

// ---------------------------------------------------------------------------
// Agent process-group teardown (lifted from agent_pty.rs).
// ---------------------------------------------------------------------------

/// PRD #163 M3 — Unix has nothing to hold here.
///
/// An agent's descendant tree is already addressable on Unix: `portable-pty`
/// `setsid`s the child, which makes it a process-group leader, so `killpg(pid,
/// …)` reaches the agent *and* everything it spawned with no bookkeeping at all.
/// This zero-sized type exists only so the spawn/teardown seam has the same
/// shape on both platforms — Windows has no implicit grouping and must own a Job
/// Object handle for the agent's whole lifetime (see the windows backend), which
/// is why the handle has to be created at spawn and carried on
/// [`crate::agent_pty::RunningAgent`].
///
/// Being a ZST, it is `Send`/`Sync`/`Default` for free and costs nothing in
/// `AgentPty`/`RunningAgent`; the Unix teardown paths ignore it entirely and
/// keep using `killpg`.
#[derive(Debug, Default, Clone, Copy)]
pub struct AgentProcessGroup;

impl AgentProcessGroup {
    /// No-op on Unix: the process group the kill paths use is the one
    /// `portable-pty`'s `setsid` already established, so there is nothing to
    /// create and nothing that can fail.
    pub fn adopt(_pid: Option<u32>) -> Self {
        Self
    }
}

/// PRD #92 F1 followup (defensive): convert a portable-pty `process_id()`
/// (a `u32`) into a positive `libc::pid_t` suitable for `killpg`, or `None` if
/// the raw value can't legally name a process group.
///
/// `killpg(pgid, sig)` has two dangerous degenerate cases for non-positive
/// `pgid`:
///   - `pgid == 0` is documented as "signal every process in *the caller's*
///     process group" — which for the daemon would mean signalling the daemon
///     itself plus every connected attach-client.
///   - `pgid < 0` is undefined behavior in POSIX and a likely overflow
///     indicator (a `u32` PID that didn't fit in `i32`).
///
/// Both should be impossible from a well-behaved `portable-pty` spawn (Linux
/// PIDs are positive `i32` values up to `i32::MAX`), but defensively checking
/// is one `if` and one unit test, which is much cheaper than the unbounded
/// blast radius of getting it wrong. On `None` the caller falls back to
/// `child.kill()` (single-PID).
pub(crate) fn pid_to_pgid(pid: u32) -> Option<libc::pid_t> {
    let signed = pid as i64;
    if signed > 0 && signed <= libc::pid_t::MAX as i64 {
        Some(signed as libc::pid_t)
    } else {
        None
    }
}

/// Low-level shared helper. Send `signal` to the child's process group,
/// falling back to `portable_pty::Child::kill` when `pid_to_pgid` rejects the
/// raw pid (F1-followup defensive boundary check). `phase` is included in
/// `tracing::warn!` payloads so a wedged child can be traced back to whichever
/// phase issued the kill. Returns `true` if the `killpg` syscall actually fired
/// (or the `child.kill` fallback was used), `false` if the syscall reported an
/// error other than ESRCH.
fn signal_child_pgroup_or_fallback(
    child: &mut Box<dyn portable_pty::Child + Send + Sync>,
    signal: libc::c_int,
    phase: &'static str,
) -> bool {
    let raw_pid = child.process_id();
    let pgid = raw_pid.and_then(pid_to_pgid);
    let Some(pgid) = pgid else {
        // PRD #92 F8 followup (auditor #2 — option b documented):
        // pid_to_pgid rejected the raw pid (either `process_id()` returned
        // `None` or the pid was outside the safe `(0, i32::MAX]` range). The
        // portable-pty `Child` trait allows `None` here, but the Unix backend
        // used by this codebase always returns `Some` in practice. The
        // `(0, i32::MAX]` boundary check is defense-in-depth against a future
        // portable-pty bug; on real Linux/macOS PIDs it never fails. The
        // fallback below uses `portable_pty::Child::kill`, which sends SIGHUP
        // — strictly weaker than the requested `signal` (typically SIGTERM or
        // SIGKILL) and limited to the direct child (no process-group
        // semantics, so descendants leak). The caller's subsequent
        // `child.wait()` is unbounded — that's acceptable for the same "this
        // branch is practically unreachable" reason.
        //
        // Auditor #5: emit a warn-level event so a descendant leak surfaced
        // via this fallback is at least observable.
        tracing::warn!(
            ?raw_pid,
            signal,
            phase = %phase,
            reason = if raw_pid.is_none() { "process_id-returned-none" } else { "pid_to_pgid-rejected" },
            "signal_child_pgroup_or_fallback: pgid unavailable — falling back to portable_pty::Child::kill (SIGHUP, single-PID; descendants will leak)"
        );
        let _ = child.kill();
        return true;
    };
    // SAFETY: `killpg(2)` is async-signal-safe; the pgid we just validated via
    // `pid_to_pgid` is the child's own PID (portable-pty `setsid`'d it, making
    // it the group leader), so this cannot affect any other agent's group.
    let rc = unsafe { libc::killpg(pgid, signal) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        let benign = err.raw_os_error() == Some(libc::ESRCH);
        if !benign {
            tracing::warn!(pgid, signal, phase = %phase, error = %err, "killpg failed");
        }
        return benign;
    }
    true
}

/// Forcefully terminate the child *and every descendant in its process group*
/// with SIGKILL and reap it. SIGKILL is preferred over
/// `portable_pty::Child::kill()` (which sends SIGHUP) because a shell can
/// ignore SIGHUP — leaving the subsequent `wait()` to block forever. SIGKILL
/// cannot be caught or ignored, so the kernel tears the process down and
/// `wait()` returns promptly. Callers should drop the master/writer/reader
/// handles before invoking this so any I/O blocked on the PTY unblocks first.
///
/// `_group` is the cross-platform teardown handle (PRD #163 M3) and is unused on
/// Unix — see [`AgentProcessGroup`]; the process group `killpg` addresses is
/// implicit in the child's own pid.
pub fn force_kill_child_and_wait(
    child: &mut Box<dyn portable_pty::Child + Send + Sync>,
    _group: &AgentProcessGroup,
) {
    signal_child_pgroup_or_fallback(child, libc::SIGKILL, "force-kill");
    let _ = child.wait();
}

/// SIGTERM-then-SIGKILL escalation used by the single-pane Ctrl+W path. Sends
/// `SIGTERM` to the child's process group, polls `try_wait` until the child
/// exits or `grace` elapses, then sends `SIGKILL` as the backstop and reaps the
/// child.
///
/// `_group` is unused on Unix (see [`force_kill_child_and_wait`]).
pub fn terminate_child_with_grace_and_wait(
    child: &mut Box<dyn portable_pty::Child + Send + Sync>,
    grace: Duration,
    _group: &AgentProcessGroup,
) {
    // Phase 1: SIGTERM the process group.
    signal_child_pgroup_or_fallback(child, libc::SIGTERM, "graceful-close-sigterm");

    // Phase 2: poll `try_wait` until the child exits or the grace elapses.
    // Polling avoids the obvious "sleep for grace then SIGKILL" alternative —
    // a child that exits promptly after SIGTERM doesn't have to wait around
    // for the deadline. 50 ms cadence is small enough to feel responsive and
    // large enough to keep CPU cost negligible (~60 polls over 3 s).
    let deadline = std::time::Instant::now() + grace;
    while std::time::Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(_) => break,
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // Phase 3: SIGKILL backstop. Reaches survivors regardless of
    // SIGTERM-trapping state.
    signal_child_pgroup_or_fallback(child, libc::SIGKILL, "graceful-close-sigkill");
    let _ = child.wait();
}

/// SIGTERM the child's process group without waiting (the daemon-wide
/// `shutdown_all_graceful` SIGTERM phase issues this to every agent in
/// parallel and polls them together). `phase` tags the `tracing` payload.
pub fn send_sigterm_to_child_group(
    child: &mut Box<dyn portable_pty::Child + Send + Sync>,
    phase: &'static str,
) {
    signal_child_pgroup_or_fallback(child, libc::SIGTERM, phase);
}

// ---------------------------------------------------------------------------
// Foreground process-group query (PRD #370 M1).
// ---------------------------------------------------------------------------

/// The pty's current foreground process-group id (`tcgetpgrp` under the
/// hood, via `portable_pty::MasterPty::process_group_leader`), or `None` if
/// the backend can't report one (e.g. the master fd is already closed).
///
/// This is the Unix half of the [`foreground_pgid`] seam; see `windows.rs`
/// for why the Windows half is an unconditional `None` rather than a
/// best-effort implementation.
pub fn foreground_pgid(master: &dyn portable_pty::MasterPty) -> Option<i32> {
    master.process_group_leader()
}

// ---------------------------------------------------------------------------
// Process-table sample (PRD #386 M1; hardened by fork issues #29/#30 and
// Greptile's P2 on upstream PR #390 — the sample is now budgeted, runs off the
// Tokio worker, and distrusts a `getsid` answer whose pid may have been
// recycled).
// ---------------------------------------------------------------------------

/// A process's POSIX session id, or a negative value if it could not be read
/// (the usual cause being that the process exited between the `ps` sample and
/// this call, giving `ESRCH`).
///
/// **This is deliberately not `ps -o sess=`**, which prints `0` for a non-root
/// caller on macOS and is useless for the discriminator. `getsid(2)` is POSIX,
/// behaves identically on macOS and Linux, works on any pid rather than only on
/// children, and needs no `/proc` parsing on Linux either.
fn getsid_or_negative(pid: i32) -> i32 {
    // SAFETY: `getsid(2)` is async-signal-safe and has no side effects; it
    // either reports the target's session id or returns -1 with `errno` set.
    unsafe { libc::getsid(pid) }
}

/// The `ps` invocation every sample uses. `-w -w` disables `ps`'s width
/// truncation on both macOS and Linux, so the argv column survives whole; the
/// trailing `=` on every `-o` field suppresses the header line entirely.
const PS_PROGRAM: &str = "ps";
const PS_ARGS: [&str; 5] = ["-A", "-w", "-w", "-o", "pid=,ppid=,tty=,args="];

/// Wall-clock budget for one whole [`process_table`] / [`process_table_async`]
/// sample — **both** `ps` invocations plus the `getsid` pass between them
/// (fork issue #29: `ps` previously ran unbounded).
///
/// Why 2 s. A healthy `ps -A` on macOS or Linux costs single-digit to low tens
/// of milliseconds, so 2 s is ~50–200× the honest cost and cannot plausibly
/// fire on a healthy machine — including this repo's own test runs, which have
/// been measured at load averages of 11–19 where a fork/exec can be delayed by
/// hundreds of milliseconds. It is also small enough to be worth having: the
/// production caller polls every 500 ms, so a wedged `ps` costs at most one
/// extra sample's worth of staleness per tick instead of parking the poll (and,
/// before the async move below, a Tokio worker) forever.
pub(crate) const PS_SAMPLE_BUDGET: Duration = Duration::from_secs(2);

/// Run `program args…` with its stdout captured, abandoning it if it has not
/// finished within `budget`. `None` means "no usable output": spawn failed, the
/// budget elapsed, or the process exited non-zero.
///
/// The child is polled through the [`std::process::Child`] this function owns
/// and is never reaped before the kill decision is made, so the SIGKILL cannot
/// land on a recycled pid. stdout is drained on a helper thread because `ps -A`
/// output routinely exceeds a pipe buffer: waiting on the child while nothing
/// reads the pipe would deadlock the very timeout this exists to enforce.
fn capture_bounded(program: &str, args: &[&str], budget: Duration) -> Option<String> {
    use std::io::Read;

    if budget.is_zero() {
        return None;
    }
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let Some(mut pipe) = child.stdout.take() else {
        // Unreachable — stdout was just configured as a pipe — but returning
        // without reaping would leave a zombie behind on every call.
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        buf
    });

    let deadline = std::time::Instant::now() + budget;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(_) => break None,
        }
        if std::time::Instant::now() >= deadline {
            tracing::warn!(
                program,
                ?budget,
                "process-table sample exceeded its budget — killing it and reporting no sample"
            );
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        // Same shape as `terminate_child_with_grace_and_wait`'s grace poll: a
        // child that finishes early is picked up on the next tick rather than
        // costing the whole budget. 5 ms is well under the cost of the `ps` it
        // is waiting on and keeps the CPU cost of the wait negligible.
        std::thread::sleep(Duration::from_millis(5));
    };

    let stdout = reader.join().ok()?;
    if !status?.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&stdout).into_owned())
}

/// The async twin of [`capture_bounded`], for callers on a Tokio runtime.
///
/// `kill_on_drop` is what makes the timeout real: when `tokio::time::timeout`
/// drops the inner future, the child is dropped, killed, and reaped by Tokio's
/// orphan reaper. That is also why this is a `tokio::process::Command` rather
/// than a `spawn_blocking` wrapper around [`capture_bounded`] — see
/// [`process_table_async`].
async fn capture_bounded_async(program: &str, args: &[&str], budget: Duration) -> Option<String> {
    if budget.is_zero() {
        return None;
    }
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let Ok(output) = tokio::time::timeout(budget, command.output()).await else {
        tracing::warn!(
            program,
            ?budget,
            "process-table sample exceeded its budget — killing it and reporting no sample"
        );
        return None;
    };
    let output = output.ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The sampling sequence itself, over an injected `capture` (called twice) and
/// an injected session-id resolver — the shared body of [`process_table`] and
/// [`process_table_async`].
///
/// **The order of the three steps is the entire fix for fork issue #30 and is
/// not incidental**: the `getsid` pass must run strictly *between* the two
/// captures. Capturing both samples first and only then reading session ids
/// leaves the whole reuse window between the second capture and `getsid`
/// unprotected — a pid can exit and be recycled there, and the confirmation
/// then vouches for a `getsid` answer that describes the replacement process.
/// (Exactly that inversion shipped in this change's first draft and was caught
/// in review.) `capture` and `session_id_of` are injected rather than called
/// directly so `the_getsid_pass_runs_between_the_two_captures` can observe the
/// sequence and fail if it is ever reordered again.
///
/// See [`super::scan::invalidate_unconfirmed_session_ids`] for the invariant
/// the confirmation enforces and for the residual window that remains. The cost
/// is the second `ps` per sample, which is the shape PRD #386's M5 measurement
/// should be taken against.
fn sample_table(
    mut capture: impl FnMut() -> Option<String>,
    session_id_of: impl Fn(i32) -> i32,
) -> Option<Vec<super::ProcessInfo>> {
    let first = capture()?;
    let mut rows = super::scan::parse_ps_table(&first, &session_id_of);
    if rows.is_empty() {
        return None;
    }
    let confirm = capture()?;
    super::scan::invalidate_unconfirmed_session_ids(&mut rows, &confirm);
    Some(rows)
}

/// [`sample_table`] for an async `capture`. Kept as a separate function rather
/// than unified because the two capture forms are genuinely different types
/// (`Option<String>` vs a future of one); the *sequence* below must stay
/// identical to the blocking twin's, and both are pinned by their own ordering
/// test.
async fn sample_table_async<F, Fut, S>(
    mut capture: F,
    session_id_of: S,
) -> Option<Vec<super::ProcessInfo>>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<String>>,
    S: Fn(i32) -> i32,
{
    let first = capture().await?;
    let mut rows = super::scan::parse_ps_table(&first, &session_id_of);
    if rows.is_empty() {
        return None;
    }
    let confirm = capture().await?;
    super::scan::invalidate_unconfirmed_session_ids(&mut rows, &confirm);
    Some(rows)
}

/// Sample every process on the machine into a [`super::ProcessInfo`] table
/// (PRD #386 M1, Route A), or `None` if the sample could not be taken —
/// `ps` failed to run, exceeded [`PS_SAMPLE_BUDGET`], or produced nothing
/// parseable.
///
/// **Blocking.** The daemon's 500 ms poll must use [`process_table_async`]
/// instead; this synchronous form is for callers that are not on a Tokio
/// runtime (tests and diagnostics).
///
/// One sample is parsed once and reusable for *every* pane in a poll, so the
/// cost is per poll cycle rather than per pane. The session id of each row
/// comes from [`getsid_or_negative`], not from `ps` — and is kept only if a
/// second `ps` still describes the same process (fork issue #30).
///
/// Route B (native enumeration — `/proc/<pid>/{stat,cmdline}` on Linux,
/// `sysctl(KERN_PROC_ALL)` on macOS) stays open behind PRD #386's M5
/// measurement; it removes the subprocess at the cost of two platform-specific
/// implementations, and is only worth taking if the measurement says so.
pub fn process_table() -> Option<Vec<super::ProcessInfo>> {
    // One deadline for the whole sample: each capture gets whatever is left of
    // [`PS_SAMPLE_BUDGET`], so a slow first `ps` cannot buy the second one a
    // fresh full budget.
    let deadline = std::time::Instant::now() + PS_SAMPLE_BUDGET;
    sample_table(
        || {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            capture_bounded(PS_PROGRAM, &PS_ARGS, remaining)
        },
        getsid_or_negative,
    )
}

/// [`process_table`] for callers on a Tokio runtime — the form the daemon's
/// shell-activity poll uses.
///
/// Greptile P2 on upstream PR #390: the poll ran the synchronous
/// `Command::output()` directly on a Tokio worker thread, so a slow or wedged
/// `ps` stalled that worker and with it every other task scheduled on it —
/// status hooks, client requests, shutdown. `tokio::process::Command` is used
/// rather than wrapping the blocking form in `spawn_blocking` because only the
/// former makes the timeout *effective*: cancelling a `spawn_blocking` call
/// returns to the caller but leaves the blocking thread wedged on the same `ps`
/// forever, consuming a pool slot per poll, whereas dropping the async future
/// kills and reaps the child (`kill_on_drop`).
pub async fn process_table_async() -> Option<Vec<super::ProcessInfo>> {
    // One deadline for the whole sample — see the blocking twin.
    let deadline = std::time::Instant::now() + PS_SAMPLE_BUDGET;
    sample_table_async(
        || {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            capture_bounded_async(PS_PROGRAM, &PS_ARGS, remaining)
        },
        getsid_or_negative,
    )
    .await
}

// ---------------------------------------------------------------------------
// Daemon-stop termination by PID (lifted from build_version_handshake.rs).
// ---------------------------------------------------------------------------

/// Convert a `u32` PID (as returned by `peer_pid()`) into the `pid_t` (`i32`)
/// shape `libc::kill` wants, refusing values that would dangerously change the
/// syscall's meaning:
/// - `pid == 0`: `kill(0, sig)` broadcasts to every process in the calling
///   process group — would take down the parent shell.
/// - `pid > i32::MAX`: the `as i32` cast would wrap to a negative value.
///   `kill(-pgid, sig)` means "signal every process in process group `pgid`" —
///   a wildcard kill. Refuse rather than send.
/// - resulting `i32 <= 0` after the cast: defense-in-depth for any path that
///   bypasses the explicit checks above.
fn checked_signal_pid(pid: u32) -> std::io::Result<libc::pid_t> {
    if pid == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "peer pid is 0; refusing to kill(0, SIGTERM) (would broadcast to process group)",
        ));
    }
    if pid > i32::MAX as u32 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "peer pid {pid} does not fit in pid_t; refusing kill() (negative i32 would target a process group)"
            ),
        ));
    }
    let signed = pid as libc::pid_t;
    if signed <= 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("peer pid {pid} resolves to non-positive pid_t {signed}; refusing kill()"),
        ));
    }
    Ok(signed)
}

/// Send `SIGTERM` to `pid` (the daemon-stop graceful signal). Guards against
/// pid 0 / overflow that would turn the signal into a process-group broadcast.
///
/// `ESRCH` (no such process) is **not** an error: it means the daemon already
/// exited, which is a clean already-gone success for the caller (the
/// `daemon stop` path racing a self-exiting daemon, and the re-resolve fallback
/// in `build_version_handshake` that documents "SIGTERM lands as ESRCH").
/// Rather than collapsing that case into an indistinguishable `Ok(())` — which
/// would force `terminate_daemon_graceful` into the same poll/escalate loop it
/// runs for a signal that *was* delivered — ESRCH surfaces as the distinct
/// [`TerminateSignal::AlreadyGone`] so the caller can short-circuit straight to
/// `Stopped`, exactly matching the pre-refactor `terminate_daemon_graceful` on
/// `main` (which special-cased ESRCH to an immediate `Ok(Stopped)`). Any other
/// errno is a genuine failure and is **not** swallowed.
pub fn terminate_pid(pid: u32) -> std::io::Result<super::TerminateSignal> {
    let signal_pid = checked_signal_pid(pid)?;
    // SAFETY: `libc::kill` is async-signal-safe and has no in-process side
    // effects beyond delivering the signal to the target PID.
    let rc = unsafe { libc::kill(signal_pid, libc::SIGTERM) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            return Ok(super::TerminateSignal::AlreadyGone);
        }
        return Err(err);
    }
    Ok(super::TerminateSignal::Delivered)
}

/// Unix has nothing to pin, so this is an empty token — see [`pin_process`].
#[derive(Debug)]
pub struct PinnedProcess;

impl Drop for PinnedProcess {
    /// Nothing to release. Declared anyway so "the pin is held until here" is
    /// expressible in the shared `daemon stop` flow on both platforms — an
    /// explicit `drop(pinned)` there is a real release on Windows and must still
    /// compile (and mean the same thing) here.
    fn drop(&mut self) {}
}

/// Pin `pid`'s identity — a **justified no-op on Unix**, kept as a seam so the
/// shared `daemon stop` flow does not grow a `cfg` branch (PRD #163 review).
///
/// POSIX offers no portable way to reserve a pid: the kernel frees it at reap
/// time regardless of what the reaper holds, and the one mechanism that would
/// (Linux `pidfd_open`) has no macOS counterpart. So Unix keeps exactly the
/// behaviour it always had — zero syscalls here, and the residual TOCTOU window
/// stays documented where it is accepted, at the top of
/// [`crate::build_version_handshake::terminate_daemon_graceful`].
///
/// Always `Ok(Some(…))`: reporting "already gone" would change the Unix control
/// flow, and reporting an error would refuse a stop that works today. The pid
/// guards inside [`terminate_pid`] / [`force_kill_pid`] remain the only gate.
pub fn pin_process(_pid: u32) -> std::io::Result<Option<PinnedProcess>> {
    Ok(Some(PinnedProcess))
}

/// Send `SIGKILL` to `pid` (the daemon-stop `--force` escalation). Same guards
/// as [`terminate_pid`].
pub fn force_kill_pid(pid: u32) -> std::io::Result<()> {
    let signal_pid = checked_signal_pid(pid)?;
    // SAFETY: same as `terminate_pid`; SIGKILL is uncatchable but the syscall
    // itself is async-signal-safe.
    let rc = unsafe { libc::kill(signal_pid, libc::SIGKILL) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Orphan watchdog (lifted from daemon.rs; test-gated, OFF in production).
// ---------------------------------------------------------------------------

/// The calling process's parent pid. Wraps `getppid(2)` (async-signal-safe,
/// infallible) so the single `unsafe` lives in one place.
pub fn current_ppid() -> i32 {
    // SAFETY: `getppid(2)` has no failure mode and no side effects.
    unsafe { libc::getppid() }
}

#[cfg(test)]
mod tests {
    use super::*;

    // PRD #92 F1 followup (auditor #3) — defensive boundary check on the
    // `u32` PID → `libc::pid_t` PGID conversion used by the `killpg` call
    // sites. The pre-followup code did `pid as i32` directly, which silently
    // wrapped overflowing `u32` values into negative `i32`s (undefined
    // behavior for `killpg`) and never guarded against `pgid == 0` (which
    // `killpg(2)` documents as "signal every process in the *caller's* process
    // group" — for the daemon that would signal itself plus every attach
    // client). Real-world Linux PIDs are positive `i32` values, so this is
    // defense-in-depth; the unit test pins the boundary semantics.

    #[test]
    fn pid_to_pgid_accepts_positive_normal_pid() {
        assert_eq!(pid_to_pgid(1), Some(1));
        assert_eq!(pid_to_pgid(12345), Some(12345));
    }

    #[test]
    fn pid_to_pgid_rejects_zero_pid() {
        // `killpg(0, ...)` would signal the caller's own group — for the
        // daemon that's a fatal self-target. Must be filtered out.
        assert_eq!(pid_to_pgid(0), None);
    }

    #[test]
    fn pid_to_pgid_accepts_max_i32_pid() {
        let max = i32::MAX as u32;
        assert_eq!(pid_to_pgid(max), Some(i32::MAX));
    }

    #[test]
    fn pid_to_pgid_rejects_overflowing_u32_pid() {
        // Anything above i32::MAX would overflow the `as i32` cast in the
        // pre-followup code into a negative pgid. The guard converts those to
        // `None` so the kill path falls back to the single-PID `child.kill()`
        // path.
        assert_eq!(pid_to_pgid(i32::MAX as u32 + 1), None);
        assert_eq!(pid_to_pgid(u32::MAX), None);
    }

    // PRD #42 review N1 — boundary check on the daemon-stop `kill()` PID guard
    // (`checked_signal_pid`, lifted here from `build_version_handshake.rs`). It
    // is security-sensitive: a `peer_pid()` of 0 would make `kill(0, SIGTERM)`
    // broadcast to the caller's whole process group (taking down the parent
    // shell), and a `u32` PID above `i32::MAX` would wrap the `as i32` cast to a
    // negative value, turning `kill()` into a process-group wildcard. These
    // tests pin the guard semantics without signalling any real process.

    #[test]
    fn checked_signal_pid_accepts_positive_normal_pid() {
        assert_eq!(checked_signal_pid(1).unwrap(), 1);
        assert_eq!(checked_signal_pid(12345).unwrap(), 12345);
        assert_eq!(checked_signal_pid(i32::MAX as u32).unwrap(), i32::MAX);
    }

    #[test]
    fn checked_signal_pid_rejects_zero_pid() {
        // `kill(0, ...)` broadcasts to the caller's process group — must be
        // refused with `InvalidInput`, never signalled.
        let err = checked_signal_pid(0).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    /// PRD #163 review — the Unix half of the pid-pin seam must stay a pure no-op,
    /// including for the pids the by-signal guards reject. Anything else (an error,
    /// or an `Ok(None)` read as "already gone") would change what `daemon stop`
    /// does on Unix, and the whole point of the seam is that it does not.
    #[test]
    fn pinning_is_a_no_op_that_never_changes_the_unix_flow() {
        assert!(pin_process(std::process::id()).unwrap().is_some());
        assert!(pin_process(0).unwrap().is_some());
        assert!(pin_process(u32::MAX).unwrap().is_some());
    }

    // -----------------------------------------------------------------------
    // Process-table sampling (fork issues #29/#30, Greptile P2 on PR #390).
    //
    // These stand on their own deliberately: `status/shell-activity/005` and
    // `007` cover this code with a real agent, but they self-skip in CI for
    // lack of Claude credentials, so the sampling contract has to be pinned
    // here or it is not pinned anywhere CI actually runs.
    // -----------------------------------------------------------------------

    /// A sample that outruns its budget must report **no sample** (`None`), not
    /// an empty or partial one — and must give up at roughly the budget rather
    /// than waiting for the child. `Some(String::new())` here would be read by
    /// the caller as "the table is empty", and a wedged `ps` would then flip
    /// every busy pane to `Idle` — the exact PRD #386 failure mode, through a
    /// different door.
    #[test]
    fn a_sample_that_outruns_its_budget_reports_no_sample() {
        let started = std::time::Instant::now();
        let captured = capture_bounded("sleep", &["30"], Duration::from_millis(200));
        assert_eq!(captured, None, "a timed-out sample must never yield output");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the budget must actually bound the call (took {:?})",
            started.elapsed()
        );
    }

    /// Same contract on the async path the daemon's poll actually uses: the
    /// budget bounds the call, and a timed-out sample is `None`.
    #[tokio::test]
    async fn an_async_sample_that_outruns_its_budget_reports_no_sample() {
        let started = std::time::Instant::now();
        let captured = capture_bounded_async("sleep", &["30"], Duration::from_millis(200)).await;
        assert_eq!(captured, None, "a timed-out sample must never yield output");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the budget must actually bound the call (took {:?})",
            started.elapsed()
        );
    }

    /// A sample that fails outright — a non-zero exit, or a program that is not
    /// there to run at all — is also "no sample", never an empty table.
    #[test]
    fn a_failed_sample_reports_no_sample() {
        assert_eq!(
            capture_bounded("false", &[], PS_SAMPLE_BUDGET),
            None,
            "a non-zero exit must not be read as an empty process table"
        );
        assert_eq!(
            capture_bounded(
                "dot-agent-deck-no-such-program-exists",
                &[],
                PS_SAMPLE_BUDGET
            ),
            None,
            "a spawn failure must not be read as an empty process table"
        );
    }

    /// The async twin of the failure contract.
    #[tokio::test]
    async fn a_failed_async_sample_reports_no_sample() {
        assert_eq!(
            capture_bounded_async("false", &[], PS_SAMPLE_BUDGET).await,
            None
        );
        assert_eq!(
            capture_bounded_async(
                "dot-agent-deck-no-such-program-exists",
                &[],
                PS_SAMPLE_BUDGET
            )
            .await,
            None
        );
    }

    /// The healthy path is unchanged by the bounding: a process that finishes
    /// inside its budget still hands back its whole stdout, on both forms.
    #[tokio::test]
    async fn a_healthy_sample_still_returns_its_whole_output() {
        assert_eq!(
            capture_bounded("echo", &["hello"], PS_SAMPLE_BUDGET).as_deref(),
            Some("hello\n")
        );
        assert_eq!(
            capture_bounded_async("echo", &["hello"], PS_SAMPLE_BUDGET)
                .await
                .as_deref(),
            Some("hello\n")
        );
    }

    /// Fork issue #30, and the defect Greptile caught in this change's first
    /// draft: the `getsid` pass must run **between** the two captures, not after
    /// both of them. Capturing twice and only then reading session ids leaves
    /// the entire window between the second capture and `getsid` unprotected —
    /// a pid recycled there is vouched for by a confirmation that was taken
    /// before the answer it is supposed to validate even existed.
    ///
    /// Drives the sequence with an injected capture and resolver that record
    /// when they are called, and asserts the order is capture → getsid →
    /// capture. Both sampling forms are pinned, because the sequence is written
    /// out once per form.
    #[tokio::test]
    async fn the_getsid_pass_runs_between_the_two_captures() {
        use std::cell::RefCell;

        const FIRST: &str = "100     1 ttys014  claude --model opus\n";
        // The confirmation reports a different parent for pid 100, so applying
        // it must invalidate the session id — proving it was applied *after*
        // the resolver ran, not merely fetched.
        const CONFIRM: &str = "100   999 ttys014  claude --model opus\n";

        let expected = ["capture", "getsid", "capture"];

        let log = RefCell::new(Vec::<&'static str>::new());
        let resolver = |_: i32| {
            log.borrow_mut().push("getsid");
            100
        };
        let mut captures = 0;
        let sync_rows = sample_table(
            || {
                log.borrow_mut().push("capture");
                captures += 1;
                Some(if captures == 1 { FIRST } else { CONFIRM }.to_string())
            },
            resolver,
        )
        .expect("a one-row sample must produce a table");
        assert_eq!(log.borrow().as_slice(), &expected);
        assert_eq!(
            sync_rows[0].session_id, -1,
            "the confirmation must be applied to the session ids the resolver produced"
        );

        let log = RefCell::new(Vec::<&'static str>::new());
        let resolver = |_: i32| {
            log.borrow_mut().push("getsid");
            100
        };
        let mut captures = 0;
        let async_rows = sample_table_async(
            || {
                log.borrow_mut().push("capture");
                captures += 1;
                std::future::ready(Some(
                    if captures == 1 { FIRST } else { CONFIRM }.to_string(),
                ))
            },
            resolver,
        )
        .await
        .expect("a one-row sample must produce a table");
        assert_eq!(log.borrow().as_slice(), &expected);
        assert_eq!(async_rows[0].session_id, -1);
    }

    /// End to end on a real machine: both sampling forms still enumerate the
    /// live process table (the `001`–`004` behaviour), and the running test
    /// process — which by construction cannot be recycled while it is asking —
    /// survives the fork-issue-#30 confirmation pass with a readable session id.
    #[tokio::test]
    async fn both_sampling_forms_still_enumerate_the_live_process_table() {
        let own_pid = std::process::id() as i32;
        for (label, table) in [
            ("sync", process_table()),
            ("async", process_table_async().await),
        ] {
            let table = table.unwrap_or_else(|| panic!("{label} sample must enumerate on unix"));
            let own = table
                .iter()
                .find(|row| row.pid == own_pid)
                .unwrap_or_else(|| panic!("{label} sample must contain the caller's own pid"));
            assert!(
                own.session_id > 0,
                "the caller's own row cannot have been recycled mid-sample, so its session id \
                 must survive the confirmation pass ({label}): {own:?}"
            );
        }
    }

    #[test]
    fn checked_signal_pid_rejects_overflowing_u32_pid() {
        // Above i32::MAX the `as i32` cast would wrap negative → a `kill(-pgid)`
        // process-group wildcard. The guard must reject with `InvalidInput`.
        assert_eq!(
            checked_signal_pid(i32::MAX as u32 + 1).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert_eq!(
            checked_signal_pid(u32::MAX).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
    }
}
