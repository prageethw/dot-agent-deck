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
reports a genuine turn over the hook socket directly (`Thinking` then
`Idle`), so the dashboard still moves through the same transition a real
delivery would drive. A bare confirmation-retry probe (an empty submitted
line) does NOT satisfy this and starts no turn.

Fix-round note (15th upstream sync): this used to print the turn-lifecycle
as bare JSONL on stdout for `dot-agent-deck wrap`'s OWN text/JSON classifier
(`classify_and_emit`, `src/wrap.rs`) to pick up. That relied on
`suppress_text_status` being `false` for this invocation -- true when this
script was written, but `codex_spawn_prep` installs and trusts the deck's
OWN Codex hooks (`crate::codex_hooks_manage`) for ANY pane declaring
`--agent codex`, REGARDLESS of what program actually runs (by design: a
declared-Codex LAUNCHER wraps a shell that only execs the real `codex`
later, so hook install can't wait to see it) -- and this e2e harness's own
`seed_durable_binary` (PRD #381, `tests/common/mod.rs`) makes that
install+trust succeed even in a fresh sandboxed `$HOME`, which is
correct and intentional on its own. Once hook trust is confirmed,
`suppress_text_status` is unconditionally `true` for a Codex-identity pane
(issue #638): a plain Python stand-in can never fire Codex's real
`UserPromptSubmit`/`Stop` hooks the way genuine `codex-cli` does, so its
stdout JSONL now reaches nobody -- the wrapper is (correctly) trusting a
native-hook channel this stand-in never uses. A stand-in in this position
has to speak that channel directly, which is what emitting straight to the
hook socket below does; it is not a workaround for a defect, it is what a
producer with confirmed native-hook trust is expected to do instead of
relying on stdout scraping.

NOTE (issue #737 harness investigation, now moot): the `READY_MARKER` line
below used to also earn a spurious fallback-classified `Thinking` event
(`dot-agent-deck wrap`'s generic non-JSON fallback, "any other non-blank
output is substantive activity") before this script had read anything. With
`suppress_text_status` now `true` for this scenario, stdout is never
classified into anything at all, so that stray event can no longer happen;
`READY_MARKER` stays purely a grid-visible marker of reaching this point.
"""
import json
import os
import socket
import sys
import time
from datetime import datetime, timezone

READY_MARKER = "STANDIN-READY"
DEFAULT_LOG_NAME = "standin-input.log"


def _send_event(event_type, user_prompt=None):
    """Emit a minimal, genuine `AgentEvent` straight to the hook socket —
    the channel a REAL, hook-trusted Codex session reports over, and the
    only one `classify_and_emit` (`src/wrap.rs`) still honours once
    `suppress_text_status` is `true` for this pane (see the module doc
    above). `pane_id`/`agent_id` are inherited from `dot-agent-deck wrap`'s
    own environment (the daemon sets them on the wrapper, and a child
    process inherits its parent's env by default), so the reuse guard in
    `AppState::apply_event` (`src/state.rs`) remaps this onto wrap's own
    fork-time session for the SAME pane/agent rather than minting a second,
    uncorrelated card.

    `user_prompt`, when given, is the same recipe
    `orchestration_remit`'s fixture uses (`confirm_submission`,
    `tests/e2e_orchestration_remit.rs`): a genuine confirmation that this
    producer submitted the delivered seed pointer, which is what lets
    `deliver_orchestrator_prompt` (`src/ui.rs`) finalize the delivery and
    the orchestration role settle into a real `Working`/`Idle` sequence
    rather than staying provisional indefinitely.
    """
    socket_path = os.environ.get("DOT_AGENT_DECK_SOCKET")
    pane_id = os.environ.get("DOT_AGENT_DECK_PANE_ID")
    if not socket_path or not pane_id:
        return
    payload = {
        "session_id": f"{pane_id}-standin-session",
        "agent_type": "codex",
        "event_type": event_type,
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "pane_id": pane_id,
        "agent_id": os.environ.get("DOT_AGENT_DECK_AGENT_ID"),
    }
    if user_prompt is not None:
        payload["user_prompt"] = user_prompt
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
            s.connect(socket_path)
            s.sendall((json.dumps(payload) + "\n").encode())
    except OSError:
        pass


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
        _send_event("thinking", user_prompt=line.rstrip("\r\n"))
        time.sleep(0.3)
        _send_event("idle")

    # Stay alive so the pane's process (and the wrap around it) does not
    # exit and confuse the harness with an unexpected SessionEnd while the
    # test is still reading the grid/log.
    time.sleep(30)


if __name__ == "__main__":
    main()
