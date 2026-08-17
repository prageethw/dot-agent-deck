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
    IN_PROGRESS_LABEL_DESCRIPTION, Identity, ParsedClaim, TRIAGE_LABELS, TYPE_LABELS,
    claim_comment_body, derive_issue_paths, dispatch_decision, gh_current_login_argv,
    issue_comment_argv, issue_edit_add_label_argv, issue_edit_assignee_argv, issue_list_argv,
    issue_view_comments_argv, label_create_argv, parse_current_assignees,
    parsed_claim_from_comment_json, pr_list_for_issue_argv, substitute_issue_number,
    triage_instruction, validate_gh_login,
};
use crate::scheduler::{Notifier, NotifyEvent, SkipReason};
use crate::spawn::{SpawnKind, SpawnRequest, spawn};
use crate::worktree_owner::Creator;

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
/// PRD 236: both producers now record [`RemovalPolicy::KeepIfDirty`]. The
/// split used to argue that PRD #120 issue-dispatch NEEDED `Force`, because
/// `dispatch_decision` treats a present worktree as "issue already claimed" —
/// a worktree left behind would skip that issue on every later fire,
/// permanently. That argument was about *visibility*, not about removal: a
/// kept worktree is only a dead end if nothing can ever see it or reclaim it
/// again. `worktree/reclaim/046` pins that it is not — `worktree list --json`
/// reports a kept, deck-marked `#120` worktree as `owned: true`, and once its
/// dirtiness is resolved and its PR merges, a bare `worktree reclaim` frees the
/// slot. So keeping is safe for #120 too: `dispatch_decision` still sees the
/// path and still skips (correctly — the issue genuinely is claimed), and the
/// operator gets a recoverable worktree instead of `Force` silently discarding
/// whatever the agent had done. [`RemovalPolicy::Force`] remains for the ONE
/// case that still needs it: `dispatch.rs`'s spawn-failure rollback, where —
/// for a single-role dispatch — no agent has been handed the worktree yet,
/// so there is no work to protect and a leftover dir/branch would wedge the
/// name for every later dispatch. (That single-role framing does not extend
/// to a multi-role orchestration, where an earlier role can already be a
/// live PTY child rooted in the worktree by the time a later role's spawn
/// fails and triggers this rollback — PRD 236 review; tracked for a
/// follow-up rather than changed here.)
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
    agents_rooted_in_worktree(records, worktree_dir) > 0
}

/// How many live agents in `records` are rooted in `worktree_dir` — the counting
/// form of [`worktree_still_in_use`], which is defined in terms of it so the two
/// can never disagree about what "rooted in" means.
///
/// Issue #575: the dispatch spawn-failure rollback reports the number back to the
/// caller, because "2 agents are still running in it" tells the user what to close
/// and a bare "still in use" does not.
pub fn agents_rooted_in_worktree(records: &[AgentRecord], worktree_dir: &Path) -> usize {
    records
        .iter()
        .filter(|r| worktree_of_record(r).as_deref() == Some(worktree_dir))
        .count()
}

/// What [`remove_worktree`] actually did — the typed replacement for the `()`
/// it used to return. A caller (the daemon's tab-close handler) needs this to
/// tell "removed" from "kept" from "removal failed", and — when kept or
/// failed — WHY, without re-probing the filesystem itself; see
/// [`crate::event::WorktreeKeptNotice`], which carries the reason across the
/// wire to an attached TUI. `#[must_use]` is the mechanical guard against the
/// exact defect this type exists to fix: a discarded outcome silently
/// reporting success (PRD 236 review).
#[must_use]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoveOutcome {
    /// The worktree was removed.
    Removed,
    /// The worktree was left in place. Carries why — see
    /// [`crate::event::KeptReason`].
    Kept(crate::event::KeptReason),
    /// `git worktree remove` itself failed (non-zero exit or spawn error) —
    /// carries the error so the caller can surface it. Previously folded
    /// into [`RemoveOutcome::Removed`] (the failure was logged, not
    /// returned), which meant a failed removal reported success: nothing
    /// broadcast (only `Kept` was), the registry entry was already dropped
    /// so nothing retried, the tree stayed on disk, and every later
    /// `dispatch_decision` filesystem check skipped that issue forever with
    /// no client-visible trace (PRD 236 review, reproduced against a locked
    /// worktree on git 2.55.0).
    RemoveFailed(String),
}

/// Remove a dispatched worktree from its clone (`git -C <clone> worktree remove
/// <worktree>`), PRESERVING the clone. Never fatal to the caller — a non-zero
/// exit (already removed, locked) or a spawn error is logged AND reported back
/// as [`RemoveOutcome::RemoveFailed`], so the tab-close path never panics or
/// blocks on it, but the caller can no longer mistake the failure for success.
///
/// `policy` decides what happens when the worktree still holds uncommitted work
/// — see [`RemovalPolicy`] for why the two producers used to need opposite
/// answers. Under [`RemovalPolicy::KeepIfDirty`] a dirty tree (or a status
/// probe that fails, so dirtiness is unknown) is left in place, logged, and
/// reported back as [`RemoveOutcome::Kept`]; under [`RemovalPolicy::Force`] the
/// tree is removed regardless.
pub async fn remove_worktree(
    worktree_dir: &Path,
    clone_dir: &Path,
    policy: RemovalPolicy,
) -> RemoveOutcome {
    let worktree = worktree_dir.to_string_lossy();
    if policy == RemovalPolicy::KeepIfDirty {
        let status = probe_worktree_dirty(&worktree).await;
        match status {
            Ok(output) if !output.trim().is_empty() => {
                tracing::warn!(
                    worktree = %worktree_dir.display(),
                    "dispatch: worktree has uncommitted changes; leaving in place"
                );
                return RemoveOutcome::Kept(crate::event::KeptReason::Dirty);
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(
                    worktree = %worktree_dir.display(),
                    error = %e,
                    "dispatch: could not check worktree status; leaving in place"
                );
                return RemoveOutcome::Kept(crate::event::KeptReason::ProbeError);
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
        Ok(()) => {
            tracing::info!(
                worktree = %worktree_dir.display(),
                "issue-dispatch: removed worktree on tab close (clone preserved)"
            );
            RemoveOutcome::Removed
        }
        Err(e) => {
            tracing::warn!(
                worktree = %worktree_dir.display(),
                error = %e,
                "issue-dispatch: worktree cleanup on close failed"
            );
            RemoveOutcome::RemoveFailed(e)
        }
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
    // The daemon's `AppState`, so an issue-dispatched ORCHESTRATION's roles are
    // registered for delegate routing — see
    // `crate::state::AppState::register_orchestration_role`.
    state: Option<&crate::state::SharedState>,
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
        ensure_labels(&cfg.repo).await;
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
            state,
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
    // Threaded to `spawn` so an issue-dispatched ORCHESTRATION's roles land in
    // the daemon's delegate-routing maps — see
    // `crate::state::AppState::register_orchestration_role`.
    state: Option<&crate::state::SharedState>,
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
    let creator_ident = Creator::issue_dispatch(task_name, issue);
    let creator = crate::worktree_reclaim::sanitize_marker_creator(&format!(
        "{}:{}",
        creator_ident.kind, creator_ident.subject
    ));
    match create_worktree(
        clone_dir,
        &paths.worktree_dir,
        &paths.branch,
        true,
        creator_ident,
    )
    .await?
    {
        WorktreeCreation::Created { marker_warning } => {
            notify_marker_warning_if_any(
                notifier,
                task_name,
                &cfg.repo,
                issue,
                &paths.worktree_dir,
                marker_warning,
            );
        }
        // `reuse_existing_branch: true` above means `BranchExists` is never
        // returned to this caller — an existing `agent/issue-<n>` is ATTACHED,
        // which is exactly what keeps the vacated slot reclaimable. `TimedOut`
        // IS reachable here (fork #282 audit S4 bounded the branch-probe /
        // `git worktree add` calls; final-pass F1/A1 further made
        // `run_status` kill the timed-out child and gave this path its own
        // best-effort cleanup — see `run_status`'s and
        // `attempt_worktree_cleanup_async`'s doc comments for the trace).
        // It is still folded into the same `SkipReason::ConcurrentCreator`
        // skip as `AlreadyClaimed` rather than getting its own reason: a
        // genuine `TimedOut` here means OUR OWN add wedged and was killed,
        // not that a concurrent dispatch won a TOCTOU race, so the rendered
        // "a concurrent dispatch claimed the worktree first" mislabels the
        // cause in that one case. Known and left as-is per the
        // reviewer/auditor final pass (non-blocking, F1/A1): every later
        // fire still stops retrying the slug either way, `cleaned_up` (when
        // `true`) still frees it for reuse, and a distinct `SkipReason` +
        // notification for this case is follow-up material rather than part
        // of this fix. The match stays exhaustive across all three so it
        // remains safe if any of them ever change again.
        WorktreeCreation::AlreadyClaimed
        | WorktreeCreation::BranchExists
        | WorktreeCreation::TimedOut { .. } => {
            notify_skip(SkipReason::ConcurrentCreator);
            return Ok(());
        }
    }

    // M2.4 — record the worktree for tab-close cleanup NOW, before the spawn's
    // prompt-delivery wait. `spawn` registers the agent (visible to a `StopAgent`
    // from a fast client) well before it returns, so recording after the spawn
    // would race a prompt close. The close watcher matches the agent to this
    // worktree by its record's cwd, not by an agent id we don't have yet.
    // PRD 236: `RemovalPolicy::KeepIfDirty`, not `Force` — a dirty worktree now
    // survives tab close instead of being destroyed. `dispatch_decision` still
    // sees the kept path and still skips the issue on every later fire (that
    // part of the old reasoning was correct and unchanged); what changes is
    // that the slot is no longer a silent dead end — `worktree list --json`
    // reports it (`owned: true`) and a bare `worktree reclaim` frees it once
    // the dirtiness is resolved and the PR merges. See [`RemovalPolicy`].
    record_worktree(
        worktrees,
        &paths.worktree_dir,
        clone_dir,
        RemovalPolicy::KeepIfDirty,
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
        // Fork #166 M2.4: the SAME string just written into the worktree's
        // `created-by:` marker above (`create_worktree`), not a second
        // derivation of it.
        owner: Some(creator),
    };
    let handle = match spawn(req, registry, notifier, event_tx, true, state).await {
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
    // `paths.branch`) — the one the spawned agent actually runs in — plus
    // the claiming host (round-3 audit A1, `issue/claim/016`) that every
    // `Identity::Worktree` constructor resolves automatically; never a
    // digest. The task/orchestration name is decoration only.
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
    // succeeded: marking any earlier would make a FAILED
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
/// Three writes, in order: the comment (always posted, appended never edited
/// in place — the log is the history), then the label (unchanged since PRD
/// #421), then the assignee (skipped entirely when `login` is `None` — no
/// resolved human to assign). Comment-FIRST, mirroring `issue_claim::do_claim`
/// (auditor A8, round-3 hardening): the OLD label-then-comment order could
/// leave the issue LABELLED with no discoverable comment if the comment
/// write failed after the label write already landed. The current GitHub
/// assignees (for the assignee's replace-to-one, PRD fork#235 FINAL round 5)
/// are resolved before ANY of these three writes, so the removal target
/// always reads the ACTUAL prior state, never one this run's own writes
/// below have already changed.
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
    // PRD fork#235 FINAL round 5, mirroring `issue_claim::run_issue_claim`'s
    // identical fix on the CLI path: the removal target is `current GitHub
    // assignees − {claimant}`, read from `gh issue view`'s own `assignees`
    // field — never from any claim comment's content or authorship (the
    // round-4 author gate this superseded did not narrow that removal, it
    // disabled it). Resolved FIRST, before this run posts its own comment
    // below — reading it any later would find this run's own just-posted
    // comment instead of the actual prior state.
    let remove: Vec<String> = if let Some(login) = login {
        fetch_current_assignees(repo, issue)
            .await
            .into_iter()
            .filter(|a| a != login)
            .collect()
    } else {
        Vec::new()
    };

    // Auditor A8: `issue_claim::do_claim`'s comment-FIRST, label-SECOND
    // ordering fix (reviewer/auditor F4) landed on the CLI path only. The
    // OLD label-then-comment order here could leave the issue LABELLED with
    // no discoverable comment if the comment write failed AFTER the label
    // write already landed — `ClaimDecision::RefuseNoIdentity`'s wedge state
    // (`issue/claim/013`'s escape hatch recovers it, but avoiding it is
    // better than merely recovering from it). Mirror the same order here.
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
        // `remove` is already `current assignees − {login}` (PRD fork#235
        // FINAL round 5) — a same-identity refresh's own login is excluded
        // by the set difference itself, so reviewer R3 / auditor A8's
        // self-cancelling `--add-assignee X --remove-assignee X` pair
        // (`issue/claim/019`) cannot arise here; no special-case guard
        // needed.
        let assignee_argv = issue_edit_assignee_argv(repo, issue, Some(login), &remove);
        if let Err(e) = run_status_args("gh", &assignee_argv).await {
            notifier.notify(NotifyEvent::IssueClaimFailed {
                task: task_name.to_string(),
                repo: repo.to_string(),
                issue,
                message: format!("failed to write the assignee: {e}"),
            });
        }
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
/// see the call site). Same best-effort discipline as [`ensure_labels`]:
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
///
/// Also ensures [`TYPE_LABELS`] (PRD fork#340 M5) in the same loop —
/// deliberately decoupled from `cargo xtask work-type-check`, which never
/// reads labels, so this is optional polish rather than something the gate
/// depends on. (N2: named `ensure_labels`, not `ensure_triage_labels` — it
/// ensures more than the triage vocabulary now.)
async fn ensure_labels(repo: &str) {
    for label in TRIAGE_LABELS.into_iter().chain(TYPE_LABELS) {
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
/// called from the label-skip arm of `dispatch_one_issue` (only when a skip
/// is already decided), to display which claimant is being skipped for.
/// Best-effort: a `gh` failure here must not turn an already-correct SKIP
/// decision into a per-issue failure, so any error degrades to `None` ("no
/// claimant recorded") rather than propagating.
async fn fetch_claim_comment(repo: &str, issue: u64) -> Option<ParsedClaim> {
    let argv = issue_view_comments_argv(repo, issue);
    let stdout = run_capture("gh", &argv).await.ok()?;
    parse_claim_comment(&stdout).ok().flatten()
}

/// [`claim_issue`]'s removal-target lookup (PRD fork#235 FINAL round 5):
/// the current GitHub assignees, straight from `gh issue view`'s own
/// `assignees` field — never a claim comment. Best-effort, same discipline
/// as [`fetch_claim_comment`]: a `gh` failure degrades to no assignees known
/// rather than failing the claim.
async fn fetch_current_assignees(repo: &str, issue: u64) -> Vec<String> {
    let argv = issue_view_comments_argv(repo, issue);
    let Ok(stdout) = run_capture("gh", &argv).await else {
        return Vec::new();
    };
    parse_current_assignees(&stdout).unwrap_or_default()
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
                .rfind(|c| {
                    c.get("body")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|body| body.starts_with(CLAIM_COMMENT_PREFIX))
                })
                .and_then(parsed_claim_from_comment_json)
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
/// benign TOCTOU race below), the worktree directory is absent but the head
/// BRANCH already exists and the caller asked not to reuse it, or the
/// `git worktree add` itself was killed for exceeding
/// [`WORKTREE_GIT_TIMEOUT`] while the directory it half-created is still
/// present. The async caller (issue-dispatch) surfaces `AlreadyClaimed` as a
/// skip; the sync caller (fork #122's orchestration-tab `SpawnPane` path)
/// surfaces it as a fail-loud refusal — there is no "concurrent fire" concept
/// for a single interactively-opened tab, so an already-claimed path there
/// means something else already occupies it.
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeCreation {
    /// `marker_warning` is `Some(error)` when `git worktree add` itself
    /// succeeded but the best-effort `dot-agent-deck-owner` write
    /// ([`mark_worktree_owned_best_effort`]) failed (issue #164) — the raw,
    /// unsanitized `mark_worktree_owned` error. Creation is still reported
    /// as `Created`, never as a failure: the worktree is fully usable, and
    /// the only consequence is that a later `reclaim` will land it on `Ask`
    /// instead of `Remove` until `--yes` is passed. Carrying the warning
    /// here (rather than only logging it, as before) is what lets a caller
    /// tell the user about it — see [`crate::worktree_reclaim::format_marker_warning`].
    Created {
        marker_warning: Option<String>,
    },
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
    TimedOut {
        cleaned_up: bool,
    },
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
/// [`create_worktree_sync`] (and, since fork #282's final-pass F1/A1,
/// [`create_worktree`] too — via [`attempt_worktree_cleanup_async`]) layers
/// cleanup on top of `TimedOut` to produce the richer [`WorktreeCreation`]
/// its own caller sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddOutcome {
    Created,
    AlreadyClaimed,
    TimedOut,
}

/// A failed `git` invocation from [`run_status`] / [`run_status_args`] /
/// [`run_status_sync`], distinguishing a genuine command failure from a
/// bounded wait expiring before the child exited. That bound used to exist
/// only on [`run_status_sync`]'s path, making `Failed` the only outcome
/// [`run_status`]/[`run_status_args`] could ever produce — no longer true as
/// of fork #282: [`create_worktree`] now wraps its own calls to
/// [`run_status_args`] in an external `tokio::time::timeout` and constructs
/// `TimedOut` from that. See [`run_status`]'s doc comment for the full
/// trace of what a timeout on this path now does (and does not) kill.
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
/// same name, so it is never deleted implicitly. See
/// [`crate::issue_dispatch::worktree_branch_probe_argv`] /
/// [`crate::issue_dispatch::worktree_add_argv`], shared with
/// [`create_worktree_sync`] so the two argv shapes cannot drift.
///
/// TOCTOU: the caller only reaches here after [`dispatch_decision`] saw the
/// worktree dir ABSENT, but a concurrent fire of the same task can create it in
/// the window before this `worktree add` runs — the add then fails on the now-
/// present path. Because we only arrive with the dir believed absent, its
/// presence after a failed add means a concurrent claim, not our error: report
/// [`WorktreeCreation::AlreadyClaimed`] (→ skip) instead of a hard failure. A
/// genuine add failure (bad ref, permissions, …) leaves the dir absent and
/// still propagates as `Err`.
///
/// Fork #282 (async half): the whole probe→add sequence runs under the same
/// [`worktree_attach_lock_path`] exclusive lock [`create_worktree_sync`]
/// already holds for its own attach race — see the lock acquisition inside
/// this function for why an unbounded `flock` wait is not safe here.
///
/// Issue #541 (upstream vfarcic/dot-agent-deck#541) reported a second,
/// narrower hazard on the same call — a concurrent `git worktree add`
/// reading another add's half-written `commondir` file mid-scan. This
/// cross-path exclusive lock already covers it for every add this deck
/// starts, sync or async, which upstream's own single-process, async-only
/// lock did not; a residual case (an add started by the user or another
/// tool, which takes no lock at all) is not retried here and would still
/// surface as a hard `Err` naming the same `commondir` failure.
///
/// Issue #425 — `creator`. This is the ONLY `git worktree add` in `src/`, so
/// it is also the only place that can claim a worktree as the deck's own at
/// the moment it comes into existence. On success it writes the ownership
/// marker `worktree_reclaim` later reads, recording `creator` so the claim
/// names the responsible dispatch rather than a bare "the deck". Written on
/// the `Created` arm only, and best-effort — see [`crate::worktree_owner`] for
/// why both of those are load-bearing.
pub async fn create_worktree(
    clone_dir: &Path,
    worktree_dir: &Path,
    branch: &str,
    reuse_existing_branch: bool,
    creator: Creator,
) -> Result<WorktreeCreation, String> {
    ensure_worktree_parent_dir(worktree_dir)?;

    // Fork #282 audit S3/S2, final-pass F3: `worktree_attach_lock_path`'s
    // git-common-dir resolution and `ensure_owner_only_dir` (blocking
    // `create_dir_all`/`set_permissions`) were both synchronous work running
    // directly on this tokio worker thread. Neither belongs there, but they
    // are no longer one bag of blocking work covered by a single
    // `spawn_blocking`: `git_common_dir_async` below is resolved as a
    // genuinely async, `tokio::time::timeout`-bounded call FIRST — bounding
    // the blocking `std::process::Command::output()` version the same way
    // would leak a blocking-pool thread on timeout (see that function's doc
    // comment) — and only the remaining fast, subprocess-free work
    // (canonicalize, hash, `ensure_owner_only_dir`) still runs inside
    // `spawn_blocking`.
    let common_dir = tokio::time::timeout(WORKTREE_GIT_TIMEOUT, git_common_dir_async(clone_dir))
        .await
        .map_err(|_| {
            format!(
                "timed out after {WORKTREE_GIT_TIMEOUT:?} resolving the git common dir for {}",
                clone_dir.display()
            )
        })?
        .map_err(|e| format!("failed to resolve worktree lock path: {e}"))?;

    let common_dir_owned = common_dir.clone();
    let worktree_dir_owned = worktree_dir.to_path_buf();
    let lock_path: PathBuf = tokio::task::spawn_blocking(move || {
        let lock_path =
            worktree_attach_lock_path_from_common_dir(&common_dir_owned, &worktree_dir_owned);
        if let Some(parent) = lock_path.parent() {
            crate::platform::fsperm::ensure_owner_only_dir(parent).map_err(|e| {
                format!(
                    "failed to prepare worktree lock directory {}: {e}",
                    parent.display()
                )
            })?;
        }
        Ok::<PathBuf, String>(lock_path)
    })
    .await
    .map_err(|e| format!("worktree lock setup task panicked: {e}"))??;

    // Fork #282 (async half): the same attach-race fix `create_worktree_sync`
    // already carries, ported to the async path. Locking on
    // `worktree_attach_lock_path` (the same lock key the sync path uses) is
    // what makes a scheduled dispatch and an interactive attach actually
    // contend with each other, not just two calls of this same function.
    //
    // Fork #282 audit B1: this used to be `tokio::time::timeout(_,
    // acquire_spawn_lock(&lock_path))` — dropping that future on timeout
    // cannot cancel the unbounded `flock`/`WaitForSingleObject` wait running
    // underneath, so it leaked one blocking-pool thread (Unix) or one
    // dedicated OS thread (Windows) per timeout, unbounded on Windows and
    // capped only by tokio's process-wide 512-thread pool on Unix — see
    // `acquire_spawn_lock_bounded`'s doc comment (`platform/lock/mod.rs`)
    // for the full trace. `acquire_spawn_lock_bounded` bounds the wait
    // INSIDE the primitive instead, so a timeout here genuinely terminates
    // it rather than merely stopping this task from awaiting it.
    let _attach_lock = crate::platform::lock::acquire_spawn_lock_bounded(
        &lock_path,
        WORKTREE_GIT_TIMEOUT,
    )
    .await
    .map_err(|e| {
        if e.kind() == std::io::ErrorKind::TimedOut {
            format!(
                "timed out after {WORKTREE_GIT_TIMEOUT:?} waiting for the worktree attach lock {}",
                lock_path.display()
            )
        } else {
            format!(
                "failed to acquire worktree attach lock {}: {e}",
                lock_path.display()
            )
        }
    })?;

    // Fork #282 audit S4/S1: this call, and the `git worktree add` below,
    // run while `_attach_lock` is held — a lock now shared with the TUI's
    // `create_worktree_sync` (see the comment on `run_status` for the
    // premise this changes). Bounded with the same `WORKTREE_GIT_TIMEOUT`
    // the sync twin's equivalent calls already use, so a wedged `git` here
    // cannot hold the shared lock indefinitely and starve every other
    // caller of this same `worktree_dir` — including the TUI.
    let branch_exists = match tokio::time::timeout(
        WORKTREE_GIT_TIMEOUT,
        run_status_args(
            "git",
            &crate::issue_dispatch::worktree_branch_probe_argv(clone_dir, branch),
        ),
    )
    .await
    {
        Ok(result) => result.is_ok(),
        Err(_) => {
            return Err(format!(
                "timed out after {WORKTREE_GIT_TIMEOUT:?} probing for branch {branch} in {}",
                clone_dir.display()
            ));
        }
    };
    if branch_exists && !reuse_existing_branch {
        return Ok(WorktreeCreation::BranchExists);
    }
    let add: Result<(), AddError> = match tokio::time::timeout(
        WORKTREE_GIT_TIMEOUT,
        run_status_killable_args(
            "git",
            &crate::issue_dispatch::worktree_add_argv(
                clone_dir,
                worktree_dir,
                branch,
                branch_exists,
            ),
        ),
    )
    .await
    {
        Ok(result) => result.map_err(AddError::Failed),
        Err(_) => Err(AddError::TimedOut(format!(
            "`git worktree add` for {} timed out after {WORKTREE_GIT_TIMEOUT:?}",
            worktree_dir.display()
        ))),
    };
    // `AddOutcome::TimedOut` is reachable here (unlike before this bound was
    // added). Fork #282 final-pass F1/A1: `run_status_killable`'s
    // `kill_on_drop(true)` means the direct child was already sent a kill
    // signal by the time we get here (see that function's doc comment), so
    // — mirroring `create_worktree_sync`'s `attempt_worktree_cleanup` —
    // attempt best-effort cleanup of whatever the add half-registered
    // before it was killed, rather than hardcoding `cleaned_up: false` and
    // leaving the slug wedged.
    Ok(match classify_worktree_add_result(worktree_dir, add)? {
        AddOutcome::Created => {
            let creator_str = crate::worktree_reclaim::sanitize_marker_creator(&format!(
                "{}:{}",
                creator.kind, creator.subject
            ));
            let marker_warning = mark_worktree_owned_best_effort(worktree_dir, &creator_str);
            WorktreeCreation::Created { marker_warning }
        }
        AddOutcome::AlreadyClaimed => WorktreeCreation::AlreadyClaimed,
        AddOutcome::TimedOut => {
            let cleaned_up = attempt_worktree_cleanup_async(clone_dir, worktree_dir).await;
            WorktreeCreation::TimedOut { cleaned_up }
        }
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
///
/// Issue #164: the shared seam both [`create_worktree`] and
/// [`create_worktree_sync`] call, immediately after a successful
/// `git worktree add`. Still logs at WARN as before, but now also RETURNS
/// the raw (unsanitized) error on failure — `None` on success — so the
/// caller can carry it into [`WorktreeCreation::Created`]'s
/// `marker_warning` instead of it being visible only in the daemon's own
/// tracing output.
fn mark_worktree_owned_best_effort(worktree_dir: &Path, creator: &str) -> Option<String> {
    match crate::worktree_reclaim::mark_worktree_owned(worktree_dir, creator) {
        Ok(()) => None,
        Err(e) => {
            tracing::warn!(
                worktree = %worktree_dir.display(),
                error = %e,
                "issue-dispatch: could not write ownership marker; this worktree will require \
                 `reclaim --yes` instead of a bare `reclaim` later"
            );
            Some(e)
        }
    }
}

/// Issue #164: surface a [`WorktreeCreation::Created`] marker-write warning
/// through the [`Notifier`] seam, if there is one — called only from
/// [`dispatch_one_issue`], the scheduled-dispatch caller of the async
/// [`create_worktree`]. Extracted into its own function so the notify
/// decision is unit-testable without driving the rest of the dispatch flow
/// (`gh` calls, spawn, claim). A `None` warning is a silent no-op: dispatch
/// proceeds and still reports `IssueDispatched` exactly as before this
/// change — this only adds a distinguishable notification alongside it,
/// never gates it.
fn notify_marker_warning_if_any(
    notifier: &dyn Notifier,
    task_name: &str,
    repo: &str,
    issue: u64,
    worktree_dir: &Path,
    marker_warning: Option<String>,
) {
    if let Some(error) = marker_warning {
        notifier.notify(NotifyEvent::IssueWorktreeMarkerWarning {
            task: task_name.to_string(),
            repo: repo.to_string(),
            issue,
            worktree: worktree_dir.display().to_string(),
            error,
        });
    }
}

/// Fork #282: lock file for [`create_worktree_sync`]'s attach-race fix.
/// `git worktree add <path> <existing-branch>` (no `-b`) is NOT serialized by
/// git's own ref lock the way `-b` creation is — measured: a 3-way `-b` race
/// gave one winner and two clean failures, while a 2-way attach race over 25
/// trials corrupted ~8% of the time, with BOTH racers exiting 0 and git
/// registering two `.git/worktrees/` admin entries for one on-disk path. The
/// racers are two separate `worker-agent-deck` PROCESSES, not two threads in
/// one process, so this needs a cross-process primitive — the same
/// `flock(2)`-based one [`crate::platform::lock`] already establishes for
/// `daemon_attach.rs`/`daemon.rs`, scoped here to `worktree_dir`, the exact
/// on-disk path being contended over (never the branch name: two different
/// target paths must never contend on one lock).
///
/// Anchored under the shared clone's common `.git` dir rather than a
/// machine-global lock root (contrast [`crate::daemon`]'s
/// `XDG_RUNTIME_DIR`-then-`~/.cache` lock-root dance for the daemon socket
/// lock): the common dir already belongs to whoever owns the repository, so
/// this introduces no new local-attacker DoS surface the way a sibling lock
/// file under a world-writable `/tmp` would, and it keeps every test's own
/// tempdir clone naturally isolated with no env-var override needed.
///
/// Fork #331 audit B2: `clone_dir` is **not** joined with a literal `.git`
/// component. In a linked worktree (which CLAUDE.md rule 1 mandates for
/// essentially all work in this repo), `.git` is a regular *file* pointing at
/// the real admin dir elsewhere — not a directory — so `clone_dir.join(".git")`
/// used to hand `ensure_owner_only_dir` a path whose parent is a file, and
/// `create_dir_all` semantics fail that with `ENOTDIR`. The same shape breaks
/// for a `git clone --separate-git-dir` checkout and for a submodule. Instead
/// this asks git itself, exactly the way [`is_shallow_repo`] in
/// `worktree_reclaim.rs` already does for the identical ambiguity (its doc
/// comment names this trap explicitly): `git rev-parse --path-format=absolute
/// --git-common-dir`, run with `clone_dir` as the working directory, resolves
/// to the ONE shared admin dir regardless of whether `clone_dir` is the main
/// working tree or one of its linked worktrees — so the lock also becomes
/// genuinely shared across every worktree of one clone, which is what the
/// changelog already claims it is.
///
/// The filename hashes the FULL `worktree_dir` path (not just its basename)
/// so two different target paths never collide onto one lock file — the same
/// reasoning as `daemon::lock_path_for`. Both `clone_dir` (via
/// `--git-common-dir`) and `worktree_dir` are canonicalized before hashing —
/// on a best-effort basis: `worktree_dir` does not exist yet at this point
/// (that's the whole point of the call), so only its parent (already created
/// by [`ensure_worktree_parent_dir`], which runs before this) is
/// canonicalized and the original basename rejoined. A canonicalization
/// failure falls back to the path as given rather than erroring — under-
/// serializing two differently-spelled paths to the same target (`/var` vs
/// `/private/var` on macOS, a symlinked checkout) is the SAME failure mode
/// this function already had before this fix, not a new one, so it does not
/// need to become fatal here.
///
/// Fork #282 audit S5: hashed with [`crate::platform::lock::fnv1a64`], not
/// `DefaultHasher` — this PR is what makes the choice load-bearing under
/// CLAUDE.md rule 12. `DefaultHasher`'s digest is not specified by std and is
/// not stable across Rust compiler versions, so two builds compiled with
/// different toolchains would derive DIFFERENT lock filenames for the same
/// `worktree_dir` and silently fail to contend — a lock that looks held and
/// taken but excludes nothing. Before this PR the async path took no lock at
/// all, so no cross-build agreement on the filename was required; now a
/// scheduled `issue_dispatch` process and an interactive TUI attach, which
/// can legitimately be different builds, must agree on it.
/// `platform::lock::spawn_mutex_name` already rejected `DefaultHasher` one
/// module over for the identical reason.
fn worktree_attach_lock_path(clone_dir: &Path, worktree_dir: &Path) -> Result<PathBuf, String> {
    Ok(worktree_attach_lock_path_from_common_dir(
        &git_common_dir(clone_dir)?,
        worktree_dir,
    ))
}

/// The part of [`worktree_attach_lock_path`] that follows resolving the
/// repository's common `.git` dir — split out (fork #282 final-pass F3) so
/// [`create_worktree`]'s async prologue can resolve `common_dir` itself via
/// the bounded [`git_common_dir_async`] instead of going through the
/// unbounded sync [`git_common_dir`]. Pure and infallible: everything that
/// can fail already happened in resolving `common_dir`.
fn worktree_attach_lock_path_from_common_dir(common_dir: &Path, worktree_dir: &Path) -> PathBuf {
    let canonical_worktree_dir = canonicalize_best_effort(worktree_dir);

    let hash = crate::platform::lock::fnv1a64(canonical_worktree_dir.to_string_lossy().as_bytes());
    // Derived from the SAME canonical path as the hash (fork #331 audit
    // F5), not the raw `worktree_dir` — two spellings whose `file_name()`
    // differs but whose canonical form matches would otherwise hash
    // identically under different filenames and so not contend.
    let basename = canonical_worktree_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("worktree");
    common_dir
        .join("dot-agent-deck-worktree-locks")
        .join(format!("{basename}-{hash:016x}.lock"))
}

/// Fork #331 audit B2: resolves the repository's shared common `.git` dir the
/// same way [`is_shallow_repo`] (`worktree_reclaim.rs`) resolves shallow-ness
/// — by asking git rather than assuming `repo_dir.join(".git")` is a
/// directory, which is false for a linked worktree, a `--separate-git-dir`
/// checkout, or a submodule. Unlike that advisory probe, a failure here is
/// fatal: without a correct common dir there is no safe place to put the
/// lock, and proceeding on a guess risks silently under-serializing (or, pre-
/// fix, an outright `ENOTDIR`) — the exact defect this function exists to
/// close.
///
/// NOTE (fork #331 audit F1, left as-is): unlike every other `git` call in
/// `create_worktree_sync`, this one runs through plain `Command::output()`
/// with no timeout, on the same synchronous TUI render/event loop S1 just
/// bounded the lock acquisition on — and it runs BEFORE that acquisition.
/// `git rev-parse --git-common-dir` is a cheap local metadata read (no
/// network, no hooks, no ref locks, no index), so the odds of it wedging are
/// far below `git worktree add`'s, but a stalled filesystem under the repo
/// would still freeze the TUI here with no bound and no cancel. Bounding it
/// properly needs a synchronous *capture* helper that does not exist today —
/// `run_status_sync` returns no stdout, and `run_capture`/`run_capture_args`
/// are `async` — so this is deliberately left unbounded rather than adding
/// that helper in this fix. Tracked as a follow-up rather than fixed here
/// (fork issue #388, scoped to this synchronous TUI-render-loop instance
/// only). [`create_worktree`]'s async prologue no longer reaches this
/// function — see [`git_common_dir_async`], its bounded counterpart, added
/// by fork #282 final-pass F3 for exactly the async call site #388 does not
/// cover.
fn git_common_dir(clone_dir: &Path) -> Result<PathBuf, String> {
    let out = std::process::Command::new("git")
        .current_dir(clone_dir)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .map_err(|e| {
            format!("failed to run git rev-parse --git-common-dir in {clone_dir:?}: {e}")
        })?;
    if out.status.success() {
        let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if dir.is_empty() {
            return Err(format!(
                "git rev-parse --git-common-dir in {clone_dir:?} printed no output"
            ));
        }
        return Ok(PathBuf::from(dir));
    }

    // `--path-format` requires git >= 2.31 (March 2021, and undocumented
    // anywhere in this repo as a minimum version); an older git rejects the
    // flag outright, which would otherwise turn this into the first FATAL
    // failure on a `clone_dir` that `main` handled fine (fork #331 audit
    // F3). Retry without it: plain `--git-common-dir` prints a path
    // relative to `clone_dir` for the main working tree, and an absolute
    // path for a linked worktree / `--separate-git-dir` checkout /
    // submodule — `Path::join` handles both (an absolute `dir` replaces
    // `clone_dir` outright; a relative one joins onto it), so one fallback
    // covers every shape the flagged call did.
    let fallback = std::process::Command::new("git")
        .current_dir(clone_dir)
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .map_err(|e| {
            format!("failed to run git rev-parse --git-common-dir in {clone_dir:?}: {e}")
        })?;
    if !fallback.status.success() {
        return Err(format!(
            "git rev-parse --git-common-dir in {clone_dir:?} failed: {}",
            String::from_utf8_lossy(&fallback.stderr).trim()
        ));
    }
    let dir = String::from_utf8_lossy(&fallback.stdout).trim().to_string();
    if dir.is_empty() {
        return Err(format!(
            "git rev-parse --git-common-dir in {clone_dir:?} printed no output"
        ));
    }
    Ok(clone_dir.join(dir))
}

/// Async counterpart to [`git_common_dir`] for [`create_worktree`]'s
/// `spawn_blocking` prologue (fork #282 final-pass F3): that prologue calls
/// this to resolve [`worktree_attach_lock_path_from_common_dir`]'s
/// `common_dir` argument, and the caller wraps the call in an external
/// `tokio::time::timeout` against [`WORKTREE_GIT_TIMEOUT`] — the same
/// pattern the probe/add calls a few lines below already use.
///
/// That pattern only genuinely bounds a call built on `tokio::process`, not
/// one built on a blocking `std::process::Command::output()` run inside
/// `spawn_blocking` — dropping the outer timeout future cancels the AWAIT,
/// but the blocking-pool thread underneath stays parked in the synchronous
/// syscall until the child itself exits, exactly the leak
/// `acquire_spawn_lock_bounded`'s doc comment (`platform/lock/mod.rs`)
/// describes for audit B1. Using `tokio::process::Command` here instead
/// means the await is genuinely cancellable — no thread is parked waiting
/// on a blocking syscall in the first place — so this is a second,
/// necessary conversion alongside the timeout wrap, not just the timeout on
/// its own.
///
/// Mirrors [`git_common_dir`]'s primary/fallback flag-support probe
/// exactly (the same `--path-format=absolute` retry-without-it shape for
/// pre-2.31 git); keep the two in sync if either changes. Unlike
/// [`run_status_killable`], this sets no `kill_on_drop`: a probe read like
/// `git rev-parse --git-common-dir` is safe to leave running detached if it
/// is ever actually killed by the timeout, the same reasoning
/// [`run_status`] (not the killable variant) applies to every other
/// non-add/cleanup caller.
async fn git_common_dir_async(clone_dir: &Path) -> Result<PathBuf, String> {
    let out = tokio::process::Command::new("git")
        .current_dir(clone_dir)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .await
        .map_err(|e| {
            format!("failed to run git rev-parse --git-common-dir in {clone_dir:?}: {e}")
        })?;
    if out.status.success() {
        let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if dir.is_empty() {
            return Err(format!(
                "git rev-parse --git-common-dir in {clone_dir:?} printed no output"
            ));
        }
        return Ok(PathBuf::from(dir));
    }

    // See `git_common_dir`'s matching comment: `--path-format` requires git
    // >= 2.31, and an older git rejects the flag outright.
    let fallback = tokio::process::Command::new("git")
        .current_dir(clone_dir)
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .await
        .map_err(|e| {
            format!("failed to run git rev-parse --git-common-dir in {clone_dir:?}: {e}")
        })?;
    if !fallback.status.success() {
        return Err(format!(
            "git rev-parse --git-common-dir in {clone_dir:?} failed: {}",
            String::from_utf8_lossy(&fallback.stderr).trim()
        ));
    }
    let dir = String::from_utf8_lossy(&fallback.stdout).trim().to_string();
    if dir.is_empty() {
        return Err(format!(
            "git rev-parse --git-common-dir in {clone_dir:?} printed no output"
        ));
    }
    Ok(clone_dir.join(dir))
}

/// Best-effort canonicalization for hashing purposes only (fork #331 audit
/// B2): if `path` exists, canonicalize it directly; otherwise canonicalize
/// its parent (which must already exist — see the caller) and rejoin the
/// original file name, so a not-yet-created `worktree_dir` still collapses
/// symlinks/relative components in the part of the path that DOES exist.
/// Falls back to `path` unchanged if neither succeeds — never fatal, since
/// this only affects whether two spellings of one target collide onto the
/// same lock file, not whether the lock is taken at all. Logs on the
/// fallback (fork #331 audit F5): the parent that reaches this branch was
/// just created by `ensure_worktree_parent_dir` moments earlier, so failing
/// to canonicalize it is genuinely anomalous, and this is a mutual-exclusion
/// primitive silently under-serializing — worth a greppable trace even
/// though it is not worth making fatal.
fn canonicalize_best_effort(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => {
            parent
                .canonicalize()
                .map(|p| p.join(name))
                .unwrap_or_else(|e| {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "canonicalize_best_effort: falling back to the raw path; \
                         racers computing this lock from differently-spelled \
                         paths will not contend"
                    );
                    path.to_path_buf()
                })
        }
        _ => path.to_path_buf(),
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
///
/// Fork #282: the whole probe→add→(cleanup) sequence below now runs under an
/// exclusive [`worktree_attach_lock_path`] lock, held for the ENTIRE
/// function (including the `TimedOut` cleanup arm). That is what closes the
/// second, Windows-only corruption shape the fork #282 investigation found:
/// a losing racer's own `AddOutcome::TimedOut` cleanup
/// ([`attempt_worktree_cleanup`]) assumes it is only ever cleaning up after
/// ITSELF, which was false when a concurrent winner could still be
/// mid-registration at the same path — the loser's `git worktree remove
/// --force` could remove the winner's just-created admin entry, producing a
/// `Created` result with no admin entry or `worktree list` row afterwards.
/// With the lock held for the whole function, only one caller's full
/// attempt — probe, add, and any cleanup it triggers — is ever in flight for
/// a given `worktree_dir` at a time; a second caller's own probe+add only
/// starts once the first has fully finished (registration AND any cleanup),
/// so it always sees the directory in its final state and classifies
/// correctly (`AlreadyClaimed` when the winner succeeded, its own fresh
/// attempt otherwise) rather than racing the winner's in-progress write.
///
/// Fork #331 audit S1: the lock acquisition itself is bounded by
/// [`WORKTREE_GIT_TIMEOUT`] — the same constant the `git` calls immediately
/// below already use — rather than blocking this thread indefinitely.
/// `create_worktree_sync` runs directly on the TUI's synchronous
/// render/event loop (see above), and an unbounded `flock`/`WaitForSingleObject`
/// wait here would reopen exactly the freeze `WORKTREE_GIT_TIMEOUT` exists to
/// prevent, ahead of the bounded calls it protects. On expiry the acquisition
/// refuses with an error rather than proceeding unlocked — this function
/// never reaches `git worktree add` without holding the lock.
pub(crate) fn create_worktree_sync(
    clone_dir: &Path,
    worktree_dir: &Path,
    branch: &str,
    creator: &str,
) -> Result<WorktreeCreation, String> {
    ensure_worktree_parent_dir(worktree_dir)?;

    let lock_path = worktree_attach_lock_path(clone_dir, worktree_dir)
        .map_err(|e| format!("failed to resolve worktree lock path: {e}"))?;
    if let Some(parent) = lock_path.parent() {
        crate::platform::fsperm::ensure_owner_only_dir(parent).map_err(|e| {
            format!(
                "failed to prepare worktree lock directory {}: {e}",
                parent.display()
            )
        })?;
    }
    let _attach_lock =
        crate::platform::lock::acquire_worktree_lock_sync(&lock_path, WORKTREE_GIT_TIMEOUT)
            .map_err(|e| {
                format!(
                    "failed to acquire worktree attach lock {}: {e}",
                    lock_path.display()
                )
            })?;

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
            let marker_warning = mark_worktree_owned_best_effort(worktree_dir, creator);
            WorktreeCreation::Created { marker_warning }
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

/// Fork #282 final-pass F1 (reviewer) / A1 (auditor): async twin of
/// [`attempt_worktree_cleanup`] for [`create_worktree`]'s `TimedOut` arm.
/// Runs after [`run_status_killable`]'s `kill_on_drop(true)` has already
/// sent the direct child a kill signal (see that function's doc comment for
/// the trace and its residual), through the same bounded, non-interactive
/// subprocess path the add itself used (stdin closed,
/// `GIT_TERMINAL_PROMPT=0`, …), reusing the identical
/// [`crate::issue_dispatch::worktree_remove_argv`] the sync twin uses so the
/// two argv shapes cannot drift. Bounded by its OWN, shorter
/// [`WORKTREE_CLEANUP_TIMEOUT`] via an external `tokio::time::timeout` —
/// there is no bound built into `run_status_killable_args` itself, unlike
/// `run_status_sync`'s internal poll loop — so a stuck cleanup cannot itself
/// hang the scheduler tick it exists to unwedge. Uses the killable variant
/// for the same reason the add does: a cleanup `git worktree remove` that
/// itself wedges must not be left to finish detached, or it can race the
/// next attempt at the same slug.
///
/// "Confirmed" means the same thing it means for the sync twin: both the
/// command exited successfully AND the directory is actually gone
/// afterward; either check failing is reported as `false` so the caller
/// reports an honest `cleaned_up: false` (and the manual
/// `git worktree remove --force` recovery command) rather than assuming
/// success it cannot back up.
async fn attempt_worktree_cleanup_async(clone_dir: &Path, worktree_dir: &Path) -> bool {
    let removed = tokio::time::timeout(
        WORKTREE_CLEANUP_TIMEOUT,
        run_status_killable_args(
            "git",
            &crate::issue_dispatch::worktree_remove_argv(clone_dir, worktree_dir),
        ),
    )
    .await
    .map(|result| result.is_ok())
    .unwrap_or(false);
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

/// Shared implementation behind [`run_status`] and [`run_status_killable`]:
/// run a subprocess that must exit zero; on failure return a message
/// carrying the program, args, exit status, and any stderr.
///
/// Fork #122/#123 audit (P2): stdin closed and a non-interactive git
/// environment applied — `GIT_TERMINAL_PROMPT=0` suppresses git's own
/// terminal credential prompt, and neutralising `GIT_ASKPASS`/`SSH_ASKPASS`
/// stops git/ssh from shelling out to an inherited askpass helper that
/// could itself block waiting on input — so a credential prompt can no
/// longer read anything and fails fast instead of waiting. No bounded wait
/// IN THIS FUNCTION, unlike [`run_status_sync`] below: this async path
/// already runs off the render/event loop (inside the issue-dispatch
/// scheduler's own tokio task), so a slow call here does not freeze the TUI
/// the way a synchronous one on the `SpawnPane` dispatch path would.
///
/// `kill_on_drop` is caller-selected rather than hardcoded (fork #282
/// final-pass F1 correction): see [`run_status_killable`]'s doc comment for
/// why a killable child is only safe for the two `create_worktree` call
/// sites that need it, and [`run_status`]'s for why every other caller
/// wants the opposite — detach-and-finish, not kill, when the caller stops
/// awaiting.
async fn run_status_impl(program: &str, args: &[&str], kill_on_drop: bool) -> Result<(), String> {
    let output = tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env("SSH_ASKPASS", "")
        .kill_on_drop(kill_on_drop)
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

/// Run a subprocess that must exit zero, detaching it (rather than killing
/// it) if the caller stops awaiting — e.g. an external `tokio::time::timeout`
/// elapsing, or the whole scheduler task being `.abort()`ed on daemon
/// shutdown (`daemon.rs`'s `scheduler_handle.abort()`).
///
/// Fork #282 final-pass F1 (reviewer) / A1 (auditor) correction: an earlier
/// version of this fix set `kill_on_drop(true)` HERE, on the theory that
/// [`create_worktree`]'s `git worktree add` needed it. That widened the
/// blast radius to every OTHER caller of this shared function reached
/// through the same scheduler task — a `gh repo clone`
/// (`ensure_clone`/`run_status("gh", ["repo", "clone", …])`), `git
/// fetch`/`git pull --ff-only`, the `gh issue` comment/label/assignee
/// writes, and tab-close's `git worktree remove` — all of which relied on
/// detaching cleanly on shutdown, not being SIGKILLed mid-flight. A killed
/// `gh repo clone` in particular leaves a partial `clone_dir` that
/// `ensure_clone` then treats as an existing-but-broken clone on every
/// subsequent fire, hard-erroring `create_worktree`'s `git_common_dir` call
/// until someone removes the directory by hand. `git worktree add` is the
/// one caller that genuinely needs a kill rather than a detach (see
/// [`run_status_killable`]) — it now goes through that dedicated variant
/// instead of widening this one.
pub(crate) async fn run_status(program: &str, args: &[&str]) -> Result<(), String> {
    run_status_impl(program, args, false).await
}

/// Like [`run_status`] but with `kill_on_drop(true)` set — for the narrow set
/// of callers where a timed-out child must not be left to finish detached.
///
/// Fork #282 audit S1 / final-pass F1 (reviewer) / A1 (auditor): the premise
/// behind [`run_status`] ("a slow call here only affects this path") stopped
/// being the whole story once [`create_worktree`] started calling it while
/// holding the attach lock it now shares with the TUI's
/// `create_worktree_sync`. That caller wraps its own calls in an external
/// `tokio::time::timeout` against `WORKTREE_GIT_TIMEOUT` so a wedged `git`
/// cannot hold the shared lock (and therefore starve the TUI) indefinitely.
/// Without `kill_on_drop(true)`, that external timeout only stops the
/// CALLER from awaiting the child — it does not kill it, so a timed-out
/// `git worktree add` keeps running in the background and can finish
/// (creating the worktree, without an ownership marker) after
/// `create_worktree` has already reported a timeout/failure and released
/// the lock, reproducing the fork #122/#123 "wedge this slug forever"
/// failure on the async path. `kill_on_drop(true)` closes that: dropping a
/// timed-out future here drops the `tokio::process::Command`'s underlying
/// `Child`, and with `kill_on_drop` set that drop issues an immediate
/// `SIGKILL`/`TerminateProcess` to the direct child instead of leaving it to
/// finish unattended. `create_worktree` then runs its own best-effort
/// cleanup (`attempt_worktree_cleanup_async`) on that same timeout arm, so a
/// worktree the child half-registered before the kill does not wedge the
/// slug either way.
///
/// Used ONLY at the two `create_worktree` call sites that need it — the
/// `git worktree add` invocation and `attempt_worktree_cleanup_async`'s own
/// cleanup call — never more broadly; see [`run_status`]'s doc comment for
/// what widening this to every caller cost.
///
/// Fork issue #133's whole-process-group escalation
/// (`terminate_child_with_grace_and_detached_reap_forcing_group_backstop`)
/// targets [`run_status_sync`] only, not this function. This path kills only
/// the DIRECT child, not its whole process group, so a hook grandchild
/// `git` forks (e.g. `post-checkout`) is not reached and can keep running
/// past the bound. Building an async equivalent of
/// `spawn_in_new_process_group` plus a group-wide kill would close that
/// residual gap but is substantial new machinery this path does not
/// currently need — closing the common case (the `git` process itself
/// hanging, not every hook it might have spawned) is what matters here.
async fn run_status_killable(program: &str, args: &[&str]) -> Result<(), String> {
    run_status_impl(program, args, true).await
}

/// Like [`run_status`] but for `String` args — the `git` worktree argv
/// helpers ([`crate::issue_dispatch::worktree_branch_probe_argv`] /
/// [`crate::issue_dispatch::worktree_add_argv`]) produce `Vec<String>`.
async fn run_status_args(program: &str, args: &[String]) -> Result<(), String> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_status(program, &refs).await
}

/// Like [`run_status_killable`] but for `String` args — see
/// [`run_status_args`], its non-killable twin.
async fn run_status_killable_args(program: &str, args: &[String]) -> Result<(), String> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_status_killable(program, &refs).await
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

/// [`remove_worktree`]'s `KeepIfDirty` dirty-probe (`git status --porcelain`
/// against the worktree). Deliberately its OWN function rather than a call
/// into the shared [`run_capture_args`]: that function is also used by `gh`
/// network calls (via [`run_capture`]) that legitimately need more room than
/// a local, no-network `git status`, so bounding it there would risk cutting
/// off a slow-but-working `gh` call. This probe, by contrast, runs against a
/// worktree an agent may have left in a stuck state — a held `index.lock`, a
/// stalled filesystem — and the close-time cleanup that calls it is
/// detached, so an unbounded wait here pins that task for the daemon's
/// lifetime with the tree kept and, before PRD 236's blocking-1 fix, no
/// notice ever reaching the user (PRD 236 review, item 5). Bounded with the
/// same [`WORKTREE_CLEANUP_TIMEOUT`] `attempt_worktree_cleanup` uses for its
/// own `git worktree remove --force`, and hardened the same way
/// [`run_status`] is (stdin closed, no credential-prompt env) since — unlike
/// `run_capture_args`'s other direct `git` use (`remote get-url origin`
/// against a clone we just provisioned) — this runs against a worktree
/// outside our control. A timeout is treated exactly like any other probe
/// failure: the caller's `Err` arm already keeps the tree fail-safe and
/// reports [`crate::event::KeptReason::ProbeError`].
async fn probe_worktree_dirty(worktree: &str) -> Result<String, String> {
    let output = tokio::time::timeout(
        WORKTREE_CLEANUP_TIMEOUT,
        tokio::process::Command::new("git")
            .args(["-C", worktree, "status", "--porcelain"])
            .stdin(Stdio::null())
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "")
            .env("SSH_ASKPASS", "")
            .output(),
    )
    .await
    .map_err(|_| {
        format!(
            "`git -C {worktree} status --porcelain` timed out after {WORKTREE_CLEANUP_TIMEOUT:?}"
        )
    })?
    .map_err(|e| format!("failed to run `git`: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "`git -C {worktree} status --porcelain` failed ({}): {}",
            output.status,
            stderr.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only this test module reads `parse_claim_fields` directly (production
    // code here goes through `parsed_claim_from_comment_json` instead, since
    // it also needs the comment's `author` — round-4 author gate), so the
    // import lives here rather than at the top of the file to avoid an
    // unused-import warning on the non-test build.
    use crate::issue_dispatch::parse_claim_fields;
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
    /// subprocess. `issue view --json ...` always reports ONE prior claim
    /// naming `$PRIOR_LOGIN`, its comment `author` set to that SAME login,
    /// AND `$PRIOR_LOGIN` as a REAL current assignee (PRD fork#235 FINAL
    /// round 5 — see this constant's own history: rounds 3/4 needed the
    /// comment's `author` field to match so the then-existing author gate
    /// would let `prior_login` resolve at all; round 5 deletes that gate and
    /// reads the removal target from the `assignees` field instead, so the
    /// stub must genuinely report the claimant as a current assignee for
    /// this test to exercise the same-identity-refresh property it is named
    /// for, rather than passing vacuously because nothing was ever assigned
    /// to begin with); `issue edit --add-assignee`/`--remove-assignee` apply
    /// into `$GHSTUB_DIR/assignees.txt` in the SAME add-then-remove order
    /// real `gh` applies (matching `tests/issue_claim.rs`'s `issue/claim/011`
    /// fix), so a self-cancelling pair — if one were ever still emitted —
    /// would net the file UNASSIGNED exactly as it would against a real
    /// `gh`; every other verb is a no-op.
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
    printf '{"comments":[{"body":"Claimed by the orchestration `prior` working `/ws/prior` on branch `prior-branch` at 2020-01-01T00:00:00Z, for @%s.","author":{"login":"%s"}}],"assignees":[{"login":"%s"}]}\n' "$PRIOR_LOGIN" "$PRIOR_LOGIN" "$PRIOR_LOGIN"
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
    /// record, that comment's `author` ALSO matching that login, AND that
    /// login already a REAL current GitHub assignee (`gh issue view`'s
    /// `assignees` field, seeded by [`CLAIM_019_GH_STUB`]) — a same-identity
    /// refresh, e.g. the same task re-dispatching an issue it already
    /// claimed. Assert the assignee ends up STILL SET to that login
    /// afterward, never unassigned.
    ///
    /// **Passes for a different reason under round 5** (PRD fork#235 FINAL
    /// round 5, checked per that round's own instruction to verify
    /// `011`/`019` still exercise what they claim): this test originally
    /// pinned reviewer R3 / auditor A8's finding that `claim_issue` emitted a
    /// self-cancelling `--add-assignee X --remove-assignee X` pair
    /// unconditionally, unlike `issue_claim::do_claim`'s explicit same-login
    /// skip guard (`issue/claim/011`). Round 5 deletes BOTH the guard and the
    /// whole prior-login-from-a-comment mechanism it special-cased — the
    /// removal target is now `current assignees − {{claimant}}`, a set
    /// difference computed from `gh issue view`'s own `assignees` field,
    /// which STRUCTURALLY excludes the claimant from their own removal set
    /// with no special-casing required. The stub was updated to report the
    /// claimant as a real current assignee (previously it reported none at
    /// all) specifically so this test keeps exercising a genuine
    /// same-identity refresh under round 5, rather than passing vacuously
    /// because there was nothing to remove regardless of any refresh logic.
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

    // --- PRD fork#235 round-4 author gate: issue/claim/024 ---

    /// A minimal synthetic `gh` for
    /// [`issue_claim_024_adversarial_task_name_cannot_self_inflict`] only:
    /// `gh issue view --json comments` always replies with the EXACT JSON
    /// written to `$GHSTUB_DIR/comment.json` at test setup (built from a
    /// REAL [`claim_comment_body`] rendering, so this test can never
    /// silently defang itself the way the ORIGINAL `issue/claim/012` did —
    /// see that test's own doc comment); every other verb is a no-op
    /// success. Every invocation is logged to `$GHSTUB_DIR/gh-calls.log` so
    /// the test can assert on argv values, mirroring `tests/issue_claim.rs`'s
    /// `Fixture::gh_calls`.
    const CLAIM_024_GH_STUB: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$GHSTUB_DIR/gh-calls.log" 2>/dev/null || true
group="$1"; sub="$2"
if [ "$group" = "issue" ] && [ "$sub" = "view" ]; then
    cat "$GHSTUB_DIR/comment.json"
    exit 0
fi
exit 0
"#;

    /// Scenario: A scheduled issue-dispatch task is named `nightly, for
    /// @torvalds,` — plain, hand-edited `ScheduledTask.name` config, no
    /// forged or hostile comment involved at all. `sanitize_clone_segment`
    /// strips only `/ \ \0 ..`, so the literal substring `, for @torvalds,`
    /// survives intact into the task's derived worktree PATH (and its
    /// `Identity::issue_dispatch` label, which embeds the same task name),
    /// and from there into the deck's OWN claim-comment body when it
    /// renders — genuinely self-inflicted, no attacker required. Before fix
    /// 3, `parse_worktree_claim`'s `rest.find(", for @")` scanned the WHOLE
    /// remaining body from the very start, so it matched the embedded `,
    /// for @torvalds,` substring inside the task-name-decorated label/path —
    /// text that comes BEFORE the real timestamp clause — long before it
    /// would ever reach a genuine trailing `, for @<login>` clause (there is
    /// none here; this comment was posted with `login: None`), and the
    /// parsed login came back `Some("torvalds")` — a value nobody ever
    /// intended as a login at all. Fix 3 bounds that same search to start
    /// AFTER the timestamp clause (`extract_timestamp`'s returned
    /// remainder), so today's [`parse_claim_fields`] correctly returns
    /// `login: None` for this exact body — pinned below as a sanity
    /// precondition on TODAY's behaviour, not yesterday's bug. The removal
    /// is refused on a SECOND, independent ground too: this fixture's `gh
    /// issue view` JSON carries no `author` field on the comment at all, so
    /// the round-4 author gate
    /// (`c.author.is_some() && c.author.as_deref() == login`) in
    /// [`claim_issue`] would refuse the removal regardless of what the
    /// login parse returns — also pinned below, so a regression in either
    /// fix alone still fails this test via the other rather than the test
    /// quietly becoming a single-cause test. Assert no `gh` call ever
    /// carries `--remove-assignee torvalds`. Companion to `issue/claim/020`,
    /// which covered `@mention` injection into the deck's own rendered
    /// comment but not this: a self-inflicted, structurally-mis-parsed
    /// false parse of the deck's OWN prior comment.
    ///
    /// **Re-pointed for round 5** (PRD fork#235 FINAL round 5): both grounds
    /// above (the parse fix, the author gate) are about to stop mattering —
    /// round 5 deletes the author gate and stops parsing ANY login out of a
    /// comment for a write at all, so this test's original two-cause defence
    /// collapses into a single, STRUCTURAL one: `torvalds` was never added
    /// as a real GitHub assignee (this fixture's `gh issue view` reports no
    /// `assignees` field at all), so the round-5 removal target — `current
    /// assignees − {{claimant}}` — never contains it, regardless of what any
    /// comment says or who wrote it. The two sanity preconditions above
    /// remain worth keeping: they still pin `parse_claim_fields`'s own
    /// correctness (a pure function, unrelated to whether its output is used
    /// for a write) and the fixture's own shape, so a regression there still
    /// surfaces via a clearly-labelled precondition failure rather than a
    /// confusing failure two steps downstream. A future reader must not read
    /// the final assertion as still guarding the author gate.
    #[spec("issue/claim/024")]
    #[test]
    #[cfg(unix)]
    fn issue_claim_024_adversarial_task_name_cannot_self_inflict() {
        let malicious_task_name = "nightly, for @torvalds,";
        let paths = derive_issue_paths(Path::new("/ws"), malicious_task_name, 24);
        // Sanity: `sanitize_clone_segment` really did leave the `, for
        // @torvalds,` substring intact in the derived path — otherwise this
        // test would pass vacuously, exactly the `012`/`020` "it passes with
        // the parser deleted" failure mode this file's own tests exist to
        // avoid repeating.
        assert!(
            paths
                .worktree_dir
                .to_string_lossy()
                .contains(", for @torvalds,"),
            "sanitize_clone_segment must leave the `, for @torvalds,` substring intact in the \
             derived path for this test to be exercising anything real; got {:?}",
            paths.worktree_dir
        );

        let identity =
            Identity::issue_dispatch(malicious_task_name, 24, &paths.worktree_dir, &paths.branch);
        let prior_body = claim_comment_body(&identity, "2020-01-01T00:00:00Z", None, None);
        let prior_parsed = parse_claim_fields(&prior_body);
        assert_eq!(
            prior_parsed.as_ref().and_then(|p| p.login.as_deref()),
            None,
            "sanity precondition (ground 1 of 2, today's CORRECT behaviour, not yesterday's \
             bug): fix 3 bounds `parse_worktree_claim`'s `, for @` search to start AFTER the \
             timestamp clause, so the earlier, task-name-derived `, for @torvalds,` substring — \
             which appears only in the decorative label/path, BEFORE the timestamp — must no \
             longer be mistaken for a genuine trailing login clause; got {prior_parsed:?} from \
             body {prior_body:?}"
        );

        let scratch = tempfile::tempdir().unwrap();
        let bindir = scratch.path().join("bin");
        std::fs::create_dir_all(&bindir).unwrap();
        let gh = bindir.join("gh");
        std::fs::write(&gh, CLAIM_024_GH_STUB).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let ghstub = scratch.path().join("ghstub");
        std::fs::create_dir_all(&ghstub).unwrap();
        let comment_json = serde_json::json!({ "comments": [{ "body": prior_body }] }).to_string();
        std::fs::write(ghstub.join("comment.json"), &comment_json).unwrap();
        // Sanity precondition (ground 2 of 2, independent of ground 1
        // above): the fixture JSON must carry no `author` field at all, so
        // the round-4 author gate refuses the removal on its own even if
        // ground 1's parse-level fix ever regressed and `login` came back
        // `Some("torvalds")` again — this test pins TWO independent
        // grounds for the refusal, not one, so it doesn't quietly collapse
        // into single-cause coverage.
        let comment_value: serde_json::Value = serde_json::from_str(&comment_json).unwrap();
        assert!(
            comment_value["comments"][0].get("author").is_none(),
            "sanity precondition (ground 2 of 2): the fixture JSON must carry no `author` field \
             — got {comment_json:?}"
        );

        let prior_path = std::env::var("PATH").unwrap_or_default();
        // SAFETY: see `issue_claim_019_dispatch_path_assignee_refresh_keeps_assignee`'s
        // identical comment above — every test run happens in CI via
        // `cargo nextest`, one process per test, so no sibling test in this
        // module ever observes this mutation. The prior value is restored
        // below regardless, before this function returns.
        unsafe {
            std::env::set_var("PATH", format!("{}:{prior_path}", bindir.display()));
            std::env::set_var("GHSTUB_DIR", &ghstub);
        }

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread runtime");
        rt.block_on(claim_issue(
            "acme/widgets",
            24,
            "nightly",
            &identity,
            Some("stub-user"),
            &crate::scheduler::StderrNotifier,
        ));

        // SAFETY: see the comment on the previous unsafe block.
        unsafe {
            std::env::set_var("PATH", prior_path);
            std::env::remove_var("GHSTUB_DIR");
        }

        let gh_calls = std::fs::read_to_string(ghstub.join("gh-calls.log")).unwrap_or_default();
        assert!(
            !gh_calls.contains("--remove-assignee torvalds"),
            "PRD fork#235 round 5: `torvalds` (from the task NAME's own `, for @torvalds,` \
             substring, embedded in the deck's OWN prior comment — no forged or hostile comment \
             involved) must never reach a `--remove-assignee` argv — comment content is never \
             consulted for a removal write at all, and `torvalds` was never a real GitHub \
             assignee either; observed gh-calls.log:\n{gh_calls}"
        );
    }

    // --- PRD fork#235 FINAL round 5: issue/claim/026 ---

    /// Scenario: the mirror image of
    /// [`issue_claim_024_adversarial_task_name_cannot_self_inflict`]. The
    /// SAME task-name-derived body carries an EARLIER, coincidental `, for
    /// @` clause — inside the decorative label/path, BEFORE the timestamp —
    /// AND a GENUINE trailing `, for @<login>` clause AFTER it (unlike
    /// `024`, whose body carries no genuine clause at all, `login: None`).
    /// The comment is authored as the deck's own currently-authenticated
    /// account (`stub-user`, matching the `login` passed to [`claim_issue`])
    /// — the BEST-CASE authorship for a comment-driven removal, and the
    /// login clause parses out precisely (`genuineuser`, proven by the sanity
    /// precondition below, not the earlier fake match). Assert the genuine
    /// login's removal is NEVER attempted regardless.
    ///
    /// **Assertion flipped for round 5** (PRD fork#235 FINAL round 5 — see
    /// this test's own history for why): under round 4 this test proved the
    /// OPPOSITE — that a well-formed, self-authored, precisely-parsed
    /// trailing login clause DID drive a removal, showing fix 3's
    /// timestamp-bound search was precise rather than merely suppressive.
    /// Round 5 deletes the whole mechanism that made that removal happen at
    /// all: `claim_issue` no longer parses ANY login out of a comment for a
    /// write, so even the most favourable case for a comment-driven removal
    /// — self-authored, no parse ambiguity, a real trailing clause that
    /// resolves cleanly — must now do NOTHING. This is deliberately the
    /// STRONGEST form of `022`/`024`/`025`'s "comment content never reaches
    /// a removal argv" property: those tests each have an independent reason
    /// the OLD mechanism would already have refused (stranger authorship,
    /// invalid shape, an unparseable clause) that could mask a
    /// still-existing removal mechanism; this one removes every such excuse,
    /// so it alone proves the removal-from-a-comment mechanism is gone, not
    /// merely blocked on this particular input.
    #[spec("issue/claim/026")]
    #[test]
    #[cfg(unix)]
    fn issue_claim_026_genuine_trailing_login_still_never_drives_a_removal() {
        let malicious_task_name = "nightly, for @torvalds,";
        let paths = derive_issue_paths(Path::new("/ws"), malicious_task_name, 26);
        assert!(
            paths
                .worktree_dir
                .to_string_lossy()
                .contains(", for @torvalds,"),
            "sanity precondition: `sanitize_clone_segment` must leave the `, for @torvalds,` \
             substring intact in the derived path for this test to be exercising anything real; \
             got {:?}",
            paths.worktree_dir
        );

        let identity =
            Identity::issue_dispatch(malicious_task_name, 26, &paths.worktree_dir, &paths.branch);
        // Unlike `024` (`login: None`, no genuine clause at all), this body
        // carries a REAL trailing `, for @genuineuser.` clause after the
        // timestamp — the case `024` does not cover.
        let prior_body =
            claim_comment_body(&identity, "2020-01-01T00:00:00Z", Some("genuineuser"), None);
        let prior_parsed = parse_claim_fields(&prior_body);
        assert_eq!(
            prior_parsed.as_ref().and_then(|p| p.login.as_deref()),
            Some("genuineuser"),
            "sanity precondition: the GENUINE trailing `, for @genuineuser.` clause (after the \
             timestamp) must win over the earlier, coincidental `, for @torvalds,` substring \
             embedded in the task-name-decorated label/path (before the timestamp) — fix 3's \
             search bound must be precise, not merely suppressive; got {prior_parsed:?} from \
             body {prior_body:?}"
        );

        let scratch = tempfile::tempdir().unwrap();
        let bindir = scratch.path().join("bin");
        std::fs::create_dir_all(&bindir).unwrap();
        let gh = bindir.join("gh");
        std::fs::write(&gh, CLAIM_024_GH_STUB).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let ghstub = scratch.path().join("ghstub");
        std::fs::create_dir_all(&ghstub).unwrap();
        // Authored as `stub-user`, the SAME login `claim_issue` is called
        // with below, so the round-4 author gate does not mask this test's
        // own result.
        let comment_json = serde_json::json!({
            "comments": [{ "body": prior_body, "author": { "login": "stub-user" } }]
        })
        .to_string();
        std::fs::write(ghstub.join("comment.json"), &comment_json).unwrap();

        let prior_path = std::env::var("PATH").unwrap_or_default();
        // SAFETY: see `issue_claim_019_dispatch_path_assignee_refresh_keeps_assignee`'s
        // identical comment above — every test run happens in CI via
        // `cargo nextest`, one process per test, so no sibling test in this
        // module ever observes this mutation. The prior value is restored
        // below regardless, before this function returns.
        unsafe {
            std::env::set_var("PATH", format!("{}:{prior_path}", bindir.display()));
            std::env::set_var("GHSTUB_DIR", &ghstub);
        }

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread runtime");
        rt.block_on(claim_issue(
            "acme/widgets",
            26,
            "nightly",
            &identity,
            Some("stub-user"),
            &crate::scheduler::StderrNotifier,
        ));

        // SAFETY: see the comment on the previous unsafe block.
        unsafe {
            std::env::set_var("PATH", prior_path);
            std::env::remove_var("GHSTUB_DIR");
        }

        let gh_calls = std::fs::read_to_string(ghstub.join("gh-calls.log")).unwrap_or_default();
        assert!(
            !gh_calls.contains("--remove-assignee genuineuser"),
            "PRD fork#235 round 5: `genuineuser` — a GENUINE, precisely-parsed trailing login \
             clause in a comment self-authored by the deck's own currently-authenticated account, \
             the best possible case for a comment-driven removal — must still NEVER reach a \
             `--remove-assignee` argv; the removal target is `current assignees − {{claimant}}`, \
             read from `gh issue view`'s own `assignees` field (which this fixture reports as \
             empty), and comment content — however well-formed, well-authored, and precisely \
             parsed — is never consulted for a write at all; observed gh-calls.log:\n{gh_calls}"
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
    // The production code keys on `worktree_dir.exists()` after a failed `git
    // worktree add`.
    //
    // Fork #282 (async half): `clone_dir` must now be a REAL git repo, not a
    // bare directory — `create_worktree` resolves the attach lock's path via
    // `git rev-parse --git-common-dir` before it ever reaches `git worktree
    // add`, and that resolution itself fails fast (as an `Err`, not
    // `AlreadyClaimed`) against a non-git directory. So the add is now made
    // to fail the way a real concurrent racer would produce: a NON-EMPTY
    // pre-existing target directory, which `git worktree add` refuses
    // outright ("already exists") regardless of whether `worktree_dir` is a
    // worktree registration or, as here, just another file already sitting
    // at that path.
    #[tokio::test]
    async fn create_worktree_already_claimed_when_dir_present() {
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

        let worktree_dir = clone_dir.join(".worktrees").join("issue-7");
        // Simulate the concurrent fire having already created (and started
        // populating) the worktree dir.
        std::fs::create_dir_all(&worktree_dir).unwrap();
        std::fs::write(worktree_dir.join("marker"), "concurrent\n").unwrap();

        let outcome = create_worktree(
            &clone_dir,
            &worktree_dir,
            "agent/issue-7",
            false,
            Creator::issue_dispatch("unit", 7),
        )
        .await;
        assert_eq!(
            outcome,
            Ok(WorktreeCreation::AlreadyClaimed),
            "an already-present worktree dir is a concurrent claim → skip, not failure"
        );
    }

    // PRD #120 — a genuine `git worktree add` failure with NO worktree dir on disk
    // stays a hard failure (Err), so real problems (bad ref, permissions, …) are
    // still surfaced as IssueDispatchFailed rather than masked as a skip.
    //
    // Fork #282 (async half): a non-git `clone_dir` still exercises this —
    // it now fails earlier, at the attach-lock path resolution rather than at
    // `git worktree add` itself, but either way it is a genuine `Err` with no
    // worktree ever created, which is all this test asserts.
    #[tokio::test]
    async fn create_worktree_propagates_genuine_failure() {
        let ws = tempfile::tempdir().unwrap();
        let clone_dir = ws.path().join("clone"); // not a git repo → fails
        std::fs::create_dir_all(&clone_dir).unwrap();
        let worktree_dir = clone_dir.join(".worktrees").join("issue-9"); // absent

        let outcome = create_worktree(
            &clone_dir,
            &worktree_dir,
            "agent/issue-9",
            false,
            Creator::issue_dispatch("unit", 9),
        )
        .await;
        assert!(
            outcome.is_err(),
            "a real failure with no worktree on disk must propagate as Err, got {outcome:?}"
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
            true,
            Creator::issue_dispatch("my-task", 77),
        )
        .await
        .expect("create_worktree must succeed against a real git repo");
        assert_eq!(
            outcome,
            WorktreeCreation::Created {
                marker_warning: None
            }
        );

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
            std::fs::read_to_string(git_dir.join(crate::worktree_owner::OWNER_MARKER_FILENAME))
                .expect("marker file must exist and be readable");
        assert!(
            content.contains("created-by: issue-dispatch:my-task#77"),
            "the async creation path must record the creator identity too, got {content:?}"
        );
    }

    // --- Issue #541: concurrent `git worktree add` reads a half-created
    // administrative directory ---

    /// A real git repo with one commit — `git worktree add` needs a commit to
    /// branch from. Disk-backed (issue #322 / CLAUDE.md rule 14): this fixture
    /// is a git repository plus its worktrees, not a scratch file.
    fn init_repo_with_commit(repo: &Path) {
        std::fs::create_dir_all(repo).expect("create repo dir");
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(args)
                .output()
                .unwrap_or_else(|e| panic!("spawn git {args:?}: {e}"));
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "--initial-branch=main", "--quiet"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        // The dev box may have `commit.gpgsign` on globally; the fixture must
        // not depend on a signing key being present.
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("README.md"), "seed\n").expect("write seed file");
        git(&["add", "README.md"]);
        git(&["commit", "--quiet", "-m", "seed"]);
    }

    /// Stage the state a `git worktree add` leaves in
    /// `$GIT_COMMON_DIR/worktrees/<name>` *between* creating the entry and
    /// finishing it — the window issue #541 is about.
    ///
    /// The byte sequence is git's own, read off an `strace` of `git worktree
    /// add` (2.55.0), which in order: `mkdir(worktrees/<name>)`, writes
    /// `locked`, writes `gitdir`, then `openat("commondir", O_CREAT|O_TRUNC)`
    /// — and only on the NEXT syscall writes `../..` into it. Every other `git
    /// worktree add` on the repo scans the worktree list before creating its own
    /// entry and reads each entry's `commondir`; on a short read git calls
    /// `die_errno()`, which prints `strerror(errno)` for an errno that was never
    /// set. That is where the reported message's giveaway `: Success` comes from.
    ///
    /// Staged rather than raced because that window is two adjacent syscalls
    /// wide. It IS reachable by genuine concurrency — measured on this box with
    /// N concurrent real `git worktree add`s against one repo: 12 failures in 960
    /// adds at N=64, 7 in 1024 at N=128, 0 in 960 at N=3 and 0 in 400 at N=16,
    /// every failure carrying the reported `fatal: failed to read
    /// .git/worktrees/<name>/commondir: Success` verbatim. About a thousand real
    /// worktree checkouts per observed failure is neither affordable in the fast
    /// tier nor a reliable gate, so the test stages the identical bytes and
    /// closes the window on a timer instead of on luck.
    fn begin_half_created_entry(repo: &Path, name: &str) -> PathBuf {
        let entry = repo.join(".git").join("worktrees").join(name);
        std::fs::create_dir_all(&entry).expect("create half-created worktree entry");
        std::fs::write(entry.join("locked"), "creating\n").expect("write locked");
        std::fs::write(
            entry.join("gitdir"),
            format!("{}\n", repo.join(name).join(".git").display()),
        )
        .expect("write gitdir");
        // The `O_CREAT|O_TRUNC` has happened; the write of `../..` has not.
        std::fs::write(entry.join("commondir"), b"").expect("create empty commondir");
        entry
    }

    /// The writer's very next syscall: `commondir` becomes readable and the
    /// window closes, exactly as it does when the concurrent add proceeds.
    fn finish_half_created_entry(entry: &Path) {
        std::fs::write(entry.join("commondir"), "../..\n").expect("finish commondir");
    }

    /// Issue #541 — three concurrent dispatches (`scheduler/dispatch/015`'s
    /// shape) must each end up with their worktree even though an unrelated
    /// `git worktree add` is mid-flight on the same repo. Each of the three
    /// scans the half-created entry, so each dies on it before its own worktree
    /// is created — the reported symptom, in the setup step, before any agent
    /// runs.
    ///
    /// The window is closed by a fourth party that holds no deck lock, so this
    /// exercises the case serialization alone cannot fix: a `git worktree add`
    /// the deck did not start (the user's own, or another tool's).
    ///
    /// Also pins the second-order defect: the add that dies on the scan has
    /// ALREADY created its `-b` branch, so a retry has to re-probe and ATTACH
    /// that branch rather than pass `-b` again — hence the per-dispatch HEAD
    /// assertion.
    #[tokio::test]
    async fn create_worktree_survives_a_concurrent_adds_half_created_entry() {
        let scratch = crate::test_temp::tempdir().expect("scratch tempdir");
        let repo = scratch.path().join("repo");
        init_repo_with_commit(&repo);
        let entry = begin_half_created_entry(&repo, "concurrent-add");

        // Closes while the three dispatches are in flight. Not a timing race:
        // without a retry there is no second attempt for the timer to rescue,
        // so a slow machine cannot turn this green by accident.
        let closing = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            finish_half_created_entry(&entry);
        });

        let mut fires = Vec::new();
        for name in ["alpha", "beta", "gamma"] {
            let clone_dir = repo.clone();
            let worktree_dir = scratch.path().join(format!("repo-dispatch-{name}"));
            fires.push(tokio::spawn(async move {
                let branch = format!("agent/dispatch-{name}");
                let outcome = create_worktree(
                    &clone_dir,
                    &worktree_dir,
                    &branch,
                    false,
                    Creator::dispatch("unit"),
                )
                .await;
                (name, worktree_dir, branch, outcome)
            }));
        }
        closing.await.expect("window-closing task");

        for fire in fires {
            let (name, worktree_dir, branch, outcome) = fire.await.expect("dispatch task");
            assert_eq!(
                outcome,
                Ok(WorktreeCreation::Created {
                    marker_warning: None
                }),
                "dispatch '{name}' must get its worktree despite a concurrent add's \
                 half-created entry; `Err(… commondir …)` is issue #541 itself, and \
                 `Ok(BranchExists)`/`… already exists` is a retry that failed to \
                 re-probe the branch its own failed attempt left behind"
            );
            assert!(
                worktree_dir.join("README.md").exists(),
                "dispatch '{name}' reported Created but its worktree has no checkout at {}",
                worktree_dir.display()
            );
            let head = run_capture_args(
                "git",
                &[
                    "-C",
                    &worktree_dir.to_string_lossy(),
                    "branch",
                    "--show-current",
                ],
            )
            .await
            .expect("read the new worktree's branch");
            assert_eq!(
                head.trim(),
                branch,
                "dispatch '{name}' must be checked out on its own branch"
            );
        }
    }

    /// Control for the test above, and the guard on the retry's blast radius: a
    /// `commondir` that never becomes readable is NOT transient — a crashed add
    /// leaves exactly this behind — so it must still surface as `Err` naming the
    /// file, not be retried away or swallowed. Bounds the retry too: if it ever
    /// became unbounded this test would hang rather than fail.
    #[tokio::test]
    async fn create_worktree_surfaces_a_half_created_entry_that_never_completes() {
        let scratch = crate::test_temp::tempdir().expect("scratch tempdir");
        let repo = scratch.path().join("repo");
        init_repo_with_commit(&repo);
        let _entry = begin_half_created_entry(&repo, "abandoned-add");
        let worktree_dir = scratch.path().join("repo-dispatch-stuck");

        let err = create_worktree(
            &repo,
            &worktree_dir,
            "agent/dispatch-stuck",
            false,
            Creator::dispatch("stuck"),
        )
        .await
        .expect_err("a permanently unreadable commondir must surface as an error");
        assert!(
            err.contains("commondir"),
            "the error must still name the file git could not read, got: {err}"
        );
        assert!(
            !worktree_dir.exists(),
            "a failed creation must not leave a worktree behind at {}",
            worktree_dir.display()
        );
    }

    /// A dispatch whose name is claimed by a LIVE worktree must be told that,
    /// not that its branch is left over from a dispatch "whose worktree is
    /// already gone" — the user can see the directory, and the leftover-branch
    /// message sends them to `git branch -D` for a tree another dispatch is
    /// working in.
    ///
    /// The mirror image of `dispatch.rs`'s
    /// `second_dispatch_of_a_name_reports_branch_exists_after_cleanup`, which
    /// pins the same distinction from the other side (dir gone → BranchExists).
    /// Serializing creation (#541) is what makes this reachable by design rather
    /// than by luck: the loser of a same-name race now always probes the branch
    /// AFTER the winner created it.
    #[tokio::test]
    async fn create_worktree_reports_a_live_claim_not_a_leftover_branch() {
        let scratch = crate::test_temp::tempdir().expect("scratch tempdir");
        let repo = scratch.path().join("repo");
        init_repo_with_commit(&repo);
        let worktree_dir = scratch.path().join("repo-dispatch-claimed");

        assert_eq!(
            create_worktree(
                &repo,
                &worktree_dir,
                "agent/dispatch-claimed",
                false,
                Creator::dispatch("claimed")
            )
            .await,
            Ok(WorktreeCreation::Created {
                marker_warning: None
            }),
            "precondition: the first dispatch claims the name"
        );

        assert_eq!(
            create_worktree(
                &repo,
                &worktree_dir,
                "agent/dispatch-claimed",
                false,
                Creator::dispatch("claimed")
            )
            .await,
            Ok(WorktreeCreation::AlreadyClaimed),
            "a second dispatch of a name whose worktree is still THERE is a live \
             claim; reporting BranchExists would tell the user their worktree is \
             gone while it is in front of them"
        );
        assert!(
            worktree_dir.exists(),
            "the live claim must be left untouched at {}",
            worktree_dir.display()
        );
    }

    // --- Issue #425: the ownership marker is written at creation time ---

    /// The marker `worktree_reclaim` reads must actually be written by the one
    /// code path that runs `git worktree add`, and it must land in the
    /// worktree's own git metadata dir rather than anywhere in the working
    /// tree. Both halves matter: a marker inside the tree makes
    /// `git status --porcelain` non-empty forever, and the reclaim gate keeps
    /// every dirty worktree — so an in-tree marker would make the worktree
    /// permanently UNreclaimable, defeating the feature it enables.
    #[tokio::test]
    async fn create_worktree_marks_the_worktree_as_deck_owned_without_dirtying_it() {
        let scratch = crate::test_temp::tempdir().expect("scratch tempdir");
        let repo = scratch.path().join("repo");
        init_repo_with_commit(&repo);
        let worktree_dir = scratch.path().join("repo-dispatch-marked");

        assert_eq!(
            create_worktree(
                &repo,
                &worktree_dir,
                "agent/dispatch-marked",
                false,
                Creator::dispatch("marked"),
            )
            .await,
            Ok(WorktreeCreation::Created {
                marker_warning: None
            })
        );

        let marker = crate::worktree_owner::marker_path(&worktree_dir)
            .expect("the created worktree must have a resolvable git metadata dir");
        assert!(
            marker.is_file(),
            "the deck must claim the worktree it just created; no marker at {}",
            marker.display()
        );
        assert!(
            !marker.starts_with(&worktree_dir),
            "the marker must live in the worktree's git metadata dir, never inside the \
             working tree — got {}",
            marker.display()
        );

        let status = std::process::Command::new("git")
            .current_dir(&worktree_dir)
            .args(["status", "--porcelain"])
            .output()
            .expect("git status");
        assert!(
            String::from_utf8_lossy(&status.stdout).trim().is_empty(),
            "marking a worktree must not make it dirty — a dirty worktree is kept by the \
             reclaim gate, so an in-tree marker would make marked worktrees permanently \
             unreclaimable; got:\n{}",
            String::from_utf8_lossy(&status.stdout)
        );

        // Idempotent: a re-created or re-attached worktree must not accumulate
        // state. Checked by parsing rather than by comparing bytes, because an
        // APPEND is exactly what would break — two concatenated documents are
        // not one document — while a legitimate rewrite changes the timestamp.
        crate::worktree_owner::write_marker(
            &worktree_dir,
            "agent/dispatch-marked",
            &Creator::dispatch("marked"),
        )
        .expect("re-marking an already-marked worktree must succeed");
        let after = std::fs::read_to_string(&marker).expect("read marker again");
        serde_json::from_str::<serde_json::Value>(&after).unwrap_or_else(|e| {
            panic!(
                "re-marking must REPLACE the marker, never append to it: after a second \
                 write the file must still be one document, but it did not parse ({e}):\n\
                 {after}"
            )
        });
    }

    /// The dangerous direction. `AlreadyClaimed` means the worktree DIRECTORY
    /// was already on disk when our `git worktree add` ran, so this process did
    /// not create it — and the marker is an ownership claim consumed by a path
    /// that DELETES directories. Claiming a directory we did not create is the
    /// one failure this marker exists to prevent, so the already-claimed arm
    /// must leave the marker alone. (A concurrent dispatch that genuinely
    /// created it writes its own marker from its own `Created` arm.)
    #[tokio::test]
    async fn create_worktree_never_marks_a_worktree_it_did_not_create() {
        let scratch = crate::test_temp::tempdir().expect("scratch tempdir");
        let repo = scratch.path().join("repo");
        init_repo_with_commit(&repo);
        let worktree_dir = scratch.path().join("repo-dispatch-foreign");

        // Somebody else's worktree, on this same repo, at the path our dispatch
        // is about to want: a real linked worktree, so it HAS a git metadata
        // dir a marker could be written into.
        let add = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["worktree", "add", "-b", "someone-elses"])
            .arg(&worktree_dir)
            .output()
            .expect("git worktree add");
        assert!(
            add.status.success(),
            "fixture precondition: {}",
            String::from_utf8_lossy(&add.stderr)
        );
        let marker = crate::worktree_owner::marker_path(&worktree_dir)
            .expect("the foreign worktree must have a resolvable git metadata dir");
        assert!(
            !marker.is_file(),
            "fixture precondition: a plain `git worktree add` leaves no marker"
        );

        assert_eq!(
            create_worktree(
                &repo,
                &worktree_dir,
                "agent/dispatch-foreign",
                false,
                Creator::dispatch("foreign"),
            )
            .await,
            Ok(WorktreeCreation::AlreadyClaimed),
            "precondition: a present worktree dir is reported as already claimed"
        );
        assert!(
            !marker.is_file(),
            "a worktree the deck did not create must never be marked as deck-owned — \
             the marker gates an unattended `git worktree remove`; found one at {}",
            marker.display()
        );
    }

    /// Scenario: issue #164. `mark_worktree_owned_best_effort` is the single
    /// seam both the async [`create_worktree`] above and the sync
    /// `create_worktree_sync` (the TUI's twin) call right after `git
    /// worktree add` succeeds, so a test against it covers the shared logic
    /// behind both creation paths' `WorktreeCreation::Created` result. A
    /// failed marker write must no longer be silently swallowed into only a
    /// `tracing::warn!` — it must come back as `Some(error)` so a caller can
    /// carry it into a user-visible warning.
    ///
    /// Deterministic without chmod/timing/a full disk (per the scoping
    /// review): mark a real worktree successfully once, then replace the
    /// marker file with a directory of the same name — `std::fs::write`
    /// then reliably fails with an `Is a directory` style error.
    #[tokio::test]
    async fn mark_worktree_owned_best_effort_surfaces_a_failed_write() {
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

        let worktree_dir = clone_dir.join(".worktrees").join("issue-164");
        let outcome = create_worktree(
            &clone_dir,
            &worktree_dir,
            "agent/issue-164",
            true,
            Creator::issue_dispatch("test-creator", 164),
        )
        .await
        .expect("create_worktree must succeed against a real git repo");
        assert_eq!(
            outcome,
            WorktreeCreation::Created {
                marker_warning: None
            },
            "a normal successful mark must carry no warning"
        );

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
        let marker_path = git_dir.join(crate::worktree_owner::OWNER_MARKER_FILENAME);
        std::fs::remove_file(&marker_path)
            .expect("the marker file must exist after a successful mark");
        std::fs::create_dir(&marker_path)
            .expect("must be able to replace the marker file with a directory");

        let warning = mark_worktree_owned_best_effort(&worktree_dir, "second-creator");
        assert!(
            warning.is_some(),
            "a directory occupying the marker path must make the write fail and be reported, \
             not swallowed"
        );
        assert!(
            worktree_dir.exists(),
            "the worktree itself must survive a marker-write failure — creation stays \
             best-effort"
        );
    }

    /// Scenario: issue #164. `notify_marker_warning_if_any` — the small seam
    /// `dispatch_one_issue` calls right after a `WorktreeCreation::Created`
    /// result — must emit a distinguishable `NotifyEvent` when there is a
    /// warning to report, carrying the raw (unsanitized) path and error:
    /// sanitizing for terminal display is the render sink's job
    /// (`format_marker_warning`, called from `StderrNotifier`), not this
    /// seam's.
    #[test]
    fn notify_marker_warning_if_any_notifies_on_write_failure() {
        #[derive(Default)]
        struct RecordingNotifier {
            events: std::sync::Mutex<Vec<NotifyEvent>>,
        }
        impl Notifier for RecordingNotifier {
            fn notify(&self, event: NotifyEvent) {
                self.events.lock().unwrap().push(event);
            }
        }

        let notifier = RecordingNotifier::default();
        notify_marker_warning_if_any(
            &notifier,
            "my-task",
            "org/repo",
            42,
            Path::new("/tmp/wt-164"),
            Some("disk full".to_string()),
        );

        let events = notifier.events.lock().unwrap();
        assert_eq!(events.len(), 1, "exactly one event must be notified");
        match &events[0] {
            NotifyEvent::IssueWorktreeMarkerWarning {
                task,
                repo,
                issue,
                worktree,
                error,
            } => {
                assert_eq!(task, "my-task");
                assert_eq!(repo, "org/repo");
                assert_eq!(*issue, 42);
                assert_eq!(worktree, "/tmp/wt-164");
                assert_eq!(error, "disk full");
            }
            other => panic!("expected IssueWorktreeMarkerWarning, got {other:?}"),
        }
    }

    /// The counterpart to the above: no warning to report means no event —
    /// the common case (the marker write almost always succeeds) must not
    /// spam the notifier on every dispatch.
    #[test]
    fn notify_marker_warning_if_any_silent_on_success() {
        #[derive(Default)]
        struct RecordingNotifier {
            events: std::sync::Mutex<Vec<NotifyEvent>>,
        }
        impl Notifier for RecordingNotifier {
            fn notify(&self, event: NotifyEvent) {
                self.events.lock().unwrap().push(event);
            }
        }

        let notifier = RecordingNotifier::default();
        notify_marker_warning_if_any(
            &notifier,
            "my-task",
            "org/repo",
            42,
            Path::new("/tmp/wt"),
            None,
        );

        assert!(
            notifier.events.lock().unwrap().is_empty(),
            "a None marker_warning must not notify anything"
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
    /// Fork #282 audit S2/S3: a THIRD case — `AddError::Failed` with the
    /// directory ABSENT — must propagate as a hard `Err` rather than being
    /// masked as `AlreadyClaimed`; this is the arm that keeps a genuine add
    /// failure (bad ref, permissions, …) from being silently swallowed as a
    /// skip, and it had lost its only exercising test to a fixture fix
    /// elsewhere in this same PR (see `create_worktree_propagates_genuine_failure`'s
    /// own comment).
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

        let never_created = ws.path().join("never-created");
        let genuine_failure = classify_worktree_add_result(
            &never_created,
            Err(AddError::Failed("add failed: bad ref".to_string())),
        );
        assert_eq!(
            genuine_failure,
            Err("add failed: bad ref".to_string()),
            "a genuine failure with the directory ABSENT must propagate as Err, not be masked as AlreadyClaimed, got {genuine_failure:?}"
        );
    }

    // --- fork issue #282: the attach race ---

    /// Scenario: fork issue #282. Two concurrent callers of
    /// `create_worktree_sync`, both attaching to the SAME already-existing
    /// branch at the SAME target path, race across many trials — a single
    /// trial proves nothing, since the issue measured this at only ~8% for
    /// one 2-way race. Asserts, for every trial, that at most one caller
    /// reports `Created`, that `git worktree list` shows the target path
    /// exactly once, and that `.git/worktrees/` holds exactly one admin
    /// entry for it. `create_worktree_sync` now holds a lock around the
    /// attach path for this same-process race, so this test pins that the
    /// lock actually serializes both callers — without it, git itself would
    /// let both win on at least some trials, producing two `Created`
    /// results and two admin entries for one on-disk path.
    #[cfg(unix)]
    #[spec("worktree/create/001")]
    #[test]
    fn create_001_concurrent_attach_never_double_creates() {
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

        /// Resolve `path` for comparison purposes only, falling back to
        /// `path` unchanged if it cannot be resolved (e.g. it does not
        /// exist). `git` reports admin-entry and `worktree list` paths
        /// through its own realpath resolution, which diverges from the raw
        /// `ws_root`-derived path on at least two platforms: macOS's `/var`
        /// -> `/private/var` symlink, and Windows's `RUNNER~1` short-name
        /// form. Comparing both sides through this same resolution makes
        /// the comparison platform-stable without canonicalizing `ws_root`
        /// itself, which stays raw because `Path::canonicalize` produces a
        /// `\\?\`-prefixed path that `git worktree add` rejects outright
        /// (see the comment on `ws_root` above) — this function is never
        /// used for a path handed to `create_worktree_sync`, only for
        /// comparing git's own output against it.
        fn canonical_for_compare(path: &Path) -> PathBuf {
            path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
        }

        /// Count `.git/worktrees/<name>/gitdir` admin entries whose target
        /// resolves to `worktree_dir`'s own `.git` — the exact
        /// admin-directory duplication the issue measured (two entries
        /// registered for one on-disk path).
        fn count_admin_entries_for(clone_dir: &Path, worktree_dir: &Path) -> usize {
            let worktrees_dir = clone_dir.join(".git").join("worktrees");
            let Ok(entries) = std::fs::read_dir(&worktrees_dir) else {
                return 0;
            };
            let target = canonical_for_compare(&worktree_dir.join(".git"));
            entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .filter(|e| {
                    std::fs::read_to_string(e.path().join("gitdir"))
                        .map(|s| canonical_for_compare(Path::new(s.trim())) == target)
                        .unwrap_or(false)
                })
                .count()
        }

        /// Count `git worktree list --porcelain` rows naming `worktree_dir`
        /// — the second, independent symptom the issue measured (the path
        /// shown twice by `git worktree list`).
        fn count_worktree_list_entries(clone_dir: &Path, worktree_dir: &Path) -> usize {
            let out = std::process::Command::new("git")
                .current_dir(clone_dir)
                .args(["worktree", "list", "--porcelain"])
                .output()
                .expect("git worktree list must spawn");
            assert!(
                out.status.success(),
                "git worktree list failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let wanted = canonical_for_compare(worktree_dir);
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|l| {
                    l.strip_prefix("worktree ")
                        .map(|p| canonical_for_compare(Path::new(p)) == wanted)
                        .unwrap_or(false)
                })
                .count()
        }

        const TRIALS: usize = 60;

        let ws = tempfile::tempdir().unwrap();
        // Deliberately NOT canonicalized: on Windows, `Path::canonicalize`
        // produces a `\\?\`-prefixed extended-length path, and `git worktree
        // add` against such a path fails outright ("could not create leading
        // directories ...: Invalid argument") — measured directly in CI. The
        // paths this test passes to `create_worktree_sync` and the ones read
        // back from `git worktree list` / `.git/worktrees/*/gitdir` are
        // always derived from this SAME `ws_root`, so lexical equality holds
        // without canonicalizing, matching how `create_worktree_records_creator_identity`
        // above already compares paths.
        let ws_root = ws.path().to_path_buf();
        let clone_dir = ws_root.join("clone");
        std::fs::create_dir_all(&clone_dir).unwrap();
        git(&clone_dir, &["init", "--initial-branch=main", "--quiet"]);
        git(&clone_dir, &["config", "user.email", "test@example.com"]);
        git(&clone_dir, &["config", "user.name", "Test"]);
        git(&clone_dir, &["config", "commit.gpgsign", "false"]);
        std::fs::write(clone_dir.join("README.md"), "seed\n").unwrap();
        git(&clone_dir, &["add", "README.md"]);
        git(&clone_dir, &["commit", "--quiet", "-m", "seed"]);

        let mut branches = Vec::with_capacity(TRIALS);
        let mut worktree_dirs = Vec::with_capacity(TRIALS);
        for i in 0..TRIALS {
            let branch = format!("race-{i}");
            git(&clone_dir, &["branch", &branch]);
            branches.push(branch);
            worktree_dirs.push(ws_root.join(format!("wt-{i}")));
        }

        let barriers: Vec<std::sync::Barrier> =
            (0..TRIALS).map(|_| std::sync::Barrier::new(2)).collect();

        // Every trial's pair races concurrently with every other trial's
        // pair too (not sequentially) — deliberate, since this mirrors how
        // real concurrent orchestrations hit the shared repository, and
        // keeps the whole test's wall-clock close to a single `git worktree
        // add`'s rather than TRIALS times that.
        let results: Vec<[Result<WorktreeCreation, String>; 2]> = std::thread::scope(|s| {
            let mut handles = Vec::with_capacity(TRIALS);
            for i in 0..TRIALS {
                let clone_dir = &clone_dir;
                let branch = &branches[i];
                let worktree_dir = &worktree_dirs[i];
                let barrier = &barriers[i];
                let h_a = s.spawn(move || {
                    barrier.wait();
                    create_worktree_sync(clone_dir, worktree_dir, branch, "racer-a")
                });
                let h_b = s.spawn(move || {
                    barrier.wait();
                    create_worktree_sync(clone_dir, worktree_dir, branch, "racer-b")
                });
                handles.push((h_a, h_b));
            }
            handles
                .into_iter()
                .map(|(a, b)| [a.join().unwrap(), b.join().unwrap()])
                .collect()
        });

        let mut failures: Vec<String> = Vec::new();
        let mut double_created = 0usize;
        let mut duplicate_admin = 0usize;
        let mut duplicate_listed = 0usize;

        for (i, pair) in results.iter().enumerate() {
            let created_count = pair
                .iter()
                .filter(|r| matches!(r, Ok(WorktreeCreation::Created { .. })))
                .count();
            if created_count != 1 {
                double_created += 1;
                failures.push(format!(
                    "trial {i}: expected exactly one Created, got a={:?} b={:?}",
                    pair[0], pair[1]
                ));
            }

            let admin_count = count_admin_entries_for(&clone_dir, &worktree_dirs[i]);
            if admin_count != 1 {
                duplicate_admin += 1;
                failures.push(format!(
                    "trial {i}: expected exactly one .git/worktrees admin entry for {:?}, found {admin_count}",
                    worktree_dirs[i]
                ));
            }

            let listed_count = count_worktree_list_entries(&clone_dir, &worktree_dirs[i]);
            if listed_count != 1 {
                duplicate_listed += 1;
                failures.push(format!(
                    "trial {i}: expected `git worktree list` to show {:?} exactly once, found {listed_count}",
                    worktree_dirs[i]
                ));
            }
        }

        assert!(
            failures.is_empty(),
            "fork issue #282: create_worktree_sync's attach-path lock failed to serialize \
             concurrent callers -- \
             {double_created}/{TRIALS} trials produced more than one `Created`, \
             {duplicate_admin}/{TRIALS} trials left more than one `.git/worktrees` admin entry, \
             {duplicate_listed}/{TRIALS} trials showed the path more than once in `git worktree \
             list`. Failures:\n{}",
            failures.join("\n")
        );
    }

    /// Scenario: fork #331 audit B2. Creates a real repo, then a SECOND,
    /// LINKED worktree of it via `git worktree add` — confirming its `.git`
    /// is a plain FILE, not a directory, which is what CLAUDE.md rule 1
    /// mandates for essentially all work in this repo. Calls
    /// `create_worktree_sync` with `clone_dir` set to that linked worktree
    /// (not the main working tree) to attach a third worktree onto an
    /// already-existing branch. Before the fix, `worktree_attach_lock_path`
    /// joined `clone_dir` with a literal `.git`, which resolves to that
    /// file rather than a directory inside a linked worktree, so
    /// `ensure_owner_only_dir(parent)` failed with `ENOTDIR` and the whole
    /// attach errored out before `git worktree add` ever ran — orchestration
    /// spawn failing outright from the exact working directory rule 1
    /// mandates. Asserts the attach succeeds.
    #[cfg(unix)]
    #[spec("worktree/create/002")]
    #[test]
    fn create_002_attach_succeeds_from_inside_a_linked_worktree() {
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
        let main_repo = ws.path().join("main");
        std::fs::create_dir_all(&main_repo).unwrap();
        git(&main_repo, &["init", "--initial-branch=main", "--quiet"]);
        git(&main_repo, &["config", "user.email", "test@example.com"]);
        git(&main_repo, &["config", "user.name", "Test"]);
        git(&main_repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(main_repo.join("README.md"), "seed\n").unwrap();
        git(&main_repo, &["add", "README.md"]);
        git(&main_repo, &["commit", "--quiet", "-m", "seed"]);

        // A linked worktree OF the main repo -- `clone_dir` below points
        // HERE, not at `main_repo`, reproducing exactly the shape rule 1
        // mandates for real work in this repo.
        let linked = ws.path().join("linked-caller");
        git(
            &main_repo,
            &[
                "worktree",
                "add",
                "-b",
                "caller-branch",
                linked.to_str().unwrap(),
            ],
        );
        assert!(
            linked.join(".git").is_file(),
            "the linked worktree's .git must be a FILE, not a directory, or this test is not \
             exercising B2 at all"
        );

        // A branch that already exists, so `create_worktree_sync` takes the
        // ATTACH path (no `-b`) -- the exact shape B2 was found on.
        git(&main_repo, &["branch", "attach-target"]);
        let target = ws.path().join("attached");

        let result = create_worktree_sync(&linked, &target, "attach-target", "tester");

        assert!(
            matches!(result, Ok(WorktreeCreation::Created { .. })),
            "attach through a linked worktree must succeed, got {result:?}"
        );
    }

    /// Scenario: fork issue #282, async twin of `worktree/create/001`. The
    /// `issue_dispatch` scheduler's own `create_worktree` (not the TUI's
    /// `create_worktree_sync`, which PR #331 already locked) races two
    /// concurrent attaches to the SAME already-existing branch at the SAME
    /// target path, across many trials -- a single trial proves nothing,
    /// since the issue's own sync-path measurement found only ~8%
    /// corruption for one 2-way race. Asserts, for every trial, that at
    /// most one caller reports `Created`, that `git worktree list` shows
    /// the target path exactly once, and that `.git/worktrees/` holds
    /// exactly one admin entry for it. `create_worktree` now holds a lock
    /// around the attach path too (fork #282, this PR), so this test pins
    /// that the lock actually serializes both callers on the async path PR
    /// #331 explicitly left open — without it, git itself would let both
    /// win on at least some trials, producing two `Created` results and two
    /// admin entries for one on-disk path.
    //
    // Written as a sync `#[test]` driving an explicit multi-thread runtime
    // rather than `#[tokio::test]`: the linkage-check (PRD #77 Decision 17)
    // ties each `#[spec(...)]` to the next plain `fn` definition and does
    // not recognize a `#[tokio::test] async fn` -- see
    // `issue_claim_019_dispatch_path_assignee_refresh_keeps_assignee` above
    // for the same pattern. Multi-thread (not current-thread) so the two
    // racers' `tokio::spawn`ed tasks can genuinely run in parallel at the
    // Rust level too, matching how the real daemon's `#[tokio::main]`
    // (multi-thread by default) schedules `create_worktree` callers.
    #[cfg(unix)]
    #[spec("worktree/create/003")]
    #[test]
    fn create_003_concurrent_async_attach_never_double_creates() {
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

        fn canonical_for_compare(path: &Path) -> PathBuf {
            path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
        }

        fn count_admin_entries_for(clone_dir: &Path, worktree_dir: &Path) -> usize {
            let worktrees_dir = clone_dir.join(".git").join("worktrees");
            let Ok(entries) = std::fs::read_dir(&worktrees_dir) else {
                return 0;
            };
            let target = canonical_for_compare(&worktree_dir.join(".git"));
            entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .filter(|e| {
                    std::fs::read_to_string(e.path().join("gitdir"))
                        .map(|s| canonical_for_compare(Path::new(s.trim())) == target)
                        .unwrap_or(false)
                })
                .count()
        }

        fn count_worktree_list_entries(clone_dir: &Path, worktree_dir: &Path) -> usize {
            let out = std::process::Command::new("git")
                .current_dir(clone_dir)
                .args(["worktree", "list", "--porcelain"])
                .output()
                .expect("git worktree list must spawn");
            assert!(
                out.status.success(),
                "git worktree list failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let wanted = canonical_for_compare(worktree_dir);
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|l| {
                    l.strip_prefix("worktree ")
                        .map(|p| canonical_for_compare(Path::new(p)) == wanted)
                        .unwrap_or(false)
                })
                .count()
        }

        const TRIALS: usize = 60;

        let ws = tempfile::tempdir().unwrap();
        let ws_root = ws.path().to_path_buf();
        let clone_dir = ws_root.join("clone");
        std::fs::create_dir_all(&clone_dir).unwrap();
        git(&clone_dir, &["init", "--initial-branch=main", "--quiet"]);
        git(&clone_dir, &["config", "user.email", "test@example.com"]);
        git(&clone_dir, &["config", "user.name", "Test"]);
        git(&clone_dir, &["config", "commit.gpgsign", "false"]);
        std::fs::write(clone_dir.join("README.md"), "seed\n").unwrap();
        git(&clone_dir, &["add", "README.md"]);
        git(&clone_dir, &["commit", "--quiet", "-m", "seed"]);

        let mut branches = Vec::with_capacity(TRIALS);
        let mut worktree_dirs = Vec::with_capacity(TRIALS);
        for i in 0..TRIALS {
            let branch = format!("async-race-{i}");
            git(&clone_dir, &["branch", &branch]);
            branches.push(branch);
            worktree_dirs.push(ws_root.join(format!("wt-{i}")));
        }

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("build multi-thread runtime");

        // Every trial's pair races concurrently with every other trial's
        // pair too (not sequentially) -- mirrors `worktree/create/001`'s own
        // reasoning: real concurrent orchestrations hit the shared
        // repository this way, and it keeps the whole test's wall-clock
        // close to a single `git worktree add`'s rather than TRIALS times
        // that.
        let results: Vec<[Result<WorktreeCreation, String>; 2]> = rt.block_on(async {
            let mut handles = Vec::with_capacity(TRIALS);
            for i in 0..TRIALS {
                let barrier = Arc::new(tokio::sync::Barrier::new(2));
                let clone_dir_a = clone_dir.clone();
                let clone_dir_b = clone_dir.clone();
                let worktree_dir_a = worktree_dirs[i].clone();
                let worktree_dir_b = worktree_dirs[i].clone();
                let branch_a = branches[i].clone();
                let branch_b = branches[i].clone();
                let barrier_a = barrier.clone();
                let barrier_b = barrier;
                let h_a = tokio::spawn(async move {
                    barrier_a.wait().await;
                    create_worktree(
                        &clone_dir_a,
                        &worktree_dir_a,
                        &branch_a,
                        true,
                        Creator::dispatch("racer-a"),
                    )
                    .await
                });
                let h_b = tokio::spawn(async move {
                    barrier_b.wait().await;
                    create_worktree(
                        &clone_dir_b,
                        &worktree_dir_b,
                        &branch_b,
                        true,
                        Creator::dispatch("racer-b"),
                    )
                    .await
                });
                handles.push((h_a, h_b));
            }
            let mut results = Vec::with_capacity(TRIALS);
            for (h_a, h_b) in handles {
                let a = h_a.await.expect("racer-a task must not panic");
                let b = h_b.await.expect("racer-b task must not panic");
                results.push([a, b]);
            }
            results
        });

        let mut failures: Vec<String> = Vec::new();
        let mut double_created = 0usize;
        let mut duplicate_admin = 0usize;
        let mut duplicate_listed = 0usize;

        for (i, pair) in results.iter().enumerate() {
            let created_count = pair
                .iter()
                .filter(|r| matches!(r, Ok(WorktreeCreation::Created { .. })))
                .count();
            if created_count != 1 {
                double_created += 1;
                failures.push(format!(
                    "trial {i}: expected exactly one Created, got a={:?} b={:?}",
                    pair[0], pair[1]
                ));
            }

            let admin_count = count_admin_entries_for(&clone_dir, &worktree_dirs[i]);
            if admin_count != 1 {
                duplicate_admin += 1;
                failures.push(format!(
                    "trial {i}: expected exactly one .git/worktrees admin entry for {:?}, found {admin_count}",
                    worktree_dirs[i]
                ));
            }

            let listed_count = count_worktree_list_entries(&clone_dir, &worktree_dirs[i]);
            if listed_count != 1 {
                duplicate_listed += 1;
                failures.push(format!(
                    "trial {i}: expected `git worktree list` to show {:?} exactly once, found {listed_count}",
                    worktree_dirs[i]
                ));
            }
        }

        assert!(
            failures.is_empty(),
            "fork issue #282: the async create_worktree's attach-path lock failed to serialize \
             concurrent callers -- \
             {double_created}/{TRIALS} trials produced more than one `Created`, \
             {duplicate_admin}/{TRIALS} trials left more than one `.git/worktrees` admin entry, \
             {duplicate_listed}/{TRIALS} trials showed the path more than once in `git worktree \
             list`. Failures:\n{}",
            failures.join("\n")
        );
    }

    /// Scenario: fork #282 final-pass F1 (reviewer) / A1 (auditor), corrected
    /// by a later F1 finding. Pins [`run_status_killable`]'s
    /// `kill_on_drop(true)` directly, at the same seam [`create_worktree`]
    /// itself uses (an external `tokio::time::timeout` wrapping the call) —
    /// rather than driving the real [`WORKTREE_GIT_TIMEOUT`] (30s, not
    /// injectable), which would make this a 30s-plus test for no added
    /// coverage; `run_status_sync`'s own tests take the same shortcut by
    /// calling it with a short explicit `timeout` instead of
    /// `create_worktree_sync`'s hardcoded constant.
    ///
    /// Deliberately exercises [`run_status_killable`], not plain
    /// [`run_status`]: an earlier version of this fix set `kill_on_drop`
    /// unconditionally on the shared function, which widened it to every
    /// other caller reached through the same scheduler task (a `gh repo
    /// clone`, `git fetch`/`pull`, the `gh issue` writes, tab-close's `git
    /// worktree remove`) and broke `gh repo clone` in particular — see
    /// [`run_status`]'s doc comment. Only the two `create_worktree` call
    /// sites that need a kill (the `git worktree add` invocation and
    /// [`attempt_worktree_cleanup_async`]) go through the killable variant
    /// now, so this test targets that variant directly rather than the
    /// shared one.
    ///
    /// A fake `git` is a shell script that touches a `started` marker
    /// immediately, sleeps a full second, then touches a `finished` marker.
    /// `run_status_killable` is called wrapped in a 100ms
    /// `tokio::time::timeout`, so the sleep has not elapsed when the timeout
    /// fires. Without `kill_on_drop(true)`, dropping that timed-out future
    /// would leave the script running in the background — it would go on to
    /// sleep out its full second and write `finished` regardless of the
    /// caller having already moved on. With `kill_on_drop(true)`, the drop
    /// sends a kill signal immediately, so `finished` must never appear even
    /// after waiting well past the script's own sleep duration.
    #[cfg(unix)]
    #[test]
    fn run_status_killable_kills_the_child_instead_of_orphaning_it() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = tempfile::tempdir().unwrap();
        let started_marker = scratch.path().join("started");
        let finished_marker = scratch.path().join("finished");
        let script = scratch.path().join("slow-git.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ntouch {}\nsleep 1\ntouch {}\n",
                started_marker.display(),
                finished_marker.display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread runtime");

        rt.block_on(async {
            let program = script.to_str().unwrap();
            let result = tokio::time::timeout(
                Duration::from_millis(100),
                run_status_killable(program, &[]),
            )
            .await;
            assert!(
                result.is_err(),
                "the external 100ms timeout should have elapsed before the script's 1s sleep \
                 finished, got {result:?}"
            );

            // Generous headroom well past the script's own 1s sleep: if
            // `kill_on_drop` were not set, the script would finish and write
            // `finished` somewhere in this window.
            tokio::time::sleep(Duration::from_millis(1500)).await;
        });

        assert!(
            started_marker.exists(),
            "the script must have actually started running for this test to mean anything"
        );
        assert!(
            !finished_marker.exists(),
            "run_status_killable's kill_on_drop should have killed the child before its 1s \
             sleep completed, but the finished marker exists -- the child ran to completion in \
             the background instead of being killed, reproducing fork #282 final-pass F1/A1"
        );
    }

    /// Scenario: fork #282 final-pass F1 correction. Pins the OTHER half of
    /// the F1 fix: plain [`run_status`] (used by every caller except the two
    /// `create_worktree` sites that need a kill) must NOT set
    /// `kill_on_drop` — a timed-out caller should leave the child to finish
    /// detached, not kill it. Same fake-`git`-script shape as
    /// [`run_status_killable_kills_the_child_instead_of_orphaning_it`], but
    /// asserts the opposite outcome: after the same 100ms external timeout
    /// elapses and the same 1500ms headroom passes, `finished` MUST exist —
    /// if it does not, `run_status` has regressed back to killing its
    /// child, reproducing the `gh repo clone` breakage F1's correction
    /// exists to prevent (a killed clone leaves a partial `clone_dir` that
    /// wedges every subsequent `ensure_clone` call).
    #[cfg(unix)]
    #[test]
    fn run_status_leaves_the_child_to_finish_detached_instead_of_killing_it() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = tempfile::tempdir().unwrap();
        let started_marker = scratch.path().join("started");
        let finished_marker = scratch.path().join("finished");
        let script = scratch.path().join("slow-git.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ntouch {}\nsleep 1\ntouch {}\n",
                started_marker.display(),
                finished_marker.display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread runtime");

        rt.block_on(async {
            let program = script.to_str().unwrap();
            let result =
                tokio::time::timeout(Duration::from_millis(100), run_status(program, &[])).await;
            assert!(
                result.is_err(),
                "the external 100ms timeout should have elapsed before the script's 1s sleep \
                 finished, got {result:?}"
            );

            // Generous headroom well past the script's own 1s sleep: with no
            // `kill_on_drop`, the detached child should finish and write
            // `finished` somewhere in this window.
            tokio::time::sleep(Duration::from_millis(1500)).await;
        });

        assert!(
            started_marker.exists(),
            "the script must have actually started running for this test to mean anything"
        );
        assert!(
            finished_marker.exists(),
            "run_status should leave a timed-out child to finish detached, but the finished \
             marker is missing -- the child was killed instead, which would break every other \
             caller of this shared function (gh repo clone, git fetch/pull, the gh issue \
             writes, tab-close's git worktree remove)"
        );
    }

    /// Scenario: fork #282 final-pass F3. Before this fix, [`create_worktree`]
    /// resolved its `git_common_dir` via the synchronous, unbounded
    /// `git_common_dir` inside a `spawn_blocking`, with NO external timeout
    /// wrapping that `.await` at all -- unlike the lock acquisition and
    /// probe/add calls a few statements later in the same function, which
    /// are all bounded by [`WORKTREE_GIT_TIMEOUT`]. A wedged `git rev-parse
    /// --git-common-dir` (a stalled filesystem under the repo) hung
    /// `create_worktree` forever, the exact bug class audit B1 fixed for the
    /// lock acquisition two statements later.
    ///
    /// A fake `git` on `PATH` is a shell script that touches a `started`
    /// marker immediately, then sleeps far longer than the timeout this test
    /// wraps the call in. `git_common_dir_async` is called wrapped in a
    /// 200ms `tokio::time::timeout`, the same pattern
    /// [`create_worktree`]'s prologue now uses; asserts the call returns
    /// promptly (well under the fake git's 5s sleep) rather than hanging,
    /// pinning that the prologue is bounded end to end.
    #[cfg(unix)]
    #[test]
    fn git_common_dir_async_is_bounded_by_an_external_timeout() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::Instant;

        let scratch = tempfile::tempdir().unwrap();
        let bindir = scratch.path().join("bin");
        std::fs::create_dir_all(&bindir).unwrap();
        let started_marker = scratch.path().join("started");
        let git_stub = bindir.join("git");
        std::fs::write(
            &git_stub,
            format!("#!/bin/sh\ntouch {}\nsleep 5\n", started_marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&git_stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let prior_path = std::env::var("PATH").unwrap_or_default();
        // SAFETY: `std::env::set_var` is process-global, but CLAUDE.md rule 5's
        // fork addendum means every test run happens in CI via `cargo
        // nextest`, which runs each test in its OWN process -- so no sibling
        // test in this module ever observes this mutation. The prior value
        // is restored below regardless, before this function returns.
        unsafe {
            std::env::set_var("PATH", format!("{}:{prior_path}", bindir.display()));
        }

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread runtime");

        let start = Instant::now();
        let result = rt.block_on(async {
            tokio::time::timeout(
                Duration::from_millis(200),
                git_common_dir_async(scratch.path()),
            )
            .await
        });
        let elapsed = start.elapsed();

        // SAFETY: see the comment on the previous unsafe block.
        unsafe {
            std::env::set_var("PATH", prior_path);
        }

        assert!(
            started_marker.exists(),
            "the fake git script must have actually started running for this test to mean \
             anything"
        );
        assert!(
            result.is_err(),
            "the external 200ms timeout should have elapsed before the fake git's 5s sleep \
             finished, got {result:?}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "git_common_dir_async wrapped in an external timeout should return promptly instead \
             of hanging -- it took {elapsed:?} to return, reproducing fork #282 final-pass F3's \
             `create_worktree` hang"
        );
    }

    /// Scenario: fork #282 final-pass F1 (reviewer) / A1 (auditor). Pins
    /// [`attempt_worktree_cleanup_async`] directly — the cleanup path
    /// [`create_worktree`]'s `TimedOut` arm now runs instead of hardcoding
    /// `cleaned_up: false`. Creates a REAL worktree the way a killed `git
    /// worktree add` would leave one (registered via a genuine `git
    /// worktree add`, standing in for the half-finished state a kill leaves
    /// behind), then asserts cleanup both reports `true` and actually makes
    /// the worktree disappear from `git worktree list` and the directory
    /// itself — matching the sync twin's "confirmed" contract exactly.
    #[cfg(unix)]
    #[test]
    fn attempt_worktree_cleanup_async_removes_a_registered_worktree_and_confirms_it() {
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
        git(&clone_dir, &["branch", "cleanup-target"]);

        let worktree_dir = ws.path().join("wt");
        git(
            &clone_dir,
            &[
                "worktree",
                "add",
                worktree_dir.to_str().unwrap(),
                "cleanup-target",
            ],
        );
        assert!(worktree_dir.exists(), "setup: worktree add must succeed");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread runtime");
        let cleaned_up =
            rt.block_on(async { attempt_worktree_cleanup_async(&clone_dir, &worktree_dir).await });

        assert!(
            cleaned_up,
            "attempt_worktree_cleanup_async must report true for a plain `git worktree remove \
             --force` against a directory it created"
        );
        assert!(
            !worktree_dir.exists(),
            "the worktree directory must actually be gone after a confirmed cleanup"
        );
        let list = std::process::Command::new("git")
            .current_dir(&clone_dir)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .expect("git worktree list must spawn");
        let listing = String::from_utf8_lossy(&list.stdout);
        assert!(
            !listing.contains(worktree_dir.to_str().unwrap()),
            "the cleaned-up worktree must no longer be registered: {listing}"
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
