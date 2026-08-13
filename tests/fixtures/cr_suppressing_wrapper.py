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

The moment the CR is dropped, this script creates an empty marker file at
the path named by the `CR_SUPPRESS_MARKER` environment variable (if set) --
independent, externally-checkable evidence that the suppression genuinely
engaged during this run, so a future bug that silently stops dropping the
byte fails loudly instead of quietly regressing back to today's weaker
proof.

Usage: cr_suppressing_wrapper.py <rows> <cols> <real-agent-argv...>
"""

import fcntl
import os
import pty
import select
import signal
import struct
import sys
import termios
import tty


def main() -> int:
    rows = int(sys.argv[1])
    cols = int(sys.argv[2])
    real_argv = sys.argv[3:]
    if not real_argv:
        os.write(2, b"cr_suppressing_wrapper: missing real agent argv\n")
        return 2
    marker_path = os.environ.get("CR_SUPPRESS_MARKER")

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
        # that `pty.fork()` already made our controlling terminal.
        try:
            os.execvp(real_argv[0], real_argv)
        except OSError as exc:
            os.write(2, f"cr_suppressing_wrapper: exec failed: {exc}\n".encode())
        os._exit(127)

    # Parent (the relay): size the inner pty to match the outer one before
    # any output can be produced against the wrong geometry.
    fcntl.ioctl(master_fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

    dropped_cr = False

    def cleanup_and_exit(*_args):
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        os._exit(0)

    # Never leak the real agent process past this relay's own lifetime: a
    # signal from the harness's teardown, or the outer pty hanging up
    # (SIGHUP, standard POSIX behaviour when its master side closes), must
    # kill the real child too.
    signal.signal(signal.SIGTERM, cleanup_and_exit)
    signal.signal(signal.SIGHUP, cleanup_and_exit)

    while True:
        try:
            ready, _, _ = select.select([0, master_fd], [], [])
        except InterruptedError:
            continue

        if master_fd in ready:
            try:
                data = os.read(master_fd, 65536)
            except OSError:
                data = b""
            if not data:
                break  # real agent exited / inner pty closed
            os.write(1, data)

        if 0 in ready:
            try:
                data = os.read(0, 65536)
            except OSError:
                data = b""
            if not data:
                break  # outer pty closed (pane torn down)
            if not dropped_cr and b"\r" in data:
                idx = data.index(b"\r")
                data = data[:idx] + data[idx + 1 :]
                dropped_cr = True
                if marker_path:
                    with open(marker_path, "w", encoding="utf-8"):
                        pass
            if data:
                os.write(master_fd, data)

    cleanup_and_exit()
    return 0


if __name__ == "__main__":
    sys.exit(main())
