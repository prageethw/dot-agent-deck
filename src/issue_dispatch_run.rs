//! Fire-time GitHub issue-dispatch flow (PRD #120, M2.1–M2.4 + M3.2 + M1.3;
//! PRD #421 M1.0–M1.3, the `in-progress` claim).
//!
//! This is the impure, daemon-side counterpart to the pure helpers in
//! [`crate::issue_dispatch`]. On each fire of an `issue_dispatch` scheduled task
//! the daemon composes those helpers with #127's spawn primitive
//! ([`crate::spawn::spawn`]) and the `gh` / `git` binaries on `PATH`:
//!
//!   1. **M2.1** — provision the repo clone under the task's `working_dir`:
//!      clone-if-missing (`gh repo clone`) / fetch + fast-forward-pull-if-present
//!      (`git -C <clone> fetch` then `git -C <clone> pull --ff-only`). An existing
//!      clone is verified to be the right repo by its `origin` before being
//!      touched (L3, fail-closed), and a refresh failure on it is non-fatal —
//!      the run continues with the refs already on disk (S3).
//!   2. enumerate the repo's open issues (`gh issue list --json number,labels`),
//!      capping at `max_per_run` **in code** on the returned order — the issue
//!      list may ignore `--limit`. PRD #421 M1.2: the `labels` field rides along
//!      on this ALREADY-MADE call, so the third idempotency signal below costs
//!      no extra `gh` invocation for an issue that isn't labelled. See
//!      [`list_open_issues`] / [`OpenIssue`].
//!   3. **M2.2 + PRD #421 M1.2** — for each issue, decide dispatch-vs-skip from
//!      the three idempotency signals, checked in order: per-issue worktree
//!      already on disk; the `in-progress` label, honoured regardless of who
//!      applied it; an open PR whose head is `agent/issue-<n>`. The worktree
//!      and label checks are both I/O-free (the label rides along on the
//!      already-made issue enumeration) and are consulted BEFORE the PR
//!      probe (review fix C1), so a transient `gh pr list` failure can never
//!      turn an already-correct worktree/label SKIP into a spurious
//!      `IssueDispatchFailed`. Only when the label IS present does a further
//!      `gh issue view --json comments` run, to look up the claimant for the
//!      skip's rendered text — see [`fetch_claim_comment`].
//!   4. **M2.2 / M2.3** — on dispatch, create the per-issue worktree on
//!      `agent/issue-<n>` (creating the branch with `-b`, or attaching a branch
//!      left behind by an earlier closed-without-PR run — B1) and [`spawn`] one
//!      agent into it, delivering the substituted prompt. The spawn primitive
//!      already branches on the worktree's `.dot-agent-deck.toml` (orchestration
//!      tab vs single-agent card) — reused, not duplicated.
//!   5. **M2.4** — record each spawned pane → worktree in a daemon-side
//!      [`WorktreeRegistry`] so closing the tab later removes the worktree (while
//!      PRESERVING the clone). See [`record_worktree`] / [`take_worktree`] /
//!      [`remove_worktree`].
//!   6. **PRD #421 M1.0/M1.1** — on a successful dispatch, claim the issue:
//!      write the `in-progress` label and post a claim comment naming the
//!      claiming task. See [`claim_issue`].
//!   7. **M3.2** — every issue runs inside its own error boundary: a failing
//!      issue (clone/worktree/`gh` error — e.g. the test stub's simulated
//!      `pr list` failure) is surfaced through the notifier and the run CONTINUES
//!      with the remaining issues. One issue never aborts the rest.
//!   8. **M1.3 + PRD #421 M1.3** — per-issue success / skip / failure events are
//!      surfaced through #127's existing [`Notifier`] seam (no parallel
//!      notification system); a skip always names which of the four causes
//!      fired.
//!
//! All GitHub/git access goes through the `gh` / `git` binaries resolved from
//! `PATH`, inheriting the daemon's environment — that is exactly what lets the
//! L2 tests isolate everything offline behind a stub `gh` on `PATH` plus a local
//! fixture remote.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::broadcast;

use crate::agent_pty::{AgentPtyRegistry, AgentRecord, TabMembership};
use crate::config::IssueDispatchConfig;
use crate::event::BroadcastMsg;
use crate::issue_dispatch::{
    CLAIM_COMMENT_PREFIX, DispatchDecision, IN_PROGRESS_LABEL, IN_PROGRESS_LABEL_COLOR,
    IN_PROGRESS_LABEL_DESCRIPTION, Identity, ParsedClaim, TRIAGE_LABELS, claim_comment_body,
    derive_issue_paths, dispatch_decision, gh_current_login_argv, issue_comment_argv,
    issue_edit_add_label_argv, issue_edit_assignee_argv, issue_list_argv, issue_view_comments_argv,
    label_create_argv, parse_claim_fields, pr_list_for_issue_argv, substitute_issue_number,
    triage_instruction, validate_gh_login,
};
use crate::scheduler::{Notifier, NotifyEvent, SkipReason};
use crate::spawn::{SpawnKind, SpawnRequest, spawn};

// ---------------------------------------------------------------------------
// M2.4 — daemon-side worktree registry (close → cleanup plumbing)
// ---------------------------------------------------------------------------

/// Daemon-owned, in-memory map: per-issue worktree dir → the clone that owns it
/// (preserved on cleanup). Shared between the fire-time dispatch flow (records
/// the worktree the moment it is created — BEFORE the spawn's prompt-delivery
/// wait returns) and the `StopAgent` handler (removes it on close).
///
/// Keyed by the **worktree path**, not the spawned agent id, on purpose: the
/// spawn primitive only returns the registry id AFTER its readiness/delivery
/// wait, so a tab closed promptly after the agent appears would race a
/// per-agent-id record. The closing agent is instead matched to its worktree via
/// its [`AgentRecord`] (orchestration cwd / single-agent cwd) — available the
/// instant the agent is registered. Wiped on daemon restart; a post-restart
/// close finds no entry and leaves the worktree in place (reclaimed by the
/// worktree-exists idempotency signal on the next fire).
pub type WorktreeRegistry = Arc<Mutex<HashMap<PathBuf, PathBuf>>>;

/// Construct an empty [`WorktreeRegistry`].
pub fn new_worktree_registry() -> WorktreeRegistry {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Record a freshly-created per-issue worktree (→ its owning clone) for
/// tab-close cleanup. Idempotent: a re-recorded worktree just refreshes the
/// clone mapping.
pub fn record_worktree(worktrees: &WorktreeRegistry, worktree_dir: &Path, clone_dir: &Path) {
    worktrees
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(worktree_dir.to_path_buf(), clone_dir.to_path_buf());
}

/// The per-issue worktree a closing agent was dispatched into, derived from its
/// [`AgentRecord`]: the orchestration cwd for an orchestration tab, else the
/// single-agent card's cwd. `None` for an agent that carries neither.
pub fn worktree_of_record(record: &AgentRecord) -> Option<PathBuf> {
    match &record.tab_membership {
        Some(TabMembership::Orchestration {
            orchestration_cwd, ..
        }) => orchestration_cwd.clone().map(PathBuf::from),
        _ => record.cwd.clone().map(PathBuf::from),
    }
}

/// If `worktree_dir` is a dispatched issue worktree, drop its registry entry and
/// return the owning clone dir; `None` otherwise (an ordinary agent's cwd, or an
/// entry already taken). The close watcher only calls this once it has confirmed
/// (via [`worktree_still_in_use`]) that the LAST agent rooted in the worktree has
/// closed, so for a multi-role orchestration the entry survives every earlier
/// sibling close and is taken exactly once, on the final close.
pub fn take_worktree(worktrees: &WorktreeRegistry, worktree_dir: &Path) -> Option<PathBuf> {
    worktrees
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(worktree_dir)
}

/// S1: whether any live agent in `records` is still rooted in `worktree_dir` —
/// its orchestration cwd (shared by EVERY role pane of a multi-role
/// orchestration) or a single-agent card's cwd. The close watcher calls this
/// AFTER `close_agent` has dropped the closing agent, so an empty result means
/// the just-closed agent was the LAST one in the worktree and it is safe to
/// remove. While a sibling role is still live the shared worktree must survive.
pub fn worktree_still_in_use(records: &[AgentRecord], worktree_dir: &Path) -> bool {
    records
        .iter()
        .any(|r| worktree_of_record(r).as_deref() == Some(worktree_dir))
}

/// Remove a dispatched worktree from its clone (`git -C <clone> worktree remove
/// <worktree> --force`), PRESERVING the clone. Best-effort: a non-zero exit
/// (already removed, locked) or a spawn error is logged, not fatal — the tab is
/// already gone.
pub async fn remove_worktree(worktree_dir: &Path, clone_dir: &Path) {
    let res = run_status(
        "git",
        &[
            "-C",
            &clone_dir.to_string_lossy(),
            "worktree",
            "remove",
            &worktree_dir.to_string_lossy(),
            "--force",
        ],
    )
    .await;
    match res {
        Ok(()) => tracing::info!(
            worktree = %worktree_dir.display(),
            "issue-dispatch: removed worktree on tab close (clone preserved)"
        ),
        Err(e) => tracing::warn!(
            worktree = %worktree_dir.display(),
            error = %e,
            "issue-dispatch: worktree cleanup on close failed"
        ),
    }
}

// ---------------------------------------------------------------------------
// Fire-time dispatch flow
// ---------------------------------------------------------------------------

/// Run the full issue-dispatch flow for one fire of an `issue_dispatch` task.
///
/// `default_command` is the resolved single-agent command (from the global
/// `default_command`, or the task's own command) — used only for clones with no
/// orchestration config; orchestration clones ignore it (the role commands win).
///
/// Never panics; the repo-level steps abort only this repo's fire (one repo per
/// task, no fan-out) and every issue runs inside its own error boundary.
#[allow(clippy::too_many_arguments)]
pub async fn run_issue_dispatch(
    task_name: &str,
    working_dir: &str,
    prompt_template: &str,
    cfg: &IssueDispatchConfig,
    default_command: Option<String>,
    registry: &Arc<AgentPtyRegistry>,
    worktrees: &WorktreeRegistry,
    notifier: &dyn Notifier,
    event_tx: Option<&broadcast::Sender<BroadcastMsg>>,
) {
    // S5 — every derived path (clone, worktree, the spawn's orchestration_cwd)
    // must be absolute: a relative workspace root would double-nest the worktree
    // under `git -C <clone> worktree add <relative>` and drop orchestration_cwd
    // (`is_valid_orchestration_cwd` requires an absolute path) → no tab-close
    // cleanup. The schedules loader already resolves relatives against $HOME, so a
    // non-absolute value here is a misconfiguration: reject this run.
    let workspace = match canonical_workspace(working_dir) {
        Ok(p) => p,
        Err(message) => {
            notifier.notify(NotifyEvent::IssueDispatchRepoError {
                task: task_name.to_string(),
                repo: cfg.repo.clone(),
                message,
            });
            return;
        }
    };
    // L2 + S4 — the clone-dir path component is a SANITIZED single segment of the
    // task name (never `/`, `..`, or absolute), so it can't nest or escape the
    // workspace. Identical to `derive_issue_paths(..).clone_dir`.
    let clone_dir = workspace.join(crate::issue_dispatch::sanitize_clone_segment(task_name));

    // M2.1 — provision the repo clone (clone-if-missing / fetch+ff-pull-if-present).
    if let Err(message) = provision_repo(&workspace, &clone_dir, &cfg.repo).await {
        notifier.notify(NotifyEvent::IssueDispatchRepoError {
            task: task_name.to_string(),
            repo: cfg.repo.clone(),
            message,
        });
        return;
    }

    // Enumerate open issues. The `--limit` in the argv is advisory; cap in code.
    let issues = match list_open_issues(cfg).await {
        Ok(v) => v,
        Err(message) => {
            notifier.notify(NotifyEvent::IssueDispatchRepoError {
                task: task_name.to_string(),
                repo: cfg.repo.clone(),
                message,
            });
            return;
        }
    };

    // PRD #421 review fix B1 — UNCONDITIONALLY ensure the `in-progress` claim
    // label exists on the repo, once per run, before any issue's claim can be
    // attempted. Real `gh issue edit --add-label` resolves the label name to
    // an ID client-side and hard-errors before any mutation when the repo has
    // never carried it, so without this the unconditional claim later in this
    // run silently no-ops on any such repo — the PRD's headline behaviour
    // then does nothing while the run still reports a clean successful
    // dispatch (reviewer F1 / auditor F1). Unlike the opt-in triage
    // vocabulary below, this is NOT gated on `cfg.triage` — the claim itself
    // never is. Best-effort like `claim_issue`: a `gh` failure here must not
    // abort the run.
    ensure_claim_label(&cfg.repo).await;

    // PRD fork#235 M2: resolve the currently-authenticated `gh` login ONCE
    // per run, beside `ensure_claim_label` above — one `gh api user` call
    // per run, not per issue. Best-effort: a failure (or an implausible
    // login) just means every claim this run writes no assignee, never that
    // the run itself fails (`scheduler/dispatch/022`).
    let login = resolve_current_login(&cfg.repo).await;

    // PRD #421 M2.0/M2.1 — opt-in: ensure the triage label vocabulary exists on
    // the repo once per run, before any issue is considered (it's a repo-level
    // concern, not a per-issue one). Best-effort like `claim_issue`: a `gh`
    // failure here must not abort the run or turn a later successful dispatch
    // into a failure.
    if cfg.triage {
        ensure_triage_labels(&cfg.repo).await;
    }

    // S2 — `max_per_run` caps the issues CONSIDERED per run (not the number newly
    // dispatched): already-claimed issues inside the cap are skipped, yielding a
    // clean "≤ max_per_run concurrent in-flight" ceiling (PRD concurrency model —
    // today's run only picks up slots yesterday's run vacated).
    for issue in issues.into_iter().take(cfg.max_per_run) {
        // M3.2 — per-issue error boundary: one failure never aborts the rest.
        if let Err(message) = dispatch_one_issue(
            task_name,
            &workspace,
            prompt_template,
            cfg,
            default_command.as_deref(),
            issue.number,
            issue.in_progress_label,
            &clone_dir,
            registry,
            worktrees,
            notifier,
            event_tx,
            login.as_deref(),
        )
        .await
        {
            notifier.notify(NotifyEvent::IssueDispatchFailed {
                task: task_name.to_string(),
                repo: cfg.repo.clone(),
                issue: issue.number,
                message,
            });
        }
    }
}

/// Process one candidate issue. `Ok(())` means it was dispatched OR skipped (a
/// skip is surfaced here, not treated as an error); `Err` is a per-issue failure
/// for the caller to surface through the notifier (M3.2).
#[allow(clippy::too_many_arguments)]
async fn dispatch_one_issue(
    task_name: &str,
    workspace: &Path,
    prompt_template: &str,
    cfg: &IssueDispatchConfig,
    default_command: Option<&str>,
    issue: u64,
    label_in_progress: bool,
    clone_dir: &Path,
    registry: &Arc<AgentPtyRegistry>,
    worktrees: &WorktreeRegistry,
    notifier: &dyn Notifier,
    event_tx: Option<&broadcast::Sender<BroadcastMsg>>,
    login: Option<&str>,
) -> Result<(), String> {
    let paths = derive_issue_paths(workspace, task_name, issue);

    let notify_skip = |reason: SkipReason| {
        notifier.notify(NotifyEvent::IssueDispatchSkipped {
            task: task_name.to_string(),
            repo: cfg.repo.clone(),
            issue,
            branch: paths.branch.clone(),
            reason,
        });
    };

    // M2.2 — idempotency BEFORE any work, evaluated as a SHORT-CIRCUIT on the
    // three signals so a later check only runs when an earlier one leaves the
    // verdict open.
    //
    // PRIMARY (the worktree is the ledger): if the per-issue worktree already
    // exists the issue is already claimed — emit a SKIP and return IMMEDIATELY,
    // WITHOUT consulting the open-PR or label signals. Probing either here
    // would be both redundant (a present worktree skips regardless) and a
    // correctness hazard: a transient `gh` failure would, via the per-issue
    // error boundary, turn this clean SKIP into a spurious IssueDispatchFailed
    // notification.
    let worktree_exists = paths.worktree_dir.exists();
    if worktree_exists {
        notify_skip(SkipReason::WorktreeExists);
        return Ok(());
    }

    // SECONDARY (PRD #421 review C1) — the `in-progress` label, honoured
    // regardless of who applied it (human, external tool, another deck, or
    // this one — no "is this my own claim?" comparison exists), consulted
    // BEFORE the open-PR probe: `label_in_progress` is exactly as I/O-free as
    // the worktree check above — it rode along on the `gh issue list --json
    // number,labels` enumeration this flow already made (see
    // `list_open_issues`/`OpenIssue`) — so it belongs ahead of any NEW `gh`
    // call for the same reason the worktree check does. Consulting it here
    // means a transient `gh pr list` failure can no longer turn an
    // already-correct label SKIP into a spurious IssueDispatchFailed — the
    // exact hazard this function's PRIMARY comment above warns against, now
    // closed for the label signal too. This DOES change reported precedence
    // for an issue that is both labelled AND has an open PR (OpenPr →
    // Labelled): the label is the explicit claim PRD #421 exists to
    // establish, and the I/O-free signal wins.
    if label_in_progress {
        // Only NOW — once we already know we are skipping — does a further
        // `gh issue view --json comments` run, to look up the claimant for
        // the rendered text (PRD #421 M1.3). Best-effort: a failure here must
        // not turn an already-correct SKIP decision into a per-issue
        // failure, so it degrades to "no claimant recorded" rather than
        // propagating.
        let claimant = fetch_claim_comment(&cfg.repo, issue).await;
        notify_skip(SkipReason::Labelled {
            claimant: claimant.map(|c| c.raw),
        });
        return Ok(());
    }

    // TERTIARY — reached ONLY when neither the worktree nor the label
    // matched: an open PR whose head is `agent/issue-<n>`. A `gh` failure
    // here is a genuine per-issue error (e.g. the stub's simulated API
    // error) and propagates via `?`. `worktree_exists` and
    // `label_in_progress` are both known-false at this point, so
    // `dispatch_decision`'s verdict here turns entirely on `open_pr` — kept
    // as a call into the shared pure decision function (PRD #421 M1.4)
    // rather than an inline `if`, so its truth table stays the single
    // source of truth for what "already claimed" means.
    let open_pr = issue_has_open_pr(&cfg.repo, issue).await?;
    match dispatch_decision(worktree_exists, open_pr, label_in_progress) {
        DispatchDecision::Skip => {
            notify_skip(SkipReason::OpenPr);
            return Ok(());
        }
        DispatchDecision::Dispatch => {}
    }

    // M2.2 — create the per-issue worktree on `agent/issue-<n>`. A concurrent
    // fire can claim it in the TOCTOU window after the idempotency check above
    // (see `create_worktree`); that benign race is a skip, not a failure —
    // mirroring the `dispatch_decision` worktree-presence skip.
    // issue #425: name the issue-dispatch task and issue this worktree is
    // for, rather than only recording that some deck created it.
    //
    // PR #215 fixup: sanitized at the point of computation (mirroring
    // `orchestration_creator_string` in `src/ui.rs`), not left to the
    // downstream `mark_worktree_owned` call inside `create_worktree` to
    // sanitize alone — `creator` also reaches `AgentSpawnOptions::owner`
    // below (the `DOT_AGENT_DECK_WORKTREE_OWNER` env var), which had no
    // sanitizer of its own. `sanitize_marker_creator` is a fixed point
    // (`f(f(x)) == f(x)`), so the marker write's own call stays harmless.
    let creator = crate::worktree_reclaim::sanitize_marker_creator(&format!(
        "issue-dispatch:{task_name}#{issue}"
    ));
    match create_worktree(clone_dir, &paths.worktree_dir, &paths.branch, &creator).await? {
        WorktreeCreation::Created => {}
        // `TimedOut` cannot actually occur on this async path today (its
        // `run_status`/`run_status_args` calls have no bound) — handled
        // identically to `AlreadyClaimed` (skip, not fail) only so the match
        // stays exhaustive and safe if that ever changes.
        WorktreeCreation::AlreadyClaimed | WorktreeCreation::TimedOut { .. } => {
            notify_skip(SkipReason::ConcurrentCreator);
            return Ok(());
        }
    }

    // M2.4 — record the worktree for tab-close cleanup NOW, before the spawn's
    // prompt-delivery wait. `spawn` registers the agent (visible to a `StopAgent`
    // from a fast client) well before it returns, so recording after the spawn
    // would race a prompt close. The close watcher matches the agent to this
    // worktree by its record's cwd, not by an agent id we don't have yet.
    record_worktree(worktrees, &paths.worktree_dir, clone_dir);

    // M2.3 — spawn one agent into the worktree, delivering the substituted
    // prompt. `spawn` branches on the worktree's `.dot-agent-deck.toml`.
    //
    // `detach_delivery = true`: the agent is still registered synchronously (so
    // the idempotency/worktree state is consistent the moment this returns), but
    // the prompt-delivery wait — which can sit out the multi-second `SessionStart`
    // fallback for a hook-less command — runs in the background. This frees the
    // scheduler's run-active window as soon as the dispatch WORK is done, so a
    // re-fire right after a tab close (PRD #120 B1 / dispatch/008) isn't skipped
    // behind the prior run's lingering delivery wait. The worktree-on-disk
    // idempotency signal still serializes overlapping fires safely.
    // PRD #421 M2.2 — when triage is on, append the triage instruction to the
    // substituted prompt so the dispatched agent applies its own labels. Only
    // the issues actually dispatched here ever see it; a skipped issue never
    // reaches this point.
    let mut prompt = substitute_issue_number(prompt_template, issue);
    if cfg.triage {
        prompt.push_str("\n\n");
        prompt.push_str(&triage_instruction());
    }
    let req = SpawnRequest {
        task_name: task_name.to_string(),
        working_dir: paths.worktree_dir.to_string_lossy().into_owned(),
        command: default_command.map(str::to_string),
        prompt,
        // Fork #166 M2.4: the SAME string just written into the worktree's
        // `created-by:` marker above (`create_worktree`), not a second
        // derivation of it.
        owner: Some(creator),
    };
    let handle = match spawn(req, registry, notifier, event_tx, true).await {
        Ok(h) => h,
        Err(e) => {
            // The spawn failed after the worktree was created/recorded: no
            // agent will ever close to trigger cleanup, so drop the registry
            // entry here. The worktree dir itself is left on disk — the next
            // fire's worktree-exists idempotency signal reclaims the issue.
            take_worktree(worktrees, &paths.worktree_dir);
            return Err(e.to_string());
        }
    };

    // M1.3 — surface the per-issue dispatch success.
    notifier.notify(NotifyEvent::IssueDispatched {
        task: task_name.to_string(),
        repo: cfg.repo.clone(),
        issue,
    });

    // PRD fork#235 round 3: derive the claimant IDENTITY from the bound spawn
    // handle's `SpawnKind`, not from `task_name` alone — an orchestration
    // dispatch names the ORCHESTRATION's own typed name in the claim
    // (`scheduler/dispatch/021`), never the scheduled task that fired it. The
    // anchor itself (CLAUDE.md rule 23) is THIS issue's own dispatched
    // worktree's absolute path plus its branch (`paths.worktree_dir` /
    // `paths.branch`) — the one the spawned agent actually runs in — never a
    // host or a digest; the task/orchestration name is decoration only.
    let identity = match &handle.kind {
        SpawnKind::Orchestration { name } => {
            Identity::orchestration(name, &paths.worktree_dir, &paths.branch)
        }
        SpawnKind::SingleAgent => {
            Identity::issue_dispatch(task_name, issue, &paths.worktree_dir, &paths.branch)
        }
    };

    // PRD #421 M1.0/M1.1 — claim the issue now that the dispatch has
    // genuinely succeeded: write the `in-progress` label, replace-to-one the
    // assignee (PRD fork#235 M2), and post a claim comment naming the
    // claiming identity. Deliberately AFTER both worktree creation and spawn
    // succeeded (`dispatch/014`): marking any earlier would make a FAILED
    // dispatch leave a false claim, permanently un-dispatchable once M1.2
    // reads the label back. A `gh` failure here must not turn this
    // already-successful dispatch into a per-issue failure — the per-issue
    // error boundary would otherwise report an `IssueDispatchFailed` for a
    // dispatch that genuinely worked, which is exactly the defect PRD #421's
    // Risks section calls out (and PRD fork#235 M2 extends to the assignee
    // write) — so `claim_issue` never propagates. Review fix C3: it no
    // longer swallows the failure into `tracing::warn!` alone either — a
    // claim failure is now surfaced through the `Notifier` seam as its own
    // distinguishable event (see [`claim_issue`]).
    claim_issue(&cfg.repo, issue, task_name, &identity, login, notifier).await;

    Ok(())
}

/// PRD #421 M1.0/M1.1 + PRD fork#235 M2: write the `in-progress` label,
/// replace-to-one the assignee, and post a claim comment naming `identity` —
/// the bound spawn handle's identity (an orchestration's own typed name, or
/// `task_name`#`issue` for a single-agent dispatch; see the call site).
///
/// Three writes, in order: label (unchanged since PRD #421), assignee
/// (skipped entirely when `login` is `None` — no resolved human to assign),
/// then the comment (always posted, appended never edited in place — the log
/// is the history).
///
/// Best-effort in the sense that mattered from the start (never propagated,
/// never turns a successful dispatch into `IssueDispatchFailed` — see the
/// call site's doc comment) — but review fix C3 corrects how the failure was
/// REPORTED: a `gh` failure here used to reach only `tracing::warn!`, a sink
/// nobody watching the deck's own notifications ever sees. That is precisely
/// the state an upgrading user with a read-only token lands in (auditor F8):
/// every dispatch still reports `IssueDispatched`, the label/comment write
/// silently fails, and a second deck pointed at the same repo has neither the
/// worktree nor the label signal to stop it duplicating the work — exactly
/// what this PRD exists to prevent. A claim failure is now its own
/// `NotifyEvent::IssueClaimFailed`, distinguishable from both a successful
/// dispatch and an `IssueDispatchFailed`: the dispatch genuinely succeeded:
/// only the claim could not be written. PRD fork#235 M2 extends the same
/// discipline to the assignee write — worded distinctly from the label and
/// comment failures per its own implementation trap: GitHub silently drops
/// an assignee lacking repo access and `gh` may still exit 0, so this reports
/// what was ATTEMPTED, never what was achieved.
async fn claim_issue(
    repo: &str,
    issue: u64,
    task_name: &str,
    identity: &Identity,
    login: Option<&str>,
    notifier: &dyn Notifier,
) {
    let label_argv = issue_edit_add_label_argv(repo, issue, IN_PROGRESS_LABEL);
    if let Err(e) = run_status_args("gh", &label_argv).await {
        notifier.notify(NotifyEvent::IssueClaimFailed {
            task: task_name.to_string(),
            repo: repo.to_string(),
            issue,
            message: format!("failed to write the in-progress label: {e}"),
        });
    }

    if let Some(login) = login {
        // PRD fork#235 M2: `prior` is the login parsed from the newest
        // existing claim comment — the deck's own receipt for what it
        // assigned last, not an assumption about who currently holds the
        // GitHub-native assignee field.
        let prior_login = fetch_claim_comment(repo, issue).await.and_then(|c| c.login);
        let assignee_argv =
            issue_edit_assignee_argv(repo, issue, Some(login), prior_login.as_deref());
        if let Err(e) = run_status_args("gh", &assignee_argv).await {
            notifier.notify(NotifyEvent::IssueClaimFailed {
                task: task_name.to_string(),
                repo: repo.to_string(),
                issue,
                message: format!("failed to write the assignee: {e}"),
            });
        }
    }

    let timestamp = chrono::Utc::now().to_rfc3339();
    let body = claim_comment_body(identity, &timestamp, login, None);
    let comment_argv = issue_comment_argv(repo, issue, &body);
    if let Err(e) = run_status_args("gh", &comment_argv).await {
        notifier.notify(NotifyEvent::IssueClaimFailed {
            task: task_name.to_string(),
            repo: repo.to_string(),
            issue,
            message: format!("failed to post the claim comment: {e}"),
        });
    }
}

/// PRD fork#235 M2: resolve the currently-authenticated `gh` login ONCE per
/// run (see the call site in [`run_issue_dispatch`]) — "whoever `gh` is
/// authenticated as on this host", the human who owns the agent working the
/// issue right now. Best-effort: any failure (spawn error, non-zero exit, an
/// empty reply, or a reply that fails [`validate_gh_login`] — the login
/// reaches both a public comment body and a `gh` argv) degrades to `None`
/// rather than failing the run; every claim this run writes then simply
/// carries no assignee (`scheduler/dispatch/022`).
async fn resolve_current_login(repo: &str) -> Option<String> {
    let argv = gh_current_login_argv();
    match run_capture("gh", &argv).await {
        Ok(out) => {
            let login = out.trim();
            if login.is_empty() {
                tracing::warn!(
                    repo,
                    "issue-dispatch: `gh api user` returned an empty login"
                );
                None
            } else if !validate_gh_login(login) {
                tracing::warn!(
                    repo,
                    login,
                    "issue-dispatch: `gh api user` returned a login that fails validation"
                );
                None
            } else {
                Some(login.to_string())
            }
        }
        Err(e) => {
            tracing::warn!(
                repo,
                error = %e,
                "issue-dispatch: failed to resolve the current gh login"
            );
            None
        }
    }
}

/// PRD #421 review fix B1: idempotently ensure [`IN_PROGRESS_LABEL`] exists on
/// `repo`, UNCONDITIONALLY (called once per run regardless of `cfg.triage` —
/// see the call site). Same best-effort discipline as [`ensure_triage_labels`]:
/// a `gh` failure here is logged and the run continues; the label being
/// missing is instead caught (and now reported, via C3) when `claim_issue`'s
/// own add-label call fails.
async fn ensure_claim_label(repo: &str) {
    let argv = label_create_argv(
        repo,
        IN_PROGRESS_LABEL,
        IN_PROGRESS_LABEL_COLOR,
        IN_PROGRESS_LABEL_DESCRIPTION,
    );
    if let Err(e) = run_status_args("gh", &argv).await {
        tracing::warn!(
            repo,
            label = IN_PROGRESS_LABEL,
            error = %e,
            "issue-dispatch: failed to ensure the in-progress claim label exists"
        );
    }
}

/// PRD #421 M2.0: idempotently ensure the triage label vocabulary exists on
/// `repo`. Best-effort per label, same discipline as [`claim_issue`]: a `gh`
/// failure on one label is logged and skipped, never propagated — it must not
/// abort the run, and a run with no labelling failure must never be reported
/// as one either.
async fn ensure_triage_labels(repo: &str) {
    for label in TRIAGE_LABELS {
        let argv = label_create_argv(repo, label.name, label.color, label.description);
        if let Err(e) = run_status_args("gh", &argv).await {
            tracing::warn!(
                repo,
                label = label.name,
                error = %e,
                "issue-dispatch: failed to ensure a triage label exists"
            );
        }
    }
}

/// PRD #421 M1.3: look up the deck's own claim comment for an issue —
/// called both from the label-skip arm of `dispatch_one_issue` (only when a
/// skip is already decided) and from [`claim_issue`]'s prior-assignee lookup
/// (PRD fork#235 M2). Best-effort: a `gh` failure here must not turn an
/// already-correct SKIP decision (or a successful claim) into a per-issue
/// failure, so any error degrades to `None` ("no claimant recorded") rather
/// than propagating.
async fn fetch_claim_comment(repo: &str, issue: u64) -> Option<ParsedClaim> {
    let argv = issue_view_comments_argv(repo, issue);
    let stdout = run_capture("gh", &argv).await.ok()?;
    parse_claim_comment(&stdout).ok().flatten()
}

/// Pure parse of `gh issue view --json comments` output into the deck's own
/// claim, if discoverable — the structured fields (PRD fork#235 M1), plus the
/// full `raw` text callers that only need the human-readable comment (the PRD
/// #421 skip-reason claimant text) can still use. Split out from
/// [`fetch_claim_comment`] so the JSON-shape logic is unit-testable without a
/// subprocess.
///
/// Takes the LAST matching comment, not the first (PRD #421 review C2 /
/// reviewer F4): `gh issue view --json comments` returns comments in
/// chronological order, and the PRD deliberately APPENDS rather than edits in
/// place precisely so a succession of claimants is preserved when one hands
/// off to another. Reading the first match reports the earliest, superseded
/// claimant instead of the current one.
fn parse_claim_comment(json: &str) -> Result<Option<ParsedClaim>, String> {
    let value: serde_json::Value = serde_json::from_str(json.trim())
        .map_err(|e| format!("failed to parse `gh issue view` JSON: {e}"))?;
    Ok(value
        .get("comments")
        .and_then(serde_json::Value::as_array)
        .and_then(|comments| {
            comments
                .iter()
                .filter_map(|c| c.get("body").and_then(serde_json::Value::as_str))
                .rfind(|body| body.starts_with(CLAIM_COMMENT_PREFIX))
                .and_then(parse_claim_fields)
        }))
}

/// Best-effort local hostname for the claim-comment body (PRD #421 M1.1) —
/// untested by any e2e assertion (the catalog leaves host/timestamp formatting
/// to the coder), so falling back on failure is fine.
///
/// Review fix C4 (auditor F6): reads the hostname IN-PROCESS rather than
/// spawning a `hostname` subprocess. The subprocess form resolved a THIRD
/// binary from `PATH` on every claim — a much more commonly-shadowed name
/// than `gh`/`git` — on an unattended, scheduled path, with the daemon's own
/// credentials in the environment; it also had no timeout on this async path
/// (like every other call here — see [`run_capture_args`]'s doc comment), so
/// a `hostname` that hung would wedge the whole dispatch. Reading it
/// in-process removes the PATH exposure and the hang together, and drops a
/// process spawn from the dispatch path entirely — no new dependency needed:
/// Unix calls `gethostname(2)` directly via `libc` (already a `cfg(unix)`
/// dependency for process-group handling elsewhere in this file); Windows has
/// no `gethostname`-equivalent windows-sys feature already enabled in this
/// crate, so it reads `COMPUTERNAME`, the same system environment variable
/// the `hostname` command itself would have reported.
pub(crate) fn local_hostname() -> String {
    #[cfg(unix)]
    {
        // SAFETY: `buf` is a valid buffer of `buf.len()` bytes for the
        // duration of the call; `gethostname` writes at most that many bytes
        // and NUL-terminates the result on success.
        let mut buf = vec![0u8; 256];
        let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
        if rc == 0 {
            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            let name = String::from_utf8_lossy(&buf[..end]);
            let name = name.trim();
            if !name.is_empty() {
                return name.to_string();
            }
        }
        "unknown-host".to_string()
    }
    #[cfg(windows)]
    {
        std::env::var("COMPUTERNAME")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown-host".to_string())
    }
    #[cfg(not(any(unix, windows)))]
    {
        "unknown-host".to_string()
    }
}

/// S5: resolve the task's `working_dir` to an ABSOLUTE workspace root. The
/// schedules loader already expands `~`/`$VAR` and resolves relatives against
/// `$HOME`, so a non-absolute value reaching the dispatch flow is a
/// misconfiguration — reject it rather than silently resolving against the
/// daemon's cwd (which would derive the wrong clone/worktree paths and drop
/// orchestration cleanup). An absolute input is normalized via
/// [`std::path::absolute`].
fn canonical_workspace(working_dir: &str) -> Result<PathBuf, String> {
    let p = Path::new(working_dir);
    if !p.is_absolute() {
        return Err(format!(
            "working_dir {working_dir:?} is not absolute; issue-dispatch requires an absolute \
             workspace root"
        ));
    }
    std::path::absolute(p)
        .map_err(|e| format!("failed to absolutize working_dir {working_dir:?}: {e}"))
}

/// M2.1: clone the repo if its dir is missing, else refresh the existing clone
/// (fetch + fast-forward pull). `gh` / `git` are resolved from `PATH` and inherit
/// the daemon's environment.
///
/// L3 (fail-closed): before touching a pre-existing clone dir, verify it is OUR
/// clone of `repo` by reading its `origin` — a missing origin (not a clone) or a
/// github.com origin for a DIFFERENT repo aborts the run without fetching,
/// pulling, writing, or deleting the dir.
///
/// S3: a refresh failure on an EXISTING clone is non-fatal — worktrees branch off
/// whatever refs are already on disk, so a transient `fetch`/`pull` error is
/// logged and the run continues. A MISSING clone that fails to clone stays fatal
/// (the run can't proceed without the repo).
async fn provision_repo(workspace: &Path, clone_dir: &Path, repo: &str) -> Result<(), String> {
    if clone_dir.is_dir() {
        let clone = clone_dir.to_string_lossy();
        let origin = run_capture_args("git", &["-C", &clone, "remote", "get-url", "origin"])
            .await
            .map_err(|e| {
                format!(
                    "clone dir {} has no usable git origin; refusing to refresh a foreign dir: {e}",
                    clone_dir.display()
                )
            })?;
        let origin = origin.trim();
        if !origin_matches_repo(origin, repo) {
            return Err(format!(
                "clone dir {} has origin {origin:?}, which does not match configured repo \
                 {repo:?}; refusing to fetch/pull (fail-closed)",
                clone_dir.display()
            ));
        }
        if let Err(e) = refresh_clone(&clone).await {
            tracing::warn!(
                clone = %clone_dir.display(),
                error = %e,
                "issue-dispatch: clone refresh failed; continuing with current refs"
            );
        }
        // Keep the per-issue `.worktrees/` dir out of the clone's `git status`
        // (idempotent, best-effort — never fails the run).
        ensure_worktrees_excluded(clone_dir);
        return Ok(());
    }
    std::fs::create_dir_all(workspace)
        .map_err(|e| format!("failed to create workspace {}: {e}", workspace.display()))?;
    run_status("gh", &["repo", "clone", repo, &clone_dir.to_string_lossy()]).await?;
    // Same hygiene on the fresh clone, so it holds across the first AND every
    // later fire.
    ensure_worktrees_excluded(clone_dir);
    Ok(())
}

/// Keep the per-issue worktrees dir (`<clone>/.worktrees/`) out of the clone's
/// `git status` WITHOUT touching the user's tracked files: append `.worktrees/`
/// to the clone's LOCAL exclude file (`<clone>/.git/info/exclude`) — never a
/// committed `.gitignore`, because the cloned repo belongs to the user and we
/// must not modify their tracked/committed files. `.worktrees/` sits in the main
/// clone's working tree and would otherwise show as untracked to anyone running
/// `git status` in the clone (agents run INSIDE a worktree, above which it isn't
/// visible — so this is hygiene for the main clone).
///
/// Idempotent: the line is appended only if not already present, so repeated
/// fires never duplicate it; `.git/info/` is created if missing. Best-effort: any
/// I/O failure is logged at WARN and swallowed — it must NEVER fail the dispatch
/// run.
fn ensure_worktrees_excluded(clone_dir: &Path) {
    const WORKTREES_EXCLUDE_LINE: &str = ".worktrees/";
    let info_dir = clone_dir.join(".git").join("info");
    let exclude_path = info_dir.join("exclude");

    // A missing exclude reads as empty — treat that as "line absent".
    let existing = std::fs::read_to_string(&exclude_path).unwrap_or_default();
    if existing
        .lines()
        .any(|line| line.trim() == WORKTREES_EXCLUDE_LINE)
    {
        return;
    }

    if let Err(e) = std::fs::create_dir_all(&info_dir) {
        tracing::warn!(
            clone = %clone_dir.display(),
            error = %e,
            "issue-dispatch: could not create .git/info to exclude .worktrees/"
        );
        return;
    }

    // Append on its own line, inserting a separating newline only when the
    // existing content lacks a trailing one.
    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(WORKTREES_EXCLUDE_LINE);
    content.push('\n');
    if let Err(e) = std::fs::write(&exclude_path, content) {
        tracing::warn!(
            clone = %clone_dir.display(),
            error = %e,
            "issue-dispatch: could not write .git/info/exclude to exclude .worktrees/"
        );
    }
}

/// S3: refresh an existing clone in place — `git fetch` then `git pull --ff-only`.
/// The caller treats any failure here as non-fatal (warn + continue).
async fn refresh_clone(clone: &str) -> Result<(), String> {
    run_status("git", &["-C", clone, "fetch"]).await?;
    run_status("git", &["-C", clone, "pull", "--ff-only"]).await
}

/// L3: whether an existing clone's `origin` is consistent with the configured
/// `repo`. A recognizable github.com origin must resolve to the same
/// `owner/name` (case-insensitive); a non-github origin — a self-hosted host or
/// the local fixture remote used in tests — cannot be attributed to an
/// `owner/name`, so it is accepted (we provisioned it). The strict case this
/// guards is a clone-dir collision where `origin` points at a DIFFERENT GitHub
/// repo than configured.
fn origin_matches_repo(origin: &str, repo: &str) -> bool {
    match github_owner_name(origin) {
        Some(found) => found == repo.to_ascii_lowercase(),
        None => true,
    }
}

/// Normalize a github.com remote URL to lowercase `owner/name`, or `None` if it
/// is not a recognizable github.com remote (other hosts, local paths, …).
/// Handles the `https://`, `http://`, `ssh://git@`, `git://`, and `git@…:` forms,
/// with or without a trailing `.git`.
fn github_owner_name(origin: &str) -> Option<String> {
    let s = origin.trim();
    let rest = s
        .strip_prefix("https://github.com/")
        .or_else(|| s.strip_prefix("http://github.com/"))
        .or_else(|| s.strip_prefix("ssh://git@github.com/"))
        .or_else(|| s.strip_prefix("git://github.com/"))
        .or_else(|| s.strip_prefix("git@github.com:"))?;
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let rest = rest.trim_end_matches('/');
    let mut parts = rest.split('/');
    let owner = parts.next()?;
    let name = parts.next()?;
    if owner.is_empty() || name.is_empty() || parts.next().is_some() {
        return None;
    }
    Some(format!(
        "{}/{}",
        owner.to_ascii_lowercase(),
        name.to_ascii_lowercase()
    ))
}

/// One entry from `gh issue list --json number,labels`: the issue number and
/// whether it already carries the `in-progress` label (PRD #421 M1.2's list-
/// embedded read mechanism — see `issue_list_argv`'s doc comment for why this
/// rides along on the enumeration call rather than a separate `gh issue view`
/// per candidate).
struct OpenIssue {
    number: u64,
    in_progress_label: bool,
}

/// Enumerate the repo's open issues (number + `in-progress` label presence) in
/// returned order.
async fn list_open_issues(cfg: &IssueDispatchConfig) -> Result<Vec<OpenIssue>, String> {
    let argv = issue_list_argv(
        &cfg.repo,
        cfg.max_per_run,
        cfg.label.as_deref(),
        cfg.query.as_deref(),
    );
    let stdout = run_capture("gh", &argv).await?;
    parse_open_issues(&stdout)
}

/// The secondary idempotency signal: whether an open PR's head is
/// `agent/issue-<n>`. A non-empty `gh pr list` JSON array means yes.
async fn issue_has_open_pr(repo: &str, issue: u64) -> Result<bool, String> {
    let argv = pr_list_for_issue_argv(repo, issue);
    let stdout = run_capture("gh", &argv).await?;
    parse_open_pr_present(&stdout)
}

/// N1: parse `gh pr list --json number` into "is there an open PR?". Malformed
/// output (invalid JSON, or valid JSON that is NOT an array) PROPAGATES as an
/// error — symmetric with [`parse_open_issues`] — so the per-issue boundary
/// skips + logs the issue (fail-safe) rather than silently reading it as "no PR
/// → dispatch", which would risk a duplicate dispatch.
fn parse_open_pr_present(json: &str) -> Result<bool, String> {
    let value: serde_json::Value = serde_json::from_str(json.trim())
        .map_err(|e| format!("failed to parse `gh pr list` JSON: {e}"))?;
    let arr = value
        .as_array()
        .ok_or_else(|| "`gh pr list` did not return a JSON array".to_string())?;
    Ok(!arr.is_empty())
}

/// Outcome of [`create_worktree`] / [`create_worktree_sync`]: either we
/// created the worktree, a concurrent creator had already claimed it (the
/// benign TOCTOU race below), or the `git worktree add` itself was killed
/// for exceeding [`WORKTREE_GIT_TIMEOUT`] while the directory it half-created
/// is still present. The async caller (issue-dispatch) surfaces
/// `AlreadyClaimed` as a skip; the sync caller (fork #122's orchestration-tab
/// `SpawnPane` path) surfaces it as a fail-loud refusal — there is no
/// "concurrent fire" concept for a single interactively-opened tab, so an
/// already-claimed path there means something else already occupies it.
///
/// Fork #122/#123 re-audit (P2): `TimedOut` is deliberately its own variant,
/// never folded into `AlreadyClaimed` — the two mean different things
/// (another actor holds this path vs. we half-created it ourselves) and
/// reporting one as the other hid the wedge entirely (see
/// [`classify_worktree_add_result`]). `cleaned_up` records whether the
/// best-effort `git worktree remove --force` attempt
/// ([`create_worktree_sync`]) confirmed the half-created directory was
/// actually removed, so the caller can tell the user either "try again" or
/// give them the exact manual command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorktreeCreation {
    Created,
    AlreadyClaimed,
    TimedOut { cleaned_up: bool },
}

/// Ensure the worktree's parent directory exists before `git worktree add`
/// runs, so the add never trips on a missing dir. Shared by
/// [`create_worktree`] and [`create_worktree_sync`] — plain `std::fs`, so it
/// needs no async/sync split.
fn ensure_worktree_parent_dir(worktree_dir: &Path) -> Result<(), String> {
    if let Some(parent) = worktree_dir.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create worktree parent {}: {e}", parent.display()))?;
    }
    Ok(())
}

/// Pure classification result of [`classify_worktree_add_result`] — the
/// shape under direct unit test (`orchestration/worktree/007`), before
/// [`create_worktree_sync`] layers cleanup on top of `TimedOut` to produce
/// the richer [`WorktreeCreation`] its own caller sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddOutcome {
    Created,
    AlreadyClaimed,
    TimedOut,
}

/// A failed `git` invocation from [`run_status`] / [`run_status_args`] /
/// [`run_status_sync`], distinguishing a genuine command failure from the
/// bounded [`run_status_sync`] wait expiring before the child exited.
/// [`run_status`]/[`run_status_args`] can never time out (no bound on that
/// path — see their doc comments) and always produce `Failed`.
#[derive(Debug)]
enum AddError {
    Failed(String),
    TimedOut(String),
}

/// TOCTOU classification shared by [`create_worktree`] and
/// [`create_worktree_sync`]: the caller only reaches here after seeing the
/// worktree dir ABSENT, but a concurrent creator can win the race in the
/// window before `git worktree add` runs — the add then fails on the now-
/// present path. Because we only arrive with the dir believed absent, its
/// presence after a failed add means a concurrent claim, not our error:
/// report [`AddOutcome::AlreadyClaimed`] instead of a hard failure. A
/// genuine add failure (bad ref, permissions, …) leaves the dir absent and
/// still propagates as `Err`.
///
/// Fork #122/#123 re-audit (P2): a *timed-out* add with the directory
/// present is a DIFFERENT situation from a concurrent claim — `git worktree
/// add` registers the worktree before checkout/hooks finish, so a killed add
/// leaves a half-created directory behind that is ours, not another actor's.
/// Reporting it as `AlreadyClaimed` was the bug: every later attempt at that
/// slug saw the same present directory and took the same "already claimed"
/// branch forever, with no cleanup and no way for the user to tell what
/// actually happened. `TimedOut` keeps that case distinct all the way to the
/// caller instead.
fn classify_worktree_add_result(
    worktree_dir: &Path,
    add_result: Result<(), AddError>,
) -> Result<AddOutcome, String> {
    match add_result {
        Ok(()) => Ok(AddOutcome::Created),
        Err(AddError::TimedOut(e)) => {
            if worktree_dir.exists() {
                Ok(AddOutcome::TimedOut)
            } else {
                Err(e)
            }
        }
        Err(AddError::Failed(e)) => {
            if worktree_dir.exists() {
                Ok(AddOutcome::AlreadyClaimed)
            } else {
                Err(e)
            }
        }
    }
}

/// M2.2: create the per-issue worktree on `agent/issue-<n>`.
///
/// B1: `git worktree remove` PRESERVES the branch, so an issue that was
/// dispatched, had its tab closed without a PR, and is still open leaves
/// `agent/issue-<n>` behind. A naive `worktree add -b <branch>` would then fail
/// ("a branch named … already exists") on EVERY later fire, permanently wedging
/// the reuse-the-vacated-slot model. So probe for the branch first: attach the
/// existing branch (no `-b`) when it is already there, and only create it (`-b`)
/// when it is not. See [`crate::issue_dispatch::worktree_branch_probe_argv`] /
/// [`crate::issue_dispatch::worktree_add_argv`], shared with
/// [`create_worktree_sync`] so the two argv shapes cannot drift.
async fn create_worktree(
    clone_dir: &Path,
    worktree_dir: &Path,
    branch: &str,
    creator: &str,
) -> Result<WorktreeCreation, String> {
    ensure_worktree_parent_dir(worktree_dir)?;
    let branch_exists = run_status_args(
        "git",
        &crate::issue_dispatch::worktree_branch_probe_argv(clone_dir, branch),
    )
    .await
    .is_ok();
    let add = run_status_args(
        "git",
        &crate::issue_dispatch::worktree_add_argv(clone_dir, worktree_dir, branch, branch_exists),
    )
    .await
    .map_err(AddError::Failed);
    // `run_status_args` has no bound (see its doc comment), so `AddOutcome::TimedOut`
    // is unreachable on this path today; handled here only so the match stays
    // exhaustive if that ever changes.
    Ok(match classify_worktree_add_result(worktree_dir, add)? {
        AddOutcome::Created => {
            mark_worktree_owned_best_effort(worktree_dir, creator);
            WorktreeCreation::Created
        }
        AddOutcome::AlreadyClaimed => WorktreeCreation::AlreadyClaimed,
        AddOutcome::TimedOut => WorktreeCreation::TimedOut { cleaned_up: false },
    })
}

/// Write the `dot-agent-deck-owner` marker (issue #144 finding 1) for a
/// worktree this call just created — best-effort, matching
/// `ensure_worktrees_excluded`'s established pattern in this file: log at
/// WARN and continue rather than failing the whole worktree creation. The
/// expensive, valuable operation (`git worktree add` itself) already
/// succeeded; a missing marker only means a future `reclaim` lands this
/// worktree on `Ask` instead of `Remove` (annoying, never unsafe — see
/// [`crate::worktree_reclaim::mark_worktree_owned`]'s doc comment).
///
/// `creator` (issue #425) names the task or orchestration responsible for
/// this worktree — forwarded verbatim to `mark_worktree_owned`, which
/// sanitises it before writing.
fn mark_worktree_owned_best_effort(worktree_dir: &Path, creator: &str) {
    if let Err(e) = crate::worktree_reclaim::mark_worktree_owned(worktree_dir, creator) {
        tracing::warn!(
            worktree = %worktree_dir.display(),
            error = %e,
            "issue-dispatch: could not write ownership marker; this worktree will require \
             `reclaim --yes` instead of a bare `reclaim` later"
        );
    }
}

/// Sync twin of [`create_worktree`] for the TUI's synchronous `SpawnPane`
/// dispatch (fork #122): `dispatch_action` is not `async` and this runs on
/// the hot input path, so it cannot `.await` the tokio-based creator. Shares
/// every decision with the async path — argv construction
/// ([`crate::issue_dispatch::worktree_branch_probe_argv`] /
/// [`crate::issue_dispatch::worktree_add_argv`]), the parent-dir creation
/// ([`ensure_worktree_parent_dir`]), and the TOCTOU classification
/// ([`classify_worktree_add_result`]) — so the two implementations cannot
/// drift on WHAT to run or how to interpret the result; only the actual
/// process spawn (blocking `std::process::Command` here, `tokio::process`
/// there) differs.
pub(crate) fn create_worktree_sync(
    clone_dir: &Path,
    worktree_dir: &Path,
    branch: &str,
    creator: &str,
) -> Result<WorktreeCreation, String> {
    ensure_worktree_parent_dir(worktree_dir)?;
    let branch_exists = run_status_sync(
        "git",
        &crate::issue_dispatch::worktree_branch_probe_argv(clone_dir, branch),
        WORKTREE_GIT_TIMEOUT,
    )
    .is_ok();
    let add = run_status_sync(
        "git",
        &crate::issue_dispatch::worktree_add_argv(clone_dir, worktree_dir, branch, branch_exists),
        WORKTREE_GIT_TIMEOUT,
    );
    Ok(match classify_worktree_add_result(worktree_dir, add)? {
        AddOutcome::Created => {
            mark_worktree_owned_best_effort(worktree_dir, creator);
            WorktreeCreation::Created
        }
        AddOutcome::AlreadyClaimed => WorktreeCreation::AlreadyClaimed,
        // Fork #122/#123 re-audit (P2): the add registered `worktree_dir`
        // before it was killed. Best-effort clean it up now, bounded by its
        // own (shorter) timeout so a stuck cleanup cannot hang the very loop
        // this exists to unwedge — see `attempt_worktree_cleanup`.
        AddOutcome::TimedOut => {
            let cleaned_up = attempt_worktree_cleanup(clone_dir, worktree_dir);
            WorktreeCreation::TimedOut { cleaned_up }
        }
    })
}

/// Fork #122/#123 re-audit (P2): best-effort cleanup for a `git worktree add`
/// killed by [`run_status_sync`]'s [`WORKTREE_GIT_TIMEOUT`] bound —
/// `git worktree add` registers the worktree before checkout/hooks finish,
/// so the kill leaves a half-created directory (and usually its
/// registration) behind that would otherwise wedge this slug permanently
/// (every later attempt sees the directory present and refuses it).
///
/// Runs through the same bounded, non-interactive subprocess path as the add
/// itself (stdin closed, `GIT_TERMINAL_PROMPT=0`, …) but with its OWN,
/// shorter timeout ([`WORKTREE_CLEANUP_TIMEOUT`]) — a stuck cleanup must not
/// hang the loop it exists to protect. Only ever targets `worktree_dir`, the
/// exact path this invocation derived and already confirmed to be the
/// intended direct sibling — never a path from anywhere else.
///
/// "Confirmed" means both the command exited successfully AND the directory
/// is actually gone afterward; either check failing is reported as `false`
/// so the caller fails loudly with the manual command rather than assuming
/// success it cannot back up.
fn attempt_worktree_cleanup(clone_dir: &Path, worktree_dir: &Path) -> bool {
    let removed = run_status_sync(
        "git",
        &crate::issue_dispatch::worktree_remove_argv(clone_dir, worktree_dir),
        WORKTREE_CLEANUP_TIMEOUT,
    )
    .is_ok();
    removed && !worktree_dir.exists()
}

/// Parse a `gh issue list --json number,labels` array into [`OpenIssue`]s, in
/// order. Entries missing a numeric `number` are skipped rather than failing
/// the whole parse; a missing/empty `labels` array (or one that doesn't name
/// `in-progress`) reads as not-labelled.
fn parse_open_issues(json: &str) -> Result<Vec<OpenIssue>, String> {
    let value: serde_json::Value = serde_json::from_str(json.trim())
        .map_err(|e| format!("failed to parse `gh issue list` JSON: {e}"))?;
    let arr = value
        .as_array()
        .ok_or_else(|| "`gh issue list` did not return a JSON array".to_string())?;
    Ok(arr
        .iter()
        .filter_map(|item| {
            let number = item.get("number").and_then(serde_json::Value::as_u64)?;
            let in_progress_label = item
                .get("labels")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|labels| {
                    labels.iter().any(|l| {
                        l.get("name").and_then(serde_json::Value::as_str) == Some(IN_PROGRESS_LABEL)
                    })
                });
            Some(OpenIssue {
                number,
                in_progress_label,
            })
        })
        .collect())
}

/// Run a subprocess that must exit zero; on failure return a message carrying
/// the program, args, exit status, and any stderr.
///
/// Fork #122/#123 audit (P2): stdin closed and a non-interactive git
/// environment applied — `GIT_TERMINAL_PROMPT=0` suppresses git's own
/// terminal credential prompt, and neutralising `GIT_ASKPASS`/`SSH_ASKPASS`
/// stops git/ssh from shelling out to an inherited askpass helper that
/// could itself block waiting on input — so a credential prompt can no
/// longer read anything and fails fast instead of waiting. No bounded wait
/// here, unlike [`run_status_sync`] below: this async path already runs off
/// the render/event loop (inside the issue-dispatch scheduler's own tokio
/// task), so a slow call here does not freeze the TUI the way a synchronous
/// one on the `SpawnPane` dispatch path would.
///
/// Fork issue #133 deliberately does not touch this function: `.output()`
/// waits unboundedly and this path never kills its child on a timeout, so it
/// has no kill whose blast radius could be too narrow — there is nothing here
/// for a process-group kill to fix, and no `AgentProcessGroup` handle worth
/// threading through an await that never times out. The asymmetry with
/// [`run_status_sync`] is intentional, not an oversight.
async fn run_status(program: &str, args: &[&str]) -> Result<(), String> {
    let output = tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env("SSH_ASKPASS", "")
        .output()
        .await
        .map_err(|e| format!("failed to run `{program}`: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(format!(
        "`{program} {}` failed ({}): {}",
        args.join(" "),
        output.status,
        stderr.trim()
    ))
}

/// Like [`run_status`] but for `String` args — the `git` worktree argv
/// helpers ([`crate::issue_dispatch::worktree_branch_probe_argv`] /
/// [`crate::issue_dispatch::worktree_add_argv`]) produce `Vec<String>`.
async fn run_status_args(program: &str, args: &[String]) -> Result<(), String> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_status(program, &refs).await
}

/// Fork #122/#123 audit (P2): the maximum time a single blocking `git`
/// invocation made from [`create_worktree_sync`] — which runs directly on
/// the TUI's synchronous render/event loop — is allowed to run before it is
/// killed. `git worktree add` does no network I/O, so under normal
/// conditions this returns in well under a second; the bound exists for the
/// pathological cases the audit named — a stuck `index.lock`, a slow
/// filesystem, or a misbehaving checkout hook — where `Command::output()`
/// would otherwise wait forever with the TUI unable to repaint, show an
/// error, or accept input. 30s is generous enough that a slow-but-working
/// worktree checkout is never killed, and short enough that a genuine wedge
/// does not read as "the TUI is just doing something".
const WORKTREE_GIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Fork #122/#123 re-audit (P2): the bound for [`attempt_worktree_cleanup`]'s
/// `git worktree remove --force` call — deliberately its OWN, shorter timeout
/// rather than reusing [`WORKTREE_GIT_TIMEOUT`], so a stuck cleanup cannot
/// itself hang for as long as the add it is cleaning up after. A plain
/// `remove` does no checkout work and no hooks run, so this only needs to
/// cover filesystem removal of a directory that may still have a partial
/// checkout in it.
const WORKTREE_CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Fork issue #133: how long [`run_status_sync`] waits after SIGTERM before
/// escalating to SIGKILL when [`WORKTREE_GIT_TIMEOUT`] (or
/// [`WORKTREE_CLEANUP_TIMEOUT`]) expires and the child's whole process group
/// is torn down via
/// [`crate::platform::proc::terminate_child_with_grace_and_detached_reap_forcing_group_backstop`].
/// This grace is spent blocking the TUI's synchronous render/event loop — the
/// exact thing the timeout above exists to protect — so it is kept far
/// shorter than either timeout rather than reusing one of them. 200ms is
/// enough for `git` and any hook it ran to notice SIGTERM and exit on the
/// common path, while staying short enough that even the worst case (a hook
/// that ignores SIGTERM entirely, forcing the SIGKILL backstop) only adds
/// 200ms on top of the timeout that already fired.
///
/// Fork issue #136: this is no longer a hard bound on the whole call, and it
/// never was one that could be stated that precisely. What changed is that
/// the *unbounded* final reap is gone: both platforms' backstops used to end
/// their SIGKILL phase with a blocking `wait()`/`wait4()` on this thread, so a
/// process wedged in uninterruptible kernel I/O (e.g. a stuck NFS mount)
/// could delay the call's return arbitrarily, no matter how short this grace
/// was set — the exact freeze `WORKTREE_GIT_TIMEOUT` exists to prevent,
/// reappearing through a narrower door. The final reap now normally runs on a
/// short-lived detached thread instead
/// ([`crate::platform::proc::detach_reap_or_fallback_sync`]), so escalation
/// is *normally* limited to this grace window plus operation overhead — and
/// this grace is now an accurate floor on that escalation, not merely an
/// approximate one: phase 2 is a single flat `sleep(grace)` with no poll
/// loop to overshoot, so nothing between the timeout firing and phase 3
/// starting can add more than the requested duration. What remains
/// unaccounted for is signal delivery, OS scheduling, and thread creation.
/// When the process-wide outstanding-reap cap is saturated, or spawning the
/// detached thread itself fails, the reap falls back to a synchronous
/// `wait()` on this same calling thread instead — reintroducing a real, if
/// rare and cap-bounded, block for that call. See
/// [`crate::platform::proc::detach_reap_or_fallback_sync`] for the cap value
/// and both fallback paths.
const WORKTREE_GIT_KILL_GRACE: Duration = Duration::from_millis(200);

/// How often [`run_status_sync`] polls its spawned child for exit while
/// enforcing its caller-supplied timeout. Short enough that the timeout
/// bound is accurate to a fraction of a second; long enough not to spin the
/// CPU on the render/event loop while a normal, fast `git` call is still
/// running.
const WORKTREE_GIT_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Configure and spawn the `git` child [`run_status_sync`] bounds, plus the
/// [`crate::platform::proc::AgentProcessGroup`] handle its later
/// [`crate::platform::proc::terminate_child_with_grace_and_wait`] escalation
/// needs (fork issue #133).
///
/// On Unix the spawn goes through
/// [`crate::platform::proc::spawn_in_new_process_group`] so the child leads
/// its own process group and a subsequent `killpg` reaches it plus any hook
/// grandchild it forked (a plain `std::process::Command::spawn()` would
/// inherit the deck's own group instead). On Windows there is no spawn-time
/// hook to make: `AgentProcessGroup::adopt(pid)` already works post-hoc there
/// (the group is a Job Object the child is added to after the fact), so the
/// child is spawned exactly as before and only then adopted.
///
/// Residual on Windows: a descendant the child forks in the window between
/// `spawn()` returning and `adopt()` running is not yet in the Job Object and
/// so is not reached by a later `TerminateJobObject` — the same class
/// `windows.rs:194` already documents for agent teardown. Closing it needs
/// `CREATE_SUSPENDED` plus a resume after `adopt()`, which is out of scope
/// here; the window is small (no code runs between the two calls below) and
/// unchanged from today's behavior.
fn spawn_git_status_child(
    program: &str,
    args: &[&str],
) -> std::io::Result<(
    std::process::Child,
    crate::platform::proc::AgentProcessGroup,
)> {
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env("SSH_ASKPASS", "");

    #[cfg(unix)]
    {
        crate::platform::proc::spawn_in_new_process_group(&mut command)
    }
    #[cfg(windows)]
    {
        let child = command.spawn()?;
        // See the doc comment above: the descendant-before-adopt window is
        // the accepted residual, not a new gap introduced here.
        let group = crate::platform::proc::AgentProcessGroup::adopt(Some(child.id()));
        Ok((child, group))
    }
}

/// Blocking twin of [`run_status_args`] for [`create_worktree_sync`] and
/// [`attempt_worktree_cleanup`], which run on the TUI's synchronous
/// `SpawnPane` dispatch path and cannot `.await`.
///
/// Fork #122/#123 audit (P2), two layers: stdin closed and a
/// non-interactive git environment applied — same three env vars as
/// [`run_status`] above — so a credential prompt fails fast instead of
/// waiting on input nothing will ever supply; and `Command::output()` —
/// which waits for termination with no bound — is replaced with `spawn()`
/// plus `try_wait()` polling against the caller-supplied `timeout`, killing
/// the child and returning [`AddError::TimedOut`] on expiry, rather than
/// leaving the render/event loop unable to repaint, show an error, or
/// accept input. `timeout` is a parameter (fork #122/#123 re-audit, P2)
/// rather than the fixed [`WORKTREE_GIT_TIMEOUT`] so [`attempt_worktree_cleanup`]
/// can give its own cleanup call a shorter, independent bound.
fn run_status_sync(program: &str, args: &[String], timeout: Duration) -> Result<(), AddError> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let (mut child, group) = spawn_git_status_child(program, &refs)
        .map_err(|e| AddError::Failed(format!("failed to run `{program}`: {e}")))?;

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    // Fork issue #133: escalate to a whole-process-group kill
                    // instead of a bare `child.kill()`, which reaps only the
                    // direct `git` process and leaves a hook grandchild (e.g.
                    // `post-checkout`) running past this bound. `child` leads
                    // its own group on Unix (see `spawn_git_status_child` /
                    // `spawn_in_new_process_group`), so `killpg` here reaches
                    // it and everything it forked.
                    //
                    // Fork issue #136: the detached-reap variant, not the
                    // synchronous one — this call runs on the TUI's
                    // synchronous render/event loop (this function's only
                    // caller path from `dispatch_action`), so the final
                    // reap must not block here. See that function's doc
                    // comment for why.
                    let boxed: Box<dyn portable_pty::Child + Send + Sync> =
                        Box::new(crate::platform::proc::test_child::StdChild(child));
                    crate::platform::proc::terminate_child_with_grace_and_detached_reap_forcing_group_backstop(
                        boxed,
                        WORKTREE_GIT_KILL_GRACE,
                        &group,
                    );
                    return Err(AddError::TimedOut(format!(
                        "`{program} {}` timed out after {timeout:?} without exiting",
                        refs.join(" "),
                    )));
                }
                std::thread::sleep(WORKTREE_GIT_POLL_INTERVAL);
            }
            Err(e) => {
                return Err(AddError::Failed(format!(
                    "failed to wait on `{program}`: {e}"
                )));
            }
        }
    };

    if status.success() {
        return Ok(());
    }
    let mut stderr_buf = Vec::new();
    if let Some(mut stderr) = child.stderr.take() {
        use std::io::Read;
        let _ = stderr.read_to_end(&mut stderr_buf);
    }
    let stderr = String::from_utf8_lossy(&stderr_buf);
    Err(AddError::Failed(format!(
        "`{program} {}` failed ({}): {}",
        refs.join(" "),
        status,
        stderr.trim()
    )))
}

/// Run a subprocess that must exit zero and return its captured stdout. Accepts
/// `String` args (the `gh` argv helpers produce `Vec<String>`).
async fn run_capture(program: &str, args: &[String]) -> Result<String, String> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_capture_args(program, &refs).await
}

/// Like [`run_capture`] but for `&str` args — the fixed-shape `git` probes
/// (e.g. `remote get-url origin`) build their argv inline.
async fn run_capture_args(program: &str, args: &[&str]) -> Result<String, String> {
    let output = tokio::process::Command::new(program)
        .args(args)
        .output()
        .await
        .map_err(|e| format!("failed to run `{program}`: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "`{program} {}` failed ({}): {}",
            args.join(" "),
            output.status,
            stderr.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use spec::spec;

    #[test]
    fn parse_open_issues_reads_number_and_labels_in_order() {
        let json = r#"[{"number":7},{"number":8,"labels":[{"name":"in-progress"}]},{"number":3}]"#;
        let issues = parse_open_issues(json).unwrap();
        let got: Vec<(u64, bool)> = issues
            .iter()
            .map(|i| (i.number, i.in_progress_label))
            .collect();
        assert_eq!(got, vec![(7, false), (8, true), (3, false)]);
    }

    #[test]
    fn parse_open_issues_empty_array() {
        assert!(parse_open_issues("[]\n").unwrap().is_empty());
    }

    #[test]
    fn parse_open_issues_rejects_non_array() {
        assert!(parse_open_issues("{}").is_err());
        assert!(parse_open_issues("not json").is_err());
    }

    #[test]
    fn parse_open_issues_other_labels_present_but_not_in_progress() {
        let json = r#"[{"number":9,"labels":[{"name":"bug"},{"name":"priority:high"}]}]"#;
        let issues = parse_open_issues(json).unwrap();
        assert_eq!(issues.len(), 1);
        assert!(!issues[0].in_progress_label);
    }

    // --- PRD #421 M1.3: `gh issue view --json comments` claimant parsing ---

    #[test]
    fn parse_claim_comment_no_comments() {
        assert_eq!(parse_claim_comment(r#"{"comments":[]}"#).unwrap(), None);
    }

    #[test]
    fn parse_claim_comment_unrelated_comment_only() {
        // A human/external tool applied the label directly — some comment may
        // exist, but none matches the deck's own claim-comment prefix.
        let json = r#"{"comments":[{"body":"unrelated"}]}"#;
        assert_eq!(parse_claim_comment(json).unwrap(), None);
    }

    #[test]
    fn parse_claim_comment_finds_deck_claim() {
        let body = "Claimed by the issue-dispatch task `dispatch-task` (issue #7) working `/work/dispatch-task/.worktrees/issue-7` on branch `agent/issue-7` at 2026-08-09T00:00:00Z, for @alice.";
        let json = format!(r#"{{"comments":[{{"body":"unrelated"}},{{"body":"{body}"}}]}}"#);
        let parsed = parse_claim_comment(&json)
            .unwrap()
            .expect("must find the claim");
        assert_eq!(
            parsed.identity,
            "worktree:/work/dispatch-task/.worktrees/issue-7@agent/issue-7"
        );
        assert_eq!(parsed.login.as_deref(), Some("alice"));
        assert_eq!(parsed.raw, body);
    }

    #[test]
    fn parse_claim_comment_rejects_non_object() {
        assert!(parse_claim_comment("not json").is_err());
    }

    #[test]
    fn parse_claim_comment_newest_claim_wins() {
        // C2 / reviewer F4: comments come back in chronological order and the
        // PRD deliberately APPENDS a new claim rather than editing the old one
        // in place on a handover, so the LAST matching comment — not the
        // first — is the current claimant.
        let json = r#"{"comments":[
            {"body":"Claimed by the issue-dispatch task `nightly-a` (issue #1) working `/work/nightly-a/.worktrees/issue-1` on branch `agent/issue-1` at 2026-08-01T00:00:00Z, for @nina."},
            {"body":"unrelated"},
            {"body":"Claimed by the issue-dispatch task `nightly-b` (issue #1) working `/work/nightly-b/.worktrees/issue-1` on branch `agent/issue-1` at 2026-08-09T00:00:00Z, for @bob."}
        ]}"#;
        let parsed = parse_claim_comment(json)
            .unwrap()
            .expect("must find the claim");
        assert_eq!(
            parsed.identity,
            "worktree:/work/nightly-b/.worktrees/issue-1@agent/issue-1"
        );
        assert_eq!(parsed.login.as_deref(), Some("bob"));
    }

    // --- PRD fork#235 round-3 hardening: issue/claim/019 ---

    /// A minimal, single-purpose synthetic `gh` for
    /// [`issue_claim_019_dispatch_path_assignee_refresh_keeps_assignee`]
    /// only — not the full stateful fixture `tests/issue_claim.rs` uses,
    /// since this test drives [`claim_issue`] directly rather than the CLI
    /// subprocess. `issue view --json comments` always reports ONE prior
    /// claim naming `$PRIOR_LOGIN` (simulating a same-identity refresh);
    /// `issue edit --add-assignee`/`--remove-assignee` apply into
    /// `$GHSTUB_DIR/assignees.txt` in the SAME add-then-remove order real
    /// `gh` applies (matching `tests/issue_claim.rs`'s `issue/claim/011`
    /// fix), so a self-cancelling pair nets the file UNASSIGNED exactly as
    /// it would against a real `gh`; every other verb is a no-op.
    const CLAIM_019_GH_STUB: &str = r#"#!/bin/sh
group="$1"; sub="$2"; shift 2 2>/dev/null || true
add_assignee=""; remove_assignee=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        --add-assignee) shift; add_assignee="$1" ;;
        --remove-assignee) shift; remove_assignee="$1" ;;
        *) ;;
    esac
    shift
done
if [ "$group" = "issue" ] && [ "$sub" = "view" ]; then
    printf '{"comments":[{"body":"Claimed by the orchestration `prior` working `/ws/prior` on branch `prior-branch` at 2020-01-01T00:00:00Z, for @%s."}]}\n' "$PRIOR_LOGIN"
    exit 0
fi
if [ "$group" = "issue" ] && [ "$sub" = "edit" ]; then
    if [ -n "$add_assignee" ]; then
        grep -qxF "$add_assignee" "$GHSTUB_DIR/assignees.txt" 2>/dev/null || printf '%s\n' "$add_assignee" >> "$GHSTUB_DIR/assignees.txt"
    fi
    if [ -n "$remove_assignee" ] && [ -f "$GHSTUB_DIR/assignees.txt" ]; then
        grep -vxF "$remove_assignee" "$GHSTUB_DIR/assignees.txt" > "$GHSTUB_DIR/assignees.txt.tmp" 2>/dev/null
        mv "$GHSTUB_DIR/assignees.txt.tmp" "$GHSTUB_DIR/assignees.txt" 2>/dev/null || true
    fi
    exit 0
fi
exit 0
"#;

    /// Scenario: [`claim_issue`] (the unattended `issue_dispatch` claim
    /// path) is called with the SAME login as a claim comment already on
    /// record — a same-identity refresh, e.g. the same task re-dispatching
    /// an issue it already claimed. Reviewer R3 / auditor A8: the
    /// `--add-assignee X --remove-assignee X` self-cancelling-pair fix
    /// landed on `issue_claim::do_claim` only (`issue/claim/011`); this
    /// function still emits the self-cancelling pair unconditionally. Assert
    /// the assignee ends up STILL SET to that login afterward, never
    /// unassigned.
    //
    // Written as a sync `#[test]` driving an explicit runtime rather than
    // `#[tokio::test]`: the linkage-check (PRD #77 Decision 17) ties each
    // `#[spec(...)]` to the next plain `fn` definition and does not
    // recognize a `#[tokio::test] async fn` — see `tests/shell_activity.rs`'s
    // `shell_activity_001` for the same pattern.
    #[spec("issue/claim/019")]
    #[test]
    #[cfg(unix)]
    fn issue_claim_019_dispatch_path_assignee_refresh_keeps_assignee() {
        let scratch = tempfile::tempdir().unwrap();
        let bindir = scratch.path().join("bin");
        std::fs::create_dir_all(&bindir).unwrap();
        let gh = bindir.join("gh");
        std::fs::write(&gh, CLAIM_019_GH_STUB).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let ghstub = scratch.path().join("ghstub");
        std::fs::create_dir_all(&ghstub).unwrap();

        let prior_path = std::env::var("PATH").unwrap_or_default();
        // SAFETY: `std::env::set_var` is process-global, but CLAUDE.md rule 5's
        // fork addendum means every test run happens in CI via `cargo nextest`,
        // which runs each test in its OWN process — so no sibling test in this
        // module ever observes this mutation. The prior value is restored
        // below regardless, before this function returns.
        unsafe {
            std::env::set_var("PATH", format!("{}:{prior_path}", bindir.display()));
            std::env::set_var("GHSTUB_DIR", &ghstub);
            std::env::set_var("PRIOR_LOGIN", "sameuser");
        }

        let identity =
            Identity::worktree(Path::new("/ws/task/.worktrees/issue-19"), "agent/issue-19");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread runtime");
        rt.block_on(claim_issue(
            "acme/widgets",
            19,
            "dispatch-task",
            &identity,
            Some("sameuser"),
            &crate::scheduler::StderrNotifier,
        ));

        // SAFETY: see the comment on the previous unsafe block.
        unsafe {
            std::env::set_var("PATH", prior_path);
            std::env::remove_var("GHSTUB_DIR");
            std::env::remove_var("PRIOR_LOGIN");
        }

        let assignees = std::fs::read_to_string(ghstub.join("assignees.txt")).unwrap_or_default();
        assert!(
            assignees.lines().any(|l| l == "sameuser"),
            "the unattended dispatch path's assignee refresh must keep the assignee — \
             `claim_issue` still emits a self-cancelling `--add-assignee sameuser \
             --remove-assignee sameuser` pair on a same-identity refresh, unlike \
             `issue_claim::do_claim`'s fix for the SAME defect (`issue/claim/011`, reviewer R3 / \
             auditor A8); got assignees.txt = {assignees:?}"
        );
    }

    #[test]
    fn record_then_take_worktree_returns_clone_once() {
        let reg = new_worktree_registry();
        let wt7 = PathBuf::from("/ws/task/.worktrees/issue-7");
        let wt8 = PathBuf::from("/ws/task/.worktrees/issue-8");
        let clone = PathBuf::from("/ws/task");
        record_worktree(&reg, &wt7, &clone);
        record_worktree(&reg, &wt8, &clone);

        // The registry primitive returns a recorded worktree's clone exactly
        // once, then drops the entry (a re-take finds nothing). The close watcher
        // only calls `take_worktree` after `worktree_still_in_use` confirms the
        // last rooted agent has closed, so this once-only take is correct even
        // for a multi-role tab. issue-8 is untouched.
        assert_eq!(take_worktree(&reg, &wt7), Some(clone.clone()));
        assert_eq!(take_worktree(&reg, &wt7), None);
        assert_eq!(take_worktree(&reg, &wt8), Some(clone));
    }

    #[test]
    fn take_worktree_none_for_unrecorded_path() {
        let reg = new_worktree_registry();
        assert_eq!(take_worktree(&reg, Path::new("/not/dispatched")), None);
    }

    /// Minimal [`AgentRecord`] for the cwd-derivation test (the struct has no
    /// `Default`); only `cwd` + `tab_membership` matter to `worktree_of_record`.
    fn record(cwd: Option<&str>, membership: Option<TabMembership>) -> AgentRecord {
        AgentRecord {
            id: "a1".into(),
            pane_id_env: None,
            display_name: None,
            cwd: cwd.map(str::to_string),
            tab_membership: membership,
            agent_type: None,
            rows: 24,
            cols: 80,
            // PRD #162: no live session state in this cwd-derivation fixture;
            // matches the registry's own `agent_records()` default (`None`).
            live: None,
        }
    }

    #[test]
    fn worktree_of_record_prefers_orchestration_cwd_else_cwd() {
        // Orchestration tab → the orchestration cwd is the worktree (its own cwd
        // is ignored).
        let orch = record(
            Some("/ignored"),
            Some(TabMembership::Orchestration {
                name: "x".into(),
                role_index: 0,
                role_name: "orchestrator".into(),
                is_start_role: true,
                orchestration_cwd: Some("/ws/task/.worktrees/issue-7".into()),
                display_title: None,
                orchestration_id: None,
            }),
        );
        assert_eq!(
            worktree_of_record(&orch),
            Some(PathBuf::from("/ws/task/.worktrees/issue-7"))
        );

        // Single-agent card → its cwd is the worktree.
        let single = record(Some("/ws/task/.worktrees/issue-9"), None);
        assert_eq!(
            worktree_of_record(&single),
            Some(PathBuf::from("/ws/task/.worktrees/issue-9"))
        );

        // Neither → None.
        assert_eq!(worktree_of_record(&record(None, None)), None);
    }

    // --- N1: pr-list parsing is symmetric with issue enumeration ---

    #[test]
    fn parse_open_pr_present_array_handling() {
        assert!(parse_open_pr_present(r#"[{"number":4242}]"#).unwrap());
        assert!(!parse_open_pr_present("[]\n").unwrap());
    }

    #[test]
    fn parse_open_pr_present_rejects_malformed_output() {
        // A non-array (valid JSON) and invalid JSON both PROPAGATE — not a silent
        // "no PR → dispatch".
        assert!(parse_open_pr_present("{}").is_err());
        assert!(parse_open_pr_present("not json").is_err());
    }

    // --- L3: origin attribution ---

    #[test]
    fn github_owner_name_normalizes_known_forms() {
        for url in [
            "https://github.com/Acme/Widgets.git",
            "https://github.com/Acme/Widgets",
            "http://github.com/acme/widgets",
            "git@github.com:acme/widgets.git",
            "ssh://git@github.com/acme/widgets.git",
            "git://github.com/acme/widgets",
        ] {
            assert_eq!(
                github_owner_name(url).as_deref(),
                Some("acme/widgets"),
                "failed to normalize {url:?}"
            );
        }
        // Non-github origins are not attributable.
        assert_eq!(github_owner_name("/tmp/ghstub/acme_widgets/remote"), None);
        assert_eq!(github_owner_name("https://gitlab.com/acme/widgets"), None);
        assert_eq!(github_owner_name("https://github.com/onlyowner"), None);
    }

    #[test]
    fn origin_matches_repo_fail_closed_on_github_mismatch_lenient_otherwise() {
        // Same GitHub repo (case-insensitive) → consistent.
        assert!(origin_matches_repo(
            "git@github.com:Acme/Widgets.git",
            "acme/widgets"
        ));
        // A DIFFERENT GitHub repo → rejected (fail-closed).
        assert!(!origin_matches_repo(
            "https://github.com/other/repo.git",
            "acme/widgets"
        ));
        // A non-github origin (the local fixture remote in tests) can't be
        // attributed → accepted.
        assert!(origin_matches_repo(
            "/tmp/ghstub/acme_widgets/remote",
            "acme/widgets"
        ));
    }

    // --- S1: shared-worktree last-close detection ---

    #[test]
    fn worktree_still_in_use_tracks_live_siblings() {
        let wt = Path::new("/ws/task/.worktrees/issue-7");
        let orch_in = |role: &str| {
            record(
                None,
                Some(TabMembership::Orchestration {
                    name: "o".into(),
                    role_index: 0,
                    role_name: role.into(),
                    is_start_role: role == "orchestrator",
                    orchestration_cwd: Some("/ws/task/.worktrees/issue-7".into()),
                    display_title: None,
                    orchestration_id: None,
                }),
            )
        };

        // Two role panes share the worktree → in use.
        let both = vec![orch_in("orchestrator"), orch_in("reviewer")];
        assert!(worktree_still_in_use(&both, wt));

        // After the reviewer closes, the orchestrator still roots it → in use.
        let one = vec![orch_in("orchestrator")];
        assert!(worktree_still_in_use(&one, wt));

        // After the last role closes → free. An unrelated agent doesn't count.
        let other = vec![record(Some("/somewhere/else"), None)];
        assert!(!worktree_still_in_use(&other, wt));
        assert!(!worktree_still_in_use(&[], wt));
    }

    // --- TOCTOU: concurrent-claim worktree race ---

    // PRD #120 — when the per-issue worktree dir is already present (a concurrent
    // fire claimed it in the window after the idempotency check), `create_worktree`
    // reports AlreadyClaimed so the caller skips the issue rather than failing it.
    // Deterministic: the production code keys solely on `worktree_dir.exists()`
    // after a failed `git worktree add`, so a non-git clone dir suffices to force
    // the add to fail; the pre-created worktree dir drives the already-claimed verdict.
    #[tokio::test]
    async fn create_worktree_already_claimed_when_dir_present() {
        let ws = tempfile::tempdir().unwrap();
        let clone_dir = ws.path().join("clone"); // not a git repo → add fails
        std::fs::create_dir_all(&clone_dir).unwrap();
        let worktree_dir = clone_dir.join(".worktrees").join("issue-7");
        // Simulate the concurrent fire having already created the worktree dir.
        std::fs::create_dir_all(&worktree_dir).unwrap();

        let outcome = create_worktree(&clone_dir, &worktree_dir, "agent/issue-7", "test").await;
        assert_eq!(
            outcome,
            Ok(WorktreeCreation::AlreadyClaimed),
            "an already-present worktree dir is a concurrent claim → skip, not failure"
        );
    }

    // PRD #120 — a genuine `git worktree add` failure with NO worktree dir on disk
    // stays a hard failure (Err), so real problems (bad ref, permissions, …) are
    // still surfaced as IssueDispatchFailed rather than masked as a skip.
    #[tokio::test]
    async fn create_worktree_propagates_genuine_failure() {
        let ws = tempfile::tempdir().unwrap();
        let clone_dir = ws.path().join("clone"); // not a git repo → add fails
        std::fs::create_dir_all(&clone_dir).unwrap();
        let worktree_dir = clone_dir.join(".worktrees").join("issue-9"); // absent

        let outcome = create_worktree(&clone_dir, &worktree_dir, "agent/issue-9", "test").await;
        assert!(
            outcome.is_err(),
            "a real add failure with no worktree on disk must propagate as Err, got {outcome:?}"
        );
    }

    /// Scenario: the async `create_worktree` (this loop's own creation
    /// path, distinct from the sync `create_worktree_sync` the TUI uses) is
    /// asked to create a worktree with a specific creator identity. The
    /// written marker must record that identity (issue #425) — proving the
    /// ASYNC path threads `creator` through too, since the two
    /// implementations don't share this plumbing.
    /// `worktree_reclaim`'s own tests cover the sync path, the marker's
    /// two-line format, backward compatibility with an older bare `"deck"`
    /// marker, and sanitization of a hostile creator name in depth.
    #[tokio::test]
    async fn create_worktree_records_creator_identity() {
        fn git(dir: &Path, args: &[&str]) {
            let out = std::process::Command::new("git")
                .current_dir(dir)
                .args(args)
                .output()
                .unwrap_or_else(|e| panic!("git {args:?} failed to spawn: {e}"));
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        let ws = tempfile::tempdir().unwrap();
        let clone_dir = ws.path().join("clone");
        std::fs::create_dir_all(&clone_dir).unwrap();
        git(&clone_dir, &["init", "--initial-branch=main", "--quiet"]);
        git(&clone_dir, &["config", "user.email", "test@example.com"]);
        git(&clone_dir, &["config", "user.name", "Test"]);
        git(&clone_dir, &["config", "commit.gpgsign", "false"]);
        std::fs::write(clone_dir.join("README.md"), "seed\n").unwrap();
        git(&clone_dir, &["add", "README.md"]);
        git(&clone_dir, &["commit", "--quiet", "-m", "seed"]);

        let worktree_dir = clone_dir.join(".worktrees").join("issue-77");
        let outcome = create_worktree(
            &clone_dir,
            &worktree_dir,
            "agent/issue-77",
            "issue-dispatch:my-task#77",
        )
        .await
        .expect("create_worktree must succeed against a real git repo");
        assert_eq!(outcome, WorktreeCreation::Created);

        let git_dir_out = std::process::Command::new("git")
            .current_dir(&worktree_dir)
            .args(["rev-parse", "--git-dir"])
            .output()
            .expect("git rev-parse --git-dir must spawn");
        assert!(git_dir_out.status.success());
        let git_dir_raw = String::from_utf8_lossy(&git_dir_out.stdout)
            .trim()
            .to_string();
        let git_dir = if Path::new(&git_dir_raw).is_absolute() {
            PathBuf::from(git_dir_raw)
        } else {
            worktree_dir.join(git_dir_raw)
        };
        let content =
            std::fs::read_to_string(git_dir.join(crate::worktree_reclaim::OWNER_MARKER_FILENAME))
                .expect("marker file must exist and be readable");
        assert!(
            content.contains("created-by: issue-dispatch:my-task#77"),
            "the async creation path must record the creator identity too, got {content:?}"
        );
    }

    // Fork #122/#123 re-audit (P2): `classify_worktree_add_result` must not
    // collapse a timed-out add into `AlreadyClaimed` just because the
    // directory it half-created is present — that is the exact bug that let
    // a wedged slug masquerade as "someone else already claimed it" forever.
    // A unit test on the classifier itself (rather than a real 30s-hook
    // integration test) is deliberate: the classification is pure and where
    // the bug lived, and a real timed-out hook would be slow and flaky.
    /// Scenario: `classify_worktree_add_result` is fed a synthetic
    /// `AddError::TimedOut` alongside a worktree directory that is present on
    /// disk (standing in for `git worktree add` having registered the
    /// directory before it was killed) and must classify it as `TimedOut`,
    /// not `AlreadyClaimed`. The same present directory fed a plain
    /// `AddError::Failed` (a genuine concurrent claim, not a timeout) must
    /// still classify as `AlreadyClaimed`, exactly as before this change.
    #[spec("orchestration/worktree/007")]
    #[test]
    fn worktree_007_timeout_classifies_distinctly_from_already_claimed() {
        let ws = tempfile::tempdir().unwrap();
        let worktree_dir = ws.path().join("worktree");
        std::fs::create_dir_all(&worktree_dir).unwrap();

        let timed_out = classify_worktree_add_result(
            &worktree_dir,
            Err(AddError::TimedOut("timed out".to_string())),
        );
        assert_eq!(
            timed_out,
            Ok(AddOutcome::TimedOut),
            "a timed-out add with the directory present must classify as TimedOut, not AlreadyClaimed, got {timed_out:?}"
        );

        let already_claimed = classify_worktree_add_result(
            &worktree_dir,
            Err(AddError::Failed("add failed".to_string())),
        );
        assert_eq!(
            already_claimed,
            Ok(AddOutcome::AlreadyClaimed),
            "a genuine (non-timeout) failure with the directory present must still classify as AlreadyClaimed, got {already_claimed:?}"
        );
    }

    // --- .worktrees/ git-status hygiene via .git/info/exclude ---

    // PRD #120 — provisioning keeps `.worktrees/` out of the clone's `git status`
    // by appending it to the clone-LOCAL `.git/info/exclude` (never a committed
    // .gitignore — the clone is the user's). Idempotent: a second fire must not
    // duplicate the line.
    #[test]
    fn ensure_worktrees_excluded_appends_once_idempotently() {
        let clone = tempfile::tempdir().unwrap();
        // Initialize the clone with a `.git/info/` structure.
        let info_dir = clone.path().join(".git").join("info");
        std::fs::create_dir_all(&info_dir).unwrap();
        let exclude_path = info_dir.join("exclude");

        // First fire writes the `.worktrees/` exclude line.
        ensure_worktrees_excluded(clone.path());
        let after_first = std::fs::read_to_string(&exclude_path).unwrap();
        assert!(
            after_first.lines().any(|l| l.trim() == ".worktrees/"),
            ".git/info/exclude should contain the .worktrees/ line, got {after_first:?}"
        );

        // Second fire must NOT duplicate it.
        ensure_worktrees_excluded(clone.path());
        let after_second = std::fs::read_to_string(&exclude_path).unwrap();
        let count = after_second
            .lines()
            .filter(|l| l.trim() == ".worktrees/")
            .count();
        assert_eq!(
            count, 1,
            "repeated fires must not duplicate the exclude line, got {after_second:?}"
        );
    }

    // --- S5: workspace absolutization ---

    #[test]
    fn canonical_workspace_requires_absolute() {
        // Relative roots are rejected on every platform (bare/`.`-prefixed are
        // relative everywhere), so these assertions need no cfg gate.
        assert!(canonical_workspace("relative/dir").is_err());
        assert!(canonical_workspace("./also/relative").is_err());

        // The accepted-absolute fixture must be a *genuinely* absolute path on
        // the host: on Windows a POSIX-style "/work/space" is NOT absolute
        // (Path::is_absolute wants a drive/prefix like `C:\`), so pick the
        // literal by platform. Precedent: commit 8796fc3 made the config-path
        // tests platform-aware for the same build-windows CI job.
        #[cfg(windows)]
        let abs_root = r"C:\work\space";
        #[cfg(not(windows))]
        let abs_root = "/work/space";
        let abs = canonical_workspace(abs_root).expect("absolute path accepted");
        assert!(abs.is_absolute());
        assert_eq!(abs, PathBuf::from(abs_root));
    }
}
