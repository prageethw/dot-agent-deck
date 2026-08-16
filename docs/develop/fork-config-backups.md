# Fork-only config backups

This repository is a fork of `vfarcic/dot-agent-deck` (the `upstream` remote). Two config files are intentionally divergent from upstream in this fork:

- `devbox.json`
- `.dot-agent-deck.toml`

Upstream ships the original per-model devbox scripts (`agent-orchestrator`, `pi-sol`, `oc-big`, `codex-big`, and so on). This fork swapped the worker-role devbox commands to two generic wrappers (`claude-sonnet-devbox`, `codex-devbox`) in PR #7. Because both files also exist upstream with different content, a future `git fetch upstream && git merge upstream/main` could touch them if upstream edits the same files — silently reverting or mangling the fork's customisation.

## The backup files

Alongside each live file sits a byte-for-byte snapshot of the current fork version:

- `devbox.json.fork-backup`
- `.dot-agent-deck.toml.fork-backup`

These are **not** an automatic guard — they do not *prevent* an upstream merge from overriding the live files. They exist so that an accidental override can be **detected and undone manually**. (A `.gitattributes merge=ours` guard was considered and deliberately not adopted; the manual backup-and-restore approach was chosen for its simplicity.)

## Check-and-restore procedure (run after any upstream sync)

After `git merge upstream/main` (or any upstream sync), diff each live file against its backup:

```bash
diff devbox.json devbox.json.fork-backup
diff .dot-agent-deck.toml .dot-agent-deck.toml.fork-backup
```

If a `diff` reports no output, the file is unchanged and nothing needs doing.

If a `diff` shows differences, the merge changed something. Then either:

- **Restore the fork's version** (if the fork's config should win):

  ```bash
  cp devbox.json.fork-backup devbox.json
  cp .dot-agent-deck.toml.fork-backup .dot-agent-deck.toml
  ```

- **Or review the diff first** — upstream's change might be worth keeping, in which case merge the two by hand rather than blindly restoring.

## Keeping the backups fresh

If a live file is intentionally changed again in the future (another fork-only config update), refresh the corresponding `.fork-backup` file **in the same commit**:

```bash
cp devbox.json devbox.json.fork-backup
# and/or
cp .dot-agent-deck.toml .dot-agent-deck.toml.fork-backup
```

Otherwise the backup goes stale and the `diff` check becomes noise — it would report differences that reflect an out-of-date snapshot rather than an upstream override.

## Scheduling `issue_dispatch` triage for this repo — a deliberately unturned-on entry

This section records the exact `~/.config/dot-agent-deck/schedules.toml` entry that would turn on `issue_dispatch` with `triage = true` against `prageethw/dot-agent-deck`, so the config is derivable in-repo rather than reconstructed each time someone asks. It is machine-local config, the same category as `devbox.json`/`.dot-agent-deck.toml` above, except that here the fork's decision is to **not** write it at all (issue #385 M3).

### What this would actually do

`issue_dispatch` is not a triage-only pass. Every fire clones/pulls the repo, enumerates open issues, and for each candidate issue **creates a worktree and spawns a real coding agent** rooted in it, delivering a prompt — see [Scheduled Tasks → Dispatching agents onto open GitHub issues](../scheduled-tasks.md#dispatching-agents-onto-open-github-issues-issue_dispatch). Setting `triage = true` on top of that only changes what gets *appended* to each dispatched issue's prompt (`triage_instruction()`, `src/issue_dispatch.rs:1168`) — it does not make the fire itself lighter weight.

So enabling this is a decision to let the deck **start real work on issues unattended, on a schedule**, not merely to keep the label vocabulary current. Weigh it as that: it spawns agents and consumes LLM credits every time it fires and finds eligible issues, whether or not each dispatch turns into something worth merging.

### The proposed entry

```toml
# ~/.config/dot-agent-deck/schedules.toml

[[scheduled_tasks]]
name = "Issues prageethw/dot-agent-deck"
cron = "0 9 * * MON-FRI"              # 09:00 on weekdays, local time
working_dir = "~/dispatch/dot-agent-deck"
command = "claude"                     # optional; wins over the global default_command for orchestration-less clones — see scheduled-tasks.md
prompt = "Work on issue {{issue_number}}"
enabled = true

[scheduled_tasks.issue_dispatch]
repo = "prageethw/dot-agent-deck"
max_per_run = 3
triage = true
# label = "agent-eligible"             # optional: restrict candidates to issues carrying this label
# query = "is:open no:assignee"        # optional: advanced gh search override
```

Every field above is derived from [`IssueDispatchConfig`](../../src/config.rs) (`src/config.rs:599-621`) and the worked `issue_dispatch` example in [Scheduled Tasks](../scheduled-tasks.md#dispatching-agents-onto-open-github-issues-issue_dispatch):

- `repo` is required, `owner/name` form, one repo per task (a locked PRD decision — several repos need several `[[scheduled_tasks]]` entries).
- `max_per_run` defaults to 3 if omitted; stated here explicitly rather than relying on the default, so the cap is legible from the file alone.
- `triage` defaults to `false`; this entry is the opt-in.
- `command` is not required for this entry to load (see the corrected note in `scheduled-tasks.md`); this repo's clone defines an `[[orchestrations]]` block (`.dot-agent-deck.toml:90`), so a `command` set here would be ignored at fire time regardless of its value — it is included for legibility, not because it does anything on this repo.

### Cadence and cap — and why these values

**Cadence: weekday mornings (`0 9 * * MON-FRI`), not hourly.** Each fire can spawn up to `max_per_run` real agents that create worktrees and attempt actual work, so the cadence is the main lever on both cost and on how much unattended, unreviewed work can pile up between someone looking at it. A daily weekday cadence bounds that to "at most once per working day" while still closing most of the gap the diagnosis in issue #385 found (issues sitting with no `size-*`/`priority-*` for however long until a human notices).

**`max_per_run = 3`**, the documented default, rather than raising it. Each of the 3 slots is a worktree + a spawned agent + an LLM session, not a lightweight label call — keeping the cap low limits the blast radius of a bad fire (e.g. a `gh` rate-limit or a mass-mislabel) to 3 issues rather than the whole backlog.

### The tradeoff, stated plainly

This is not "turn on labeling" — it is "let the deck decide, unattended, which open issues to start real agent work on, on a schedule." `.github/workflows/issue-triage.yml` already keeps `needs-triage` accurate for every hand-filed issue at zero LLM cost and with no worktrees created — that covers the labeling gap issue #385 set out to close. This `issue_dispatch` entry is a materially bigger step: it spends LLM credits per fire (whether or not each dispatch turns into a merged PR) and produces worktrees/branches/PRs against real issues without a human choosing which ones first. Turning it on is a separate decision from the labeling fix, and is left to the maintainer rather than made here — this section documents the entry so the decision is a copy-paste away, not a reason it has already been made.
