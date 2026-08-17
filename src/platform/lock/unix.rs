//! Unix spawn lock: exclusive `flock(2)` on a lock file. Behavior-preserving
//! lift of the former `daemon_attach::{acquire_spawn_lock, SpawnLock}`.

use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::time::{Duration, Instant};

/// RAII guard for the `spawn.lock` flock. Drop releases the lock by closing the
/// file descriptor (and explicitly `LOCK_UN`'ing for clarity).
pub struct SpawnLock {
    file: std::fs::File,
}

impl Drop for SpawnLock {
    fn drop(&mut self) {
        // SAFETY: fd is valid for the lifetime of self.file; flock(LOCK_UN)
        // on a held lock is safe and reverses the LOCK_EX taken in
        // acquire_spawn_lock. Closing the file (next, via File::Drop) would
        // also release the lock — the explicit unlock just keeps the
        // semantics readable.
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

/// Open or create `path` and acquire an exclusive `flock(2)` on it, blocking
/// the CALLING thread until granted. This is the core primitive; the async
/// [`acquire_spawn_lock`] below just runs it on `spawn_blocking`.
pub fn acquire_spawn_lock_sync(path: &Path) -> std::io::Result<SpawnLock> {
    use std::os::unix::fs::OpenOptionsExt;

    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    // SAFETY: passing a valid fd and a valid op constant; flock(2) does
    // not retain any reference to the address space, so the unsafe is a
    // formality of the libc binding.
    let res = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if res != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(SpawnLock { file })
}

/// Open or create `path` and acquire an exclusive `flock(2)` on it. flock is
/// blocking, so we run the syscall on `spawn_blocking` to avoid stalling other
/// tasks scheduled on the same tokio worker when contention is real (i.e.,
/// another caller on this host is mid-spawn).
///
/// Reused by both the lazy-spawn machinery and the daemon's own
/// `run_daemon_with` to serialize its probe-remove-bind sequence against
/// concurrent `daemon serve` starts (PRD #93 auditor BLOCKER — two daemons
/// probing a stale socket would otherwise both `remove_file` and both `bind`,
/// clobbering each other's clients).
pub async fn acquire_spawn_lock(path: &Path) -> std::io::Result<SpawnLock> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || acquire_spawn_lock_sync(&path))
        .await
        .map_err(std::io::Error::other)?
}

/// Asynchronous, bounded counterpart to [`acquire_spawn_lock`] (fork #282
/// audit B1) — see that name's doc comment in `platform/lock/mod.rs` for why
/// this exists instead of `tokio::time::timeout(_, acquire_spawn_lock(path))`.
///
/// Runs the ALREADY-bounded [`acquire_spawn_lock_sync_bounded`] inside
/// `spawn_blocking`, rather than the unbounded [`acquire_spawn_lock_sync`]:
/// the sync primitive itself returns on expiry (it polls `LOCK_EX | LOCK_NB`
/// against a deadline), so the `spawn_blocking` task completes and its
/// thread is released back to the pool on timeout — not left parked in a
/// blocking `flock(LOCK_EX)` forever.
pub async fn acquire_spawn_lock_bounded(
    path: &Path,
    timeout: Duration,
) -> std::io::Result<SpawnLock> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || acquire_spawn_lock_sync_bounded(&path, timeout))
        .await
        .map_err(std::io::Error::other)?
}

/// Bounded counterpart to [`acquire_spawn_lock_sync`] (fork #331 audit S1):
/// open or create `path` and acquire an exclusive `flock(2)` on it, refusing
/// with an `ErrorKind::TimedOut` error rather than blocking the calling
/// thread indefinitely if the lock cannot be granted within `timeout`. For
/// [`crate::issue_dispatch_run::create_worktree_sync`] (the sync TUI hot
/// path, which cannot `.await` the unbounded primitive above and runs
/// directly on the render/event loop), an unbounded wait here would reopen
/// the freeze `WORKTREE_GIT_TIMEOUT` exists to prevent on the `git` calls
/// either side of it.
///
/// `flock(2)` has no wait-with-timeout form, so this polls `LOCK_EX |
/// LOCK_NB` against a deadline rather than blocking on a single syscall —
/// the standard shape for a bounded flock. Under real contention the loser
/// typically crosses only one or two polls before the holder's own bounded
/// `git` call finishes and releases.
pub fn acquire_spawn_lock_sync_bounded(
    path: &Path,
    timeout: Duration,
) -> std::io::Result<SpawnLock> {
    use std::os::unix::fs::OpenOptionsExt;

    const POLL_INTERVAL: Duration = Duration::from_millis(20);

    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;

    // `Instant + Duration` panics on overflow (fork #331 audit F4) -- the
    // mirror-image of the Windows twin's `u32::MAX` sentinel trap. No
    // reachable caller passes a `timeout` anywhere near that large today,
    // but saturating to a distant-but-valid deadline keeps this fail-closed
    // (a bounded, if generous, wait) instead of panicking the caller.
    const FAR_FUTURE: Duration = Duration::from_secs(60 * 60 * 24 * 365 * 50);
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(|| Instant::now() + FAR_FUTURE);
    loop {
        // SAFETY: same as `acquire_spawn_lock_sync` — valid fd, valid op
        // constant; flock(2) does not retain any reference to the address
        // space.
        let res = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if res == 0 {
            return Ok(SpawnLock { file });
        }
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::WouldBlock {
            return Err(err);
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "timed out after {timeout:?} waiting for the lock on {}",
                    path.display()
                ),
            ));
        }
        std::thread::sleep(POLL_INTERVAL.min(deadline - now));
    }
}
