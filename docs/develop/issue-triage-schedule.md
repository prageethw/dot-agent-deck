# Scheduling `issue_dispatch` triage for this repo

This records the exact `~/.config/dot-agent-deck/schedules.toml` entry that would turn on `issue_dispatch` with `triage = true` against `prageethw/dot-agent-deck`, so the config is derivable in-repo rather than reconstructed from `src/config.rs`/`docs/scheduled-tasks.md` each time someone asks. Nothing here is turned on by this doc — it is a proposal, not a config change (issue #385 M3).

## What this would actually do

`issue_dispatch` is not a triage-only pass. Every fire clones/pulls the repo, enumerates open issues, and for each candidate issue **creates a worktree and spawns a real coding agent** rooted in it, delivering a prompt — see [Scheduled Tasks → Dispatching agents onto open GitHub issues](../scheduled-tasks.md#dispatching-agents-onto-open-github-issues-issue_dispatch). Setting `triage = true` on top of that only changes what gets *appended* to each dispatched issue's prompt (`triage_instruction()`, `src/issue_dispatch.rs:1168`) — it does not make the fire itself lighter weight.

So enabling this is a decision to let the deck **start real work on issues unattended, on a schedule**, not merely to keep the label vocabulary current. Weigh it as that: it spawns agents and consumes LLM credits every time it fires and finds eligible issues, whether or not each dispatch turns into something worth merging.

## The proposed entry

```toml
# ~/.config/dot-agent-deck/schedules.toml

[[scheduled_tasks]]
name = "Issues prageethw/dot-agent-deck"
cron = "0 9 * * MON-FRI"              # 09:00 on weekdays, local time
working_dir = "~/dispatch/dot-agent-deck"
command = "claude"                     # required to load; used only if the clone has no [[orchestrations]] block
prompt = "Work on issue {{issue_number}}"
enabled = true

[scheduled_tasks.issue_dispatch]
repo = "prageethw/dot-agent-deck"
max_per_run = 3
triage = true
# label = "agent-eligible"             # optional: restrict candidates to issues carrying this label
# query = "is:open no:assignee"        # optional: advanced gh search override
```

Every field above is derived from [`IssueDispatchConfig`](../../src/config.rs) (`src/config.rs:598-622`) and the worked `issue_dispatch` example in [Scheduled Tasks](../scheduled-tasks.md#dispatching-agents-onto-open-github-issues-issue_dispatch):

- `repo` is required, `owner/name` form, one repo per task (a locked PRD decision — several repos need several `[[scheduled_tasks]]` entries).
- `max_per_run` defaults to 3 if omitted; stated here explicitly rather than relying on the default, so the cap is legible from the file alone.
- `triage` defaults to `false`; this entry is the opt-in.
- `command` is still required for the entry to load even though, per the field's own doc comment, it is ignored at fire time whenever the target `working_dir`'s clone defines an `[[orchestrations]]` block — this repo does, so `command` here is a load-time formality, not what actually runs.

## Cadence and cap — and why these values

**Cadence: weekday mornings (`0 9 * * MON-FRI`), not hourly.** Each fire can spawn up to `max_per_run` real agents that create worktrees and attempt actual work, so the cadence is the main lever on both cost and on how much unattended, unreviewed work can pile up between someone looking at it. A daily weekday cadence bounds that to "at most once per working day" while still closing most of the gap the diagnosis in issue #385 found (issues sitting with no `size-*`/`priority-*` for however long until a human notices).

**`max_per_run = 3`**, the documented default, rather than raising it. Each of the 3 slots is a worktree + a spawned agent + an LLM session, not a lightweight label call — keeping the cap low limits the blast radius of a bad fire (e.g. a `gh` rate-limit or a mass-mislabel) to 3 issues rather than the whole backlog.

**`new_tab_per_fire` is omitted (defaults to `false`) because it has no effect here.** `src/daemon.rs`'s `make_schedule_callback` branches on whether `task.issue_dispatch` is set and, when it is, `return`s a callback that runs `run_issue_dispatch` — the `new_tab_per_fire`/tab-reuse `spawn_or_reuse` path further down the same function, the only place that field is ever read, is unreachable for an `issue_dispatch` task. Each dispatched issue already gets its own worktree and card keyed by issue number regardless of this field's value, so setting it would be a no-op dressed up as a decision.

## The tradeoff, stated plainly

This is not "turn on labeling" — it is "let the deck decide, unattended, which open issues to start real agent work on, on a schedule." M1 of issue #385 (`.github/workflows/issue-triage.yml`) already keeps `needs-triage` accurate for every hand-filed issue at zero LLM cost and with no worktrees created — that covers the labeling gap this PRD set out to close. This `issue_dispatch` entry is a materially bigger step: it spends LLM credits per fire (whether or not each dispatch turns into a merged PR) and produces worktrees/branches/PRs against real issues without a human choosing which ones first. Turning it on is a separate decision from the labeling fix, and is left to the maintainer rather than made here.
