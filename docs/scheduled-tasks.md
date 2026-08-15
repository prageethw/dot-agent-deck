---
sidebar_position: 5.7
title: Scheduled Tasks
---

# Scheduled Tasks

Scheduled tasks let you say *"every weekday at 09:00, run this prompt in this directory"* and have the result land in the deck where you can read it after a notification — no opening a terminal at the right time, `cd`-ing to the right place, and pasting the prompt by hand.

Each task pairs a **schedule** (when it runs) with a **working directory and a prompt** (what runs). When the schedule comes due, the deck opens a tab in that directory and hands the prompt to a fresh agent — or to an orchestration, if that directory defines one — exactly as if you had started it yourself from the new-deck dialog.

> **The scheduler lives in the daemon**
>
> Scheduling runs inside the long-lived **daemon**, not the TUI, so fires keep happening after you close the deck window. While the daemon is stopped nothing fires — and a fire that comes due during the downtime is **not** run later (there is no catch-up) — but your schedules **resume on the next daemon start**, because their definitions live on disk. See [Daemon must be running](#daemon-must-be-running).

## Creating a scheduled task

You can create and edit schedules three ways, listed below easiest-first.

### 1. Agent-driven authoring (primary)

The easiest door: converse with an agent that builds the entry and runs the commands for you. There are two ways in, and both open the same guided authoring session:

- **From the new-deck / new-pane dialog** — open it (`Ctrl+n`), confirm a directory, and cycle the **Mode** field to the end — past your project's workload modes — to the built-in **`schedule`** option (marked as an *authoring session*).
- **From the Scheduled Tasks dialog** — press **`s`** on the dashboard, then **`a`** / **`[Add]`** to author a new one (or **`e`** / **`[Edit]`** to start from an existing row's values). This now mirrors the `Ctrl+n` flow: first a **directory picker** (the dir you choose becomes the authoring session's working directory, and is pre-seeded as the schedule's own working directory), then a small **New Schedule** / **Edit Schedule** form with a **Dir** and a free-text **Command** field (pre-filled from your `default_command`). Confirm to start the authoring session in that directory running that command; **`Esc`** / **`[Cancel]`** returns you to the dialog.

Either way a throwaway authoring session opens — running your chosen agent command, which defaults to your configured [`default_command`](configuration.md#default-command) and falls back to `claude` when that is unset — and walks you through it. It:

- asks you for the fields (name, cron, working dir, command, prompt, …);
- asks for the **command that launches your agent** — it must result in a `claude`, `opencode`, `pi`, `codex`, or `devin` process, either directly (`claude`, `claude --model opus`, `opencode --model gpt-4o`, `pi`, `codex`, `devin`) or via a project wrapper that ends up launching one (`devbox run agent-new`, `npm run agent`). Those are the CLIs the deck integrates with for **live status tracking**; a command that doesn't result in one still runs but gets no status tracking, so the agent won't suggest unrelated CLIs (e.g. `gemini`). The command is **required** (there is no `$SHELL` fallback);
- lets you **test the prompt in the same session** ("run it now, show me") before committing;
- **confirms the full entry** with you, then calls `schedule add` (or `schedule update` on the edit path).

The agent writes the entry for you, so you don't have to get the cron syntax or prompt formatting right by hand. When it's done it tells you that **this authoring pane existed only to create the schedule and can be closed** — when the schedule later fires, a single-agent run **appears live in its own pane** on the deck, while an orchestration-targeted run opens in its tab when the deck is (re)opened.

This is also where the [management dialog](#management-the-scheduled-tasks-dialog) sends you for **add** and **edit**.

### 2. The `schedule` CLI

Scriptable, and the fast path for trivial edits:

```bash
# Add a task (validated, then saved to the global file). --command is REQUIRED:
dot-agent-deck schedule add \
  --name morning-digest \
  --cron "0 9 * * MON-FRI" \
  --working-dir ~/scheduled/morning-digest \
  --command claude \
  --prompt "Generate the morning brief. Notify when done." \
  --enabled true

# Update fields of an existing task (no --new-name; rename is forbidden):
dot-agent-deck schedule update --name morning-digest --cron "0 8 * * MON-FRI"

# Pause / resume without deleting:
dot-agent-deck schedule disable --name morning-digest
dot-agent-deck schedule enable  --name morning-digest

# Inspect:
dot-agent-deck schedule list

# Fire now, or ask a running daemon to re-read the file:
dot-agent-deck schedule run-now --name morning-digest
dot-agent-deck schedule reload

# Remove the definition (does NOT close an open tab for the task):
dot-agent-deck schedule remove --name morning-digest
```

| Subcommand | Purpose |
|---|---|
| `add` | Append a new task. **`--command` is required** (no `$SHELL` fallback). |
| `update` | Change fields of an existing task by `name`. No rename. |
| `remove` | Delete a task **definition** (leaves any open tab alive). |
| `list` | Show each task with its enabled/disabled state and next-fire time. |
| `enable` / `disable` | Flip `enabled` without deleting the definition. |
| `run-now` | Fire the task immediately via the running daemon. |
| `reload` | Tell the running daemon to re-read `schedules.toml`. |

After any command that changes a task, the CLI tells a running daemon to reload, so it picks the change up immediately. If no daemon is running that's fine — the change loads on the next `daemon serve`.

### 3. Hand-edit the file

The TOML is human-readable; edit `~/.config/dot-agent-deck/schedules.toml` directly (see the [reference](#reference-the-global-config-file) below for the format), then run `dot-agent-deck schedule reload` (or just let the next daemon start pick it up).

## Management: the "Scheduled Tasks" dialog

Press **`s`** on the dashboard (lowercase; the legacy uppercase **`S`** also works) to open the **Scheduled Tasks** manager — your one place to see and manage every schedule. Its **`[Scheduled Tasks s]`** button is **always present on the dashboard**: it doesn't wait for a schedule to exist, because the manager's **`[Add]`** action is itself how you create the first one. The dialog lists your schedules and lets you act on them, but you don't edit fields inside it — changes flow through the authoring agent, the CLI, or the file.

Rows are **click-selectable**. Each row shows the task **name**, a **status** indicator, and its **next-fire** time:

| Status | Meaning |
|---|---|
| `live` | The task currently has a live reused tab/agent. |
| `idle` | Enabled, but no live tab right now. |
| `disabled` | `enabled = false`. Its next-fire cell shows `—`. |

Actions — the footer buttons mirror the keys, shown as `[Add a]` `[Edit e]` `[Delete d]` `[Run now r]`:

| Key / Button | Action |
|---|---|
| `a` / `[Add a]` | **Add** — opens a **directory picker → New Schedule form** (Dir + free-text Command), then spawns the seeded authoring agent in that directory. |
| `Enter` / `e` / `[Edit e]` | **Edit** the selected row — opens the same **directory picker → Edit Schedule form** (picker starts at the row's directory), then spawns the seeded authoring agent **pre-filled** with the row's current values (it calls `schedule update`). |
| `d` then `y` / `[Delete d]` | **Delete** the selected row's **definition only** (a confirmation appears first). It does **not** close an open/running tab for that schedule — deleting a schedule must not nuke a conversation you're reading. |
| `r` / `[Run now r]` | **Run now** — fire the selected task immediately. |
| `j` / `k` | Move the selection. |
| `Esc` / `q` / `s` | Close the dialog. |

**Edits take effect on the next fire.** Change a schedule's prompt — or any field that affects a fire (cron, working dir, command, `new_tab_per_fire`) — and the next fire uses the new values, not the ones from when you first created the task.

There is deliberately **no inline enable/disable toggle** and **no in-place field editing** — that keeps the terminal dialog simple. Pause a task via the agent, `schedule disable <name>`, or a file edit. **Rename is forbidden** via the edit path because `name` is the reuse-tab key; to rename, remove + add.

## Reference: the global config file

You rarely need to touch this directly — the doors above write it for you — but here is the on-disk format and every field, for hand-editing and debugging.

Schedule **definitions** live in a single global, per-user file:

```
~/.config/dot-agent-deck/schedules.toml
```

(honoring `$XDG_CONFIG_HOME` when set; override the path with the `DOT_AGENT_DECK_SCHEDULES` environment variable). It is **global** — not the per-project `.dot-agent-deck.toml` — because the daemon is global; which schedules are active must not depend on which directory you last launched the deck from.

Each task is a `[[scheduled_tasks]]` block:

```toml
[[scheduled_tasks]]
name = "morning-digest"
cron = "0 9 * * MON-FRI"
working_dir = "~/scheduled/morning-digest"
command = "claude"
prompt = """
Generate a brief: Barcelona weather forecast for today, plus the list of
GitHub issues opened in the last 24h across vfarcic/dot-ai and
vfarcic/dot-agent-deck. Notify when done.
"""
new_tab_per_fire = false
enabled = true
```

### Field reference

| Field | Type | Required | Description |
|---|---|---|---|
| `name` | string | yes | Unique id. Also the key that ties a task to its reused tab — see [Tab reuse](#tab-reuse). Renaming is forbidden (it would orphan an open reused tab); treat a rename as remove + add. |
| `cron` | string | yes | A **5-field POSIX** cron expression (`min hour day-of-month month day-of-week`), e.g. `0 9 * * MON-FRI`. Evaluated in **local time**. 6/7-field forms (with a seconds field) are also accepted. |
| `working_dir` | string | yes | Directory the fire spawns into. `~` and `$VAR` / `${VAR}` are expanded at load time; a relative path resolves against `$HOME` (never the authoring agent's cwd). Created with `mkdir -p` if missing. |
| `command` | string | **yes** | The agent command for the **single-agent** card (e.g. `claude`, `opencode`, `pi`, `codex`, or `devin`), mirroring the new-deck dialog's command field. **Required**: `schedule add` errors without it and the loader **rejects (skips) a command-less entry** — there is **no `$SHELL` fallback**. Required **universally**, including orchestration-target schedules: it is still validated at load, but **ignored at fire** when the target dir defines an `[[orchestrations]]` block (the orchestration's role commands win). |
| `prompt` | string | yes | The prompt delivered into the spawned agent (or the orchestrator role). |
| `new_tab_per_fire` | bool | no (default `false`) | `false` reuses one tab per task; `true` opens a fresh tab every fire. See [Tab reuse](#tab-reuse). |
| `enabled` | bool | no (default `true`) | `false` keeps the definition but stops it firing. |

> **Local time & daylight saving**
>
> Cron is evaluated in the host's **local time** — there is no timezone field. At a daylight-saving transition this means a fire may be **skipped** (the spring-forward hour never occurs) or **run twice** (the fall-back hour repeats). This is an accepted tradeoff of local-time scheduling; if you need exactness across a DST boundary, avoid scheduling inside the transition hour.

### What happens when a task fires

When a task fires, the scheduler reads the **target `working_dir`'s** `.dot-agent-deck.toml`:

- If it defines an **`[[orchestrations]]`** block → an **orchestration tab** is opened rooted at that directory and the prompt is delivered to the `orchestrator` role (the task's `command` is ignored here).
- Otherwise → a **single agent card** is opened, running `command`, and the prompt is delivered to it.

**The first prompt waits for the agent to be ready.** When a fire spawns a new agent, the deck waits for it to finish starting up before delivering the prompt, so nothing is lost while the agent is still coming up.

A single malformed `[[scheduled_tasks]]` entry never crashes the daemon or blocks the other (valid) entries — the bad entry is reported and skipped. A **command-less entry is one such rejected entry** (see the `command` field above).

## Tab reuse

Most scheduled tasks should **reuse** one tab, because you primarily learn about fires through notifications and open the deck to dig into a result only when you choose to.

- **Default (`new_tab_per_fire = false`)** — a task reuses the same tab/card each fire. Yesterday's weather output is replaced by today's. One weather tab, ever.
- **Opt-in (`new_tab_per_fire = true`)** — each fire opens a fresh tab, for audit-style tasks where you want per-fire history.

Tab reuse is tracked only while the daemon keeps running, so a daemon restart clears it — the first fire after a restart starts a fresh tab even when reuse is on.

### If a fire lands while you're typing

If a reuse fire lands while you are actively typing in that tab, the new prompt **waits** and is delivered once you pause (a short debounce, ~5s by default). If you are not typing, it is delivered immediately. The debounce window is tunable via the `DOT_AGENT_DECK_REUSE_DEBOUNCE_MS` environment variable (milliseconds).

## Daemon must be running

Scheduling depends on the daemon being up. The behavior on daemon stop / upgrade / restart / reboot is honest and documented:

- Stopping the daemon (`daemon stop`, `daemon restart`, an upgrade, or a crash) **terminates every running agent** and **wipes the in-memory reuse registry**.
- **There is no catch-up.** Fires that come due while the daemon is down are **not replayed** — an "every 09:00" task that was offline at 09:00 simply misses that day. There is no persistent queue and no last-fire timestamp.
- Schedule **definitions survive** because they are reloaded from the global `schedules.toml` the next time the daemon starts.
- The daemon is **lazy-spawned** by the next `dot-agent-deck` invocation and is **not** auto-respawned after it exits.

The daemon also auto-exits after a short idle window when there are no clients and no live agents — but a **registered enabled schedule keeps it alive**, so a daily task survives the gaps between fires (and the gap before its first fire) as long as the daemon isn't explicitly stopped.

## Dispatching agents onto open GitHub issues (`issue_dispatch`)

> **The task type itself is always available; only the guided creation UI is experimental.**
>
> A configured `issue_dispatch` task **always runs** — once the `[scheduled_tasks.issue_dispatch]` sub-table below is present (whether you hand-write it or author it with the CLI), the daemon fires it on schedule with no flag required. What *is* gated behind the `experimental` feature flag is the in-deck **guided `schedule: issues` authoring option** in the new-pane dialog (a convenience for building one of these tasks conversationally). To enable that option, set `experimental = true` under a `[features]` table in your `.dot-agent-deck.toml`, or launch with `DOT_AGENT_DECK_EXPERIMENTAL=1` (the environment variable wins over the file). Everything below describes the task type, which works regardless of the flag.

The examples so far run **one** prompt in **one** directory per fire. An **`issue_dispatch`** task is a specialized variant that, on each fire, looks at the **open GitHub issues of one repo** and spins up an agent **per issue** — so *"every weekday at 09:00, pull up to five open issues from `vfarcic/dot-ai` and start an agent on each"* becomes a single schedule instead of a morning of manual cloning, worktree-making, and prompt-pasting.

You turn an ordinary scheduled task into an issue-dispatch task by adding a `[scheduled_tasks.issue_dispatch]` sub-table to it. The shared fields (`name`, `cron`, `working_dir`, `prompt`, `enabled`) keep their meaning; the sub-table adds the GitHub-specific knobs:

```toml
[[scheduled_tasks]]
name = "Issues vfarcic/dot-ai"        # default-seeded to "Issues <repo>"
cron = "0 9 * * MON-FRI"              # 09:00 on weekdays, local time
working_dir = "~/dispatch"            # the workspace root — see "Where things land" below
prompt = "Work on issue {{issue_number}}"   # per-issue template; {{issue_number}} is substituted per issue
enabled = true

[scheduled_tasks.issue_dispatch]
repo = "vfarcic/dot-ai"               # ONE repo, "owner/name"
max_per_run = 5                       # hard cap on how many issues a single fire dispatches
# label = "agent-eligible"            # optional: only issues carrying this label
# query = "is:open no:assignee"       # optional: advanced gh search override
# triage = true                       # optional: opt in to per-issue triage — see "Opt-in triage" below
```

> **`command` is optional to load, and only matters for orchestration-less clones**
>
> Unlike a plain scheduled task, an `issue_dispatch` task does **not** need a `command` to load — `validate_task` returns before reaching the command check for any task carrying an `issue_dispatch` sub-table, unlike the required-field #127 single-spawn path. If you set one anyway it is not inert: it becomes the resolved `default_command` and wins over the global [`default_command`](configuration.md#default-command) for any dispatched clone whose `.dot-agent-deck.toml` defines no `[[orchestrations]]` block. A clone that does define one ignores it entirely — the orchestration's role commands win regardless.
>
> **`new_tab_per_fire` has no effect here either, for an unrelated reason.** The `issue_dispatch` callback returns before reaching the `new_tab_per_fire`/tab-reuse branch further down the same function — each dispatched issue already gets its own worktree and card keyed by issue number, so the field is never read for this task type. Setting it is a no-op dressed up as a decision.

Rather than hand-write the sub-table, you can author the same task with the validated CLI — pass `--repo` (and the optional `--max-per-run` / `--label` / `--query` / `--triage`) and omit `--command`:

```bash
# --label and --triage are both optional — see "Opt-in triage" below for --triage.
dot-agent-deck schedule add \
  --repo vfarcic/dot-ai \
  --max-per-run 5 \
  --name "Issues vfarcic/dot-ai" \
  --cron "0 9 * * MON-FRI" \
  --working-dir ~/dispatch \
  --prompt "Work on issue {{issue_number}}" \
  --label agent-eligible \
  --triage
```

A malformed `--repo` (not an `owner/name` slug) is rejected before anything is written. The CLI validates, writes the global config atomically, and triggers a live daemon reload — exactly as for a plain task.

`--triage` is a bare flag (present = on) and, like `--max-per-run` / `--label` / `--query`, is only meaningful alongside `--repo`; it maps onto the `triage` field described below.

### What a fire does, issue by issue

When an `issue_dispatch` task fires it:

1. **Provisions the repo** under the workspace root — clones it on the first run, `git fetch` + fast-forward `pull` on later runs.
2. **Enumerates open issues** via `gh` (honoring the optional `label` and `query`), then takes the first `max_per_run` in the order GitHub returns them. Nothing past the cap is touched.
3. For each candidate issue, **creates a per-issue worktree** on a branch named `agent/issue-<n>`.
4. **Spawns an agent** rooted in that worktree and delivers your `prompt` with `{{issue_number}}` substituted. Because the agent runs *inside* the issue's worktree, it can deduce the repo and issue from its surroundings — the issue number alone is enough context. Set `prompt = "/prd-full {{issue_number}}"` to drive your own skill instead.

Each issue is handled in its **own error boundary**: if one issue fails (a `gh` rate-limit, a clone error), it is reported in the daemon log and the run **continues** with the remaining issues — one bad issue never aborts the rest.

### Where things land

Everything for a task lives under the task's own `working_dir` (the **workspace root**), so nothing is written to the daemon's launch directory:

| Path | What |
|---|---|
| `<working_dir>/<name>` | The **clone** of the repo (created once, reused and pulled thereafter). |
| `<working_dir>/<name>/.worktrees/issue-<n>` | The **per-issue worktree** for issue `<n>`. |
| `agent/issue-<n>` | The **branch** each worktree checks out. |

### Idempotency: three signals, one explicit claim

There is no separate state file. Before dispatching an issue, the task checks three signals, in order, and **skips** the issue if any one of them is true:

1. its `.worktrees/issue-<n>` worktree **already exists on disk** (the primary signal — the filesystem itself records which issues are in flight, so re-running a dispatch, whether the cron fires again or you press **Run now**, does not double-dispatch work already underway);
2. the issue carries the **`in-progress` label** (the secondary signal, PRD #421);
3. an **open PR** already has head branch `agent/issue-<n>` (the tertiary signal — a deterministic check, not fuzzy `Closes #n` body parsing).

The worktree and label checks are both read for free — the filesystem check costs no I/O, and the label rides along on the issue list already fetched to find candidates — so both are checked before the PR probe, the one signal that needs its own GitHub call; a transient failure there can then never mask an already-decided worktree/label skip as a failure. The worktree and PR signals are **inferred** — they are side-effects of a dispatch having happened, read back off the filesystem or GitHub's PR state. The label is different: it is an **explicit claim**, and it is honoured no matter who applied it — this deck's own prior dispatch, a human working the issue by hand, or an external tool. Nothing compares "is this my own claim?"; the label alone is enough to skip. Because the label is checked ahead of the PR probe, an issue that is both labelled and has an open PR reports the label cause.

**On a successful dispatch** — after the worktree is created and the agent is spawned — the task writes the `in-progress` label onto the issue and appends a comment naming the **claimant**: the scheduled task's name, the host it ran on, and a timestamp. The label is the machine-readable signal dispatch actually gates on; the comment is the human-readable record of *who* made the claim. Provenance never changes the skip decision — it only makes an already-decided skip legible when you go looking at the issue. If the label write or comment post itself fails — most commonly a `gh` token with no write access to issues/labels — the dispatch is still reported as a success (the worktree was created and the agent spawned), but a separate notification reports that the claim could not be written; see "Requirements & caveats" below for what that means in practice.

**A failed dispatch leaves the issue completely unmarked**, deliberately. Labelling before the outcome is known would let a failed dispatch produce a false claim — permanently un-dispatchable, since nothing would ever clear a label nobody is coming back to fix. The worktree/PR signals don't have this problem: a fire that fails after creating the worktree still leaves the worktree in place, so the *next* fire's worktree-exists check reclaims the same issue rather than skipping it forever.

Skipping is always **reported** in the daemon log — never silent: a stray `in-progress` label left by a human silently starves the backlog, and from the outside that looks like nothing happening rather than like an issue being deliberately excluded. Each skip names its cause. There are four causes; the label cause renders two ways depending on whether a claimant could be found:

| Cause | Rendered reason |
|---|---|
| the worktree already exists | `the worktree already exists` |
| an open PR already targets the branch | `an open PR already targets this branch` |
| labelled `in-progress`, claimant known | ``labelled `in-progress` ({comment})`` |
| labelled `in-progress`, no claimant recorded | ``labelled `in-progress`, no claimant recorded`` |
| a concurrent dispatch won the race | `a concurrent dispatch claimed the worktree first` |

The last row is a benign race, not a failure: two fires can both pass the idempotency check before either has created its worktree, and only one wins — the loser skips rather than erroring. "No claimant recorded" covers a best-effort claimant lookup (reading the issue's comments back to find the claim) that itself failed to find one — the label is still honoured as a claim; the skip is simply reported without saying who made it.

A skipped issue is left alone. Concurrency falls out of the same mechanism: a fire only fills the slots that earlier dispatches vacated by being closed, up to `max_per_run`.

### Opt-in triage

Set `triage = true` in the `[scheduled_tasks.issue_dispatch]` sub-table (or pass `--triage` to `schedule add`) to have each dispatched issue triaged by the agent working it, instead of landing with no priority/size signal at all. It is **off by default**.

When triage is on, a fire first ensures a seven-label vocabulary exists on the repo (`gh label create --force`, with a fixed colour and description per label, so re-running converges on the same colour/description each label already has rather than randomizing them — a bare `--force` with no `--color` would pick a fresh random colour on every fire): `priority-high`, `priority-medium`, `priority-low`, `size-high`, `size-medium`, `size-low`, and `needs-triage`. The `in-progress` claim label (above) gets the same treatment, unconditionally, so the claim never silently fails on a repo that has never carried it. It then appends a triage instruction to the prompt of **every issue it actually dispatches** — a skipped issue never sees it.

**The deck itself never classifies anything.** The instruction asks the spawned agent to apply one size label and, when it can tell, one priority label, using its own `gh` calls — the same way it would apply any other label while working the issue. Under uncertainty the agent is instructed to apply `needs-triage` and leave priority unset rather than guess, because a **wrong** priority is worse than an **absent** one: an absent priority is visibly unclassified, while a wrong one is indistinguishable from a considered judgment until someone checks.

Two things this deliberately does **not** do, so nobody infers a behavior that isn't there:

- It does not triage your whole backlog — only the issues a fire actually dispatches ever get labeled or see the instruction.
- Recording a priority does not currently affect dispatch order. `max_per_run` still takes issues in the order `gh` returns them; nothing sorts by `priority-*` yet.

### Cleanup: closing a tab tries to remove its worktree

Dispatched tabs/cards persist until **you** close them — you stay in control of review, further iteration, or discarding the work. **Closing a dispatched tab/card attempts to remove its worktree** (`git worktree remove`); the **clone is preserved** (only the per-issue worktree ever goes away). A **clean** worktree — no uncommitted or untracked changes — is removed and its slot is freed immediately. A **dirty** worktree, or one whose status couldn't be checked, is left in place instead of discarding the work, and you're notified where it's sitting; the slot stays claimed (subsequent fires still skip that issue) until you resolve the worktree's contents and run `worktree reclaim` yourself. Until the worktree is actually removed — by the close, or by a later reclaim — it keeps "claiming" its issue.

> **Requirements & caveats**
>
> - The **GitHub CLI (`gh`) must be installed and authenticated** — all GitHub access (issue enumeration, the PR idempotency check, and the initial clone) goes through it.
> - **The `gh` token needs write access to issues and labels** (`repo` scope, or the finer-grained `issues:write` equivalent), not just read — PRD #421's claim label/comment and, when `triage` is on, the label vocabulary are both writes. A read-only token still dispatches issues correctly, but every claim attempt fails: the dispatch is reported as a success (the worktree was created and the agent spawned), and a separate notification reports that the label/comment could not be written. Without a write-capable token, two decks pointed at the same repo can dispatch the same issue repeatedly, since neither the worktree signal (machine-local) nor the label signal (never written) stops the second one.
> - **GitHub only, for now.** Issue dispatch is built on the GitHub CLI, so other forges (GitLab, Gitea, Bitbucket, …) aren't supported yet. If you'd like dispatch for another provider, please [open an issue](https://github.com/vfarcic/dot-agent-deck/issues) — it helps us gauge demand.
> - Like every scheduled task, this runs in the **daemon**: fires that come due while the daemon is down are **not** replayed (see [Daemon must be running](#daemon-must-be-running)).
> - **Detaching vs. stopping.** *Detaching* (closing the deck/TUI window) leaves the dispatched agents and their tabs **running in the daemon** — reconnect and they're still there. Only **stopping the daemon** (`daemon stop`, a restart, an upgrade, or a crash) terminates them. After a stop, the per-issue worktrees **remain on disk** (so they keep claiming their issues and the scheduler won't re-dispatch them), but the tabs themselves are **not** auto-restored on the next launch. Run `git worktree remove` (or reopen and close the tab) to release a slot manually.

## Worked examples

### A daily single-agent digest

```toml
# ~/.config/dot-agent-deck/schedules.toml

[[scheduled_tasks]]
name = "morning-digest"
cron = "0 9 * * MON-FRI"          # 09:00 on weekdays, local time
working_dir = "~/scheduled/morning-digest"
command = "claude"                 # required — the single-agent card's command (claude, opencode, pi, codex, or devin)
prompt = """
Generate a brief: Barcelona weather forecast for today, plus GitHub issues
opened in the last 24h across vfarcic/dot-ai and vfarcic/dot-agent-deck.
Notify when done.
"""
new_tab_per_fire = false           # reuse one tab (default)
enabled = true
```

`~/scheduled/morning-digest` has no `.dot-agent-deck.toml`, so the fire opens a single `claude` card there and delivers the prompt.

### A scheduled task that targets an orchestration

If the target directory defines an orchestration, the fire opens an orchestration tab and delivers the prompt to the `orchestrator` role. The schedule's `command` is **still required** (every schedule needs one to load) but is **ignored at fire** — the orchestration's role commands win.

`~/work/release-audit/.dot-agent-deck.toml`:

```toml
[[orchestrations]]
name = "release-audit"

[[orchestrations.roles]]
name = "orchestrator"
command = "claude"
start = true

[[orchestrations.roles]]
name = "reviewer"
command = "claude"
```

`~/.config/dot-agent-deck/schedules.toml`:

```toml
[[scheduled_tasks]]
name = "weekly-release-audit"
cron = "0 8 * * MON"               # 08:00 every Monday
working_dir = "~/work/release-audit"
command = "claude"                 # required to load; ignored at fire (the orchestration's role commands win)
prompt = """
Audit everything merged into main since last Monday: changelog accuracy,
breaking changes, and follow-up issues to open. Delegate the per-area review.
"""
enabled = true
```
