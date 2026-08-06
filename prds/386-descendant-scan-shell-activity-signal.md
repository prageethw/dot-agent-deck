# PRD #386: A shell-activity signal that actually fires — descendant-process scan

**Status**: In progress — M1 and M2 landed (process-table primitive + structural discriminator); M4 removed from scope on 2026-08-06 (see Work Log)
**Priority**: High (the pane status a user reads is wrong for minutes at a time, and the mechanism meant to prevent it has never fired)
**Created**: 2026-08-06
**GitHub Issue**: [#386](https://github.com/vfarcic/dot-agent-deck/issues/386) (filed upstream — the `Stop → Idle` assumption exists in upstream's own code)
**Related**: [#370](https://github.com/vfarcic/dot-agent-deck/issues/370) (`prds/370-shell-activity-status.md`) — **supersedes its mechanism, not its goal**; see "Relationship to #370" below. [#234](https://github.com/vfarcic/dot-agent-deck/issues/234) (`prds/234-screen-state-observation-hookless-agents.md`) — adjacent, different mechanism (vt100 screen-diff for hookless agents); do not conflate. Code: `src/platform/proc/{mod.rs,unix.rs,windows.rs}` (`foreground_pgid`, to be joined by the descendant scan), `src/agent_pty.rs` (`RunningAgent::shell_foreground_busy`, `shell_foreground_busy_snapshot`), `src/daemon.rs` (`run_shell_activity_monitor`, the `pane_hook_session_id` gate), `src/state.rs` (`AppState::apply_event`'s `ShellBusy`/`ShellIdle` arms, `SessionState::shell_synthetic_working`), `src/hook.rs` (`"Stop" => EventType::Idle` — read, not changed).

## Problem Statement

Two separate, independently-measured defects combine to leave a pane advertising `Idle` while it is plainly busy. Both were measured on 2026-08-06 against a real logged-in interactive Claude agent driven on a real PTY, with events captured from a sandbox daemon started with `DOT_AGENT_DECK_LOG` (the full logs are in `.dot-agent-deck/370-diagnosis-notes.md` and `.dot-agent-deck/hook-silence-notes.md`).

**Defect 1 — the deck equates "the agent's turn ended" with "the pane has nothing running", and Claude Code's background-shell hand-off makes that false.** Claude Code's Bash tool has a **120-second default timeout**; a command that exceeds it is **not killed**, it is *moved to a background shell*. `PostToolUse` fires at the cap, the agent ends its turn, the `Stop` hook fires, `src/hook.rs:105` maps `Stop → EventType::Idle`, and `src/state.rs:3614-3617` sets `SessionStatus::Idle`. Measured stream for a 200-second `ping` run under default Bash settings: `ToolStart` at `01:20:53.307`, `ToolEnd` at `01:22:53.695` (**+120.4 s** — the cap, not the command), `Idle` at `01:23:00.372`, while `ps` showed the `ping` still alive at 02:23 elapsed. Claude's own transcript for that call, verbatim: *"Command did not complete within its 120s timeout and was moved to the background (ID: bye2ifewm). Output is being written to: … You will be notified when it completes."* The control run isolates the cap as the sole cause: the identical 200-second command with `timeout: 300000` passed to the Bash tool held `Working` for the full 200.4 s with no intervening event at all. Extrapolated to the originally-reported ~700-second `cargo test-e2e`, the pane reads `Working` for 2 minutes and `Idle` for the remaining ~9.7.

This is not a regression — both halves of the mapping landed in `8a17388` (PRD #1) and have never changed. What changed is on Claude Code's side. The deck-side assumption is **agent-agnostic**: every adapter maps "turn ended" onto `Idle` — `Stop` for Claude and Codex (`src/hook.rs:105`, shared arm), `session.idle`/`session.status=idle` for OpenCode (`src/hook.rs:348,354`), `--type finished` for Pi. Only Claude was measured.

**Defect 2 — PRD #370, the mechanism written to backstop exactly this, has never fired in any real pane configuration.** For an **agent pane**, Claude's Bash-tool child runs on **pipes, in a new session**, off the pane's PTY entirely (measured: TTY `??`, `Ss`, its own pgid). The pane's PTY child and every process on the pane's tty share one pgid, so `tcgetpgrp(pane_pty)` never moves and `RunningAgent::shell_foreground_busy` (`src/agent_pty.rs:1688`) computes `6387 != 6387` → **`Some(false)`, permanently**. Same shape on all five role panes measured. For a **bare shell pane** the pgid mechanism genuinely works, but the pane is dropped earlier by the `pane_hook_session_id` gate in `run_shell_activity_monitor` (`src/daemon.rs:897`), whose map is populated only from real agent hook events (`src/hook.rs`). #370 shipped green because its one end-to-end test (`shell_activity_monitor_reflects_a_real_foreground_command`, `src/daemon.rs:1261`) types `sleep 2` **directly into the pane's PTY** *and* hand-seeds a synthetic `SessionStart` carrying `pane_id: "pane-370"` — neither step is something a real pane performs. #370's M5 and M6 are both unchecked; its PTY-attached L2 test was attempted and reverted.

The two defects are independent, and fixing the second fixes the first: `apply_event`'s existing `ShellBusy` arm (`src/state.rs:3628-3638`) already promotes exactly `Idle`/`Unknown` → `Working`, which is precisely the state Defect 1 leaves the pane in.

## Solution Overview

Replace `tcgetpgrp` with a **descendant-process scan**. The `pane_hook_session_id` gate **stays** — see the Work Log entry for 2026-08-06 (scope reduction): this PRD covers **agent panes only**, and bare shell panes remain unmonitored exactly as #370 intended.

The whole of the rest of #370 is kept as-is: the 500 ms edge-triggered `run_shell_activity_monitor` poll, the synthesized `EventType::ShellBusy`/`ShellIdle` riding the same broadcast pipeline real hooks use, and `apply_event`'s precedence rules (`ShellBusy` only promotes `Idle`/`Unknown`; `ShellIdle` only reverts a promotion this mechanism made; any real event clears `shell_synthetic_working`). Only the *primitive underneath* changes. That is why this repairs Defect 1 **with no change to hook semantics at all** — nothing about `Stop → Idle` moves — and why it covers foreground *and* background commands, all agents, and bare shell panes in one mechanism.

### The discriminator

The #370 notes feared a descendant scan was unviable, because a Claude pane *always* has long-lived children — `npm exec @upstash/context7-mcp`, `engram mcp`, `caffeinate -i -t 300` — so a naive "has descendants" test is `true` 100% of the time and would pin every pane at `Working` forever. The discriminator that answers this was measured on 2026-08-06 (`.dot-agent-deck/386-argv-notes.md` §3/§4, against Claude Code 2.1.220), and it is **structural, not textual**: Claude Code `setsid`-detaches its Bash-tool child into a **POSIX session of its own**, while every other child of the agent stays in the agent's session on the pane's tty.

Measured by `getsid(2)` on a live agent pane — the agent is `claude --model opus`, pid 51757, itself in the pane's session 51698:

```
bash-tool shell subtree  pid=63120  sid=63118  pgid=63118      <- own session
claude agent             pid=51757  sid=51698  pgid=51698
mcp context7             pid=51787  sid=51698  pgid=51698
engram mcp               pid=51789  sid=51698  pgid=51698
caffeinate               pid=60798  sid=51698  pgid=51698
pane leader (devbox)     pid=51698  sid=51698  pgid=51698
```

The same pane's `ps` table, taken during a Bash-tool call, shows the direct children of the agent and where each one sits:

```
PID    PPID   PGID   TTY      STAT  ARGS
61296  51757  61296  ??       Ss    /bin/zsh -c source …/shell-snapshots/snapshot-zsh-… && eval …   <- Bash tool
51787  51757  51698  ttys014  S+    npm exec @upstash/context7-mcp
51788  51757  51698  ttys014  S+    npm exec task-master-ai
51789  51757  51698  ttys014  S+    engram mcp --tools=agent
51807  51757  51698  ttys014  S+    …/Python …/pysemgrep mcp
60798  51757  51698  ttys014  S+    caffeinate -i -t 300
```

**Every** confounder the #370 notes feared — `context7`, `task-master`, `engram`, `pysemgrep`, `caffeinate` — stays in the agent's session (`sid=51698`) on the pane's tty. Only the Bash-tool child is `setsid`-detached into a session of its own. Nested processes inside the Bash call inherit the shell's session, so the whole subtree is covered by the same test (measured: a pipeline sub-shell 61314 and its `ugrep` 61316 both carried the Bash-tool shell's sid and pgid). Verified equally for a `run_in_background` call — same `??`/`Ss`/own-session shape.

A pane is **busy** iff its PTY child has a transitive descendant satisfying all of:

1. **Descendant.** Reachable from `RunningAgent.child.process_id()` by following `ppid` (the pane's PTY child itself does not count).
2. **Detached from the pane's terminal — corroborating, and never sufficient on its own.** The process has no controlling terminal (`ps` TTY column `??` on macOS, `?` on Linux) and is a session leader (`ps` STAT contains `s`). True of every Bash-tool child measured, and useful as an assertion in fixtures and as a sanity check on a captured table — **but see the CI trap below: it must never stand in for condition 3.**
3. **In a different POSIX session than the agent.** `getsid(descendant) != getsid(agent_pid)`. **This is the load-bearing condition, and it replaces the argv match this PRD originally specified here.** It is one libc call per descendant: no `/proc` parsing, no argv reads, no string matching. `getsid(2)` is POSIX and behaves identically on macOS and Linux, it works on any pid rather than only on children, and it is immune to any change in what Claude Code puts on the command line.

Two structural alternatives were considered and are worse. `pgid(descendant) != pgid(agent)` false-positives on a child that merely called `setpgid` without detaching its session. "No controlling terminal" alone is the trap below.

#### The CI trap — "no controlling terminal" alone collapses where the agent has no terminal either

> **A bare "the descendant has no controlling terminal" test is meaningless in CI, where the agent itself has no controlling terminal, so *every* descendant matches and the pane pins at `Working` forever.** The implementation must compare the descendant's session id against **the agent's own** session id, and must never fall back to a bare no-ctty test.

This is measured, not feared: in a Linux container, `docker run` without `-t` gives PID 1 `tty_nr=0` in `/proc/<pid>/stat`, while `docker run -t` gives `tty_nr=34816`. A test written against a developer machine — where the pane genuinely has a tty and the no-ctty test therefore looks correct — passes locally and asserts nothing at all in CI. M1's and M2's tests run on both platforms and CI is the Linux one, so this hazard is live for both milestones.

#### The argv shape, demoted to a cross-check

The Bash-tool argv shape is **kept, as a secondary check** — not as the primary test. It is worth keeping precisely because the two predicates fail on **disjoint** sets (see Risks): the structural test dies if Claude Code stops `setsid`-ing its Bash child and false-positives on an MCP server that detaches itself, neither of which touches the argv; the argv test dies on prologue rewording, `CLAUDE_CODE_SHELL_PREFIX`, sandbox mode, and the missing-snapshot variant, none of which touches the session id. Together they catch strictly more than either alone, and the argv is also what lets an already-detected busy subtree be *attributed* to Claude Code's Bash tool rather than to something unknown.

The narrowest form that survives measurement:

> `argv[argc-1]` contains **`shell-snapshots/snapshot-`** **and** `&& eval `, with **`\builtin unalias -- 'unsetenv'`** as the segment that survives the no-snapshot variant (`argc == 4`, `[shell, "-c", "-l", cmd]`, where the `source …` segment is absent from the string entirely).

**`argc == 3`.** The entire prologue plus the user's command is **one** argv element, so any predicate must substring-match inside `argv[argc-1]` and must never tokenise argv.

Predicates that were measured and **rejected** — recorded so they are not re-proposed:

| Rejected predicate | Why |
|---|---|
| `argv[0] == "/bin/zsh"` | The interpreter follows `CLAUDE_CODE_SHELL` → `$SHELL`, and only falls back to a `which`/`{/bin,/usr/bin,/usr/local/bin,/opt/homebrew/bin}` × `{zsh,bash}` search. On this machine the passwd shell is `/bin/bash` while `$SHELL` is `/bin/zsh` — the child followed `$SHELL`. `~/.claude/shell-snapshots/` here already holds a `snapshot-bash-*.sh`, so the bash interpreter is not hypothetical. |
| `setopt NO_EXTENDED_GLOB` | zsh-only. Under bash the same slot emits `shopt -u extglob`, and the segment can be omitted altogether. |
| `.claude/shell-snapshots` | Breaks under `CLAUDE_CONFIG_DIR`, which relocates the Claude home. Match `shell-snapshots/snapshot-` instead. |
| filename regex anchored on `-\d{13}-[a-z0-9]{6}\.sh` | The base36 suffix is `Math.random().toString(36).substring(2,8)` and **can be shorter than 6 chars**. |
| `pwd -P >\|` | Too generic — a real user command could contain it. |
| tokenising argv | `argc == 3`; everything is inside one element. |

Foreground and background Bash calls have the **identical** process shape, so a single signal covers both — which is why this subsumes the narrower "teach the deck about outstanding background shells" alternative (parsing `tool_input.run_in_background` and the `tool_response` "moved to the background" text), and does so without English-text matching and without the unsolved problem of *when to clear* such a marker.

**Design point to settle at implementation start (see Open Questions):** the structural test is agent-agnostic — it needs only the agent's pid — so it can be the signal for *every* agent kind, measured or not. The argv cross-check is the part coupled to another product's internals, and the recommendation below is that it be per-agent data, required only where the shape has actually been measured.

## Relationship to #370

**This PRD supersedes #370's mechanism. It does not supersede its goal.**

#370's success criteria were right and are carried forward verbatim into this PRD: a pane running an agent that shells out to a long-running command shows `Working` for the duration; a genuinely idle pane is not falsely `Working`; a more specific agent-emitted status (`WaitingForInput`, `Error`) is never silently overridden; no measurable per-tick performance regression. What was wrong was the **instrument** — `tcgetpgrp` answers "who owns the terminal", and an agent that spawns its children on pipes never cedes the terminal, so the answer is correct and useless.

Everything #370 built downstream of the primitive is **kept, not rewritten**: the wire types (`EventType::ShellBusy`/`ShellIdle` and the `#[serde(other)] Unknown` catch-all), `PROTOCOL_VERSION` 7, the daemon poll task, the precedence rules, and their unit tests. This PRD replaces one function's body and deletes one gate.

`prds/370-shell-activity-status.md` gets a Work Log entry recording that its mechanism never fired, that M5/M6 were never completed, and that it is superseded here. Its history stays intact and is not rewritten.

## Scope

**In Scope**

- A descendant-process scan primitive in `src/platform/proc/` (Unix), with the discriminator above; `None` on Windows, matching `foreground_pgid`'s existing shape.
- Rewiring `RunningAgent::shell_foreground_busy` (or a successor named for what it now measures, e.g. `shell_activity_busy`) onto that primitive. `foreground_pgid` itself may stay as a Unix helper or be retired — it has no other caller.
- A per-agent table for the argv cross-check's shape, if the recommendation there is accepted.
- Test coverage per the Test Plan below, including a real-agent PTY-attached test that hand-seeds nothing.
- Changelog fragment; a note in `docs/develop/` if the process-shape coupling warrants one.

**Out of Scope**

- **Dropping the `pane_hook_session_id` gate** (`src/daemon.rs:897`), and with it the bare-shell-pane case. Removed from scope on 2026-08-06 — see the Work Log entry. The gate is not an oversight: its own doc comment on `main` calls it a *"documented M2 scope boundary (PRD #370), not a bug: this mechanism promotes an agent's OWN idle gaps, not a shell nobody's tracking."* And the reported bug does not need it removed — the affected pane was an **agent** pane, which has a hook session and passes the gate fine; it failed at the `tcgetpgrp` check, which the structural `getsid` discriminator fixes on its own. **#386 covers agent panes only**, and bare shell panes stay unmonitored exactly as #370 intended.
- **Windows process walking.** The primitive returns `None` on Windows, exactly as `foreground_pgid` does today. Documented gap, not solved here.
- **Any change to hook semantics.** `Stop → EventType::Idle → SessionStatus::Idle` stays exactly as it is. This PRD does not touch `src/hook.rs`'s mapping, does not add a background-shell marker, and does not change what any adapter reports. The repair happens entirely in the `ShellBusy` promotion that already exists.
- Setting `BASH_DEFAULT_TIMEOUT_MS` in the agent environment to widen Claude's cap — a workaround that changes agent behaviour the user did not ask for.
- PRD #234's vt100 screen-diff mechanism.
- Re-litigating #370's precedence rules, poll cadence, or wire format.

## Technical Approach

**The primitive.** `pub fn descendant_shell_activity(root_pid: i32, shapes: &[ShellToolShape]) -> Option<bool>` in `src/platform/proc/unix.rs`, `None` unconditionally in `windows.rs`. It needs `pid` and `ppid` for every process on the machine to build the descendant walk, the **session id** of the agent and of each candidate descendant for the discriminator, and — for the argv cross-check only — controlling-tty presence, session-leader flag, and full argv. Two implementation routes:

- **Route A (recommended first cut): one `ps -Ao pid,ppid,stat,tty,args` per poll**, parsed once into a table and reused for *every* pane in that poll. Identical invocation on macOS and Linux, no new dependency, and one fork/exec per poll cycle rather than per pane. Cadence relaxes from #370's 500 ms to 1 s to keep the cost obviously negligible.
- **Session ids do not come from `ps`.** `ps -o sess=` prints `0` on macOS for a non-root caller and is useless here; read the session id with `libc::getsid(pid)` instead, on both platforms, for the agent pid and for each candidate descendant. That is a handful of syscalls per pane per poll and needs no `/proc` parsing on Linux either.
- **Route B (optimization, behind a profiling gate): native enumeration** — `/proc/<pid>/{stat,cmdline}` on Linux, `sysctl(KERN_PROC_ALL)` + `KERN_PROCARGS2` on macOS via the `libc` dependency already declared Unix-only in `Cargo.toml`. No subprocess at all, but two platform-specific implementations to maintain.

Start with A, measure, and only take B if the measurement says so. M5 exists to make that a real measurement rather than an assumption.

**The scan.** Build a `ppid → children` index once per poll, walk down from each pane's PTY child pid, and stop at the first descendant matching the discriminator. Depth is bounded in practice (agent → shell → command) but the walk must carry a visited set: a `ppid` table sampled non-atomically can contain a cycle after PID reuse.

**The gate stays.** `run_shell_activity_monitor` skips a pane when `pane_hook_session_id` returns `None`, because it needs *some* `SessionState` to update. That is left exactly as it is (scope reduction, 2026-08-06): an agent pane always has a hook session and passes the gate, so the reported bug is fully addressed without touching it, and no session-resolution fallback chain — and no placeholder-session creation — is introduced by this PRD.

## Success Criteria

- A pane whose agent runs a Bash command longer than Claude's 120 s cap reads `Working` for the **whole** command, not for the first two minutes. Measured on the real path, not on a hand-seeded state.
- A pane whose agent runs a long *foreground* command (under the cap) also emits `ShellBusy` — the signal fires on the process shape, not on the cap.
- A pane sitting at an agent's idle prompt, with MCP servers and `caffeinate` alive as children, reads `Idle` — no false `Working`.
- A more specific agent-emitted status (`WaitingForInput`, `Error`, `Thinking`) is never overridden by this signal.
- No measurable daemon overhead regression from the scan.

## Milestones

Each is independently testable; the test that proves each one is named in the Test Plan.

- [x] **M1 — Process-table primitive.** Enumerate `pid`/`ppid`/tty/session-leader/argv on Unix (Route A) plus `getsid` for the agent and each candidate descendant, `None` on Windows. Cycle-safe descendant walk. No deck wiring yet.
- [x] **M2 — The discriminator.** The structural test: a descendant whose session id differs from the agent's own (`getsid(descendant) != getsid(agent_pid)`), with the argv shape as a per-agent cross-check layered on top. Pure classification over a process table — testable against both real spawned processes and captured fixtures. The one thing this milestone must not do is compare against a *constant* (no ctty, session-leader) instead of against the agent's session id; that variant passes on a developer machine and is vacuous in CI.
- [ ] **M3 — Wire it into the pane primitive.** `RunningAgent::shell_foreground_busy`'s body swaps to the descendant scan; `shell_foreground_busy_snapshot` and `run_shell_activity_monitor` are otherwise untouched. This is the milestone at which an agent pane starts producing `ShellBusy` for the first time.
- [ ] ~~**M4 — Drop the `pane_hook_session_id` gate.**~~ **Out of scope** (2026-08-06 — see Work Log). The gate stays, #386 covers agent panes only, and bare shell panes remain unmonitored as #370 intended. Left in place rather than renumbered so every existing reference to M5/M6/M7 keeps resolving.
- [ ] **M5 — Overhead measurement and cadence.** Measure the poll's cost with a realistic process table and pane count; confirm or revise the 1 s cadence; decide Route A vs Route B on the number, and record the number in this PRD.
- [ ] **M6 — Real-agent proof and rot canary.** The two real-agent PTY tests in the Test Plan (short foreground call; >120 s call crossing the cap). Neither hand-seeds a `SessionStart`.
- [ ] **M7 — Docs, changelog, cross-version check.** Changelog fragment; a `docs/develop/` note on the process-shape coupling and how it is detected when it rots; CLAUDE.md rule 12 cross-version manual test (see below).

## Test Plan

#370's lesson is precise and worth stating as the standard this plan is held to: **a green suite around a mechanism nothing feeds is indistinguishable from a working feature.** #370 had a passing end-to-end test, passing precedence tests, and a shipped feature that fired in zero real configurations. Every test below is therefore stated as *what it would prove in a real pane*, and the two that carry the burden of proof drive a real agent with nothing seeded by hand.

**M1 — process-table primitive (fast tier, L1).** Spawn a real child from the test, on pipes, via `setsid`, as a grandchild of a test-owned process; assert the primitive finds it as a descendant, reports a session id **different from the test process's own**, reports no controlling terminal and session-leader true, and returns its full argv. Assert the walk terminates on a synthetic table containing a `ppid` cycle. *Proves in a real pane*: nothing on its own — this is a mechanism test, and #370's failure was exactly a correct mechanism test attached to nothing. It is included so a later failure localises, not as evidence the feature works.

**M2 — discriminator (fast tier, L1).** Two captured fixture tables from the measurement notes, both carrying a session-id column: one containing the measured Bash-tool child (`sid=63118`, its own; TTY `??`, `Ss`) plus the five measured long-lived children (`context7`, `task-master`, `engram`, `pysemgrep`, `caffeinate` — all at the agent's `sid=51698`, on the pane tty), and one containing only the long-lived children. Assert `true` for the first and `false` for the second **with the argv cross-check disabled**, which is what pins the claim that the session-id test alone already excludes every measured confounder. A third case guards the CI trap directly: the same two tables with *every* process at `tty_nr = 0` — the agent included, as in a container — must still classify identically, because the test compares against the agent's session id rather than against the presence of a terminal. *Proves in a real pane*: that the false-positive case that would pin every pane at `Working` forever is excluded — against processes actually observed in a live deck, not invented ones — and that the exclusion does not evaporate in CI.

**M3 — pane primitive, real process (fast tier, L1 mechanism test).** Spawn a real PTY pane running a shell, have it launch a genuine `setsid`-detached child on pipes (with the Bash-tool argv shape, so the cross-check is exercised too), and assert `shell_foreground_busy` flips `false → true → false` across that child's lifetime. Explicitly the test #370 never had: **the pane's child is on pipes, in its own session, off the PTY, exactly as a real agent's is.** *Proves in a real pane*: that the primitive answers correctly for the process topology that made #370 useless — but still with a stand-in, so it is not sufficient on its own.

**M4 — no tests.** The milestone is out of scope (2026-08-06 — see Work Log), so its gate-removal test and its agent-pane regression guard are both gone. That frees catalog slots `status/shell-activity/005` and `006`, and the three real-agent e2e tests below take them: **M6a → `005`**, **M6b → `006`**, **M6c → `007`**. (`001`/`002` are M1, `003` is M2, `004` is M3.) The renumber is prose only here — those catalog entries do not exist yet and `tests/CATALOG.md` belongs to whoever writes them.

**M6a — real agent, short foreground call (e2e tier, PTY-attached, real agent, rot canary) — `status/shell-activity/005`.** A real interactive Haiku Claude agent in a real spawned pane (`prepare_claude_home` + per-folder trust + `--allowedTools Bash`, following `scheduler/dispatch/013`), prompted to run a ~20-second foreground command against a uniquely-named sentinel fixture file. Assert a `ShellBusy` event is emitted for that pane **during** the call. The status is already `Working` from `ToolStart` here, so the assertion must be on the event, not the badge — otherwise the test passes with the signal dead, which is precisely how #370 shipped. *Proves in a real pane*: that Claude Code still `setsid`-detaches its Bash-tool child — and, secondarily, that the argv cross-check still matches what it spawns today. **This is the rot detector, and what it detects has changed with the discriminator.** It no longer guards mainly against argv rot; it guards against *Claude Code ceasing to `setsid` the Bash child*, which is a total false negative and is otherwise completely silent, and against an MCP server or plugin starting to detach itself, which is a false positive. Both are structural changes in another product that no fixture can notice. When either happens, this test goes red — cheaply, in ~20 s, rather than silently degrading in the field. It must not be replaced by a fixture-based test, which would pass forever against a topology nobody spawns any more.

**M6b — real agent, call crossing the 120 s cap (e2e tier, PTY-attached, real agent, user-visible) — `status/shell-activity/006`.** The same rig, prompted to run a >120 s foreground command under **default** Bash settings — no `run_in_background`, no `timeout` parameter — reproducing `sbx4` exactly. Assert that the pane's rendered badge reads `Working` at a sample taken **after** the `Stop`-driven `Idle` would have landed (measured at cap + ~7 s), and that the command is still running at that moment. Note for whoever writes it: `sleep` is unusable as the instrument — Claude Code blocks long `sleep` at the tool layer ("The system is blocking this command because it detects long sleep commands") and no `ToolStart` is emitted at all; the diagnosis used `ping -c N 127.0.0.1 > /dev/null` for real, non-sleep work. *Proves in a real pane*: **the reported bug, as the user sees it** — this is the CLAUDE.md rule 4 test that validates the feature as a user actually uses and sees it. It costs ~2.5 minutes of e2e wall clock; that is the price of proving the thing the PRD exists for, and it belongs in the pre-PR e2e tier (flaky-tolerant, credentialed, not run by CI).

**M6c — no false positive with a real agent idle (e2e tier, real agent) — `status/shell-activity/007`.** The same rig, agent brought up and left at its idle prompt with its MCP servers and `caffeinate` alive, sampled after the poll interval: the pane must read `Idle`. *Proves in a real pane*: the M2 fixture claim, against the live process table rather than a captured one.

**M5 — overhead (measurement, recorded not asserted).** Poll cost against a realistic table, reported as a number in this PRD. Not a pass/fail test; a threshold assertion here would be a flake generator on a loaded machine.

**Deliberately not claimed.** No test here proves the behaviour for Codex, OpenCode, or Pi. Only Claude's shell-tool shape was measured, and inventing fixtures for the others would manufacture exactly the false confidence #370 shipped with. See Risks.

## Risks

- **Claude Code ceasing to `setsid` its Bash-tool child — total false negative, and silent.** The whole signal rests on that one behaviour. If a future release spawns the Bash child inside the agent's own session, `getsid(descendant) == getsid(agent)` everywhere, no descendant ever matches, and the pane goes back to reading `Idle` during long commands with no error logged anywhere. It is not a documented interface. **How a test catches it:** M6a, the canary — a real agent, a real ~20-second Bash call, asserting the `ShellBusy` event fires.
- **An MCP server (or plugin, or hook) that `setsid`s itself — false positive, and it pins the pane at `Working` forever.** A permanently-wrong badge is *worse* than the stale `Idle` it replaces, because it is unfalsifiable to the user. This is now the **load-bearing assumption of the design**, and it was measured **once, on one machine, with one MCP configuration**: `context7`, `task-master`, `engram`, `pysemgrep`, and `caffeinate` all stayed in the agent's session. Nothing guarantees the next MCP server does. M6c is the only test that checks this against a live process table, and it only ever checks *this* machine's configuration.
- **PID namespaces / bwrap sandboxing.** If Linux sandboxing is ever enabled for the Bash tool, the payload runs in its own PID namespace; a host-side descendant walk may not enumerate it, and the pids it does see may not correspond. **Neither predicate is known to survive that** — it needs its own measurement if sandboxing is turned on. This deck runs unsandboxed today.
- **Sandbox mode changes the argv shape entirely — relevant to the cross-check only.** On macOS the argv becomes `env [-u VAR…] [K=V…] /usr/bin/sandbox-exec -p <profile> <shell> -c <cmdString>`, with the shell defaulting to bash via PATH; on Linux it is bwrap + `socat` + a seccomp filter. `argv[0]`-based tests break outright; the `argv[argc-1]` substring test survives, because the command string is still the last element. The structural test is untouched. This deck's agents run `claude --model opus --permission-mode auto` — unsandboxed — and the measured argv carries no `env`/`sandbox-exec` prefix.
- **Linux argv is source-inferred, not observed — and moot if the primary test is structural.** The measurement was taken on macOS; the Linux argv rests on a source read of the shipped bundle, whose only platform branch is `isWindows`. That is low risk but unverified, and it is a risk of the *cross-check* alone: the session-id test reads no argv at all, so nothing about the discriminator depends on it. What *was* verified on Linux, in an `alpine:3` container: `/proc/<pid>/stat` field 6 is the session and field 7 the `tty_nr`, and a `setsid`-detached child shows `session=8 tty_nr=0` — the same shape as macOS's `??`/`Ss`/own-session.
- **The Bash-tool topology is measured against Claude only.** Codex, OpenCode, Pi, and `dot-agent-deck wrap` are inference. The session-id test at least *asks the right question* for all of them without needing a per-agent table — it compares each agent against its own session — but whether their shell children detach at all is unmeasured, so the signal may simply never fire for them. That is the failure mode to watch for: silence, not noise.
- **Cost of the poll.** A `ps -A` per second on a busy machine is not free, and the deck's own workers routinely drive load averages above 20. `getsid` per descendant is negligible next to it. M5 exists to put a number on the whole thing before this ships.
- **`ps` output parsing.** Column widths, argv truncation, and STAT letters vary between macOS and Linux. Route A trades a dependency for a parsing surface; M1's tests must run on both, and CI covers Linux while development is on macOS.

**Retired risk — placeholder sessions surfacing new cards.** The gate removal that would have needed session resolution is out of scope (2026-08-06 — see Work Log), so no placeholder session is ever created by this PRD. Recorded as **avoided, not mitigated**: if the gate is ever dropped in a later PRD, the risk returns in full and needs its own test rather than an assumption.

## CLAUDE.md rule 12 — cross-version contract

**Does this change the TUI↔daemon contract? No.** The wire shape does not move: `EventType::ShellBusy`/`ShellIdle`, the `#[serde(other)] Unknown` catch-all, and the broadcast envelope all landed with #370 and are untouched here. **No `PROTOCOL_VERSION` bump is owed** — it stays at 7 (upstream is at 6; that gap is #370's, not this PRD's).

**Is a `.breaking.md` fragment owed? No, on current reading — but the question is not free, because this PRD changes the *meaning* of an existing wire event.** After this lands, `ShellBusy` means "a detached descendant matching the shell-tool shape exists" where before it meant "the PTY foreground pgid differs from the pane child's pgid". A same-wire/different-meaning change is precisely what `docs/develop/versioning.md` reserves `.breaking.md` for. The reason it does not apply here is that the *old* meaning never produced an event in any real configuration — an old daemon emits `ShellBusy` for agent panes exactly never, so no mixed-version pair can observe the two meanings disagreeing. **Confirm this at implementation time rather than inheriting it**, and if the cross-version test below shows an old build emitting `ShellBusy` in any real scenario, add the fragment.

**Cross-version manual test (required before the PR):** build the previous release, run its daemon with an agent under it, run this branch's TUI against that older daemon, and confirm a delegate still routes and work-done/status hooks still arrive. #370's equivalent check was completed only at handshake level (`daemon hello`, version 6 vs 7) and its PRD says so explicitly; this one should be completed for real, since #370's shortfall on exactly this step is part of why its M5 is still unchecked.

**Bump policy** (while `0.x`): this is a bugfix with no contract break → **patch**.

## CLAUDE.md rule 9 — experimental flag

**Asked and answered: the flag does not apply, and this is recorded rather than skipped.** Rule 9 triggers on a *new user-visible surface* — a pane, field, command, tab, footer entry, or keybinding. This PRD adds none of those. It changes **when an existing status renders**: the `Working` badge that already exists, on the pane card that already exists, driven by the `SessionStatus` field that already exists. There is no new surface to gate and no wrapper function to add to `src/features.rs`.

The user should still be given the chance to overrule this if they want the corrected behaviour opt-in for a release, but the default reading of rule 9 is that it does not apply.

## Open Questions

**1 — Fork-only or upstream? (for the user to decide; the tradeoff, not a recommendation, is what this PRD supplies.)** The **bug is real upstream**: `"Stop" => EventType::Idle` and `EventType::Idle → SessionStatus::Idle` are upstream's own code, unchanged since PRD #1, and issue #386 was filed upstream for exactly that reason. The **mechanism is not**: PRD #370 merged into this fork's `main` via fork PR #14 (`d6e1d21`) and is absent from `upstream/main` — no `run_shell_activity_monitor`, no `EventType::ShellBusy`/`ShellIdle`, and `PROTOCOL_VERSION` is 6 there against 7 here.

- *Fork-only*: matches where the code lives, ships fast, and keeps the fork's own regression risk contained. Cost — upstream keeps a real, measured, user-visible bug that this fork has already diagnosed in full, and the fork carries yet another divergence to reconcile at every sync.
- *Upstream*: fixes the bug where it actually is, and the diagnosis is strong enough to justify the PR. Cost — it means upstreaming **#370 as well**, since this PRD is a rewrite of #370's primitive and cannot stand without #370's poll task, wire types, and precedence rules. That is a substantially larger PR, and it carries a `PROTOCOL_VERSION` 6 → 7 bump that upstream has not accepted. It would also mean upstream inherits the process-shape coupling and its rot risk.

A middle path exists and may be the honest one: **land it fork-only now**, and offer upstream the *diagnosis* (already done — issue #386 carries the full measurement, the control run, and the transcript) plus an offer to upstream #370 and this together if the maintainer wants them. That keeps the fork moving without sitting on a bug report upstream cannot act on.

**2 — Should the argv cross-check be a hard gate?** Recommendation: **no** — the session-id test is the signal, and the argv shape is a per-agent refinement, present for Claude (where it is measured) and absent for agents whose shape has not been. Rationale: a hard universal gate means the signal is silently dead for every unmeasured agent, which repeats #370's failure mode; a structural-only signal for those agents over-triggers at worst, which is visible and fixable. The counter-argument — that over-triggering pins a pane at `Working` and is worse than silence — is real and the user may weigh it the other way. Either way, the argv shape must be data (a per-agent table), not an inlined string literal. Note that this question got smaller with the measurement: it used to decide whether the *primary* test was string-based, and now decides only how much a secondary confirmation is allowed to veto.

**3 — Session resolution once the gate is dropped. CLOSED 2026-08-06 — resolved by not doing it.** The question only exists if the gate is dropped, and it is not: M4 is out of scope, so `pane_hook_session_id` stays the gate and there is nothing to resolve. Kept here rather than deleted, because the reasoning is what a future PRD would need to re-open it. The options considered were a fallback chain (`pane_hook_session_id` → `pane_session_id` → a placeholder session keyed `pane-<pane_id>`) and relaxing the gate to "any pane the registry knows about", accepting that a shell pane's status has nowhere to land. Neither is needed: an agent pane always has a hook session, so **a pane with no hook session now resolves to nothing at all — it is simply not observed**, which is exactly #370's documented scope boundary rather than a gap this PRD leaves behind.

**4 — Route A (`ps` per poll) or Route B (native)?** Deferred to M5's measurement rather than argued in advance.

## Provenance

The measurements this PRD rests on were taken on 2026-08-06 and are recorded in full in `.dot-agent-deck/370-diagnosis-notes.md` (why #370's signal never fires; live `ps` from the user's running deck) and `.dot-agent-deck/hook-silence-notes.md` (the 120 s cap, the sbx3/sbx4 control pair, the process shapes, and the discarded instruments — a 150 ms `ps` sampler too coarse to catch hooks, a Python `pty.fork` probe that reproduced neither case, and `sleep` as a long-foreground instrument, which Claude Code blocks at the tool layer). A third file, `.dot-agent-deck/386-argv-notes.md`, records the 2026-08-06 follow-up measurement against Claude Code 2.1.220 that produced the structural discriminator: the `getsid` table above, the complete unelided Bash-tool argv (read via `sysctl KERN_PROCARGS2`, since `ps -o args=` joins argv with spaces and cannot show element boundaries), the shell-resolution and prologue-assembly logic read verbatim out of the shipped bundle, the rejected argv predicates, and the Linux/container checks. Its own discarded instruments are worth keeping: `ps -o sess=` prints `0` for a non-root caller on macOS, and an isolated `HOME` for a probe agent does not authenticate (`Not logged in`) without the account linkage from the host `.claude.json`. All three files are gitignored working notes, not committed artefacts; the numbers that matter are reproduced in this PRD and in issue #386 so the record survives them.

## Work Log

### 2026-08-06 — scope reduced: the `pane_hook_session_id` gate stays, #386 covers agent panes only

**Decided by the user**, on the evidence below, and recorded here because it removes a milestone.

**What changed.** **M4 — "Drop the `pane_hook_session_id` gate"** is **out of scope**. The gate stays exactly as it is, and this PRD covers **agent panes only**; bare shell panes remain unmonitored, precisely as #370 intended.

**Why.** The gate is not an oversight. Its own doc comment on `main` calls it a *"documented M2 scope boundary (PRD #370), not a bug: this mechanism promotes an agent's OWN idle gaps, not a shell nobody's tracking."* And the reported bug does not need it removed: the affected pane was an **agent** pane, which has a hook session and passes the gate fine. It failed at the `tcgetpgrp` check — which the structural `getsid` discriminator (M2) fixes on its own. Dropping the gate would have bought a case nobody reported at the cost of the one genuine design choice in the PRD.

**What moved with it.** M4's milestone entry is marked out of scope **in place** rather than renumbered, so every existing reference to M5/M6/M7 keeps resolving. The Scope section moves the gate into **Out of Scope**; the Technical Approach's "Dropping the gate" paragraph becomes "The gate stays"; the bare-shell-pane success criterion is removed. **Open Question 3** ("what does a pane with no hook session resolve to?") is **closed by not doing it** — kept, with the reasoning, rather than deleted. The **placeholder-session risk** ("creating placeholder sessions can surface new cards in the UI") is retired as **avoided, not mitigated**, and returns in full if a later PRD drops the gate. The Test Plan loses M4's gate-removal test and its agent-pane regression guard, freeing catalog slots `005` and `006`, so the three real-agent e2e tests renumber: **M6a → `status/shell-activity/005`**, **M6b → `006`**, **M6c → `007`**.

**Not changed.** The Problem Statement's Defect 2 still records that the bare-shell-pane path is gated — that is a measurement, and it stays accurate. What changed is whether this PRD acts on it.

### 2026-08-06 — M1 and M2 implemented

`src/platform/proc/scan.rs` (new, cross-platform) carries `ProcessInfo`, the cycle-safe `descendants` walk, the structural `descendant_shell_activity` discriminator, and `ShellToolShape`/`CLAUDE_BASH_TOOL_SHAPE` for the argv cross-check; `process_table` is the one platform-specific piece (`ps -A -w -w -o pid=,ppid=,tty=,args=` on Unix, `None` on Windows). Session ids come from `libc::getsid(pid)`, never `ps -o sess=` — which prints `0` for a non-root caller on macOS. `session_leader` is derived as `getsid(pid) == pid` rather than from `ps`'s STAT letters, which is exact and removes one column of `ps` formatting from the parsing surface. The discriminator reads **only** session ids and never `has_controlling_tty`, so the CI trap cannot be walked into; `status/shell-activity/003`'s third and fourth cases pin that. Nothing is wired into `RunningAgent`, the daemon, or any status — that is M3.

### 2026-08-06 — condition 3 rewritten from argv matching to a structural session-id test

The PRD as originally written made condition 3 an **argv string match** on Claude Code's Bash-tool command line (`/bin/zsh -c source …/.claude/shell-snapshots/… && eval …`), with conditions 1∧2 (descendant, no controlling terminal) as the structural part. A follow-up measurement against Claude Code 2.1.220 (`.dot-agent-deck/386-argv-notes.md` §3/§4) changed that design, and this revision supersedes the argv-matching approach in the PRD's original form.

**What was measured.** Claude Code `setsid`-detaches its Bash-tool child into its own POSIX session (`sid=63118`), while the agent (`sid=51698`) and **every** confounder the #370 notes feared — `context7`, `task-master`, `engram`, `pysemgrep`, `caffeinate` — remain in the agent's session on the pane's tty. The whole Bash-tool subtree inherits that session, and a `run_in_background` call has the identical shape.

**Why the design changed.** `getsid(descendant) != getsid(agent_pid)` is one libc call: no `/proc` parsing, no string matching, identical on macOS and Linux, and immune to any change in what Claude Code puts on the command line. It is strictly better than the argv match on every axis that mattered, and it is agent-agnostic in a way an argv table can never be. The argv material was **kept, demoted to a cross-check**, because the two predicates fail on disjoint sets and together catch more than either alone.

**What else moved.** M2 and its test-plan entry now describe the structural discriminator; the M6a canary's justification shifted from "the argv rotted" to "Claude stopped `setsid`-ing, or an MCP server started detaching" — still worth having, different reason; the Risks section was rewritten around what the measurement actually supports, with the MCP-server-that-detaches false positive named as the load-bearing assumption (measured once, on one machine, with one MCP configuration); and Open Question 2 shrank from "is the primary test string-based?" to "how much veto does a secondary confirmation get?". Open Question 3 (`pane_hook_session_id` replacement) is unaffected.

**The hazard this created and where it is recorded.** A bare "no controlling terminal" test collapses in CI, where the agent itself has no ctty, so every descendant matches. That is now called out in the body under its own heading, in M2's milestone text, and as a third assertion in M2's test plan entry — not as a footnote, because it is exactly the kind of thing that passes on a developer machine and asserts nothing where it runs.
