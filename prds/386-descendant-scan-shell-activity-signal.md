# PRD #386: A shell-activity signal that actually fires — descendant-process scan

**Status**: Not started — PRD written, no implementation
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

Replace `tcgetpgrp` with a **descendant-process scan**, and drop the `pane_hook_session_id` gate so bare shell panes are observed too.

The whole of the rest of #370 is kept as-is: the 500 ms edge-triggered `run_shell_activity_monitor` poll, the synthesized `EventType::ShellBusy`/`ShellIdle` riding the same broadcast pipeline real hooks use, and `apply_event`'s precedence rules (`ShellBusy` only promotes `Idle`/`Unknown`; `ShellIdle` only reverts a promotion this mechanism made; any real event clears `shell_synthetic_working`). Only the *primitive underneath* changes. That is why this repairs Defect 1 **with no change to hook semantics at all** — nothing about `Stop → Idle` moves — and why it covers foreground *and* background commands, all agents, and bare shell panes in one mechanism.

### The discriminator

The #370 notes feared a descendant scan was unviable, because a Claude pane *always* has long-lived children — `npm exec @upstash/context7-mcp`, `engram mcp`, `caffeinate -i -t 300` — so a naive "has descendants" test is `true` 100% of the time and would pin every pane at `Working` forever. The measurement in `.dot-agent-deck/hook-silence-notes.md` [14] supplies the missing discriminator: **those children sit inside the pane's own process group, on the pane's own tty; the Bash-tool child does not.**

Measured, during real runs:

```
40561  40282  40561  ??  Ss  /bin/zsh -c source …/.claude/shell-snapshots/… && eval 'sleep 420; ls -1'   <- background call
43253  42838  43253  ??  Ss  /bin/zsh -c source …/.claude/shell-snapshots/… && eval 'ping -c 200 …'      <- foreground call
 6456   6444   6387  ttys019 S+  npm exec @upstash/context7-mcp                                          <- MCP server
 6458   6444   6387  ttys019 S+  engram mcp --tools=agent                                                <- MCP server
 6571   6444   6387  ttys019 S+  caffeinate -i -t 300                                                    <- keep-awake
```

A pane is **busy** iff its PTY child has a transitive descendant satisfying all of:

1. **Descendant.** Reachable from `RunningAgent.child.process_id()` by following `ppid` (the pane's PTY child itself does not count).
2. **Detached from the pane's terminal.** The process has **no controlling terminal** (`ps` TTY column `??` on macOS, `?` on Linux). Corroborating sub-condition, same measurement: it is a **session leader** (`ps` STAT contains `s`). This condition alone excludes all three long-lived children measured, because every one of them stays on the pane's tty inside the pane's pgroup.
3. **Shell-tool argv shape.** Its command line is an interpreter invoked with `-c` whose command string references the agent's shell-snapshot directory and `eval` — for Claude, `/bin/zsh -c source …/.claude/shell-snapshots/… && eval '<command>'`.

Foreground and background Bash calls have the **identical** process shape, so a single signal covers both — which is why this subsumes the narrower "teach the deck about outstanding background shells" alternative (parsing `tool_input.run_in_background` and the `tool_response` "moved to the background" text), and does so without English-text matching and without the unsolved problem of *when to clear* such a marker.

**Design point to settle at implementation start (see Open Questions):** condition 3 is what was measured, but conditions 1∧2 alone already discriminate correctly against every process observed, and condition 3 is the part coupled to another product's internals. The recommendation below is that 1∧2 form the signal and 3 be an additional, per-agent, *configurable* filter — required for agents whose shape has been measured, absent for those it has not — rather than a hard-coded universal gate.

## Relationship to #370

**This PRD supersedes #370's mechanism. It does not supersede its goal.**

#370's success criteria were right and are carried forward verbatim into this PRD: a pane running an agent that shells out to a long-running command shows `Working` for the duration; a genuinely idle pane is not falsely `Working`; a more specific agent-emitted status (`WaitingForInput`, `Error`) is never silently overridden; no measurable per-tick performance regression. What was wrong was the **instrument** — `tcgetpgrp` answers "who owns the terminal", and an agent that spawns its children on pipes never cedes the terminal, so the answer is correct and useless.

Everything #370 built downstream of the primitive is **kept, not rewritten**: the wire types (`EventType::ShellBusy`/`ShellIdle` and the `#[serde(other)] Unknown` catch-all), `PROTOCOL_VERSION` 7, the daemon poll task, the precedence rules, and their unit tests. This PRD replaces one function's body and deletes one gate.

`prds/370-shell-activity-status.md` gets a Work Log entry recording that its mechanism never fired, that M5/M6 were never completed, and that it is superseded here. Its history stays intact and is not rewritten.

## Scope

**In Scope**

- A descendant-process scan primitive in `src/platform/proc/` (Unix), with the discriminator above; `None` on Windows, matching `foreground_pgid`'s existing shape.
- Rewiring `RunningAgent::shell_foreground_busy` (or a successor named for what it now measures, e.g. `shell_activity_busy`) onto that primitive. `foreground_pgid` itself may stay as a Unix helper or be retired — it has no other caller.
- **Dropping the `pane_hook_session_id` gate** (`src/daemon.rs:897`) so a bare shell pane that never emitted an agent hook event is observed, plus whatever session resolution that requires (see Open Questions — this is the one piece with a genuine design choice in it).
- A per-agent table for condition 3's argv shape, if the recommendation there is accepted.
- Test coverage per the Test Plan below, including a real-agent PTY-attached test that hand-seeds nothing.
- Changelog fragment; a note in `docs/develop/` if the process-shape coupling warrants one.

**Out of Scope**

- **Windows process walking.** The primitive returns `None` on Windows, exactly as `foreground_pgid` does today. Documented gap, not solved here.
- **Any change to hook semantics.** `Stop → EventType::Idle → SessionStatus::Idle` stays exactly as it is. This PRD does not touch `src/hook.rs`'s mapping, does not add a background-shell marker, and does not change what any adapter reports. The repair happens entirely in the `ShellBusy` promotion that already exists.
- Setting `BASH_DEFAULT_TIMEOUT_MS` in the agent environment to widen Claude's cap — a workaround that changes agent behaviour the user did not ask for.
- PRD #234's vt100 screen-diff mechanism.
- Re-litigating #370's precedence rules, poll cadence, or wire format.

## Technical Approach

**The primitive.** `pub fn descendant_shell_activity(root_pid: i32, shapes: &[ShellToolShape]) -> Option<bool>` in `src/platform/proc/unix.rs`, `None` unconditionally in `windows.rs`. It needs, for every process on the machine: `pid`, `ppid`, controlling-tty presence, session-leader flag, and full argv. Two implementation routes:

- **Route A (recommended first cut): one `ps -Ao pid,ppid,stat,tty,args` per poll**, parsed once into a table and reused for *every* pane in that poll. Identical invocation on macOS and Linux, no new dependency, and one fork/exec per poll cycle rather than per pane. Cadence relaxes from #370's 500 ms to 1 s to keep the cost obviously negligible.
- **Route B (optimization, behind a profiling gate): native enumeration** — `/proc/<pid>/{stat,cmdline}` on Linux, `sysctl(KERN_PROC_ALL)` + `KERN_PROCARGS2` on macOS via the `libc` dependency already declared Unix-only in `Cargo.toml`. No subprocess at all, but two platform-specific implementations to maintain.

Start with A, measure, and only take B if the measurement says so. M5 exists to make that a real measurement rather than an assumption.

**The scan.** Build a `ppid → children` index once per poll, walk down from each pane's PTY child pid, and stop at the first descendant matching the discriminator. Depth is bounded in practice (agent → shell → command) but the walk must carry a visited set: a `ppid` table sampled non-atomically can contain a cycle after PID reuse.

**Dropping the gate.** `run_shell_activity_monitor` currently skips a pane when `pane_hook_session_id` returns `None`, because it needs *some* `SessionState` to update. With the gate gone, a pane with no hook session still needs one. Options are laid out in Open Questions; the recommendation is a fallback chain — `pane_hook_session_id` → `pane_session_id` → ensure a placeholder session (`insert_placeholder_session` already keys these as `pane-<pane_id>`, `src/state.rs:2555`) — so the existing agent-pane path is bit-for-bit unchanged and only the previously-skipped case gains behaviour.

## Success Criteria

- A pane whose agent runs a Bash command longer than Claude's 120 s cap reads `Working` for the **whole** command, not for the first two minutes. Measured on the real path, not on a hand-seeded state.
- A pane whose agent runs a long *foreground* command (under the cap) also emits `ShellBusy` — the signal fires on the process shape, not on the cap.
- A pane sitting at an agent's idle prompt, with MCP servers and `caffeinate` alive as children, reads `Idle` — no false `Working`.
- A bare shell pane that never emitted an agent hook event shows `Working` while a foreground command runs, with nothing hand-seeded.
- A more specific agent-emitted status (`WaitingForInput`, `Error`, `Thinking`) is never overridden by this signal.
- No measurable daemon overhead regression from the scan.

## Milestones

Each is independently testable; the test that proves each one is named in the Test Plan.

- [ ] **M1 — Process-table primitive.** Enumerate `pid`/`ppid`/tty/session-leader/argv on Unix (Route A), `None` on Windows. Cycle-safe descendant walk. No deck wiring yet.
- [ ] **M2 — The discriminator.** Conditions 1∧2 (descendant, no controlling terminal, session leader) plus the per-agent condition-3 argv shape. Pure classification over a process table — testable against both real spawned processes and captured fixtures.
- [ ] **M3 — Wire it into the pane primitive.** `RunningAgent::shell_foreground_busy`'s body swaps to the descendant scan; `shell_foreground_busy_snapshot` and `run_shell_activity_monitor` are otherwise untouched. This is the milestone at which an agent pane starts producing `ShellBusy` for the first time.
- [ ] **M4 — Drop the `pane_hook_session_id` gate.** Session resolution falls back per the chain above so a bare shell pane is observed. Agent-pane behaviour provably unchanged.
- [ ] **M5 — Overhead measurement and cadence.** Measure the poll's cost with a realistic process table and pane count; confirm or revise the 1 s cadence; decide Route A vs Route B on the number, and record the number in this PRD.
- [ ] **M6 — Real-agent proof and rot canary.** The two real-agent PTY tests in the Test Plan (short foreground call; >120 s call crossing the cap). Neither hand-seeds a `SessionStart`.
- [ ] **M7 — Docs, changelog, cross-version check.** Changelog fragment; a `docs/develop/` note on the process-shape coupling and how it is detected when it rots; CLAUDE.md rule 12 cross-version manual test (see below).

## Test Plan

#370's lesson is precise and worth stating as the standard this plan is held to: **a green suite around a mechanism nothing feeds is indistinguishable from a working feature.** #370 had a passing end-to-end test, passing precedence tests, and a shipped feature that fired in zero real configurations. Every test below is therefore stated as *what it would prove in a real pane*, and the two that carry the burden of proof drive a real agent with nothing seeded by hand.

**M1 — process-table primitive (fast tier, L1).** Spawn a real child from the test, on pipes, via `setsid`, as a grandchild of a test-owned process; assert the primitive finds it as a descendant, reports no controlling terminal and session-leader true, and returns its full argv. Assert the walk terminates on a synthetic table containing a `ppid` cycle. *Proves in a real pane*: nothing on its own — this is a mechanism test, and #370's failure was exactly a correct mechanism test attached to nothing. It is included so a later failure localises, not as evidence the feature works.

**M2 — discriminator (fast tier, L1).** Two captured fixture tables from the diagnosis notes: one containing the measured Bash-tool child (`/bin/zsh -c source …/shell-snapshots/… && eval …`, TTY `??`, `Ss`) plus the three measured long-lived children (`npm exec @upstash/context7-mcp`, `engram mcp`, `caffeinate -i -t 300`, all on the pane tty in the pane pgroup), and one containing only the long-lived children. Assert `true` for the first and `false` for the second, and assert `false` for the second **with condition 3 disabled**, pinning the claim that conditions 1∧2 alone already exclude the MCP servers. *Proves in a real pane*: that the false-positive case that would pin every pane at `Working` forever is excluded — against processes actually observed in a live deck, not invented ones.

**M3 — pane primitive, real process (fast tier, L1 mechanism test).** Spawn a real PTY pane running a shell, have it launch a genuine detached session-leader child on pipes with the Bash-tool argv shape, and assert `shell_foreground_busy` flips `false → true → false` across that child's lifetime. Explicitly the test #370 never had: **the pane's child is on pipes, off the PTY, exactly as a real agent's is.** *Proves in a real pane*: that the primitive answers correctly for the process topology that made #370 useless — but still with a stand-in, so it is not sufficient on its own.

**M4 — gate removal (fast tier, daemon integration).** Adapt `shell_activity_monitor_reflects_a_real_foreground_command` (`src/daemon.rs:1261`) with **the hand-seeded `SessionStart` deleted**, so a bare `/bin/sh` pane running a real foreground command flips `Idle → Working → Idle` with nothing seeded. A second test asserts an agent pane that *does* have a hook session still resolves to that same session id and not to a placeholder — the regression this milestone could plausibly cause. *Proves in a real pane*: that the bare-shell-pane case works without the one step no real pane performs. This is the single most direct answer to #370's "it passed only because the test constructed the state".

**M6a — real agent, short foreground call (e2e tier, PTY-attached, real agent, rot canary).** A real interactive Haiku Claude agent in a real spawned pane (`prepare_claude_home` + per-folder trust + `--allowedTools Bash`, following `scheduler/dispatch/013`), prompted to run a ~20-second foreground command against a uniquely-named sentinel fixture file. Assert a `ShellBusy` event is emitted for that pane **during** the call. The status is already `Working` from `ToolStart` here, so the assertion must be on the event, not the badge — otherwise the test passes with the signal dead, which is precisely how #370 shipped. *Proves in a real pane*: that the argv shape in condition 3 still matches what Claude Code actually spawns today. **This is the rot detector.** When Claude changes its shell-snapshot invocation, this test goes red — cheaply, in ~20 s, rather than silently degrading in the field.

**M6b — real agent, call crossing the 120 s cap (e2e tier, PTY-attached, real agent, user-visible).** The same rig, prompted to run a >120 s foreground command under **default** Bash settings — no `run_in_background`, no `timeout` parameter — reproducing `sbx4` exactly. Assert that the pane's rendered badge reads `Working` at a sample taken **after** the `Stop`-driven `Idle` would have landed (measured at cap + ~7 s), and that the command is still running at that moment. Note for whoever writes it: `sleep` is unusable as the instrument — Claude Code blocks long `sleep` at the tool layer ("The system is blocking this command because it detects long sleep commands") and no `ToolStart` is emitted at all; the diagnosis used `ping -c N 127.0.0.1 > /dev/null` for real, non-sleep work. *Proves in a real pane*: **the reported bug, as the user sees it** — this is the CLAUDE.md rule 4 test that validates the feature as a user actually uses and sees it. It costs ~2.5 minutes of e2e wall clock; that is the price of proving the thing the PRD exists for, and it belongs in the pre-PR e2e tier (flaky-tolerant, credentialed, not run by CI).

**M6c — no false positive with a real agent idle (e2e tier, real agent).** The same rig, agent brought up and left at its idle prompt with its MCP servers and `caffeinate` alive, sampled after the poll interval: the pane must read `Idle`. *Proves in a real pane*: the M2 fixture claim, against the live process table rather than a captured one.

**M5 — overhead (measurement, recorded not asserted).** Poll cost against a realistic table, reported as a number in this PRD. Not a pass/fail test; a threshold assertion here would be a flake generator on a loaded machine.

**Deliberately not claimed.** No test here proves the behaviour for Codex, OpenCode, or Pi. Only Claude's shell-tool shape was measured, and inventing fixtures for the others would manufacture exactly the false confidence #370 shipped with. See Risks.

## Risks

- **False positives are the central risk.** A signal that says "busy" when a pane is idle pins it at `Working` permanently and is *worse* than the stale `Idle` it replaces, because a permanently-wrong badge is unfalsifiable to the user. The three long-lived children measured (`context7-mcp`, `engram mcp`, `caffeinate`) are exactly this hazard, and condition 2 excludes all three. What is **not** excluded by measurement is the general case: any agent, plugin, or MCP server that happens to spawn a detached session-leader child on pipes would read as work. Condition 3 is the guard against that, and it is the condition most likely to rot. M6c is the only test that checks this against a live process table.
- **The discriminator is measured against Claude only.** Codex, OpenCode, Pi, and `dot-agent-deck wrap` are inference. Codex ships a Claude-compatible hooks engine and its own shell timeout, so the same shape is *likely*, but likely is not measured. If condition 3 is a hard universal gate, the signal is silently dead for every unmeasured agent — which is #370's failure mode repeating in a new place. This is the strongest argument for the per-agent recommendation in Open Questions.
- **Process-shape matching is coupled to another product's internals and will rot.** `/bin/zsh -c source …/.claude/shell-snapshots/… && eval …` is not a documented interface; Claude Code can change it in any release, and when it does, the signal goes quiet with no error anywhere. **How a test catches it:** M6a is the canary — a real agent, a real ~20-second Bash call, asserting the `ShellBusy` event fires. It cannot pass while the shape is stale, and it costs 20 seconds. It must not be replaced by a fixture-based test, which would pass forever against a shape nobody spawns any more. This risk is also the reason condition 2 carries the structural weight and condition 3 is refinement: if condition 3 rots, the fallback is over-triggering (detectable, noisy) rather than silence (undetectable).
- **Cost of the poll.** A `ps -A` per second on a busy machine is not free, and the deck's own workers routinely drive load averages above 20. M5 exists to put a number on it before this ships.
- **Session resolution when the gate is dropped.** Creating placeholder sessions for panes that never had one can surface new cards in the UI. The recommended fallback chain is ordered so the existing agent-pane path never changes, but this needs an explicit test (M4's second assertion), not an assumption.
- **`ps` output parsing.** Column widths, argv truncation, and STAT letters vary between macOS and Linux. Route A trades a dependency for a parsing surface; M1's tests must run on both, and CI covers Linux while development is on macOS.

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

**2 — Should condition 3 (argv shape) be a hard gate?** Recommendation: **no** — conditions 1∧2 are the signal, and condition 3 is a per-agent refinement, present for Claude (where it is measured) and absent for agents whose shape has not been. Rationale: a hard universal gate means the signal is silently dead for every unmeasured agent, which repeats #370's failure mode; a structural-only signal for those agents over-triggers at worst, which is visible and fixable. The counter-argument — that over-triggering pins a pane at `Working` and is worse than silence — is real and the user may weigh it the other way. Either way, condition 3 must be data (a per-agent table), not an inlined string literal.

**3 — Session resolution once the gate is dropped.** Recommended chain: `pane_hook_session_id` → `pane_session_id` → ensure a placeholder session keyed `pane-<pane_id>`. Alternative: keep the scan gated but relax the gate to "any pane the registry knows about", accepting that a shell pane's status has nowhere to land. The recommendation is preferred because the bare-shell-pane case is explicitly in scope; the risk it carries (new cards appearing) is what M4's second test exists to pin.

**4 — Route A (`ps` per poll) or Route B (native)?** Deferred to M5's measurement rather than argued in advance.

## Provenance

The measurements this PRD rests on were taken on 2026-08-06 and are recorded in full in `.dot-agent-deck/370-diagnosis-notes.md` (why #370's signal never fires; live `ps` from the user's running deck) and `.dot-agent-deck/hook-silence-notes.md` (the 120 s cap, the sbx3/sbx4 control pair, the process shapes, and the discarded instruments — a 150 ms `ps` sampler too coarse to catch hooks, a Python `pty.fork` probe that reproduced neither case, and `sleep` as a long-foreground instrument, which Claude Code blocks at the tool layer). Both files are gitignored working notes, not committed artefacts; the numbers that matter are reproduced in this PRD and in issue #386 so the record survives them.
