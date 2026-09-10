#!/usr/bin/env python3
"""Deterministic Codex stand-in that models a genuine startup gap between
`dot-agent-deck wrap`'s fork-time readiness report and the moment a real
interactive process is actually able to consume stdin.

Issue #737 / tests `orchestration/seed/019` and `orchestration/seed/020`
(`tests/e2e_orchestration_seed_synthetic.rs`): the wrapper reports its own
`SessionStart` the instant it forks this process
(`Emitter::emit_fork_session_start`, `src/wrap.rs`) -- a CARD-SURFACING
signal, not a readiness one, by that function's own doc comment -- well
before this script has done anything at all. Real `codex-cli` is the same
shape, just with its own internal delay: the wrapper's fork-time report
predates the real TUI's raw-mode initialization (and the moment it can
genuinely consume stdin) by anywhere from tens of milliseconds to multiple
seconds under load. The deck's `deliver_orchestrator_prompt` (`src/ui.rs`)
treats ANY event that sets the pane's observed `agent_type` as proof the
agent is ready for its one-shot seed prompt, and waits only
`SPAWN_TIME_READINESS_BUFFER` (500ms) past that before writing it -- nowhere
near enough for a genuinely slow-starting agent.

Controlled by `STANDIN_READY_DELAY_MS` (milliseconds, default 0): this
script does not touch stdin at all until that many milliseconds have
elapsed, then explicitly discards whatever accumulated on it
(`termios.tcflush(fd, TCIFLUSH)`) before ever reading -- mirroring a real
TUI's raw-mode transition, which is what makes a too-early write genuinely
LOST rather than merely delayed. A plain blocking `read()` alone cannot model
this: without the explicit flush, cooked-mode line discipline would simply
queue the deck's already-completed line and hand it to the first `readline()`
call regardless of how late that call happens, which would NOT reproduce the
loss issue #737 describes (`state.rs`'s own measurement against real Codex:
"written before [the repaint], the payload is gone -- not parked, gone").

Once it genuinely reads a non-empty line (proof something survived to reach
it), it behaves like an agent that received a prompt: it logs the raw line to
`STANDIN_LOG_PATH` (an ABSOLUTE path -- orchestration roles run inside
isolated-clone-provisioned worktrees, not the test harness's own workdir, so
a cwd-relative log file would land somewhere the test can't find it; falls
back to a relative `standin-input.log` if the caller doesn't set it), then
emits the same turn-lifecycle JSONL `codex-standin.sh` does, so the wrapper's
ordinary JSONL classification (`classify_and_emit`) still drives the
dashboard through Thinking -> Idle for the case where delivery genuinely
lands. A bare confirmation-retry probe (an empty submitted line) does NOT
satisfy this and starts no turn.
"""
import os
import sys
import time

READY_MARKER = "STANDIN-READY"
DEFAULT_LOG_NAME = "standin-input.log"


def main() -> None:
    delay_ms = int(os.environ.get("STANDIN_READY_DELAY_MS", "0"))
    if delay_ms > 0:
        time.sleep(delay_ms / 1000.0)

    try:
        import termios

        fd = sys.stdin.fileno()
        # TCIFLUSH discards data received but not yet read -- exactly the
        # "queued while nothing was listening" bytes a too-early write
        # leaves behind, which a bare `read()` cannot model on its own.
        termios.tcflush(fd, termios.TCIFLUSH)
    except Exception:
        pass

    sys.stdout.write(f"{READY_MARKER}\n")
    sys.stdout.flush()

    line = sys.stdin.readline()
    log_path = os.environ.get("STANDIN_LOG_PATH", DEFAULT_LOG_NAME)
    with open(log_path, "w", encoding="utf-8") as handle:
        handle.write(line)

    if line.strip():
        for payload in (
            '{"type":"turn.started"}',
            '{"type":"item.started","item":{"type":"command_execution","command":"ls"}}',
            '{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":2}}',
        ):
            sys.stdout.write(payload + "\n")
            sys.stdout.flush()
            time.sleep(0.3)

    # Stay alive so the pane's process (and the wrap around it) does not
    # exit and confuse the harness with an unexpected SessionEnd while the
    # test is still reading the grid/log.
    time.sleep(30)


if __name__ == "__main__":
    main()
