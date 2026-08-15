# Empty gates: signals that look like success without having checked

A check that cannot fail is not a gate. A check that never ran is not a pass. Neither is distinguishable, at a glance, from a genuinely clean result — and in this repo both happen routinely.

CLAUDE.md rule 8 states the policy: **before treating any check as evidence, read the surface that carries the result — the review body, the alert list, the job log — not the check's colour.** This document is the operational companion: the specific surfaces, the exact commands, and the failure modes that have actually cost time here.

## The catalogue

Each of these can report success while having verified nothing. They are not hypothetical; every one has been observed on this repo.

One caveat on that, since a reviewer went looking: the `Closes` row leaves **no trace in a merged PR**, because the fix is always applied before merge. It was observed on PR #233, where `Closes #160, #212, #216, #210.` returned `closingIssuesReferences = [160]` and three issues would have silently stayed open; the body was corrected to repeat the keyword and re-verified as `[160, 210, 212, 216]` before merging. Searching merged bodies for the broken form will therefore find nothing — which is itself the pattern this table is about.

| Signal | How it goes empty | What to read instead |
|---|---|---|
| **`sonarqube`** | Every scan step is gated on `if: env.SONAR_TOKEN != ''`. On a fork PR the secret is absent, so the job runs one `echo` and goes green **having analysed nothing**. | The job log — a skipped run and a clean run look identical on the board. |
| **`semgrep`** | **Not** an empty gate: it is blocking via an explicit `--error` (`ci.yml:441`). Listed because a stale claim that it runs `--no-error` circulated in this repo's own docs — see below. | Findings still live in GitHub code scanning, not in the check. |
| **`e2e`** | `continue-on-error: true` by design, so the **workflow** conclusion reads `success` even when the job failed outright. | The **job** conclusion and the nextest failure list. |
| **`linkage-check`** | Silently pairs a `#[spec]` above an `async fn` with an unrelated later function and still reports `ok` (fork [#234](https://github.com/prageethw/dot-agent-deck/issues/234)). | Until fixed: use the `#[test]` + `block_on(inner())` wrapper shape for async specs, as `shell_activity_008` does. |
| **A `Closes` line** | `Closes #1, #2, #3` links **only the first**. GitHub requires the keyword repeated per issue: `Closes #1, closes #2, closes #3`. | `gh pr view <n> --json closingIssuesReferences`. |
| **A `CONFLICTING` PR** | The most complete form: **no `pull_request` run is created at all** (fork [#150](https://github.com/prageethw/dot-agent-deck/issues/150)). There is no check to be green or red, and a `pull_request_target` workflow like `PR Labeler` still fires beside it, so the PR looks alive. | Ask what you expected to see and did not. The board omits the row rather than misreporting it. |
| **A delegated worker** | Displacement and deep thought look identical — no error, no signal. | The worktree: new commits, dirty files, and file mtimes. |

### The semgrep claim, as a worked example

While writing this document, its `semgrep` row initially said the job runs with `--no-error` and therefore reports success regardless of findings. That is what **CLAUDE.md rule 8 currently claims**. It is wrong, and it was caught only because [`CONTRIBUTING.md`](../../CONTRIBUTING.md) says the opposite — that the job is blocking via `--error`.

`ci.yml:441` passes `--error`, and the comment at `ci.yml:380-382` records the correction explicitly:

> Note `--no-error` is semgrep's DEFAULT for `semgrep scan` — removing it would NOT have made this job blocking, which is what the pre-#146 comment here incorrectly claimed. Blocking requires `--error` explicitly.

So the `semgrep` job **is** a real gate. Two things follow, and both are the point of this document:

1. A stale claim in a rule that is re-read every session is worse than no claim, because it is trusted by default and nothing re-derives it.
2. The only reason it surfaced is that two documents disagreed. A single source stating it confidently would have been copied forward indefinitely — which is exactly how it got into rule 8 in the first place.

Rule 8 is corrected in the same change that adds this document. When two sources disagree about a gate, prefer `ci.yml` itself over either.

## Reading the `e2e` job

Two traps, both of which have cost real time.

**The workflow conclusion is not the job conclusion.** `e2e` is `continue-on-error: true`, so a failed job leaves the workflow reporting `success`. Read per-job:

```bash
gh run view <run-id> --repo prageethw/dot-agent-deck --json jobs \
  --jq '.jobs[] | "\(.name)\t\(.conclusion)"'
```

Note also that on a push to `main` the CI and E2E workflows are **separate workflow runs**, not two jobs of one run. Watching only the `ci.yml`-named run will miss the e2e result entirely.

```bash
gh run list --commit <sha> --repo prageethw/dot-agent-deck \
  --json databaseId,workflowName,status,conclusion
```

**The jobs API lags.** The `e2e` job can report `status: in_progress` / `conclusion: null` for **minutes** after every one of its steps — including "Complete job" — has finished. This appears to be a GitHub-side reporting lag specific to `continue-on-error` jobs. The log becomes fetchable as soon as the steps genuinely finish:

```bash
gh api repos/prageethw/dot-agent-deck/actions/jobs/<job-id>/logs --allow-escape-sequences
```

Do not conclude the job is still running from the status field alone.

**Read the failure list, not the count.** A branch cut before a known-failing test was fixed will carry that failure legitimately, and a *second*, real failure sitting next to it is easy to wave through. This happened during fork #160's merge: `dashboard_001` was the expected stale failure, and `codex_wrap_001_synthetic_jsonl_reaches_dashboard` was sitting beside it. It turned out to be `FLAKY 2/3` — failed once, passed on retry — but that was established by reading the summary, not by assuming.

nextest distinguishes the outcomes explicitly, and the distinction matters:

- `FAIL` after all retries — a real failure.
- `FLAKY n/3` — failed then passed; recovered. **Only ever seen in CI**: `.config/nextest.toml` sets `retries = 0`, and the `/3` comes from `--retries 2` on the e2e job's command line alone. A local run has no retries to be flaky across.
- `LEAK` — passed, but left something behind.
- *"Every test passed on its first attempt."* — the only phrasing that means no retry was consumed anywhere. Note this line is **not** nextest's: it is emitted by `e2e.yml`'s own summary step (`:279`), so it exists only for that job.

## A displaced worker looks exactly like a thinking one

Delegating to a role whose pane is already busy can displace the in-flight task. The `dot-agent-deck delegate` call reports `Delegated to <role>.` either way, the displaced worker never signals `work-done`, and the daemon counts it outstanding indefinitely — surfacing hours later as a staleness report for work that no longer exists.

Observed twice in one session (2026-08-11): a `coder` root-causing fork #224 and another implementing fork #160 were each displaced by a later delegation to the same role. Neither reported. Both worktrees were left clean at their original SHA with nothing lost, because neither had committed yet — but ~40 minutes passed before the loss was noticed. A third round was displaced *after* editing five files, leaving an **orphaned uncommitted diff** that had to be verified against the reviewer's findings table and recovered by a second worker.

The mechanism is not confirmed — two coders were observed running concurrently at one point, so it is not simply "one pane per role". What is confirmed is the *symptom*, and the mitigations are cheap:

- **Check the role is idle before delegating**, and act on the answer:
  ```bash
  dot-agent-deck daemon status | grep coder | grep -c Working
  ```
- **Diagnose a silent worker from the worktree, not the pane.** New commits, dirty files, and `ls -lt` mtimes on the files its task named tell you whether work happened and when it stopped. A clean worktree at the original SHA means nothing was lost; a dirty one means there is work to recover.
- **When recovering an orphaned diff**, state in the task what the dirty state should contain, so the worker can distinguish "the round that died" from "something else touched this". CLAUDE.md rule 1 otherwise requires a worker to refuse a dirty worktree, correctly — that precondition has to be explicitly inverted, and only for that task.

Whether the product should queue rather than displace is an open question. It has not been filed as an issue because the mechanism could not be stated defensibly, and a bug report that misdescribes its own reproduction is worse than none. Anyone who can reproduce it deliberately should file it.

## Grep the shape, not the line numbers

A finding that enumerates sites goes stale the moment anything above them moves, and an enumeration is also silently incomplete if the pattern exists under a different name.

Both happened during the fork #160 / #224 work:

- Fork [#237](https://github.com/prageethw/dot-agent-deck/issues/237) was filed listing six sites. A later commit in the same PR shifted every one by 28–29 lines, and the issue's table was wrong until corrected. Worse, the same helper existed in a second file under a *different name* (`pane_column_left_edge`), used at five more sites — two of them in the higher-risk shape the issue said was the dangerous one. The real count was eleven, not six.
- A review finding named three truncating `Duration::from_secs(X.as_secs() * N)` sites. The fix was implemented exactly as filed, and a later review found **two more** carrying the same shape. No behaviour changed, but the latent trap survived a round that believed it had closed it.

So: when writing a finding, **name the pattern and how to find it**, then list known instances as examples rather than as the scope. When implementing one, grep the shape and report what you found beyond the named sites — including instances you decided *not* to change, and why. During the #160 work that distinction mattered: one `.as_secs() * N` hit was deriving a plain loop-iteration count from an already-correct `Duration`, not reconstructing a truncated one, and was correctly left alone.

## Why this keeps happening

These are not unrelated bugs. Every one is the same shape: **a mechanism that answers confidently on incomplete information**, where the confident-and-wrong answer is indistinguishable from the correct one.

The defects found during the fork #221/#224/#160 work were the same shape in the product and the tests: a `--mine` command printing a definitive empty answer while holding contradicting evidence; a shell-activity sample reporting a confident `Idle` when it had failed to read; a test guard asserting a safety property it did not provide; a panicking helper used as a retry predicate, aborting on the first sample while looking like it retried for three seconds.

The useful habit is narrow and cheap: **ask what this signal would look like if it had not actually checked** — and if the answer is "the same", go read the surface underneath it.

## Upstream note

Most of this is fork-specific. The `e2e:` job is fork-only (see CLAUDE.md rule 5), and the `SONAR_TOKEN` gate is this repo's CI configuration. The delegation behaviour is upstream product behaviour, but is recorded here as an unconfirmed observation rather than offered upstream as a claim — see CLAUDE.md rule 19 for when that distinction matters.

*(This paragraph originally also listed `--no-error` on `semgrep` as fork CI configuration — the exact claim this document retracts, left standing 85 lines below the example retracting it. Caught in review. It is recorded rather than quietly deleted because it is the same failure twice in one file: a correction applied where it was noticed and nowhere else.)*
