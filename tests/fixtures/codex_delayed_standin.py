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

NOTE (issue #737 harness investigation): the `READY_MARKER` line below is
printed BEFORE the genuine stdin read, deliberately -- the test needs a
grid-visible way to confirm this script reached that point without touching
stdin. But `dot-agent-deck wrap`'s `classify_and_emit` (`src/wrap.rs`) tees
every stdout line of a Codex-identity child through a text classifier, and
for this stand-in `suppress_text_status` is `false` (this is a plain Python
script, not real `codex-cli`, so it never fires the native
`UserPromptSubmit`/`Stop` hooks that would otherwise make the wrapper stand
the text classifier down) -- so this marker line itself gets classified by
the generic non-JSON fallback ("any other non-blank output is substantive
activity") and reaches the daemon as a real `Thinking` AgentEvent, despite
predating any genuine input. This is expected, independent, already-accepted
behavior of the wrapper's fallback classifier (see the `CODEX` ruleset's own
"Accepted risk" doc comment in `src/wrap.rs`) -- not a bug in this script,
and not something this script can avoid while still proving its own
readiness on the grid. `tests/e2e_orchestration_seed_synthetic.rs`'s
`orchestration_seed_019` test tolerates that stray `Thinking` and instead
asserts on `Idle`, which can only follow this script's own `turn.completed`
JSONL below -- printed only once a real, non-empty stdin line was read.
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
