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

/// Fork issue #163 rework: observe whether the process named by `pid` has
/// exited, **without reaping it**. `try_wait`'s underlying `waitpid(WNOHANG)`
/// reaps as a side effect of merely checking, which releases the pid back to
/// the kernel for recycling before the caller gets a chance to signal its
/// group — the exact hazard this issue is about. `libc::waitid` with
/// `WNOWAIT` reports the same exit `try_wait` would, but leaves the process a
/// zombie, which keeps its process group reserved for as long as the caller
/// needs it (POSIX keeps a process group alive while any member is within
/// its process lifetime, and a zombie still is — see
/// `signal_child_pgroup_or_fallback` and `spawn_in_new_process_group`'s
/// `setsid`, which is what makes this pid equal its own pgid).
///
/// Returns `Ok(true)` once `pid` has exited, `Ok(false)` while it is still
/// running. A `waitid` failure is returned as `Err` rather than guessed at;
/// the caller distinguishes `ECHILD` (meaning `pid` was already reaped by
/// something else) from any other error, since only `ECHILD` legitimately
/// means "stop polling" — see this function's precondition below.
///
/// # Precondition: the caller must be the sole reaper of `pid`
///
/// The pid-recycling safety this function exists to provide (fork #163)
/// depends entirely on **nothing else in this process reaping `pid` between
/// this peek and the caller's own unconditional signal-and-reap step**. As
/// of this writing that holds: there is no wildcard `waitpid(-1, …)`
/// anywhere in the process, `SIGCHLD` is never set to `SIG_IGN`, no
/// exit-watcher thread calls `wait()` on an agent's child, and tokio's
/// process driver only reaps children it owns via per-pid `try_wait`, never
/// a wildcard wait. If a future change introduces any of those — a
/// `waitpid(-1)` reaper, a `SIG_IGN` on `SIGCHLD`, or a per-agent wait
/// thread — this function's `ECHILD` arm silently stops being "already
/// reaped, safe to signal anyway" and starts being exactly the recycled-pid
/// hazard fork #163 was about, with no test or comment left to object.
pub fn peek_child_exited_without_reaping(pid: u32) -> std::io::Result<bool> {
    // SAFETY: `waitid` is async-signal-safe. `info` is zeroed before the
    // call because a successful `WNOHANG` call that finds no reportable
    // state change is documented to leave `si_pid` untouched rather than
    // zeroing it itself (a known kernel quirk, not specific to this
    // codebase) — zeroing here first is what makes `si_pid() == pid` a
    // reliable "did anything happen" signal instead of stale stack memory.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: `&mut info` is a valid, uniquely-owned `siginfo_t` for the
    // duration of this call; `WNOWAIT` means the kernel only reports state,
    // it does not reap, so this cannot race any other reaper of `pid`.
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `info` was just populated by the successful `waitid` call
    // above (or left at its zeroed default when nothing changed), and
    // `si_pid` is the field `waitid`'s `SIGCHLD`-shaped report always uses.
    let observed_pid = unsafe { info.si_pid() };
    Ok(observed_pid == pid as libc::pid_t)
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
pub const PS_SAMPLE_BUDGET: Duration = Duration::from_secs(2);

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
///
/// Fork issue #160: returns [`super::ProcessTableOutcome::Failed`] rather than
/// a bare `None` when the sample does not produce a table — Unix always
/// *attempts* the sample, so it is never [`super::ProcessTableOutcome::Unsupported`]
/// here (that variant is the Windows backend's alone). The underlying capture
/// already `tracing::warn!`s the proximate cause (budget exceeded, non-zero
/// exit, spawn failure); this return value is what lets the daemon's poll
/// itself log loudly at the point the signal actually degrades, instead of
/// only in a lower-level log line with no "shell activity" context attached.
pub async fn process_table_async() -> Result<Vec<super::ProcessInfo>, super::ProcessTableOutcome> {
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
    .ok_or(super::ProcessTableOutcome::Failed)
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
    use spec::spec;

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

    /// Fork issue #212: the capture bounds how long a sample may take
    /// (`PS_SAMPLE_BUDGET`) but nothing bounds how many BYTES it may read —
    /// measured during PR #206's security audit at a 300,160-byte row for a
    /// single process with a 300 KB argv, entirely untruncated. A capture
    /// that exceeds a size cap must report "no sample" (`None`) — the
    /// identical shape a time-budget overrun already reports (see
    /// `a_sample_that_outruns_its_budget_reports_no_sample` and its async
    /// twin above) — so `process_table`/`process_table_async`'s existing
    /// `None` → [`super::super::ProcessTableOutcome::Failed`] mapping picks
    /// this up with no new code path, and the daemon's existing fail-safe
    /// (`last_known` untouched, nothing emitted — see
    /// `run_shell_activity_monitor` in `daemon.rs`) handles it exactly like a
    /// timed-out sample, unchanged.
    ///
    /// Scenario: `head -c 400000 /dev/zero` finishes in well under the 2s
    /// time budget, so a pure time bound cannot catch it — instead it
    /// produces a 400,000-byte capture, which today is returned whole on both
    /// the sync and async forms.
    #[spec("status/shell-activity/010")]
    #[test]
    fn shell_activity_010_a_capture_exceeding_its_size_cap_reports_no_sample() {
        // `#[test]` + `block_on` rather than `#[tokio::test]`: `cargo xtask
        // linkage-check`'s Decision-17 name scan looks for the first line
        // starting with `fn`, which an `async fn` signature does not — see
        // `shell_activity_008` in `daemon.rs` for the same wrapper shape.
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build size-cap runtime")
            .block_on(
                shell_activity_010_a_capture_exceeding_its_size_cap_reports_no_sample_inner(),
            );
    }

    async fn shell_activity_010_a_capture_exceeding_its_size_cap_reports_no_sample_inner() {
        let oversized_args = ["-c", "400000", "/dev/zero"];
        assert_eq!(
            capture_bounded("head", &oversized_args, PS_SAMPLE_BUDGET),
            None,
            "a capture whose output exceeds the size cap must never yield output, even though \
             it finished well inside the time budget"
        );
        assert_eq!(
            capture_bounded_async("head", &oversized_args, PS_SAMPLE_BUDGET).await,
            None,
            "same contract on the async form the daemon's poll actually uses"
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
    ///
    /// Fork issue #210: unlike the sampling-contract tests above, this test had
    /// no retry at all — a single `None`/`Err` from one exhausted `ps` sample
    /// panicked outright with a message ("sample must enumerate on unix") that
    /// reads as a broken sampler, not as a timeout. `WINDOW` retries each form
    /// across a window derived from `PS_SAMPLE_BUDGET` (a multiple of it, so
    /// the two cannot drift apart) before treating the sample as genuinely
    /// unavailable; only once a table IS in hand does a missing own-pid row
    /// become the distinct, real-defect failure message.
    #[tokio::test]
    async fn both_sampling_forms_still_enumerate_the_live_process_table() {
        const WINDOW: Duration = Duration::from_secs(PS_SAMPLE_BUDGET.as_secs() * 3);
        let own_pid = std::process::id() as i32;

        let sync_deadline = std::time::Instant::now() + WINDOW;
        let mut sync_table = process_table();
        while sync_table.is_none() && std::time::Instant::now() < sync_deadline {
            std::thread::sleep(Duration::from_millis(50));
            sync_table = process_table();
        }
        let sync_table = sync_table.unwrap_or_else(|| {
            panic!(
                "sync sample: every process_table() attempt timed out against its \
                 {PS_SAMPLE_BUDGET:?} budget across the whole {WINDOW:?} retry window — a \
                 sampler timeout under machine load, not evidence the platform cannot enumerate"
            )
        });

        let async_deadline = tokio::time::Instant::now() + WINDOW;
        // `.ok()`: this test only cares that a live machine enumerates, not
        // about the `Failed`/`Unsupported` distinction fork issue #160 added
        // to the async form's error type.
        let mut async_table = process_table_async().await.ok();
        while async_table.is_none() && tokio::time::Instant::now() < async_deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
            async_table = process_table_async().await.ok();
        }
        let async_table = async_table.unwrap_or_else(|| {
            panic!(
                "async sample: every process_table_async() attempt timed out against its \
                 {PS_SAMPLE_BUDGET:?} budget across the whole {WINDOW:?} retry window — a \
                 sampler timeout under machine load, not evidence the platform cannot enumerate"
            )
        });

        for (label, table) in [("sync", sync_table), ("async", async_table)] {
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
