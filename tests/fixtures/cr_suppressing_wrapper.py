#!/usr/bin/env python3
"""Test-only PTY relay for orchestration/seed/015's suppressed-first-CR
scenario (fork#197 M4 / fork#257 M2). NOT production code: used only as the
`orchestrator_command` of a throwaway e2e orchestration config, standing in
for the real agent binary from the daemon's point of view.

Hosts the REAL agent (argv[3:], e.g. `claude --model ... --allowedTools
Bash`) behind a freshly-opened inner pty and relays every byte in both
directions completely unchanged, with ONE deliberate exception: the very
first 0x0D (carriage return) byte seen on the daemon->agent direction is
dropped rather than forwarded. That CR is the terminator of the production
`deliver_orchestrator_prompt` write (`src/ui.rs`) that carries the seed
prompt text; dropping it means the text lands in the agent's composer but
never submits, so the composer sits populated-but-unsubmitted exactly as if
the real write's confirming CR had been lost in flight. Every later byte
(including a confirmation-retry's own bare CR) passes through untouched, so
if a submission is later observed, the retry's CR is the only thing in this
run that could have produced it.

fork#257 review round (P1/P1b, reviewer + auditor): the evidence this
script produces no longer rests on the same branch that performs the drop.
Two independent cumulative counters -- total bytes actually read from the
daemon on the daemon->agent direction, and total bytes actually WRITTEN to
the agent via a write-all loop that reports genuine completion rather than
`len(data)` optimism -- are tracked across the whole run. The marker file
this script creates the instant it drops the CR records those counts, not a
flag; a mutation that stops removing the byte but leaves the
`dropped_cr`/marker-creation code path intact still creates a marker, but
the counts inside it show zero bytes missing, so the assertion built from
those counts (not from the marker's mere existence) is what catches it. See
`tests/e2e_orchestration_seed_retry_real.rs` for that assertion.

fork#257 review round (P2a/P2c): `CR_SUPPRESS_MARKER` is now REQUIRED and
validated -- absolute, parent directory must already exist -- and the relay
refuses to start (before spawning the agent) if it is missing or invalid.
The marker file itself is created with O_CREAT|O_EXCL, so a pre-existing
object at that path -- including a symlink placed there by a same-user race
-- makes marker creation fail loudly instead of silently accepting
attacker-controlled "evidence". The Rust harness supplies a fresh, private
(0700) per-run directory (`common::harness_tempdir()`) for the marker, so
no two runs -- concurrent or sequential -- can ever share a marker path.

fork#257 review round (P1b/audit, forwarding integrity): both directions
now use a write-all loop (`write_all`) that retries short writes and
EINTR/EAGAIN instead of the single best-effort `os.write` POSIX permits to
under-deliver on a PTY, and reads (`read_some`) retry transparently on
EINTR rather than folding it into a fake EOF. Any I/O failure other than a
genuine EOF raises `RelayIOError`, which is treated as a non-zero fixture
failure (after reaping the agent), not a silent clean exit.

fork#257 review round (P2b/audit, teardown): `pty.fork()` makes the agent a
session/process-group leader via its implicit `setsid()`, so cleanup now
signals the whole process GROUP (`os.killpg`), escalates to SIGKILL if the
child has not exited within a bounded timeout, and blocks on `waitpid`
before the relay itself exits -- no orphaned descendant of the real agent
can survive relay teardown.

fork#257 review round (audit, credential-execution surface): the real
agent's argv[0] is resolved to an absolute, executable path BEFORE
`pty.fork()`, so a reordered or poisoned `PATH` cannot substitute an
arbitrary program at the point this process holds real agent credentials.
The child also closes every fd above 2 before `execvp`, so nothing
non-close-on-exec inherited from the launcher reaches the credentialed
agent (P3, nice-to-have, done since it was cheap).

Usage: cr_suppressing_wrapper.py <rows> <cols> <real-agent-argv...>
"""

import fcntl
import os
import pty
import select
import shutil
import signal
import struct
import sys
import termios
import time
import tty


class RelayIOError(Exception):
    """A forwarding read/write/marker operation failed for a reason other
    than EINTR or a genuine peer-closed EOF."""


def read_some(fd):
    """Read once from `fd`, retrying transparently on EINTR. Raises
    RelayIOError on any other failure instead of silently folding it into a
    fake EOF the way a bare `except OSError: data = b""` would -- a genuine
    I/O failure becomes a loud fixture failure, not a misleadingly clean
    exit that a test could mistake for the peer having closed cleanly."""
    while True:
        try:
            return os.read(fd, 65536)
        except InterruptedError:
            continue
        except OSError as exc:
            raise RelayIOError(f"read from fd {fd} failed: {exc}") from exc


def write_all(fd, data):
    """Write every byte of `data` to `fd`, looping on short writes and
    retrying EINTR/EAGAIN -- POSIX permits a single `os.write` call to a PTY
    to write fewer bytes than requested, and the relay's byte-forwarding
    counters below are only trustworthy if a completed call really did send
    everything. Raises RelayIOError on any other failure; never returns
    having partially written silently."""
    view = memoryview(data)
    while view:
        try:
            n = os.write(fd, view)
        except InterruptedError:
            continue
        except BlockingIOError:
            select.select([], [fd], [])
            continue
        except OSError as exc:
            raise RelayIOError(f"write to fd {fd} failed: {exc}") from exc
        if n <= 0:
            raise RelayIOError(f"write to fd {fd} made no progress")
        view = view[n:]


def reap_process_tree(pid, timeout=5.0):
    """Terminate `pid`'s entire process group -- `pty.fork()`'s child is
    always a session/process-group leader via its implicit `setsid()` -- and
    block until it is actually reaped, escalating to SIGKILL if it has not
    exited within `timeout`. Never leaves an orphaned descendant of the real
    agent running past the relay's own lifetime."""
    try:
        os.killpg(pid, signal.SIGTERM)
    except (ProcessLookupError, PermissionError):
        pass
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            reaped, _status = os.waitpid(pid, os.WNOHANG)
        except ChildProcessError:
            return
        if reaped == pid:
            return
        time.sleep(0.05)
    try:
        os.killpg(pid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass
    try:
        os.waitpid(pid, 0)
    except ChildProcessError:
        pass


def resolve_marker_path():
    """Validate `CR_SUPPRESS_MARKER` BEFORE spawning the agent: required,
    must be an absolute path, and its parent directory must already exist.
    Exits the process loudly on any violation instead of letting a
    misconfigured marker silently degrade the proof it exists to provide."""
    raw = os.environ.get("CR_SUPPRESS_MARKER")
    if not raw:
        os.write(
            2,
            b"cr_suppressing_wrapper: CR_SUPPRESS_MARKER is required and "
            b"must name an absolute path\n",
        )
        sys.exit(2)
    if not os.path.isabs(raw):
        os.write(
            2,
            f"cr_suppressing_wrapper: CR_SUPPRESS_MARKER must be an "
            f"absolute path, got {raw!r}\n".encode(),
        )
        sys.exit(2)
    parent = os.path.dirname(raw)
    if not os.path.isdir(parent):
        os.write(
            2,
            f"cr_suppressing_wrapper: CR_SUPPRESS_MARKER's parent directory "
            f"does not exist: {parent!r}\n".encode(),
        )
        sys.exit(2)
    return raw


def resolve_agent_executable(argv0):
    """Resolve `argv0` to an absolute, executable path before `pty.fork()`,
    so `execvp` never re-resolves it through a `PATH` that could have
    changed underneath this process at exactly the moment it is about to
    hand off real agent credentials."""
    resolved = argv0 if os.path.isabs(argv0) else shutil.which(argv0)
    if resolved is None or not os.path.isfile(resolved) or not os.access(resolved, os.X_OK):
        os.write(
            2,
            f"cr_suppressing_wrapper: could not resolve an executable agent "
            f"binary for {argv0!r}\n".encode(),
        )
        sys.exit(2)
    return resolved


def write_marker(path, bytes_from_daemon, bytes_to_agent):
    """Create the marker with O_CREAT|O_EXCL so a pre-existing object at
    `path` -- including a symlink dropped by a same-user race -- makes
    creation fail rather than silently accept it as evidence. Content is
    the independent byte counts the Rust-side assertion derives its
    pass/fail from, not a flag set beside the drop."""
    try:
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except OSError as exc:
        raise RelayIOError(f"failed to create marker at {path!r}: {exc}") from exc
    try:
        os.write(fd, f"{bytes_from_daemon} {bytes_to_agent}\n".encode())
    except OSError as exc:
        raise RelayIOError(f"failed to write marker at {path!r}: {exc}") from exc
    finally:
        os.close(fd)


def main() -> int:
    rows = int(sys.argv[1])
    cols = int(sys.argv[2])
    real_argv = sys.argv[3:]
    if not real_argv:
        os.write(2, b"cr_suppressing_wrapper: missing real agent argv\n")
        return 2
    marker_path = resolve_marker_path()
    real_argv = [resolve_agent_executable(real_argv[0])] + real_argv[1:]

    # Our own stdin is the OUTER pty slave the daemon writes keystrokes into
    # (the daemon spawned this script attached to it exactly as it would
    # have spawned the real agent). Put it into raw/non-canonical mode
    # immediately -- mirrors what any real full-screen TUI agent does on its
    # own startup, and without it the kernel line discipline would buffer
    # input until a newline instead of delivering bytes as they land.
    tty.setraw(0)

    pid, master_fd = pty.fork()
    if pid == 0:
        # Child: becomes the real agent, attached to a brand-new pty slave
        # that `pty.fork()` already made our controlling terminal. Close
        # every fd above stderr so nothing non-close-on-exec inherited from
        # the launcher reaches the credentialed agent.
        try:
            max_fd = os.sysconf("SC_OPEN_MAX")
        except (ValueError, OSError):
            max_fd = 1024
        os.closerange(3, max_fd)
        try:
            os.execvp(real_argv[0], real_argv)
        except OSError as exc:
            os.write(2, f"cr_suppressing_wrapper: exec failed: {exc}\n".encode())
        os._exit(127)

    # Parent (the relay): size the inner pty to match the outer one before
    # any output can be produced against the wrong geometry.
    fcntl.ioctl(master_fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

    dropped_cr = False
    marker_written = False
    # Cumulative counts for the daemon->agent direction ONLY -- the
    # direction the drop happens on -- tracked independently of
    # `dropped_cr` so the marker's content reflects what was actually read
    # and actually forwarded, not what the drop branch merely believes it
    # did.
    bytes_from_daemon_total = 0
    bytes_to_agent_total = 0

    def cleanup_and_exit(code=0):
        reap_process_tree(pid)
        try:
            os.close(master_fd)
        except OSError:
            pass
        os._exit(code)

    def handle_signal(*_args):
        cleanup_and_exit(0)

    # Never leak the real agent process past this relay's own lifetime: a
    # signal from the harness's teardown, or the outer pty hanging up
    # (SIGHUP, standard POSIX behaviour when its master side closes), must
    # kill the real child too.
    signal.signal(signal.SIGTERM, handle_signal)
    signal.signal(signal.SIGHUP, handle_signal)

    try:
        while True:
            try:
                ready, _, _ = select.select([0, master_fd], [], [])
            except InterruptedError:
                continue

            if master_fd in ready:
                data = read_some(master_fd)
                if not data:
                    break  # real agent exited / inner pty closed
                write_all(1, data)

            if 0 in ready:
                data = read_some(0)
                if not data:
                    break  # outer pty closed (pane torn down)
                bytes_from_daemon_total += len(data)
                if not dropped_cr and b"\r" in data:
                    idx = data.index(b"\r")
                    data = data[:idx] + data[idx + 1 :]
                    dropped_cr = True
                write_all(master_fd, data)
                bytes_to_agent_total += len(data)
                if dropped_cr and not marker_written:
                    write_marker(marker_path, bytes_from_daemon_total, bytes_to_agent_total)
                    marker_written = True
    except RelayIOError as exc:
        os.write(2, f"cr_suppressing_wrapper: {exc}\n".encode())
        cleanup_and_exit(1)
        return 1

    cleanup_and_exit(0)
    return 0


if __name__ == "__main__":
    sys.exit(main())
