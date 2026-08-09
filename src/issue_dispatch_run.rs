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
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

use crate::agent_pty::{AgentPtyRegistry, AgentRecord, TabMembership};
use crate::config::IssueDispatchConfig;
use crate::event::BroadcastMsg;
use crate::issue_dispatch::{
    Claimant, DispatchDecision, IN_PROGRESS_LABEL, IN_PROGRESS_LABEL_COLOR,
    IN_PROGRESS_LABEL_DESCRIPTION, TRIAGE_LABELS, claim_comment_body, derive_issue_paths,
    dispatch_decision, issue_comment_argv, issue_edit_add_label_argv, issue_list_argv,
    issue_view_comments_argv, label_create_argv, pr_list_for_issue_argv, substitute_issue_number,
    triage_instruction,
};
use crate::scheduler::{Notifier, NotifyEvent, SkipReason};
use crate::spawn::{SpawnRequest, spawn};

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
pub type WorktreeRegistry = Arc<Mutex<HashMap<PathBuf, WorktreeEntry>>>;

/// What the close handler needs to clean up one recorded worktree: the clone
/// that owns it (always preserved) and which removal policy applies.
///
/// The policy travels WITH the entry because the tab-close handler
/// (`daemon_protocol.rs`) is shared by both producers and cannot otherwise tell
/// them apart — it sees only a path. Inferring provenance from the path shape
/// (`<clone>/.worktrees/issue-<n>` vs. the `<repo>-dispatch-<slug>` sibling)
/// would silently apply the wrong policy the moment either layout changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    /// The clone that owns the worktree. Preserved by removal.
    pub clone_dir: PathBuf,
    /// Removal policy — see [`RemovalPolicy`].
    pub policy: RemovalPolicy,
}

/// Whether a recorded worktree may be removed while it still holds
/// uncommitted work.
///
/// The two producers want opposite things, and both are right for their case:
///
/// * [`RemovalPolicy::Force`] — PRD #120 issue-dispatch. The worktree lives
///   inside a daemon-owned `gh repo clone`, never a human checkout, and the
///   reuse-the-vacated-slot model *depends* on the directory actually going
///   away: `dispatch_decision` treats a present worktree as "issue already
///   claimed", so a worktree left behind skips that issue on every later fire,
///   permanently. Forcing is what keeps the slot reclaimable.
/// * [`RemovalPolicy::KeepIfDirty`] — PRD #220 dispatch. The name is chosen by
///   an LLM and the tree is a sibling of the user's own checkout, so Ctrl+W
///   reads as "close this view", not "destroy uncommitted work". A leaked
///   worktree costs disk; a force-removed one costs work, and that asymmetry
///   decides it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemovalPolicy {
    /// Remove unconditionally (`--force`), discarding uncommitted changes.
    Force,
    /// Refuse to remove a worktree with uncommitted changes; leave it in place
    /// and log so the user can recover the work.
    KeepIfDirty,
}

/// Construct an empty [`WorktreeRegistry`].
pub fn new_worktree_registry() -> WorktreeRegistry {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Record a freshly-created worktree (→ its owning clone + removal policy) for
/// tab-close cleanup. Idempotent: a re-recorded worktree just refreshes the
/// entry.
pub fn record_worktree(
    worktrees: &WorktreeRegistry,
    worktree_dir: &Path,
    clone_dir: &Path,
    policy: RemovalPolicy,
) {
    worktrees.lock().unwrap_or_else(|e| e.into_inner()).insert(
        worktree_dir.to_path_buf(),
        WorktreeEntry {
            clone_dir: clone_dir.to_path_buf(),
            policy,
        },
    );
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

/// If `worktree_dir` is a dispatched worktree, drop its registry entry and
/// return it (owning clone + removal policy); `None` otherwise (an ordinary
/// agent's cwd, or an entry already taken). The close watcher only calls this
/// once it has confirmed (via [`worktree_still_in_use`]) that the LAST agent
/// rooted in the worktree has closed, so for a multi-role orchestration the
/// entry survives every earlier sibling close and is taken exactly once, on the
/// final close.
pub fn take_worktree(worktrees: &WorktreeRegistry, worktree_dir: &Path) -> Option<WorktreeEntry> {
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
/// <worktree>`), PRESERVING the clone. Best-effort: a non-zero exit (already
/// removed, locked) or a spawn error is logged, not fatal — the tab is already
/// gone.
///
/// `policy` decides what happens when the worktree still holds uncommitted work
/// — see [`RemovalPolicy`] for why the two producers need opposite answers.
/// Under [`RemovalPolicy::KeepIfDirty`] a dirty tree (or a status probe that
/// fails, so dirtiness is unknown) is left in place and logged; under
/// [`RemovalPolicy::Force`] the tree is removed regardless, which is what keeps
/// PRD #120's vacated slot reclaimable.
pub async fn remove_worktree(worktree_dir: &Path, clone_dir: &Path, policy: RemovalPolicy) {
    let worktree = worktree_dir.to_string_lossy();
    if policy == RemovalPolicy::KeepIfDirty {
        let status = run_capture_args("git", &["-C", &worktree, "status", "--porcelain"]).await;
        match status {
            Ok(output) if !output.trim().is_empty() => {
                tracing::warn!(
                    worktree = %worktree_dir.display(),
                    "dispatch: worktree has uncommitted changes; leaving in place"
                );
                return;
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(
                    worktree = %worktree_dir.display(),
                    error = %e,
                    "dispatch: could not check worktree status; leaving in place"
                );
                return;
            }
        }
    }

    let clone = clone_dir.to_string_lossy();
    let mut args = vec!["-C", &clone, "worktree", "remove", &worktree];
    if policy == RemovalPolicy::Force {
        args.push("--force");
    }
    let res = run_status("git", &args).await;
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
        notify_skip(SkipReason::Labelled { claimant });
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
    match create_worktree(clone_dir, &paths.worktree_dir, &paths.branch, true).await? {
        WorktreeCreation::Created => {}
        // `reuse_existing_branch: true` above means `BranchExists` is never
        // returned to this caller — an existing `agent/issue-<n>` is ATTACHED,
        // which is exactly what keeps the vacated slot reclaimable. Treated as a
        // skip alongside `AlreadyClaimed` so the match stays exhaustive if that
        // ever changes.
        WorktreeCreation::AlreadyClaimed | WorktreeCreation::BranchExists => {
            notify_skip(SkipReason::ConcurrentCreator);
            return Ok(());
        }
    }

    // M2.4 — record the worktree for tab-close cleanup NOW, before the spawn's
    // prompt-delivery wait. `spawn` registers the agent (visible to a `StopAgent`
    // from a fast client) well before it returns, so recording after the spawn
    // would race a prompt close. The close watcher matches the agent to this
    // worktree by its record's cwd, not by an agent id we don't have yet.
    // `RemovalPolicy::Force`: this worktree lives inside a daemon-owned clone,
    // and the reuse-the-vacated-slot model depends on the directory actually
    // going away on tab close — a tree left behind makes `dispatch_decision`
    // skip the issue on every later fire. See [`RemovalPolicy`].
    record_worktree(
        worktrees,
        &paths.worktree_dir,
        clone_dir,
        RemovalPolicy::Force,
    );

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
        // `None`: issue-dispatch keeps deriving the shape from the cloned repo's
        // own config, exactly as before the PRD #220 selector existed.
        resolved_target: None,
        // Unchanged behaviour: the prompt is delivered verbatim. Giving this path
        // the orchestrator context is #222's work, not this PR's.
        compose_orchestrator_context: false,
    };
    if let Err(e) = spawn(req, registry, notifier, event_tx, true).await {
        // The spawn failed after the worktree was created/recorded: no agent
        // will ever close to trigger cleanup, so drop the registry entry here.
        // The worktree dir itself is left on disk — the next fire's
        // worktree-exists idempotency signal reclaims the issue.
        take_worktree(worktrees, &paths.worktree_dir);
        return Err(e.to_string());
    }

    // M1.3 — surface the per-issue dispatch success.
    notifier.notify(NotifyEvent::IssueDispatched {
        task: task_name.to_string(),
        repo: cfg.repo.clone(),
        issue,
    });

    // PRD #421 M1.0/M1.1 — claim the issue now that the dispatch has
    // genuinely succeeded: write the `in-progress` label and post a claim
    // comment naming the claiming task. Deliberately AFTER both worktree
    // creation and spawn succeeded (`dispatch/014`): marking any earlier would
    // make a FAILED dispatch leave a false claim, permanently un-dispatchable
    // once M1.2 reads the label back. A `gh` failure here must not turn this
    // already-successful dispatch into a per-issue failure — the per-issue
    // error boundary would otherwise report an `IssueDispatchFailed` for a
    // dispatch that genuinely worked, which is exactly the defect PRD #421's
    // Risks section calls out — so `claim_issue` never propagates. Review fix
    // C3: it no longer swallows the failure into `tracing::warn!` alone
    // either — a claim failure is now surfaced through the `Notifier` seam as
    // its own distinguishable event (see [`claim_issue`]).
    claim_issue(&cfg.repo, issue, task_name, notifier).await;

    Ok(())
}

/// PRD #421 M1.0/M1.1: write the `in-progress` label and post a claim comment
/// naming `task_name` — the scheduler-side claimant (`ScheduledTask.name`),
/// the only claimant this fire-time flow ever has.
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
/// only the claim could not be written.
async fn claim_issue(repo: &str, issue: u64, task_name: &str, notifier: &dyn Notifier) {
    let label_argv = issue_edit_add_label_argv(repo, issue, IN_PROGRESS_LABEL);
    if let Err(e) = run_status_args("gh", &label_argv).await {
        notifier.notify(NotifyEvent::IssueClaimFailed {
            task: task_name.to_string(),
            repo: repo.to_string(),
            issue,
            message: format!("failed to write the in-progress label: {e}"),
        });
    }

    let host = local_hostname();
    let timestamp = chrono::Utc::now().to_rfc3339();
    let claimant = Claimant::Task {
        name: task_name.to_string(),
    };
    let body = claim_comment_body(&claimant, &host, &timestamp);
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

/// The literal prefix [`claim_comment_body`] always produces — used to
/// recognize the deck's OWN claim comment among an issue's comments, as
/// opposed to any other comment a human left.
const CLAIM_COMMENT_PREFIX: &str = "Claimed by ";

/// PRD #421 M1.3: look up the deck's own claim comment for an issue already
/// known to carry the `in-progress` label — called ONLY from the label-skip
/// arm of `dispatch_one_issue`, i.e. only when a skip is already decided.
/// Best-effort: a `gh` failure here must not turn an already-correct SKIP
/// decision into a per-issue failure, so any error degrades to `None` ("no
/// claimant recorded") rather than propagating.
async fn fetch_claim_comment(repo: &str, issue: u64) -> Option<String> {
    let argv = issue_view_comments_argv(repo, issue);
    let stdout = run_capture("gh", &argv).await.ok()?;
    parse_claim_comment(&stdout).ok().flatten()
}

/// Pure parse of `gh issue view --json comments` output into the deck's own
/// claim-comment text, if discoverable. Split out from [`fetch_claim_comment`]
/// so the JSON-shape logic is unit-testable without a subprocess.
///
/// Takes the LAST matching comment, not the first (PRD #421 review C2 /
/// reviewer F4): `gh issue view --json comments` returns comments in
/// chronological order, and the PRD deliberately APPENDS rather than edits in
/// place precisely so a succession of claimants is preserved when one hands
/// off to another. Reading the first match reports the earliest, superseded
/// claimant instead of the current one.
fn parse_claim_comment(json: &str) -> Result<Option<String>, String> {
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
                .map(str::to_string)
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
fn local_hostname() -> String {
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

/// Outcome of [`create_worktree`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeCreation {
    /// The worktree was created.
    Created,
    /// The worktree DIRECTORY is already there — a concurrent fire claimed it in
    /// the benign TOCTOU window described below. Callers surface this as a skip
    /// rather than a failure.
    AlreadyClaimed,
    /// The worktree directory is absent but the head BRANCH already exists, and
    /// the caller asked not to reuse it (`reuse_existing_branch: false`).
    ///
    /// Distinct from [`Self::AlreadyClaimed`] because the two need different
    /// messages and have different fixes: "another dispatch is using this" (wait
    /// or pick another name) versus "a previous dispatch left this branch
    /// behind" (delete the branch, or pick another name). Collapsing them made
    /// a reused name report a worktree conflict that the user could see was not
    /// true — the directory is plainly gone — with no hint of the real cause.
    BranchExists,
}

/// M2.2: create the per-issue worktree on `agent/issue-<n>`. The `.worktrees`
/// parent is created first so the add never trips on a missing dir.
///
/// B1: `git worktree remove` PRESERVES the branch, so an issue that was
/// dispatched, had its tab closed without a PR, and is still open leaves
/// `agent/issue-<n>` behind. A naive `worktree add -b <branch>` would then fail
/// ("a branch named … already exists") on EVERY later fire, permanently wedging
/// the reuse-the-vacated-slot model. So probe for the branch first: when
/// `reuse_existing_branch` is true, attach the existing branch (no `-b`) when it is
/// already there, and only create it (`-b`) when it is not. When
/// `reuse_existing_branch` is false, an existing branch is reported as
/// [`WorktreeCreation::BranchExists`] so the caller can refuse the dispatch and
/// say WHY — the branch may hold committed work from a previous dispatch of the
/// same name, so it is never deleted implicitly.
///
/// TOCTOU: the caller only reaches here after [`dispatch_decision`] saw the
/// worktree dir ABSENT, but a concurrent fire of the same task can create it in
/// the window before this `worktree add` runs — the add then fails on the now-
/// present path. Because we only arrive with the dir believed absent, its
/// presence after a failed add means a concurrent claim, not our error: report
/// [`WorktreeCreation::AlreadyClaimed`] (→ skip) instead of a hard failure. A
/// genuine add failure (bad ref, permissions, …) leaves the dir absent and
/// still propagates as `Err`.
pub async fn create_worktree(
    clone_dir: &Path,
    worktree_dir: &Path,
    branch: &str,
    reuse_existing_branch: bool,
) -> Result<WorktreeCreation, String> {
    if let Some(parent) = worktree_dir.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create worktree parent {}: {e}", parent.display()))?;
    }
    let clone = clone_dir.to_string_lossy();
    let wt = worktree_dir.to_string_lossy();
    let branch_ref = format!("refs/heads/{branch}");
    let branch_exists = run_status(
        "git",
        &[
            "-C",
            &clone,
            "rev-parse",
            "--verify",
            "--quiet",
            &branch_ref,
        ],
    )
    .await
    .is_ok();
    if branch_exists && !reuse_existing_branch {
        return Ok(WorktreeCreation::BranchExists);
    }
    let add = if branch_exists {
        run_status("git", &["-C", &clone, "worktree", "add", &wt, branch]).await
    } else {
        run_status("git", &["-C", &clone, "worktree", "add", &wt, "-b", branch]).await
    };
    match add {
        Ok(()) => Ok(WorktreeCreation::Created),
        // Concurrent claim (TOCTOU): the dir is present now though we arrived
        // believing it absent — treat as already-claimed. A real failure leaves
        // the dir absent and surfaces as the original error.
        Err(e) => {
            if worktree_dir.exists() {
                Ok(WorktreeCreation::AlreadyClaimed)
            } else {
                Err(e)
            }
        }
    }
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
pub async fn run_status(program: &str, args: &[&str]) -> Result<(), String> {
    let output = tokio::process::Command::new(program)
        .args(args)
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

/// Run a subprocess that must exit zero and return its captured stdout. Accepts
/// `String` args (the `gh` argv helpers produce `Vec<String>`).
async fn run_capture(program: &str, args: &[String]) -> Result<String, String> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_capture_args(program, &refs).await
}

/// Like [`run_status`] but for `String` args — mirrors [`run_capture`] for the
/// `gh` argv helpers, which produce `Vec<String>`.
async fn run_status_args(program: &str, args: &[String]) -> Result<(), String> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_status(program, &refs).await
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
        let json = r#"{"comments":[{"body":"unrelated"},{"body":"Claimed by scheduled task `dispatch-task` on `host` at 2026-08-09T00:00:00Z."}]}"#;
        assert_eq!(
            parse_claim_comment(json).unwrap().as_deref(),
            Some("Claimed by scheduled task `dispatch-task` on `host` at 2026-08-09T00:00:00Z.")
        );
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
            {"body":"Claimed by scheduled task `nightly-a` on `host-1` at 2026-08-01T00:00:00Z."},
            {"body":"unrelated"},
            {"body":"Claimed by scheduled task `nightly-b` on `host-2` at 2026-08-09T00:00:00Z."}
        ]}"#;
        assert_eq!(
            parse_claim_comment(json).unwrap().as_deref(),
            Some("Claimed by scheduled task `nightly-b` on `host-2` at 2026-08-09T00:00:00Z.")
        );
    }

    #[test]
    fn record_then_take_worktree_returns_clone_once() {
        let reg = new_worktree_registry();
        let wt7 = PathBuf::from("/ws/task/.worktrees/issue-7");
        let wt8 = PathBuf::from("/ws/task/.worktrees/issue-8");
        let clone = PathBuf::from("/ws/task");
        record_worktree(&reg, &wt7, &clone, RemovalPolicy::Force);
        record_worktree(&reg, &wt8, &clone, RemovalPolicy::Force);

        // The registry primitive returns a recorded worktree's entry exactly
        // once, then drops it (a re-take finds nothing). The close watcher
        // only calls `take_worktree` after `worktree_still_in_use` confirms the
        // last rooted agent has closed, so this once-only take is correct even
        // for a multi-role tab. issue-8 is untouched.
        let taken = take_worktree(&reg, &wt7).expect("issue-7 was recorded");
        assert_eq!(taken.clone_dir, clone);
        assert_eq!(taken.policy, RemovalPolicy::Force);
        assert_eq!(take_worktree(&reg, &wt7), None);
        assert_eq!(take_worktree(&reg, &wt8).map(|e| e.clone_dir), Some(clone));
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

        let outcome = create_worktree(&clone_dir, &worktree_dir, "agent/issue-7", false).await;
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

        let outcome = create_worktree(&clone_dir, &worktree_dir, "agent/issue-9", false).await;
        assert!(
            outcome.is_err(),
            "a real add failure with no worktree on disk must propagate as Err, got {outcome:?}"
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
