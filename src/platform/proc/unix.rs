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

/// Fork issue #133 — spawn `command` as the leader of a brand-new process
/// group, so a subsequent `killpg` targeting the returned pid reaches the
/// whole tree it spawns (including hook grandchildren such as
/// `post-checkout`) rather than a group the child never led.
///
/// A `portable-pty`-spawned agent already gets this for free — the pty
/// allocation itself calls `setsid` — but a plain `std::process::Command`
/// child (e.g. the `git` invocation behind `run_status_sync`) inherits the
/// deck's own process group, so `killpg` on its pid would target the
/// **deck's** group, not the child's. This makes the child a session/process-
/// group leader before `exec` runs, which is what makes `getpgid(pid) == pid`
/// hold afterward and the existing `killpg` teardown paths address the right
/// group.
///
/// `command` must be configured (args, env, stdio) but not yet spawned.
///
/// # Safety / panics
///
/// `pre_exec`'s closure runs in the forked child, strictly between `fork` and
/// `exec` — at that point only the calling thread exists and only
/// async-signal-safe operations are defined behavior, and unwinding a panic
/// across the fork boundary is undefined behavior. `setsid(2)` is
/// async-signal-safe, and a hypothetical failure is reported back to the
/// parent as an `io::Error` (which `Command::spawn` propagates) rather than
/// panicking.
pub fn spawn_in_new_process_group(
    command: &mut std::process::Command,
) -> std::io::Result<(std::process::Child, AgentProcessGroup)> {
    use std::os::unix::process::CommandExt;

    // SAFETY: the closure only calls `setsid(2)` (async-signal-safe) and maps
    // its one failure mode to an `io::Error` instead of panicking — see the
    // doc comment above for why both properties are required here.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn()?;
    let group = AgentProcessGroup::adopt(Some(child.id()));
    Ok((child, group))
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
        // fallback below uses `portable_pty::Child::kill`, whose behavior
        // depends on the concrete `Child` impl behind the trait object: for
        // `StdChild` — the only production user of these paths — it is
        // `std::process::Child::kill()`, a guarded SIGKILL (std
        // short-circuits to `Ok(())` without signaling if the child has
        // already been waited on). portable-pty's own PTY `Child` impl is
        // different: a raw, unguarded `libc::kill(pid, SIGHUP)` that
        // deliberately bypasses std's reaped-pid guard. Either way this
        // fallback is limited to the direct child (no process-group
        // semantics, so descendants leak) and carries no recycled-pid guard
        // of its own — the forcing entry points' precondition that the
        // caller must not have reaped `child` is what actually protects it
        // here. The caller's subsequent `child.wait()` is unbounded — that's
        // acceptable for the same "this branch is practically unreachable"
        // reason.
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
    group: &AgentProcessGroup,
) {
    force_kill_child_group(child, group);
    let _ = child.wait();
}

/// The SIGNAL half of [`force_kill_child_and_wait`]: `killpg(SIGKILL)` the
/// child's whole process group and return **without reaping**.
///
/// Issue #581 — the two halves are separable because they starve each other
/// when a caller holds several agents. The `wait()` above is unbounded (see the
/// note in [`signal_child_pgroup_or_fallback`]): a child wedged in
/// uninterruptible kernel I/O does not die on SIGKILL until that I/O completes,
/// so a loop that signals-then-waits per agent parks in the first wedged
/// agent's `wait()` and never signals the agents behind it. Splitting the
/// signal out lets such a caller deliver every kill first and reap afterwards —
/// see `AgentPtyRegistry::force_kill_and_reap_all`, the only caller.
///
/// `_group` is unused on Unix for the same reason as in
/// [`force_kill_child_and_wait`].
pub fn force_kill_child_group(
    child: &mut Box<dyn portable_pty::Child + Send + Sync>,
    _group: &AgentProcessGroup,
) {
    signal_child_pgroup_or_fallback(child, libc::SIGKILL, "force-kill");
}

/// SIGTERM-then-SIGKILL escalation used by the single-pane Ctrl+W path. Sends
/// `SIGTERM` to the child's process group, polls `try_wait` until the child
/// exits or `grace` elapses, then sends `SIGKILL` as the backstop and reaps the
/// child.
///
/// Historical gap (fork issue #133 P1, unfixed here on purpose — see
/// [`terminate_child_with_grace_and_wait_forcing_group_backstop`]): phase 3 is
/// skipped whenever `try_wait` shows the *direct* child already exited during
/// the grace window, which leaves a same-group descendant that traps or
/// ignores SIGTERM alive past this call. Left as-is for this path
/// deliberately — whether that gap matters for an agent pane is a separate
/// question with its own risk, not one to resolve as a side effect of the
/// git-worktree fix.
///
/// `_group` is unused on Unix (see [`force_kill_child_and_wait`]).
pub fn terminate_child_with_grace_and_wait(
    child: &mut Box<dyn portable_pty::Child + Send + Sync>,
    grace: Duration,
    _group: &AgentProcessGroup,
) {
    terminate_child_with_grace_and_wait_impl(child, grace, false);
}

/// [`terminate_child_with_grace_and_wait`], except phase 3's SIGKILL is
/// **always** delivered to the whole process group, even when the direct
/// child already exited during the grace window.
///
/// Fork issue #133 P1: the non-forcing function above assumes an exited
/// direct child means the group is already empty. That assumption fails
/// whenever the direct child is a cooperative process-group leader (`git`
/// exits promptly on SIGTERM) but a same-group descendant it forked is not
/// (a `post-checkout` hook that traps or ignores SIGTERM) — the leader gets
/// reaped, escalation stops, and the hook survives past the caller's bound.
/// That is the exact orphan fork issue #133 exists to kill, so the worktree
/// timeout path
/// ([`crate::issue_dispatch_run::run_status_sync`]) uses this variant
/// instead. `killpg` returning `ESRCH` (nothing left to signal) is already
/// treated as benign by `signal_child_pgroup_or_fallback`, so forcing the
/// call when the group is genuinely empty costs nothing.
///
/// Fork issue #143: unlike the non-forcing function, this variant never
/// polls `try_wait` during the grace window — see
/// [`terminate_child_with_grace_and_wait_impl`]'s forcing branch for why
/// that poll is exactly what let phase 3 derive a pgid from an
/// already-reaped, potentially-recycled pid.
///
/// **Precondition:** the caller must not already have reaped `child` (no
/// prior `try_wait`/`wait` that returned `Ok(Some(_))`). Phase 3 derives the
/// process-group id it signals from `child.process_id()`, which after a reap
/// names a released, recyclable pid — calling this on an already-reaped
/// child reintroduces fork #143 through the front door, and the natural way
/// to reach for this function is exactly that: a caller that polled
/// `try_wait`, saw the leader exit, and chose this variant specifically
/// because it still forces the group kill for a surviving descendant.
///
/// The single-pane Ctrl+W / respawn path keeps calling the non-forcing
/// function above unchanged — see its doc comment for why.
pub fn terminate_child_with_grace_and_wait_forcing_group_backstop(
    child: &mut Box<dyn portable_pty::Child + Send + Sync>,
    grace: Duration,
    _group: &AgentProcessGroup,
) {
    terminate_child_with_grace_and_wait_impl(child, grace, true);
}

/// Fork issue #136: like
/// [`terminate_child_with_grace_and_wait_forcing_group_backstop`], except the
/// final reap after phase 3's SIGKILL runs on a short-lived **detached**
/// thread instead of blocking the caller.
///
/// The forcing function above is correct about *what* to kill but not about
/// *how long the call may take*: its final `child.wait()` is unbounded, and a
/// process wedged in uninterruptible kernel I/O (a stuck NFS mount is the
/// classic case) does not die on SIGKILL until that I/O completes, so `wait()`
/// blocks until it does. The only caller of this function —
/// [`crate::issue_dispatch_run::run_status_sync`]'s timeout escalation — runs
/// on the TUI's synchronous render/event loop, which is exactly the freeze
/// [`crate::issue_dispatch_run::WORKTREE_GIT_TIMEOUT`] exists to prevent, so
/// that unbounded wait can re-introduce the freeze through a narrower door.
/// Moving only the reap off the loop closes it without changing the SIGTERM →
/// grace → SIGKILL ordering above.
///
/// Takes `child` by value, not `&mut`, because the whole point is to move it
/// into the detached thread that performs the final `wait()` — a borrow
/// cannot outlive this call. The agent Ctrl+W / respawn paths are unaffected:
/// they keep calling [`terminate_child_with_grace_and_wait`], which this
/// function does not touch or share an implementation with.
///
/// **Precondition:** the caller must not already have reaped `child` (no
/// prior `try_wait`/`wait` that returned `Ok(Some(_))`). Phase 3 derives the
/// process-group id it signals from `child.process_id()`, which after a reap
/// names a released, recyclable pid — calling this on an already-reaped
/// child reintroduces fork #143 through the front door, and the natural way
/// to reach for this function is exactly that: a caller that polled
/// `try_wait`, saw the leader exit, and chose this variant specifically
/// because it still forces the group kill for a surviving descendant.
pub fn terminate_child_with_grace_and_detached_reap_forcing_group_backstop(
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    grace: Duration,
    _group: &AgentProcessGroup,
) {
    // Phase 1: SIGTERM the process group.
    signal_child_pgroup_or_fallback(&mut child, libc::SIGTERM, "worktree-timeout-sigterm");

    // Phase 2: fork issue #143 — wait out the grace window WITHOUT polling
    // `try_wait`. `try_wait`'s underlying `waitpid(WNOHANG)` reaps the direct
    // child as a side effect of merely checking whether it exited, and a
    // reaped pid is released back to the kernel for the next unrelated
    // `setsid`'d process (any deck-spawned agent pane included) to receive —
    // which phase 3 would then `killpg` by mistake. Leaving the child
    // unreaped for the whole window is what keeps phase 3's target safe: an
    // exited-but-unreaped child sits as a zombie, and what actually stays
    // reserved is the zombie's **process group**, not merely its pid — POSIX
    // keeps a process group alive as long as any member is within its
    // process lifetime, and a zombie still is. This is sound here only
    // because `spawn_in_new_process_group`'s `setsid` makes this child's
    // pgid equal its own pid, so "the pid can't be recycled" and "the pgid
    // can't be reallocated" happen to describe the same guarantee — a future
    // change that breaks pgid == pid would need to re-derive this reasoning
    // against the pgid directly.
    //
    // This is not free: skipping the poll means this call always blocks the
    // calling thread for the full `grace` (200ms on the only production
    // caller, `run_status_sync`), rather than returning as soon as an exited
    // `git` was observed. That cost only lands after
    // `WORKTREE_GIT_TIMEOUT`/`WORKTREE_CLEANUP_TIMEOUT` has already elapsed
    // with the render loop blocked, and the worst case actually improves:
    // the old poll-then-sleep loop could overshoot the window by nearly a
    // full 50ms cadence, where this is a deterministic bound.
    std::thread::sleep(grace);

    // Phase 3: SIGKILL backstop, always sent to the whole group — forcing,
    // like the synchronous-reap variant above, so an already-exited direct
    // child does not skip a same-group descendant it forked (fork #133). A
    // `killpg` that reports ESRCH (group already empty) is already treated as
    // benign by `signal_child_pgroup_or_fallback`; delivering the signal to a
    // group whose leader is a zombie but whose descendants are still alive
    // reaches those descendants normally.
    signal_child_pgroup_or_fallback(&mut child, libc::SIGKILL, "worktree-timeout-sigkill");

    // Hand the child to a short-lived detached thread for the final blocking
    // reap, bounded and guaranteed-reaping — see
    // [`super::detach_reap_or_fallback_sync`]'s doc comment for the cap
    // (fork issue #136 finding 2) and the never-drop-the-child guarantee
    // (finding 1) this shares with the Windows backend. Unconditional now
    // (fork #143): phase 2 no longer learns whether the child already
    // exited, so there is no cheap early-return left to take — `wait()`
    // still returns promptly for an already-zombied child.
    super::detach_reap_or_fallback_sync(
        child,
        "worktree-git-reap",
        "worktree-timeout-sigkill-reap",
    );
}

fn terminate_child_with_grace_and_wait_impl(
    child: &mut Box<dyn portable_pty::Child + Send + Sync>,
    grace: Duration,
    force_group_backstop: bool,
) {
    // Phase 1: SIGTERM the process group.
    signal_child_pgroup_or_fallback(child, libc::SIGTERM, "graceful-close-sigterm");

    if force_group_backstop {
        // Fork issue #143: a forcing caller always reaches phase 3 below
        // regardless of whether the direct child (the group leader) exited
        // during the grace window — that is the whole point of "forcing"
        // (fork #133). Reaching phase 3 unconditionally means it must never
        // observe the direct child's exit via `try_wait`/`wait` first:
        // `try_wait`'s underlying `waitpid(WNOHANG)` reaps as a side effect
        // of merely checking, and a reaped pid is released back to the
        // kernel for the next unrelated `setsid`'d process to receive —
        // which the group signal below would then hit by mistake. So this
        // branch never polls: it sleeps out the grace window untouched (an
        // exited-but-unreaped child sits as a zombie, and what actually
        // stays reserved is the zombie's **process group**, not merely its
        // pid — a zombie is still within its process lifetime, so POSIX
        // keeps the group alive; `spawn_in_new_process_group`'s `setsid`
        // makes this child's pgid equal its own pid, which is the only
        // reason "pid can't be recycled" and "pgid can't be reallocated"
        // coincide here), sends the SIGKILL, and only then performs the
        // single reaping wait.
        std::thread::sleep(grace);
        signal_child_pgroup_or_fallback(child, libc::SIGKILL, "graceful-close-sigkill");
        let _ = child.wait();
        return;
    }

    // Phase 2 (non-forcing only): poll `try_wait` until the child exits or
    // the grace elapses. Polling avoids the obvious "sleep for grace then
    // SIGKILL" alternative — a child that exits promptly after SIGTERM
    // doesn't have to wait around for the deadline. 50 ms cadence is small
    // enough to feel responsive and large enough to keep CPU cost negligible
    // (~60 polls over 3 s). Safe to reap early here specifically because the
    // non-forcing path below skips phase 3 entirely once the direct child is
    // reaped, so it never derives a pgid from the now-stale pid (fork #143 —
    // contrast with the forcing branch above, which cannot make that trade).
    let deadline = std::time::Instant::now() + grace;
    let mut child_reaped = false;
    while std::time::Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => {
                child_reaped = true;
                break;
            }
            Ok(None) => {}
            Err(_) => break,
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // Phase 3 (non-forcing only): SIGKILL backstop, skipped once the direct
    // child is already gone — original behavior; fork #133's gap here is
    // intentional, see [`terminate_child_with_grace_and_wait`]'s doc.
    if !child_reaped {
        signal_child_pgroup_or_fallback(child, libc::SIGKILL, "graceful-close-sigkill");
        // A child already reaped above has nothing left to `wait` for — and
        // `portable_pty::Child::wait` is not guaranteed safe to call twice.
        let _ = child.wait();
    }
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
// Process-table sample (PRD #386 M1; hardened by fork issue #30 and Greptile's
// P2 on upstream PR #390 — the async form runs off the Tokio worker so a
// wedged `ps` cannot stall it, and every row's `getsid` answer is distrusted
// unless a second sample confirms the pid was not recycled between the two).
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

/// The one `ps` invocation both samplers share, so the sync and async paths can
/// never drift into asking `ps` for different columns.
///
/// `-w -w` disables `ps`'s width truncation on both macOS and Linux, so the argv
/// column survives whole; the trailing `=` on every `-o` field suppresses the
/// header line entirely.
const PS_TABLE_ARGS: [&str; 5] = ["-A", "-w", "-w", "-o", "pid=,ppid=,tty=,args="];

/// The sampling sequence itself, over an injected `capture` (called twice) —
/// the shared body of [`process_table`] and [`process_table_async`].
///
/// **The order of the three steps is the entire fix for fork issue #30 and is
/// not incidental**: the `getsid` pass (inside [`super::scan::parse_ps_table`])
/// must run strictly *between* the two captures. Capturing both samples first
/// and only then reading session ids leaves the whole reuse window between the
/// second capture and `getsid` unprotected — a pid can exit and be recycled
/// there, and the confirmation then vouches for a `getsid` answer that
/// describes the replacement process. (Exactly that inversion shipped in this
/// change's first draft and was caught in review.) `capture` is injected
/// rather than called directly so `the_getsid_pass_runs_between_the_two_captures`
/// can observe the sequence and fail if it is ever reordered again.
///
/// See [`super::scan::invalidate_unconfirmed_session_ids`] for the invariant
/// the confirmation enforces and for the residual window that remains. The cost
/// is a second `ps` per sample, which is the shape PRD #386's M5 measurement
/// should be taken against.
///
/// `session_id_of` is injected (production callers pass [`getsid_or_negative`])
/// so a test can control what session id each pid resolves to without shelling
/// out to a real `getsid(2)`.
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
/// (PRD #386 M1, Route A), or `None` if `ps` could not be run or produced
/// nothing parseable.
///
/// The table is not atomic: it is captured at one instant and the `getsid(2)`
/// pass happens at a later one, so a pid that exits in between can be recycled
/// and `getsid` then answers about a *different* process than the row
/// describes (fork issue #30). Every row's identity is therefore confirmed
/// with a SECOND `ps` sample taken right after the first — see [`sample_table`]
/// for why the ordering matters and
/// [`super::scan::invalidate_unconfirmed_session_ids`] for the exact invariant.
///
/// **Synchronous and unbounded — never call this from an async task** (issue
/// #429). It blocks the calling thread for the whole run — two `ps`
/// invocations back to back — measured at ~49 ms each on an idle 16-core Linux
/// box with ~620 processes, and forever if `ps` wedges in D-state on a stuck
/// filesystem. [`process_table_async`] is the variant for a Tokio context.
/// This one remains for synchronous callers and tests.
///
/// Route B (native enumeration — `/proc/<pid>/{stat,cmdline}` on Linux,
/// `sysctl(KERN_PROC_ALL)` on macOS) stays open behind PRD #386's M5
/// measurement; it removes the subprocess at the cost of two platform-specific
/// implementations, and is only worth taking if the measurement says so.
pub fn process_table() -> Option<Vec<super::ProcessInfo>> {
    sample_table(
        || {
            let output = std::process::Command::new("ps")
                .args(PS_TABLE_ARGS)
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output()
                .ok()?;
            if !output.status.success() {
                return None;
            }
            Some(String::from_utf8_lossy(&output.stdout).into_owned())
        },
        getsid_or_negative,
    )
}

/// [`process_table`] for an async caller: the same two-sample sequence, but
/// awaited instead of blocked on (issue #429).
///
/// Two properties the synchronous version cannot offer, both load-bearing for
/// the daemon's 2 Hz shell-activity poll:
///
/// - **It does not occupy a Tokio worker thread.** The wait for `ps` to exit is
///   a real `await` on the runtime's child reaper, so the worker goes back to
///   the queue for the ~49 ms each sample takes instead of sitting in
///   `waitpid`. That cost, twice per tick for the confirmation pass, is what
///   previously stalled hook ingestion, client requests and daemon shutdown
///   behind this signal. `spawn_blocking` would only relocate that stall to
///   the blocking pool; worse, `tokio::time::timeout` around a
///   `spawn_blocking` handle does not cancel the thread, so a
///   permanently-wedged `ps` at 2 Hz would leak one pool thread per tick until
///   the 512-thread cap. Awaiting an async child is what actually fixes it.
/// - **It is cancel-safe, so a timeout can genuinely bound it.** `kill_on_drop`
///   means dropping this future — which is exactly what
///   [`tokio::time::timeout`] does on expiry — kills whichever `ps` child is
///   still running and leaves it to the runtime's orphan reaper rather than
///   abandoning it.
///
/// **Callers MUST wrap this in a timeout**; it has no internal deadline, and a
/// `ps` wedged in D-state never returns. The deadline lives at the call site
/// (see `run_shell_activity_monitor`) because the *interpretation* of a blown
/// deadline is the caller's: a timed-out sample means "no opinion", never "not
/// busy".
pub async fn process_table_async() -> Option<Vec<super::ProcessInfo>> {
    sample_table_async(
        || async {
            // `output()` forces `stdout`/`stderr` to pipes (tokio, unlike
            // `std`, leaves `stdin` alone — hence the explicit null), and
            // `wait_with_output` drains both concurrently, so the
            // captured-and-discarded stderr cannot deadlock.
            let output = tokio::process::Command::new("ps")
                .args(PS_TABLE_ARGS)
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true)
                .output()
                .await
                .ok()?;
            if !output.status.success() {
                return None;
            }
            Some(String::from_utf8_lossy(&output.stdout).into_owned())
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
