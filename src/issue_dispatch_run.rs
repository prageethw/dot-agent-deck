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
//!      skip's rendered text — see [`fetch_claim_comment`]. With `claim_gating`
//!      in effect (upstream PRD #421 Phase 3, default OFF) another deck's live
//!      `agent-deck/claimed` record on GitHub is a further skip, read only when
//!      the first three leave the issue open, via
//!      [`crate::issue_dispatch::dispatch_decision`]. A skip because the
//!      worktree exists also refreshes this deck's own claim record once it is
//!      half a TTL old.
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
//!      notification system); a skip always names which cause
//!      fired (worktree exists, open PR, `in-progress` label, another deck's
//!      live claim, or a concurrent creator).
//!   9. **PRD #421 Phase 2 (upstream's second claim system, kept beside the
//!      `in-progress` claim of step 6)** — after a successful spawn, claim the
//!      issue on GitHub: provision the `agent-deck/claimed` label just in time,
//!      add it, and write or edit this deck's claimant comment. Best-effort and
//!      never fatal, with one notification per issue whose claim fell short.
//!      Nothing ever removes the label or the comment.
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
    CLAIM_COMMENT_PREFIX, Claimant, DispatchDecision, IN_PROGRESS_LABEL, IN_PROGRESS_LABEL_COLOR,
    IN_PROGRESS_LABEL_DESCRIPTION, Identity, ParsedClaim, SkipCause, TRIAGE_LABELS, TYPE_LABELS,
    build_dispatch_prompt, claim_comment_body, derive_issue_paths, dispatch_decision,
    gh_current_login_argv, issue_comment_argv, issue_edit_add_label_argv, issue_edit_assignee_argv,
    issue_list_argv, issue_view_comments_argv, label_create_argv, parse_current_assignees,
    parsed_claim_from_comment_json, pr_list_for_issue_argv, validate_gh_login,
};
use crate::scheduler::{Notifier, NotifyEvent};
use crate::spawn::{SpawnKind, SpawnRequest, spawn};
use crate::terminal_sanitize::{sanitize_for_terminal_display, sanitize_path_for_terminal_display};
use crate::worktree_owner::Creator;

// ---------------------------------------------------------------------------
// M2.4 — daemon-side worktree registry (close → cleanup plumbing)
// ---------------------------------------------------------------------------

/// Daemon-owned, in-memory map: per-issue worktree dir → the clone that owns it
/// (preserved on cleanup). Shared between the fire-time dispatch flow (records
/// the worktree the moment it is created, before the spawn) and the `StopAgent`
/// handler (removes it on close).
///
/// Keyed by the **worktree path**, not the spawned agent id, on purpose: the
/// spawn primitive only returns the registry id after the agent is registered
/// and visible to a `StopAgent`, so a tab closed promptly after the agent
/// appears would race a per-agent-id record. The closing agent is instead matched to its worktree via
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
    ///
    /// Under [`RemovalPolicy::IsolatedClone`] the entry's own directory is
    /// NOT a linked worktree of this path at all — it is an independent
    /// clone `git clone`d FROM it — so "owns" and "preserved by removal"
    /// don't apply the same way; this field is then informational only
    /// (which source repo the clone came from), unused by
    /// [`remove_worktree`], which never touches it under that policy.
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
    /// PRD fork#325 M3 (issue #490 fix round, reviewer B2 / auditor A3): the
    /// entry is an isolated `git clone` (`provision_isolated_clone_sync`),
    /// not a linked worktree of `clone_dir` — `git worktree remove` does not
    /// apply to it at all (it targets a directory that is not a working
    /// tree of any repo and fails with exit 128). Unlike a linked worktree,
    /// removing it also destroys its OWN `.git`, so a clean working tree is
    /// not proof the branch is safe to discard: any commits made only on
    /// this clone's local branch have no copy anywhere else, whereas a
    /// removed linked worktree's branch survives in the shared object
    /// store it came from. `remove_worktree` therefore never attempts
    /// removal under this policy at all — it always reports
    /// [`RemoveOutcome::Kept`] with
    /// [`crate::event::KeptReason::IsolatedClone`], regardless of
    /// dirtiness. This stays true even now that PRD fork#325 M4c has
    /// shipped an actually-safe automatic removal path
    /// (`crate::worktree_reclaim::isolated_clone_report`'s five-condition,
    /// `headRefOid`-based eligibility rule, plus `remove_isolated_clone_dir`)
    /// — that path lives entirely on the separate `worktree reclaim` CLI
    /// surface (a deliberate, operator-invoked, `--yes`-gated pass over
    /// every discovered isolated clone), not on this daemon-side tab-close
    /// removal path, which fires automatically the instant the last agent
    /// rooted in a worktree closes. Reusing M4c's eligibility rule here
    /// would need its own decision (the tab-close path has no equivalent of
    /// `worktree reclaim`'s explicit `--yes` confirmation, and M4c's own
    /// documented residual — no liveness signal, only provenance and
    /// content safety, see that rule's own doc comment — is a materially
    /// different risk on a path that runs unattended on every close rather
    /// than only when an operator asks). Not attempted in this milestone;
    /// this policy is unchanged by M4c.
    IsolatedClone,
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

/// Does `worktree_dir` hold uncommitted work? The ONE `git status --porcelain`
/// this feature asks, so the removal path and issue #717's close-confirmation
/// preview cannot disagree about what "dirty" means.
///
/// `Err` is "the question could not be answered" (no `git`, the directory is
/// gone, a locked index), never "clean" — both callers treat it as a reason to
/// KEEP, because the fail-safe direction for a deletion gate is to decline.
pub async fn worktree_is_dirty(worktree_dir: &Path) -> Result<bool, String> {
    let worktree = worktree_dir.to_string_lossy();
    let output = run_git_capture(&["-C", &worktree, "status", "--porcelain"]).await?;
    Ok(!output.trim().is_empty())
}

/// Issue #717: the close-confirmation dialog's advance warning that a
/// dispatched worktree is about to be KEPT rather than removed.
///
/// Used by both halves of the report — the close dialog's PREDICTION
/// ([`kept_worktree_preview`], made while the agent is still alive) and the
/// daemon's post-cleanup FACT ([`remove_worktree`], measured once the agent is
/// reaped). Carries the path because recovering the work means going to it —
/// the whole point of the report.
///
/// `confirmed_dirty` separates the two ways a tree ends up kept, which read
/// differently to a human:
///
/// * `true` — [`worktree_is_dirty`] answered yes. It can be stated flatly.
/// * `false` — the tree is kept, but not because uncommitted work was measured:
///   the probe failed, or blew the preview's deadline, or the `git worktree
///   remove` itself failed. `KeepIfDirty` keeps the tree in all of those cases,
///   so the report is still true; it just has to be phrased conditionally.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KeptWorktree {
    /// Absolute path of the worktree that will be left on disk.
    pub path: String,
    /// Whether `git status --porcelain` actually reported uncommitted work, as
    /// opposed to the probe being inconclusive. See the type docs.
    pub confirmed_dirty: bool,
}

/// Issue #717: would closing the panes in `pane_ids` leave a dispatched
/// worktree behind, and if so, which one?
///
/// Answers the question the close-confirmation dialog needs BEFORE the
/// keystroke, from the only process that can: the registry that knows the
/// removal policy and the filesystem that holds the tree both live here, and in
/// remote mode neither is on the client's machine at all.
///
/// Three properties are deliberate:
///
/// * **It PEEKS at the registry, never [`take_worktree`].** This runs while the
///   user is still deciding, and a preview that consumed the entry would strand
///   the directory forever when they pressed Cancel.
/// * **Only [`RemovalPolicy::KeepIfDirty`] is reported.** A `Force` worktree is
///   removed regardless, so there is nothing kept to tell anyone about.
/// * **The probe is time-boxed** by `probe_timeout`, because this sits on an
///   interactive key path while `remove_worktree`'s copy does not. A probe that
///   does not finish downgrades to `confirmed_dirty: false` rather than
///   dropping the warning — the tree is kept either way, so the useful half of
///   the report (the path) survives a slow checkout. The timeout abandons the
///   `git` child rather than killing it, and deliberately: `git status`
///   refreshes the index as it walks, so a SIGKILL mid-write is a worse
///   outcome than letting a short read-only process finish and be reaped by
///   tokio's orphan queue.
pub async fn kept_worktree_preview(
    records: &[AgentRecord],
    worktrees: &WorktreeRegistry,
    pane_ids: &[String],
    probe_timeout: Duration,
) -> Option<KeptWorktree> {
    kept_worktree_preview_with(
        records,
        worktrees,
        pane_ids,
        probe_timeout,
        worktree_is_dirty,
    )
    .await
}

/// [`kept_worktree_preview`] with the dirty-probe as a parameter. Production
/// has exactly one caller, which passes [`worktree_is_dirty`]; the parameter
/// exists so a test can hand in a probe that never answers and reach the
/// deadline arm by construction (fork issue #811, upstream issue #1449). A
/// real `git status` cannot make the deadline win deterministically:
/// `tokio::time::timeout` polls the probe before its timer, so a probe that
/// has already finished when the timer fires beats even a 1ns deadline.
async fn kept_worktree_preview_with(
    records: &[AgentRecord],
    worktrees: &WorktreeRegistry,
    pane_ids: &[String],
    probe_timeout: Duration,
    probe: impl AsyncFn(&Path) -> Result<bool, String>,
) -> Option<KeptWorktree> {
    let mut seen: Vec<PathBuf> = Vec::new();
    for pane_id in pane_ids {
        let Some(record) = records
            .iter()
            .find(|r| r.pane_id_env.as_deref() == Some(pane_id.as_str()))
        else {
            continue;
        };
        let Some(worktree) = worktree_of_record(record) else {
            continue;
        };
        if seen.contains(&worktree) {
            continue;
        }
        seen.push(worktree.clone());
        let policy = worktrees
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&worktree)
            .map(|entry| entry.policy);
        if policy != Some(RemovalPolicy::KeepIfDirty) {
            continue;
        }
        let confirmed_dirty = match tokio::time::timeout(probe_timeout, probe(&worktree)).await {
            Ok(Ok(true)) => true,
            // Clean: this tree is about to be REMOVED, and saying so would be
            // the noise that makes the warning worth ignoring. Say nothing.
            Ok(Ok(false)) => continue,
            // A failed probe is what `remove_worktree` itself treats as a
            // reason to keep, so report the path — under wording that does not
            // claim more than was measured.
            Ok(Err(e)) => {
                tracing::debug!(
                    worktree = %worktree.display(),
                    error = %e,
                    "close preview: could not check worktree status"
                );
                false
            }
            // A blown deadline is not an answer either way: the removal path
            // runs the same probe with no deadline and may still find it clean.
            // Report conditionally rather than dropping the path.
            Err(_) => {
                tracing::debug!(
                    worktree = %worktree.display(),
                    "close preview: worktree status probe timed out"
                );
                false
            }
        };
        return Some(KeptWorktree {
            path: worktree.to_string_lossy().into_owned(),
            confirmed_dirty,
        });
    }
    None
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
    /// The worktree was removed. Carries the identity (issue #469) that
    /// removed it — the caller-supplied `remover` [`remove_worktree`] was
    /// given, mirroring [`RemoveFailed`](RemoveOutcome::RemoveFailed)'s own
    /// caller-supplied string. Stored RAW, not the sanitized copy that
    /// reaches the log line — safe today because the only non-test consumers
    /// discard this payload entirely, but its sibling variant
    /// [`RemoveFailed`](RemoveOutcome::RemoveFailed) already crosses the wire
    /// as [`crate::event::WorktreeKeptNotice`], so any future sink that
    /// surfaces THIS payload (to a TUI, a log, anywhere) must sanitize it
    /// first, the same way [`remove_worktree`]'s own log line does.
    Removed(String),
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

/// Build the argv for `git -C <clone_dir> worktree remove [--force] -- <worktree_dir>`
/// — a pure function so the `--` end-of-options separator (issue #469 /
/// #144 finding 4) can be pinned by a plain data assertion instead of a
/// PATH-shadowed shell-stub test, mirroring `issue_dispatch.rs`'s
/// `worktree_remove_argv` (PR #458), which solved the identical problem for
/// its own caller. Unlike that sibling, `force` is conditional here (this
/// caller uses [`RemovalPolicy::KeepIfDirty`] too), so it takes a `bool`
/// rather than always pushing `--force`.
fn remove_worktree_argv(clone_dir: &str, worktree_dir: &str, force: bool) -> Vec<String> {
    let mut args = vec![
        "-C".to_string(),
        clone_dir.to_string(),
        "worktree".to_string(),
        "remove".to_string(),
    ];
    if force {
        args.push("--force".to_string());
    }
    args.push("--".to_string());
    args.push(worktree_dir.to_string());
    args
}

/// Remove a dispatched worktree from its clone (`git -C <clone> worktree remove
/// <worktree>`), PRESERVING the clone — except under
/// [`RemovalPolicy::IsolatedClone`], whose entry is not a linked worktree at
/// all and for which this function never runs `git worktree remove` (see
/// below). Never fatal to the caller — a non-zero exit (already removed,
/// locked) or a spawn error is logged AND reported back as
/// [`RemoveOutcome::RemoveFailed`], so the tab-close path never panics or
/// blocks on it, but the caller can no longer mistake the failure for success.
///
/// `policy` decides what happens: under [`RemovalPolicy::IsolatedClone`] the
/// entry is kept unconditionally, with no probe at all — see that variant's
/// own doc comment for why dirtiness is irrelevant to it. Under
/// [`RemovalPolicy::KeepIfDirty`] a dirty tree (or a status probe that fails,
/// so dirtiness is unknown) is left in place, logged, and reported back as
/// [`RemoveOutcome::Kept`]; under [`RemovalPolicy::Force`] the tree is removed
/// regardless.
///
/// Issue #717: this is the AUTHORITATIVE answer about whether a tree was kept,
/// and the only one that is: it is measured after `close_agent` has reaped the
/// agent, so nothing is writing to the tree any more and the verdict cannot go
/// stale. The close dialog's [`kept_worktree_preview`] runs while the agent is
/// still alive and is therefore a prediction — a useful one, because it arrives
/// while the user can still cancel, but not a fact.
///
/// `remover` (issue #469) names the caller responsible for this removal —
/// forwarded verbatim into [`RemoveOutcome::Removed`] and the success log,
/// mirroring how [`attempt_worktree_cleanup`]/[`attempt_worktree_cleanup_async`]
/// attribute their own removals. Caller-supplied and just as unauthenticated
/// as those siblings' `remover`, so it is sanitised before logging.
pub async fn remove_worktree(
    worktree_dir: &Path,
    clone_dir: &Path,
    policy: RemovalPolicy,
    remover: &str,
) -> RemoveOutcome {
    let worktree = worktree_dir.to_string_lossy();
    // PRD #1589 (upstream, auditor S5, adopted): the path and git's error text
    // are producer values — a directory name may carry a newline or a terminal
    // control — so each is escaped and bounded before it reaches the log.
    // `remover` is sanitised separately, as before (issue #469).
    let logged = |value: &str| {
        crate::config_validation::escape_field_for_log(
            value,
            crate::daemon_stop::MAX_LOGGED_PATH_CHARS,
        )
    };
    let shown_worktree = logged(&worktree);
    // An exhaustive match, not `if policy == …` chains (fix round 2, P3-10):
    // a future fourth `RemovalPolicy` variant now fails to compile here
    // instead of silently falling through to the destructive tail below.
    match policy {
        RemovalPolicy::IsolatedClone => {
            // See [`RemovalPolicy::IsolatedClone`]'s doc comment: `clone_dir`
            // is not a linked worktree of anything (`git worktree remove`
            // would fail with exit 128), and a clean working tree does not
            // prove it is safe to `remove_dir_all` — this clone's `.git` may
            // hold the only copy of commits made on its local branch. Kept
            // unconditionally; not even a dirty-status probe is run, since the
            // outcome is the same either way.
            tracing::info!(
                worktree = %shown_worktree,
                remover = %crate::terminal_sanitize::sanitize_for_terminal_display(remover),
                "dispatch: isolated clone kept in place (not a linked worktree; see RemovalPolicy::IsolatedClone)"
            );
            return RemoveOutcome::Kept(crate::event::KeptReason::IsolatedClone);
        }
        RemovalPolicy::KeepIfDirty => {
            // The ONE `git status --porcelain` this feature asks
            // ([`worktree_is_dirty`], upstream's consolidation, adopted over
            // the fork's own `probe_worktree_dirty`): the removal path and
            // issue #717's close-confirmation preview cannot disagree about
            // what "dirty" means, and it is bounded and hook/fsmonitor-free.
            match worktree_is_dirty(worktree_dir).await {
                Ok(true) => {
                    tracing::warn!(
                        worktree = %shown_worktree,
                        remover = %crate::terminal_sanitize::sanitize_for_terminal_display(remover),
                        "dispatch: worktree has uncommitted changes; leaving in place"
                    );
                    return RemoveOutcome::Kept(crate::event::KeptReason::Dirty);
                }
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(
                        worktree = %shown_worktree,
                        remover = %crate::terminal_sanitize::sanitize_for_terminal_display(remover),
                        error = %logged(&e),
                        "dispatch: could not check worktree status; leaving in place"
                    );
                    return RemoveOutcome::Kept(crate::event::KeptReason::ProbeError);
                }
            }
        }
        RemovalPolicy::Force => {}
    }

    // The fork's argv builder (the `--` end-of-options separator, issue #469 /
    // #144 finding 4) run through upstream's `run_git_status` (issue #1181:
    // ambient `GIT_DIR` neutralised, hooks and fsmonitor off, bounded; rule 13
    // forbids a `git` program literal in this module's production half).
    let args = remove_worktree_argv(
        &clone_dir.to_string_lossy(),
        &worktree,
        policy == RemovalPolicy::Force,
    );
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let res = run_git_status(&arg_refs).await;
    match res {
        Ok(()) => {
            tracing::info!(
                worktree = %shown_worktree,
                remover = %crate::terminal_sanitize::sanitize_for_terminal_display(remover),
                "dispatch: removed worktree (clone preserved)"
            );
            RemoveOutcome::Removed(remover.to_string())
        }
        Err(e) => {
            tracing::warn!(
                worktree = %shown_worktree,
                remover = %crate::terminal_sanitize::sanitize_for_terminal_display(remover),
                error = %logged(&e),
                "dispatch: worktree cleanup failed"
            );
            RemoveOutcome::RemoveFailed(e)
        }
    }
}

/// The `remover` ([`remove_worktree`]'s attribution string, issue #469) the
/// `close` verb's removal path ([`remove_worktree_outcome`]) logs under.
pub const CLOSE_VERB_REMOVER: &str = "close";

/// PRD #1589 D6 (upstream): what [`remove_worktree_outcome`] did, with the
/// ways a tree ends up kept told apart — [`KeptWorktree`] folds a failed
/// status probe and a failed removal into one `confirmed_dirty: false`, which
/// is enough for a warning and not for the `close` verb's report.
///
/// This is the `close` verb's typed VIEW of the fork's richer
/// [`RemoveOutcome`] (the one implementation is [`remove_worktree`]); the two
/// map onto each other through [`crate::event::KeptReason`] (ruling 48 of the
/// 2026-11-05 upstream merge):
///
/// | `WorktreeRemoval`   | `RemoveOutcome`                                  | `KeptReason`      |
/// |---------------------|--------------------------------------------------|-------------------|
/// | `Removed`           | `Removed(remover)`                               | (none)            |
/// | `KeptDirty`         | `Kept(KeptReason::Dirty)`                        | `Dirty`           |
/// | `KeptCouldNotCheck` | `Kept(KeptReason::ProbeError)`                   | `ProbeError`      |
/// | `RemoveFailed`      | `RemoveFailed(error)` / `Kept(RemovalFailed)`    | `RemovalFailed`   |
/// | `KeptIsolatedClone` | `Kept(KeptReason::IsolatedClone)`                | `IsolatedClone`   |
///
/// `KeptIsolatedClone` is the one variant upstream's enum lacked: the fork's
/// [`RemovalPolicy::IsolatedClone`] entries (PRD fork#325 M3) are never
/// removed by this path at all, and folding that into `KeptCouldNotCheck`
/// would tell the user a probe failed when none ran. The mapping is lossy in
/// one direction only: `RemoveFailed` carries no error text here (upstream's
/// type never did); a caller that needs the text uses [`remove_worktree`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeRemoval {
    /// Removed; the clone and the branch are kept.
    Removed,
    /// Kept: `git status --porcelain` reported uncommitted work.
    KeptDirty,
    /// Kept: the status probe failed, so dirtiness is unknown.
    KeptCouldNotCheck,
    /// Kept: `git worktree remove` failed.
    RemoveFailed,
    /// Kept: the entry is an isolated clone ([`RemovalPolicy::IsolatedClone`]),
    /// never removed by this path (fork PRD fork#325 M3).
    KeptIsolatedClone,
}

impl WorktreeRemoval {
    /// The issue #717 report of this outcome: `None` when the tree was
    /// removed. `confirmed_dirty` is true only for [`Self::KeptDirty`].
    pub fn kept(self, worktree_dir: &Path) -> Option<KeptWorktree> {
        let path = worktree_dir.to_string_lossy().into_owned();
        match self {
            WorktreeRemoval::Removed => None,
            WorktreeRemoval::KeptDirty => Some(KeptWorktree {
                path,
                confirmed_dirty: true,
            }),
            WorktreeRemoval::KeptCouldNotCheck
            | WorktreeRemoval::RemoveFailed
            | WorktreeRemoval::KeptIsolatedClone => Some(KeptWorktree {
                path,
                confirmed_dirty: false,
            }),
        }
    }

    /// Why the tree was kept, in the fork's wire vocabulary; `None` when it was
    /// removed. This is the `WorktreeRemoval` -> [`crate::event::KeptReason`]
    /// half of the mapping table above.
    pub fn kept_reason(self) -> Option<crate::event::KeptReason> {
        match self {
            WorktreeRemoval::Removed => None,
            WorktreeRemoval::KeptDirty => Some(crate::event::KeptReason::Dirty),
            WorktreeRemoval::KeptCouldNotCheck => Some(crate::event::KeptReason::ProbeError),
            WorktreeRemoval::RemoveFailed => Some(crate::event::KeptReason::RemovalFailed),
            WorktreeRemoval::KeptIsolatedClone => Some(crate::event::KeptReason::IsolatedClone),
        }
    }

    /// The `BroadcastMsg::WorktreeKept` payload for this outcome (the fork's
    /// [`crate::event::WorktreeKeptNotice`], which stays the broadcast shape,
    /// ruling 48): `None` when the tree was removed, `error: None` always (see
    /// the type docs).
    pub fn kept_notice(self, worktree_dir: &Path) -> Option<crate::event::WorktreeKeptNotice> {
        self.kept_reason()
            .map(|reason| crate::event::WorktreeKeptNotice {
                path: worktree_dir.to_string_lossy().into_owned(),
                reason,
                error: None,
            })
    }
}

/// [`remove_worktree`], answering with the typed [`WorktreeRemoval`] and
/// attributed to [`CLOSE_VERB_REMOVER`]. Adds nothing to the removal itself:
/// the one implementation is [`remove_worktree`], so the `close` verb and the
/// `StopAgent` tab-close path cannot disagree about when a tree is kept.
pub async fn remove_worktree_outcome(
    worktree_dir: &Path,
    clone_dir: &Path,
    policy: RemovalPolicy,
) -> WorktreeRemoval {
    use crate::event::KeptReason;
    match remove_worktree(worktree_dir, clone_dir, policy, CLOSE_VERB_REMOVER).await {
        RemoveOutcome::Removed(_) => WorktreeRemoval::Removed,
        RemoveOutcome::Kept(KeptReason::Dirty) => WorktreeRemoval::KeptDirty,
        RemoveOutcome::Kept(KeptReason::ProbeError) => WorktreeRemoval::KeptCouldNotCheck,
        RemoveOutcome::Kept(KeptReason::RemovalFailed) | RemoveOutcome::RemoveFailed(_) => {
            WorktreeRemoval::RemoveFailed
        }
        RemoveOutcome::Kept(KeptReason::IsolatedClone) => WorktreeRemoval::KeptIsolatedClone,
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
    // Issue #171 — fail CLOSED, before any `gh`/`git` invocation at all: when
    // `cfg.repo_allowlist` is configured and `cfg.repo` is not a member,
    // refuse the entire run for this config entry. Deliberately the very
    // first check in this function, ahead of even `canonical_workspace` —
    // this must abort before `provision_repo`'s `gh repo clone`/`git fetch`
    // and before `list_open_issues`'s `gh issue list`, not merely before the
    // later claim writes, mirroring `derive_repo_slug`'s (`worktree_reclaim.rs`)
    // "refuse rather than guess" contract for a misconfigured/over-broad
    // target rather than letting a partial run touch the wrong repo.
    if let Some(allowlist) = &cfg.repo_allowlist
        && !allowlist.iter().any(|allowed| allowed == &cfg.repo)
    {
        let message = format!(
            "repo {:?} is not in the configured repo_allowlist {allowlist:?}; refusing to \
             dispatch or write (fail-closed, issue #171)",
            cfg.repo
        );
        tracing::warn!(repo = %cfg.repo, task = task_name, "issue-dispatch: {message}");
        notifier.notify(NotifyEvent::IssueDispatchRepoError {
            task: task_name.to_string(),
            repo: cfg.repo.clone(),
            message,
        });
        return;
    }

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
    ensure_claim_label(&cfg.repo, cfg.dry_run).await;

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
        ensure_labels(&cfg.repo, cfg.dry_run).await;
    }

    // S2 — `max_per_run` caps the issues CONSIDERED per run (not the number newly
    // dispatched): already-claimed issues inside the cap are skipped, yielding a
    // clean "≤ max_per_run concurrent in-flight" ceiling (PRD concurrency model —
    // today's run only picks up slots yesterday's run vacated).
    // PRD #421 M2.6(a) — the claim label is provisioned just in time: once per
    // run, plus at most one re-attempt when a later label add fails while it has
    // not succeeded, and only in a run that is about to write a claim, so a fire
    // that dispatches nothing provisions no label (#483). Such a fire can still
    // read claims, and edit this deck's own record from a worktree skip
    // (PRD #421 M3.1).
    let mut claim_run = ClaimRun::new(&local_hostname());

    for issue in issues.into_iter().take(cfg.max_per_run) {
        let issue_number = issue.number;
        let label_in_progress = issue.in_progress_label;
        loop {
            // M3.2 — per-issue error boundary: one failure never aborts the rest.
            match dispatch_one_issue(
                task_name,
                &workspace,
                prompt_template,
                cfg,
                default_command.as_deref(),
                issue_number,
                label_in_progress,
                &clone_dir,
                registry,
                worktrees,
                notifier,
                event_tx,
                state,
                login.as_deref(),
                &mut claim_run,
            )
            .await
            {
                Ok(IssueOutcome::Handled) => break,
                // The claims are written before the success is surfaced, so
                // "dispatched" is the issue's last word in this run: nothing
                // writes to GitHub for it after that line. The `in-progress`
                // claim (PRD #421 M1, rules 14/23) was already written inside
                // `dispatch_one_issue`; this is upstream's second claim system
                // (`agent-deck/claimed` label + claim record, ruling 36 keeps
                // both).
                Ok(IssueOutcome::Dispatched { claims_unchecked }) => {
                    claim_issue_record(task_name, cfg, issue_number, &mut claim_run, notifier)
                        .await;
                    // M1.3 — surface the per-issue dispatch success.
                    notifier.notify(NotifyEvent::IssueDispatched {
                        task: task_name.to_string(),
                        repo: cfg.repo.clone(),
                        issue: issue_number,
                        claims_unchecked,
                    });
                    break;
                }
                Err(message) => {
                    notifier.notify(NotifyEvent::IssueDispatchFailed {
                        task: task_name.to_string(),
                        repo: cfg.repo.clone(),
                        issue: issue_number,
                        message,
                    });
                    break;
                }
                // PRD #1487 re-check, reviewer R3: not reported as a failure.
                // Retried once the restart is called off; if it goes ahead, this
                // issue and the rest of the run are the successor's next fire.
                Ok(IssueOutcome::DeferredForRestart) => {
                    tracing::info!(
                        task = task_name,
                        issue = issue_number,
                        "issue dispatch deferred: the daemon is restarting"
                    );
                    if !registry.wait_for_admission().await {
                        tracing::info!(
                            task = task_name,
                            "the daemon is going down; the rest of this run is left to its successor"
                        );
                        return;
                    }
                }
            }
        }
    }
}

/// The `remover` ([`remove_worktree`]'s attribution string, issue #469) the
/// restart-deferral path of [`settle_failed_issue_spawn`] logs under.
const RESTART_DEFERRAL_REMOVER: &str = "issue-dispatch:restart-deferral";

/// What happens to an issue's freshly created worktree when its spawn fails.
///
/// PRD #1487 re-check, reviewer R3: a restart's admission freeze is not a
/// failure of this issue. A worktree left on disk would claim the issue on
/// every later fire with no agent in it — a permanent loss — so the fresh,
/// empty worktree is removed again and the issue deferred.
///
/// That removal can fail (a lock, permissions), and a failure must not read as
/// a clean deferral (final audit F2). So before removing it the worktree is
/// marked as abandoned by `creator` ([`crate::worktree_owner::mark_abandoned_spawn`]),
/// and a failed removal is reported as this issue's failure, naming the
/// directory. The mark is what lets the next attempt — this daemon's next
/// fire, or the successor's — retry the removal ([`reclaim_abandoned_spawn`])
/// instead of reading the leftover as the issue already being claimed. The
/// mark is written only here, for the worktree this attempt created and
/// recorded; a removal that succeeds deletes it with the worktree. Marking and
/// removing run under the per-repository worktree lock, so a reclaimer — which
/// removes only under that lock — never sees the mark of a removal still in
/// progress. When that lock cannot be taken (it timed out, or the clone's lock
/// path could not be resolved), nothing is removed: the worktree is only
/// marked, and the issue reports a failure the next attempt recovers from by
/// reclaiming it under the lock — so this removal can never overlap another
/// dispatch's worktree operation.
///
/// Any other failure: no agent will ever close to trigger cleanup, so the
/// registry entry is dropped here. The worktree dir itself is left on disk —
/// the next fire's worktree-exists idempotency signal reclaims the issue.
async fn settle_failed_issue_spawn(
    e: crate::spawn::SpawnError,
    worktrees: &WorktreeRegistry,
    worktree_dir: &Path,
    creator: Creator,
) -> Result<IssueOutcome, String> {
    if !matches!(e, crate::spawn::SpawnError::DaemonRestarting) {
        take_worktree(worktrees, worktree_dir);
        return Err(e.to_string());
    }
    // Only a worktree this attempt recorded is ours to remove.
    let Some(entry) = take_worktree(worktrees, worktree_dir) else {
        return Ok(IssueOutcome::DeferredForRestart);
    };
    let repo_lock = acquire_worktree_lock(&entry.clone_dir).await;
    let marked = {
        let dir = worktree_dir.to_path_buf();
        tokio::task::spawn_blocking(move || {
            crate::worktree_owner::mark_abandoned_spawn(&dir, &creator)
        })
        .await
        .unwrap_or_else(|join| Err(join.to_string()))
    };
    // No lock, no removal: marked only, it is left for a reclaimer, which
    // removes under the lock.
    if repo_lock.is_none() {
        return Err(match marked {
            Ok(_) => format!(
                "the daemon is restarting, so no agent was started, and the repository's \
                 worktree lock could not be taken to remove the issue's new worktree {} again; \
                 the next attempt retries the removal",
                worktree_dir.display()
            ),
            Err(why) => format!(
                "the daemon is restarting, so no agent was started, and the issue's new worktree \
                 {} could neither be removed again (the repository's worktree lock could not be \
                 taken) nor marked for a retry ({why}); remove it by hand to release the issue",
                worktree_dir.display()
            ),
        });
    }
    // Ruling 58 of the 2026-11-05 upstream merge: the fork's `remove_worktree`
    // answers with the richer `RemoveOutcome` (upstream's answered
    // `Option<KeptWorktree>`, `None` = removed). The recorded policy is kept
    // (`KeepIfDirty` for a dispatch, ruling 43), so a tree that somehow holds
    // work is left in place and reported as not removed, never forced.
    if matches!(
        remove_worktree(
            worktree_dir,
            &entry.clone_dir,
            entry.policy,
            RESTART_DEFERRAL_REMOVER,
        )
        .await,
        RemoveOutcome::Removed(_)
    ) {
        return Ok(IssueOutcome::DeferredForRestart);
    }
    Err(match marked {
        Ok(_) => format!(
            "the daemon is restarting, so no agent was started, and the issue's new worktree \
             {} could not be removed again; the next attempt retries the removal",
            worktree_dir.display()
        ),
        Err(why) => format!(
            "the daemon is restarting, so no agent was started, and the issue's new worktree \
             {} could not be removed again or marked for a retry ({why}); remove it by hand \
             to release the issue",
            worktree_dir.display()
        ),
    })
}

/// What [`reclaim_abandoned_spawn`] found at an issue's existing worktree.
#[derive(Debug, PartialEq, Eq)]
enum Reclaim {
    /// Not a worktree this task abandoned for this issue, or something is
    /// using it: the issue stays claimed.
    NotAbandoned,
    /// It was, and it has been removed — or another reclaimer removed it first:
    /// the caller looks again, and the issue can be dispatched if it is gone.
    Removed,
    /// It was, and removing it failed again, or the per-repository lock that
    /// guards the removal could not be taken: the issue's failure to report.
    Failed(String),
}

/// Retry the removal [`settle_failed_issue_spawn`] could not finish, before the
/// worktree-exists idempotency check reads the leftover as a claim (PRD #1487
/// final audit F2).
///
/// Removes nothing it cannot attribute to that failed spawn: the worktree must
/// carry the deck's ownership marker and an abandoned-spawn marker naming this
/// task and issue, must not be recorded by a live dispatch in `worktrees`, and
/// must have nothing in `agents` rooted in it — no live agent and no spawn
/// still starting there ([`AgentPtyRegistry::dir_in_use`], asked afresh each
/// time).
///
/// Those checks pass once without any lock, which is all most fires need: an
/// unmarked worktree costs no lock wait. The removal itself then runs under
/// the per-repository worktree lock [`create_worktree`] takes, and every check
/// is made again under it, immediately before `git worktree remove` — with the
/// abandoned-spawn generation required to be the exact one the first pass
/// read. So two reclaimers (an old daemon and its successor, or two decks) can
/// never both remove: the second finds the worktree gone, or re-created
/// without the mark, or abandoned again under a new generation, and leaves it
/// (PRD #1487, the F2 reclaim race). A lock that cannot be taken removes
/// nothing.
///
/// Issue #1612: the last agent check is not a read followed by a removal. It
/// takes a hold on the worktree in `agents` ([`AgentPtyRegistry::hold_dir_for_removal`])
/// in the same acquisition that finds nothing rooted there, and keeps it
/// through the removal; while it is held, every start this daemon is asked for
/// inside the worktree is refused. So an agent started in the abandoned
/// worktree is either seen by that check — its spawn is reserved before its
/// child exists, long before its record is published — and keeps the worktree,
/// or refused, and never has its directory removed under it.
async fn reclaim_abandoned_spawn(
    worktree_dir: &Path,
    clone_dir: &Path,
    creator: Creator,
    worktrees: &WorktreeRegistry,
    agents: &AgentPtyRegistry,
) -> Reclaim {
    reclaim_abandoned_spawn_paced(
        worktree_dir,
        clone_dir,
        creator,
        worktrees,
        agents,
        std::future::ready(()),
        std::future::ready(()),
    )
    .await
}

/// [`reclaim_abandoned_spawn`], awaiting `before_lock` between the unlocked
/// first pass and taking the lock — the window the regression tests hold open
/// to let a second reclaimer remove and re-create the worktree — and
/// `before_remove` between the last check and the removal, the window issue
/// #1612's regression test holds open to start an agent in the worktree.
async fn reclaim_abandoned_spawn_paced(
    worktree_dir: &Path,
    clone_dir: &Path,
    creator: Creator,
    worktrees: &WorktreeRegistry,
    agents: &AgentPtyRegistry,
    before_lock: impl std::future::Future<Output = ()>,
    before_remove: impl std::future::Future<Output = ()>,
) -> Reclaim {
    let Some(generation) =
        abandoned_generation_if_unused(worktree_dir, &creator, worktrees, agents).await
    else {
        return Reclaim::NotAbandoned;
    };
    before_lock.await;

    let Some(_repo_lock) = acquire_worktree_lock(clone_dir).await else {
        return Reclaim::Failed(format!(
            "the issue's worktree {} was left by a spawn the daemon refused while restarting, \
             and the repository's worktree lock that guards removing it could not be taken; \
             the next attempt retries",
            worktree_dir.display()
        ));
    };
    // `Path::exists` is a blocking `stat`; the same probe (any error reads as
    // absent) runs off the async workers (Qodo 4219656655).
    if tokio::fs::metadata(worktree_dir).await.is_err() {
        return Reclaim::Removed;
    }
    // Issue #1612: the final agent check, made as a hold that lasts until the
    // removal below has finished — see `reclaim_abandoned_spawn`.
    let Some(_removal_hold) = agents.hold_dir_for_removal(worktree_dir) else {
        return Reclaim::NotAbandoned;
    };
    if abandoned_generation_if_unused(worktree_dir, &creator, worktrees, agents).await
        != Some(generation)
    {
        return Reclaim::NotAbandoned;
    }
    before_remove.await;
    // Rulings 42/43 and 58 of the 2026-11-05 upstream merge: `KeepIfDirty`,
    // not upstream's `Force`. No agent ever ran in this tree, so a clean leftover
    // is removed exactly as upstream does; but a tree that holds uncommitted
    // work (somebody edited it between the refusal and this retry) is the
    // user's, and discarding it would be a data-loss change, not a rule-24
    // case 1-3. It is kept and reported, and the issue stays unclaimable until
    // the work is resolved, which is what every other kept dispatch worktree on
    // this fork does.
    match remove_worktree(
        worktree_dir,
        clone_dir,
        RemovalPolicy::KeepIfDirty,
        RESTART_DEFERRAL_REMOVER,
    )
    .await
    {
        RemoveOutcome::Removed(_) => {
            tracing::info!(
                worktree = %worktree_dir.display(),
                "issue-dispatch: removed a worktree left by a spawn refused during a restart"
            );
            Reclaim::Removed
        }
        RemoveOutcome::Kept(crate::event::KeptReason::Dirty) => Reclaim::Failed(format!(
            "the issue's worktree {} was left by a spawn the daemon refused while restarting, \
             and it now holds uncommitted changes, so it was kept; commit or discard them, or \
             remove the worktree by hand, to release the issue",
            worktree_dir.display()
        )),
        RemoveOutcome::Kept(_) | RemoveOutcome::RemoveFailed(_) => Reclaim::Failed(format!(
            "the issue's worktree {} was left by a spawn the daemon refused while restarting, \
             and removing it failed again; the next attempt retries",
            worktree_dir.display()
        )),
    }
}

/// The generation of `creator`'s abandoned-spawn mark on `worktree_dir`, unless
/// a live dispatch records the worktree or something in `agents` is rooted in
/// it.
async fn abandoned_generation_if_unused(
    worktree_dir: &Path,
    creator: &Creator,
    worktrees: &WorktreeRegistry,
    agents: &AgentPtyRegistry,
) -> Option<String> {
    let recorded = worktrees
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(worktree_dir);
    if recorded || agents.dir_in_use(worktree_dir) {
        return None;
    }
    let dir = worktree_dir.to_path_buf();
    let creator = creator.clone();
    tokio::task::spawn_blocking(move || {
        crate::worktree_owner::abandoned_spawn_generation(&dir, &creator)
    })
    .await
    .ok()
    .flatten()
}

/// What [`dispatch_one_issue`] did with an issue it did not fail on.
#[derive(Debug, Clone, PartialEq, Eq)]
enum IssueOutcome {
    /// Skipped as already claimed (the skip is surfaced there).
    Handled,
    /// An agent was spawned for the issue, so the caller claims it on GitHub
    /// (PRD #421 M2.2) — never on any other outcome — and surfaces the
    /// dispatch. `claims_unchecked` is why claim gating could not read the
    /// issue's claims, which the dispatch line reports.
    Dispatched { claims_unchecked: Option<String> },
    /// PRD #1487 re-check, reviewer R3: refused because the daemon is
    /// restarting. The issue's worktree was removed again, so the slot stays
    /// open for the next attempt — this daemon's, if the restart is called
    /// off, or the successor's next fire.
    DeferredForRestart,
}

/// Process one candidate issue. `Ok(Dispatched)` means an agent was spawned,
/// `Ok(Handled)` that it was skipped (a skip is surfaced here, not treated as
/// an error), and
/// `Ok(DeferredForRestart)` that the daemon refused the agent for now; `Err` is
/// a per-issue failure for the caller to surface through the notifier (M3.2).
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
    claim_run: &mut ClaimRun,
) -> Result<IssueOutcome, String> {
    let paths = derive_issue_paths(workspace, task_name, issue);

    let notify_skip = |cause: SkipCause| {
        notifier.notify(NotifyEvent::IssueDispatchSkipped {
            task: task_name.to_string(),
            repo: cfg.repo.clone(),
            issue,
            branch: paths.branch.clone(),
            cause,
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
    //
    // Except for a leftover this task abandoned for this issue when its spawn
    // was refused during a restart and the removal failed: that is retried
    // here first, and only a worktree that is still there afterwards claims
    // the issue (PRD #1487 final audit F2).
    // `Path::exists` is a blocking `stat`: `tokio::fs::metadata` is the same
    // probe (any error reads as absent) run off the async workers.
    let mut worktree_exists = tokio::fs::metadata(&paths.worktree_dir).await.is_ok();
    if worktree_exists {
        match reclaim_abandoned_spawn(
            &paths.worktree_dir,
            clone_dir,
            Creator::issue_dispatch(task_name, issue),
            worktrees,
            registry,
        )
        .await
        {
            Reclaim::NotAbandoned => {
                refresh_claim_on_worktree_skip(task_name, cfg, issue, claim_run).await;
                notify_skip(SkipCause::WorktreeExists);
                return Ok(IssueOutcome::Handled);
            }
            Reclaim::Failed(message) => return Err(message),
            Reclaim::Removed => {
                worktree_exists = tokio::fs::metadata(&paths.worktree_dir).await.is_ok()
            }
        }
        if worktree_exists {
            refresh_claim_on_worktree_skip(task_name, cfg, issue, claim_run).await;
            notify_skip(SkipCause::WorktreeExists);
            return Ok(IssueOutcome::Handled);
        }
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
        // Ruling 44: `SkipCause::LabelInProgress` carries the deck's own
        // claim-comment text (`ParsedClaim::raw`, rendered through
        // `sanitize_claimant_for_render`), `None` for a label a human or an
        // external tool applied.
        notify_skip(SkipCause::LabelInProgress(claimant.map(|c| c.raw)));
        return Ok(IssueOutcome::Handled);
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

    // QUATERNARY (PRD #421 M3.2, upstream's `claim_gating`, default OFF) —
    // reached only when neither local signal skipped, so a GitHub failure here
    // can never turn a clean skip into a failure: another deck's live claim.
    // Read only with gating in effect, and a claim that cannot be read
    // dispatches (fail toward a duplicate, never a wedge), which the dispatch
    // line then says. A READ only: a dry run (below) stays write-free.
    let mut claims_unchecked = None;
    let claimant = if !open_pr && cfg.claim.gating_in_effect() {
        match live_claim(task_name, &cfg.repo, issue, cfg.claim.ttl(), claim_run).await {
            Ok(claimant) => claimant,
            Err(error) => {
                tracing::warn!(
                    task = task_name,
                    repo = cfg.repo,
                    issue,
                    error = %error,
                    "issue-dispatch: the issue's claims could not be read; dispatching without them"
                );
                claims_unchecked = Some(error);
                None
            }
        }
    } else {
        None
    };
    // `label_in_progress` is known-false here (the label skip returned above),
    // so a `LabelInProgress` cause cannot come out of this call and there is no
    // claimant to fill into it; the other causes carry no payload.
    if let DispatchDecision::Skip(cause) =
        dispatch_decision(worktree_exists, open_pr, label_in_progress, claimant)
    {
        notify_skip(cause);
        return Ok(IssueOutcome::Handled);
    }

    // Issue #171 (auditor F2 fix, PR #753): a DRY RUN stops here, having made
    // exactly the same idempotency decision a real run would, WITHOUT doing
    // any of the work a real dispatch does — no worktree, no branch, no agent
    // spawn, and (as already true before this fix) no `gh` write. This is
    // deliberately BEFORE `create_worktree`, not merely before `claim_issue`
    // at the bottom of this function: `create_worktree` writes the exact same
    // `created-by:` marker and registers the worktree in `WorktreeRegistry` a
    // real dispatch would, so letting a dry run reach it would leave real
    // on-disk state behind. The PRIMARY idempotency signal above
    // (`worktree_exists`, "the worktree is the ledger") would then read that
    // dry-run-created worktree as "already claimed" on the very next REAL
    // fire and skip it — silently dropping the real claim comment/label/
    // assignee for good. Stopping before any disk/process side effect makes
    // that poisoning structurally impossible rather than merely avoided by
    // convention: a dry run can never leave anything for a later real run's
    // ledger to trip over, because it never writes to that ledger at all.
    if cfg.dry_run {
        notifier.notify(NotifyEvent::IssueDispatchDryRun {
            task: task_name.to_string(),
            repo: cfg.repo.clone(),
            issue,
        });
        return Ok(IssueOutcome::Handled);
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
        // It is still folded into the same `SkipCause::ConcurrentCreator`
        // skip as `AlreadyClaimed` rather than getting its own reason: a
        // genuine `TimedOut` here means OUR OWN add wedged and was killed,
        // not that a concurrent dispatch won a TOCTOU race, so the rendered
        // "a concurrent dispatch claimed the worktree first" mislabels the
        // cause in that one case. Known and left as-is per the
        // reviewer/auditor final pass (non-blocking, F1/A1): every later
        // fire still stops retrying the slug either way, `cleaned_up_by`
        // (when `Some`) still frees it for reuse, and a distinct `SkipCause` +
        // notification for this case is follow-up material rather than part
        // of this fix. The match stays exhaustive across all three so it
        // remains safe if any of them ever change again.
        WorktreeCreation::AlreadyClaimed
        | WorktreeCreation::BranchExists
        | WorktreeCreation::TimedOut { .. } => {
            notify_skip(SkipCause::ConcurrentCreator);
            return Ok(IssueOutcome::Handled);
        }
    }

    // M2.4 — record the worktree for tab-close cleanup NOW, before the spawn.
    // `spawn` registers the agent (visible to a `StopAgent`
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
    // `detach_delivery = true` has no effect since PRD #1666: the agent is
    // registered and its prompt posted to the inbox before `spawn` returns, and
    // nothing waits on the agent, so the scheduler's run-active window is freed
    // as soon as the dispatch WORK is done (PRD #120 B1 / dispatch/008: a
    // re-fire right after a tab close is not skipped behind a lingering
    // delivery wait). The worktree-on-disk idempotency signal still serializes
    // overlapping fires safely.
    // PRD #421 M2.2 — when triage is on, append the triage instruction to the
    // substituted prompt so the dispatched agent applies its own labels. Only
    // the issues actually dispatched here ever see it; a skipped issue never
    // reaches this point.
    // Issue #172 — `build_dispatch_prompt` also fires an audit record (issue,
    // repo, timestamp, the instruction template itself — never the issue's
    // own untrusted body/comments) every time it appends the triage
    // instruction, so every dispatch that grants it leaves a durable trace
    // behind regardless of what the agent then does with it.
    let prompt = build_dispatch_prompt(prompt_template, issue, &cfg.repo, cfg.triage);
    let req = SpawnRequest {
        task_name: task_name.to_string(),
        working_dir: paths.worktree_dir.to_string_lossy().into_owned(),
        command: default_command.map(str::to_string),
        prompt,
        // `None`: issue-dispatch keeps deriving the shape from the cloned repo's
        // own config, exactly as before the PRD #220 selector existed.
        resolved_target: None,
        // Unchanged behaviour: the prompt is delivered verbatim. Giving this path
        // the orchestrator context is #222's work, not this PR's — and when it is
        // done, the value that belongs here is `Unattended`: a scheduled per-issue
        // dispatch has no one at the pane either (issue #703).
        compose_orchestrator_context: None,
        // Fork #166 M2.4: the SAME string just written into the worktree's
        // `created-by:` marker above (`create_worktree`), not a second
        // derivation of it.
        owner: Some(creator),
        // PRD #1666 S5: labelled as the session's starting prompt.
        seed_reason: Some("seed".to_string()),
    };
    let handle = match spawn(req, registry, notifier, event_tx, true, state).await {
        Ok(h) => h,
        // No agent will ever close to trigger cleanup, so the registry entry is
        // dropped here (the worktree dir itself is left on disk: the next fire's
        // worktree-exists signal reclaims the issue), except that a spawn
        // refused during a daemon restart is deferred and its fresh worktree
        // removed again (PRD #1487, upstream's `settle_failed_issue_spawn`).
        Err(e) => {
            return settle_failed_issue_spawn(
                e,
                worktrees,
                &paths.worktree_dir,
                Creator::issue_dispatch(task_name, issue),
            )
            .await;
        }
    };

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
    // distinguishable event (`NotifyEvent::IssueInProgressClaimFailed`, ruling
    // 44; upstream's `IssueClaimFailed` is the OTHER claim system's event, see
    // [`claim_issue`]).
    //
    // Ruling 36 of the 2026-11-05 upstream merge: BOTH claim systems run on a
    // dispatch. This is the fork's `in-progress` claim (rules 14/23, written
    // here, before the success is surfaced); upstream's `agent-deck/claimed`
    // label + claim record is written by the caller on
    // `IssueOutcome::Dispatched` ([`claim_issue_record`]).
    //
    // `cfg.dry_run` is always `false` by the time control reaches here — the
    // dry-run early return above (auditor F2 fix) sends every `dry_run` fire
    // back to the caller long before `create_worktree`/`spawn`. Still passed
    // through rather than hardcoded to `false`: `claim_issue`'s own dry-run
    // gating is independently unit-tested and stays correct as a defense in
    // depth if this call site is ever reordered.
    claim_issue(
        &cfg.repo,
        issue,
        task_name,
        &identity,
        login,
        notifier,
        cfg.dry_run,
    )
    .await;

    // M1.3 — the caller claims the issue on GitHub (upstream's record) and then
    // surfaces the success.
    Ok(IssueOutcome::Dispatched { claims_unchecked })
}

/// PRD #421 M3.2 — the claimant whose record on `issue` is live, read from
/// GitHub now: `None` when the issue does not carry [`CLAIM_LABEL`]
/// (removing it by hand releases the claim) or no record on it is live. This
/// deck's own records count as dead here — the worktree and open-PR checks,
/// which skip before this runs, are their evidence — so a record gates here
/// only when it is foreign, written by the repository's owner, a member or a
/// collaborator, and edited within `ttl` of GitHub's clock ([`ClaimRun::now`]).
/// Each read draws on the run's claim budget, and
/// any failure, the budget running out included, is an `Err` the caller
/// dispatches on.
///
/// [`CLAIM_LABEL`]: crate::issue_dispatch::CLAIM_LABEL
async fn live_claim(
    task_name: &str,
    repo: &str,
    issue: u64,
    ttl: chrono::Duration,
    run: &mut ClaimRun,
) -> Result<Option<Claimant>, String> {
    let label = run.claim_label(repo, issue).await?;
    if !label.present {
        return Ok(None);
    }
    let records = run.claim_records(repo, issue).await?;
    if records.is_empty() {
        return Ok(None);
    }
    let me = crate::issue_dispatch::ClaimIdentity {
        user_id: run.account().await?,
        task: task_name,
        host_id: &run.host_id,
    };
    Ok(crate::issue_dispatch::live_claimant(
        &records,
        &me,
        false,
        label.added_at,
        run.now(),
        ttl,
    ))
}

/// PRD #421 M3.1 — the owner's refresh, taken from the skip its own worktree
/// causes: when this deck has a record on `issue` whose clock — its
/// `updated_at` on GitHub, floored by when the label was first added — is at
/// least half the TTL old, edit it with a new claim time, so the record stays
/// live for as long as the work does. Runs whether or not gating is on (the
/// switch turns off the gate, not the claim), never creates a record, and is
/// best-effort: a failure is logged and the issue is skipped all the same.
async fn refresh_claim_on_worktree_skip(
    task_name: &str,
    cfg: &IssueDispatchConfig,
    issue: u64,
    run: &mut ClaimRun,
) {
    // Issue #171 (fork): a DRY RUN never writes to GitHub, and editing this
    // deck's own claim record is a write. The worktree-exists skip reaches this
    // before the dry-run gate in `dispatch_one_issue`, so the gate is repeated
    // here rather than assumed.
    if cfg.dry_run {
        return;
    }
    match refresh_own_claim(task_name, &cfg.repo, issue, cfg.claim.ttl_hours, run).await {
        Ok(Some(comment_id)) => tracing::info!(
            task = task_name,
            repo = cfg.repo,
            issue,
            comment_id,
            "issue-dispatch: claim record refreshed"
        ),
        Ok(None) => {}
        Err(error) => tracing::warn!(
            task = task_name,
            repo = cfg.repo,
            issue,
            error = %error,
            "issue-dispatch: could not refresh the claim record"
        ),
    }
}

/// [`refresh_claim_on_worktree_skip`]'s work: `Ok(Some(id))` when the record
/// `id` was edited, `Ok(None)` when there is no own record or it is not due.
/// A record stating a lifetime other than `ttl_hours` is due whatever its age
/// ([`claim_record_ttl_outdated`]), and every edit writes `ttl_hours` into it.
///
/// [`claim_record_ttl_outdated`]: crate::issue_dispatch::claim_record_ttl_outdated
async fn refresh_own_claim(
    task_name: &str,
    repo: &str,
    issue: u64,
    ttl_hours: u32,
    run: &mut ClaimRun,
) -> Result<Option<u64>, String> {
    use crate::issue_dispatch::{
        ClaimIdentity, claim_record_ttl_outdated, claim_refresh_due, edit_claim_record_argv,
        next_claim_time, render_claim_record_with_ttl,
    };
    let ttl = chrono::Duration::hours(i64::from(ttl_hours));
    let records = run.claim_records(repo, issue).await?;
    if records.is_empty() {
        return Ok(None);
    }
    let user_id = run.account().await?;
    let me = ClaimIdentity {
        user_id,
        task: task_name,
        host_id: &run.host_id,
    };
    let Some(own) = records.into_iter().find(|found| found.is_own(&me)) else {
        return Ok(None);
    };
    let now = run.now();
    // The label floor can only make the record younger, so the events are
    // read only when the record's own clock already says a refresh is due.
    if !claim_record_ttl_outdated(&own.record, ttl_hours) {
        if !claim_refresh_due(now, own.updated_at, ttl) {
            return Ok(None);
        }
        let label = run.claim_label(repo, issue).await?;
        if !claim_refresh_due(now, own.updated_at.max(label.added_at), ttl) {
            return Ok(None);
        }
    }
    // A new claim time, so the edit changes the body: GitHub leaves
    // `updated_at` alone on an edit that repeats it.
    let body = render_claim_record_with_ttl(
        task_name,
        &run.host_id,
        next_claim_time(now, own.record.claimed_at),
        ttl_hours,
    );
    run.call(
        &edit_claim_record_argv(repo, own.comment_id, &body),
        "refreshing the claim comment",
        CLAIM_CALL_MAX_BYTES,
    )
    .await?;
    Ok(Some(own.comment_id))
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
/// `NotifyEvent::IssueInProgressClaimFailed`, distinguishable from both a successful
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
    // Issue #171: when true, none of this function's three writes actually
    // run `gh` — each is logged (what would have been sent) and skipped. The
    // removal-target READ just below is unaffected: a dry run still reports
    // what it would have done against genuine current state.
    dry_run: bool,
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
    if let Err(e) = run_gh_write(dry_run, &comment_argv, "post the claim comment", repo).await {
        notifier.notify(NotifyEvent::IssueInProgressClaimFailed {
            task: task_name.to_string(),
            repo: repo.to_string(),
            issue,
            message: format!("failed to post the claim comment: {e}"),
        });
    }

    let label_argv = issue_edit_add_label_argv(repo, issue, IN_PROGRESS_LABEL);
    if let Err(e) = run_gh_write(dry_run, &label_argv, "write the in-progress label", repo).await {
        notifier.notify(NotifyEvent::IssueInProgressClaimFailed {
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
        if let Err(e) = run_gh_write(dry_run, &assignee_argv, "write the assignee", repo).await {
            notifier.notify(NotifyEvent::IssueInProgressClaimFailed {
                task: task_name.to_string(),
                repo: repo.to_string(),
                issue,
                message: format!("failed to write the assignee: {e}"),
            });
        }
    }
}

/// Issue #171: execute one `gh` WRITE call — unless `dry_run`, in which case
/// log what WOULD have run (the full argv, so the log is actionable) and skip
/// execution, reporting `Ok(())` rather than attempting anything. A dry run
/// must never surface a write it never attempted as an
/// `IssueInProgressClaimFailed`/label-ensure warning, so this always succeeds.
async fn run_gh_write(
    dry_run: bool,
    argv: &[String],
    what: &str,
    repo: &str,
) -> Result<(), String> {
    if dry_run {
        tracing::info!(
            repo,
            what,
            argv = %argv.join(" "),
            "issue-dispatch: dry-run — would run `gh {}` to {what}, but performing no write",
            argv.join(" ")
        );
        return Ok(());
    }
    run_status_args("gh", argv).await
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
async fn ensure_claim_label(repo: &str, dry_run: bool) {
    let argv = label_create_argv(
        repo,
        IN_PROGRESS_LABEL,
        IN_PROGRESS_LABEL_COLOR,
        IN_PROGRESS_LABEL_DESCRIPTION,
    );
    if let Err(e) = run_gh_write(dry_run, &argv, "ensure the in-progress claim label", repo).await {
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
async fn ensure_labels(repo: &str, dry_run: bool) {
    for label in TRIAGE_LABELS.into_iter().chain(TYPE_LABELS) {
        let argv = label_create_argv(repo, label.name, label.color, label.description);
        if let Err(e) = run_gh_write(dry_run, &argv, "ensure a triage label", repo).await {
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
        verify_clone_origin(clone_dir, repo).await?;
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
    let placed = clone_via_staging(clone_dir, |staging| async move {
        run_status("gh", &["repo", "clone", repo, &staging.to_string_lossy()]).await
    })
    .await?;
    // A clone another attempt put there first is not ours, so it gets the same
    // L3 check a pre-existing directory does before anything uses it — two
    // decks sharing a workspace and task name need not share a repo (Qodo,
    // PR #1304).
    if placed == StagedClone::LostRace {
        verify_clone_origin(clone_dir, repo).await?;
    }
    // Same hygiene on the fresh clone, so it holds across the first AND every
    // later fire.
    ensure_worktrees_excluded(clone_dir);
    Ok(())
}

/// [`provision_isolated_clone_sync`]'s possible successes — deliberately its
/// OWN, narrower enum rather than reusing [`WorktreeCreation`]: that type
/// also carries `BranchExists`, which describes a `git worktree add` refusal
/// this function's `git checkout` step can't produce (it always attaches an
/// existing branch rather than refusing it, exactly like `create_worktree_sync`
/// does — see [`crate::issue_dispatch::isolated_clone_checkout_argv`]), and
/// giving this caller a type that can only ever express what can actually
/// happen is better than an exhaustive match padded with a "structurally
/// unreachable" arm (the pattern [`create_worktree_sync`]'s own caller
/// already accepts for `BranchExists`, but not one worth repeating here).
///
/// PRD fork#325 fix round (auditor A4): `TimedOut` was originally omitted on
/// the theory that a plain `git clone` can't produce the TOCTOU shape
/// `git worktree add` can — true for the ADD half, but false for the whole
/// function: `run_status_sync` bounds every git invocation here (the clone
/// itself, and the branch checkout) at [`WORKTREE_GIT_TIMEOUT`], and a
/// killed clone leaves a half-created `clone_dir` behind exactly like a
/// killed `git worktree add` does. `cleaned_up_by` carries the same meaning
/// as [`WorktreeCreation::TimedOut`]'s field of the same name.
///
/// PRD fork#325 fix round 2 (reviewer P1): this variant is now also what a
/// FAILED (not just timed-out) checkout produces, and what a `Failed` clone
/// with the directory present produces (P2-B — a `git clone` that already
/// created `clone_dir` before failing, e.g. "Clone succeeded, but checkout
/// failed", is not a concurrent claim by another actor the way a `git
/// worktree add` failure is; see `provision_isolated_clone_sync`'s doc
/// comment). Both routes are covered by [`handle_isolated_clone_add_error`],
/// which is what actually makes this doc comment's "the clone itself, and
/// the branch checkout" claim true — round 1 stated it before the checkout
/// step was wired to produce this variant at all.
///
/// PRD fork#325 fix round 3 (reviewer C1/C2, auditor C1/C2): round 2's
/// `Failed`-with-directory-present handling had two remaining defects, both
/// fixed by splitting this variant in two rather than continuing to collapse
/// every non-`AlreadyClaimed` failure into `TimedOut`. First (C2): every
/// genuine `Failed` outcome was reported to the user with the SAME
/// "isolated clone timed out … try again" wording `TimedOut` uses, discarding
/// git's actual captured error text — the real cause was visible only in the
/// log. Second (C1): `handle_isolated_clone_add_error` cleaned up (i.e.
/// `remove_dir_all`'d) `clone_dir` on ANY `Failed` outcome with the directory
/// present, on the theory that a concurrent claim through deck code is
/// impossible under the attach lock — true for deck code, but not for a
/// human running `git worktree add` manually into this exact destination
/// path during the window between this function's existence check and its
/// `git clone` invocation (the attach lock does not stop a process that
/// never takes it). `git clone` then fails with "destination path … already
/// exists", and `clone_dir` is the human's real worktree, not ours to
/// delete. `Failed` now carries the real error text and is reported/cleaned
/// up only when the directory is genuinely ours (see
/// `clone_destination_predates_attempt` and
/// [`handle_isolated_clone_add_error`]); a destination that predates this
/// attempt is reported as `AlreadyClaimed` instead, exactly as round 1 did,
/// leaving it untouched.
#[derive(Debug)]
pub(crate) enum IsolatedCloneOutcome {
    Created {
        marker_warning: Option<String>,
        origin_warning: Option<String>,
    },
    AlreadyClaimed,
    TimedOut {
        cleaned_up_by: Option<String>,
    },
    /// A genuine (non-timeout) `git clone`/`git checkout` failure where
    /// `clone_dir` is confirmed OURS (round 3, reviewer C1/C2) — `error` is
    /// git's own captured stderr, to be shown to the user verbatim rather
    /// than a generic message; `cleaned_up_by` mirrors `TimedOut`'s field of
    /// the same name.
    Failed {
        error: String,
        cleaned_up_by: Option<String>,
    },
    /// PRD fork#544 M3: `clone_dir` existed, and
    /// [`resume_existing_isolated_clone`]'s three-part eligibility check
    /// plus health probe all passed — reattached with a read-only `git
    /// fetch origin` (never a fast-forward/rebase/merge/checkout of a
    /// different ref) and otherwise zero git mutation. `fetch_warning` is
    /// `Some` only when that best-effort fetch itself failed (e.g. no
    /// `origin` configured); it never fails resumption itself.
    Resumed {
        fetch_warning: Option<String>,
    },
    /// PRD fork#544 M3: `clone_dir` existed but failed
    /// [`resume_existing_isolated_clone`]'s eligibility check — see
    /// [`ResumeRejection`] for which of the four distinguishable reasons.
    /// Never auto-deletes or auto-repairs the directory; refuses only.
    Rejected(ResumeRejection),
}

/// PRD fork#325 M3 sync twin, structurally mirroring [`provision_repo`]'s
/// clone-if-absent shape, for `src/ui.rs`'s `Action::SpawnPane` dispatch —
/// synchronous, exactly like [`create_worktree_sync`] is the sync twin of
/// [`create_worktree`] for the same reason (that dispatch runs on the TUI's
/// synchronous render/event loop and cannot `.await`).
///
/// Deliberate deviation from the design draft's suggestion to derive an
/// `owner/name` repo slug from `source_dir`'s `origin` remote and route
/// through `gh repo clone` (mirroring `provision_repo` exactly): verified
/// unreliable for precisely the case this gate exists to handle. The
/// `orchestration/worktree/014` e2e fixture this feature is pinned against —
/// an ordinary `git init`-only checkout, no `origin` configured at all — is
/// not a test artifact; it is representative of any local project a user
/// opens the deck against without ever adding a remote. Requiring an
/// `origin` would make the Nth-concurrent-orchestration gate refuse to
/// isolate exactly the ordinary case it exists to isolate. `source_dir` is
/// already a valid, present, local git repository by construction — it is
/// the SAME `req.dir` [`create_worktree_sync`] would otherwise `git
/// worktree add` against — so a plain LOCAL `git clone` needs no network
/// access, no `gh` auth, and no repo-identity derivation at all: it
/// produces an independent `.git` object store (this function's whole
/// purpose — `orchestration/worktree/014` asserts exactly that, a distinct
/// `git rev-parse --git-common-dir`), while still getting git's own
/// local-clone hardlink optimization for object storage for free.
///
/// `clone_dir` is the SAME resolved sibling path (`<launch-dir>-<slug>`)
/// [`create_worktree_sync`] would have targeted — not a separately-named
/// location — so the on-disk result is where the user's typed slug says it
/// should be regardless of which provisioning mechanism the gate picked; see
/// the PRD's Design step 5 ("where the isolated clone lives on disk" is
/// otherwise undecided by the PRD itself). A pre-existing directory at that
/// path is therefore treated exactly like [`create_worktree_sync`] treats
/// one — [`IsolatedCloneOutcome::AlreadyClaimed`], a refusal, never a silent
/// reuse/refresh — since the path was already meant to be fresh.
///
/// PRD fork#325 fix round (auditor A3): an [`worktree_attach_lock_path`]
/// attach lock IS now taken here, held for the whole function exactly the
/// way [`create_worktree_sync`] holds it — the doc paragraph this replaces
/// claimed one was unnecessary because `clone_dir` is "keyed off
/// `sanitize_clone_segment`-shaped identity upstream, at the call site",
/// which the audit verified is FALSE: `sanitize_clone_segment` has no call
/// site anywhere near `Action::SpawnPane`'s dispatch (its only two callers
/// are the scheduled issue-dispatch path); `clone_dir` here is a pure
/// function of the picked directory and the user's typed slug, so two racing
/// callers — two deck processes, or a deck plus a scheduled dispatch —
/// resolve the IDENTICAL destination, and `clone_dir.exists()` -> `git
/// clone` is a genuine TOCTOU (`git clone` creates the destination directory
/// before populating it). Locked via `worktree_attach_lock_path(source_dir,
/// clone_dir)` — the SAME two paths [`create_worktree_sync`] would be called
/// with for this identical resolved sibling path (see the comment at this
/// function's `src/ui.rs` call site: "Same resolved sibling PATH… only the
/// provisioning mechanism differs") — so this also closes the CROSS-
/// mechanism race the fix-round audit didn't name explicitly but the same
/// reasoning implies: a shared-checkout `create_worktree_sync` call and an
/// isolated-clone `provision_isolated_clone_sync` call racing for the same
/// target path now contend for the exact same lock file, not two unrelated
/// ones.
///
/// PRD fork#325 M4a (shipped, PR #510): the marker written here is what
/// [`crate::worktree_reclaim::discover_isolated_clones`]'s dedicated
/// sibling-directory scan now finds — [`crate::worktree_reclaim`]'s
/// ownership-marker scanning for LINKED worktrees still assumes every marked
/// directory is one of a known clone root, but an isolated clone is
/// discovered through that separate scan instead, not through this one.
/// M4c (shipped, PR #526) goes further still: a clone this scan finds can be
/// automatically reclaimed under `isolated_clone_report`'s five-condition,
/// `headRefOid`-based eligibility rule — see that function's own doc
/// comment and `docs/develop/shared-clone-architecture.md`.
///
/// PRD fork#325 fix round (reviewer P1-1, P1-2): two behaviors this function
/// used to leave unfinished, both now handled after the clone succeeds —
/// `branch`: `git clone` alone lands the clone on the SOURCE's HEAD branch,
/// never the slug the user typed, so a `git checkout` (attach-or-create, via
/// [`crate::issue_dispatch::isolated_clone_checkout_argv`]) follows,
/// matching what the shared-checkout arm gets from
/// [`crate::issue_dispatch::worktree_add_argv`]'s identical split; `origin`:
/// a plain local `git clone` sets `origin` to `source_dir`'s own local
/// filesystem PATH, not `source_dir`'s own `origin` URL — verified to make
/// `gh` unusable inside the clone and, worse, make `git push origin
/// HEAD:refs/heads/<branch>` (the exact form CLAUDE.md rule 1 mandates)
/// succeed SILENTLY into the user's own root checkout instead of reaching
/// GitHub. Fixed by reading `source_dir`'s own origin URL and pointing the
/// clone's `origin` at that URL instead. When `source_dir` itself has no
/// origin configured (the `orch-clone-gate` fixture's own shape — an
/// ordinary local project with no remote added) there is nothing better to
/// point at, so the clone's default local-path `origin` is REMOVED rather
/// than left in place: a later `git push origin` then fails loudly ("no such
/// remote") instead of silently landing in the source checkout, which is the
/// same risk either way and the whole reason this needed fixing at all.
///
/// PRD fork#325 fix round 2 (reviewer P1, P2-A, P2-B, P2-C): four follow-up
/// fixes to the above, one of which taught the others their exact ordering.
/// The origin fixup used to run entirely AFTER the checkout, so a checkout
/// failure left this variant's own hazard — the local-path `origin` — in
/// place with no cleanup at all (P1); the checkout now gets the same
/// timeout/failure cleanup the clone step already had, and a `Failed` clone
/// or checkout with `clone_dir` present is no longer misreported as
/// `AlreadyClaimed` (P2-B — see [`handle_isolated_clone_add_error`], which
/// now covers both steps). The origin fixup's own two `git remote` calls no
/// longer discard their result silently (P2-C — see
/// [`point_isolated_clone_origin`] / [`remove_isolated_clone_origin_default`]).
///
/// The SET-URL half of the origin fixup was moved earlier, before the
/// branch probe/checkout, exactly as P1 wants. The REMOVE half (source has
/// no origin) was deliberately NOT moved there too, even though it is "the
/// same fixup" conceptually: `git remote remove` deletes
/// `refs/remotes/origin/*`, not just the remote's config, and P2-A's
/// widened branch probe (below) needs exactly those refs to still be
/// present at probe/checkout time for a branch that only exists on the
/// source as a remote-tracking ref in this fresh clone. Moving the removal
/// early was tried and broke P2-A outright — a branch that exists only as
/// `refs/remotes/origin/<b>` again became unattachable, in precisely the
/// "no origin configured" shape the `orch-clone-gate` fixture represents —
/// caught by `provision_isolated_clone_sync_attaches_branch_that_exists_only_as_remote_tracking_ref`
/// failing in CI. So the removal runs where round 1 ran it, after a
/// successful checkout; only the set-url half moved.
///
/// PRD fork#325 fix round 3 (reviewer P3-1): the deferred-removal window
/// described above still left the plain `git clone` default local-path
/// `origin` in place for its whole duration when the source has no origin —
/// see [`ISOLATED_CLONE_NO_ORIGIN_SENTINEL`]'s doc comment for the fix.
///
/// Fork issue #595: resolves the git repository TOPLEVEL `dir` sits under,
/// plus `dir`'s own path relative to that toplevel (e.g. `baseline/intent`,
/// or empty when `dir` already IS the toplevel). Every ordinary git
/// subcommand discovers its repository by walking up from the working
/// directory, but `git clone <path>`/`git fetch <path>` treat `<path>` as a
/// transport target instead and require it to literally contain `.git` —
/// so launching an orchestration from a project directory that is a
/// SUBDIRECTORY of its repo (`.dot-agent-deck.toml` living at
/// `<repo>/sub/dir`) made `provision_isolated_clone_sync`'s `git clone`
/// fail instantly with "does not exist", even though `dir` is a perfectly
/// ordinary, valid place to run git commands from. Returns `None` when
/// `dir` is not inside a git repository at all — pre-existing, unaffected
/// behavior; callers fall back to `dir` itself, so the subsequent clone
/// attempt fails exactly as it always has.
///
/// Fork issue #595 fix round 2 (auditor F1): asks git for `--show-toplevel`
/// ALONE and derives the relative prefix in Rust via `strip_prefix`,
/// instead of also asking for `--show-prefix` on a second output line.
/// `git rev-parse` does not escape its output and a newline is a legal path
/// byte, so splitting two values apart by line let a directory name
/// containing a literal `\n` desynchronize the split: the toplevel read
/// short (pointing at a different, real repository) while the "prefix"
/// absorbed the rest of the real path — including, when that remainder
/// started with `/`, an ABSOLUTE path that made `PathBuf::join` below
/// discard the clone entirely and hand role panes a cwd outside it. A
/// single `git rev-parse --show-toplevel` invocation has no second value to
/// desynchronize against, so this class of ambiguity cannot recur.
pub(crate) fn resolve_git_toplevel(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let output = match run_capture_sync(
        &[
            "-C".to_string(),
            dir.to_string_lossy().into_owned(),
            "rev-parse".to_string(),
            "--show-toplevel".to_string(),
        ],
        WORKTREE_GIT_TIMEOUT,
    ) {
        Ok(output) => output,
        // Fork issue #595 reviewer F8: a probe timeout on a slow or stalled
        // filesystem used to collapse into the exact same `None` this
        // returns for "not in a git repository at all", with nothing
        // anywhere distinguishing the two. The caller's fallback (revert to
        // `dir` itself, so the subsequent clone fails exactly as it always
        // has) is correct either way, so this stays a `None` — only the log
        // line is new.
        Err(AddError::TimedOut(_)) => {
            tracing::warn!(
                dir = %sanitize_path_for_terminal_display(dir),
                "issue-dispatch: `git rev-parse --show-toplevel` timed out resolving the \
                 project's git toplevel; falling back to the picked directory as-is, as if it \
                 were not inside a git repository"
            );
            return None;
        }
        Err(AddError::Failed(_)) => return None,
    };
    let toplevel = output.strip_suffix('\n').unwrap_or(output.as_str());
    let toplevel = toplevel.strip_suffix('\r').unwrap_or(toplevel);
    if toplevel.is_empty() {
        return None;
    }
    let toplevel = PathBuf::from(toplevel);
    // `--show-toplevel` canonicalizes symlinks in its answer (e.g. macOS
    // `/tmp` -> `/private/tmp`) — canonicalize `dir` the same way before
    // stripping, or a symlinked picked directory spuriously fails to be
    // recognized as a descendant of `toplevel` and the prefix silently
    // comes back empty (reviewer F7).
    //
    // Canonicalize `toplevel` too, to the SAME representation, rather than
    // stripping a canonicalized `dir` against git's raw (uncanonicalized)
    // answer directly — caught in CI on Windows, where `Path::canonicalize`
    // returns the `\\?\`-prefixed verbatim form while git's own
    // `--show-toplevel` never does, so the two sides disagreed on their
    // root component and `strip_prefix` failed even for the ordinary,
    // no-symlinks-involved nested case, silently returning an empty
    // prefix. Both sides now go through the identical `.canonicalize()`
    // call, so whatever representation it produces on a given platform is
    // shared by both operands being compared.
    //
    // Falls back to the raw (as returned by git / as passed in) form on
    // either side when canonicalization itself fails (e.g. a dangling
    // symlink component); `strip_prefix` then simply may not match, which
    // is the same safe "no subpath" fallback as the not-inside-`toplevel`
    // case below.
    let canonical_toplevel = toplevel.canonicalize().unwrap_or_else(|_| toplevel.clone());
    let canonical_dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let prefix = canonical_dir
        .strip_prefix(&canonical_toplevel)
        .unwrap_or(Path::new(""))
        .to_path_buf();
    Some((toplevel, prefix))
}

// Fork issue #595 fix round 3 (reviewer N2): `dispatch.rs`, this wrapper's
// last production caller, now resolves `source_dir` itself up front (the
// same way `src/ui.rs`'s `Action::SpawnPane` already did) and calls
// [`provision_isolated_clone_sync_resolved`] directly, so this
// self-resolving wrapper has no production caller left at all — every
// remaining reference is a test in this module, `src/ui.rs`, or
// `src/worktree_reclaim.rs` reaching for the convenience of not having to
// resolve a toplevel by hand. `#[cfg(test)]` records that honestly rather
// than leaving a production-shaped function clippy's `dead_code` lint
// would otherwise (correctly) flag.
#[cfg(test)]
pub(crate) fn provision_isolated_clone_sync(
    source_dir: &Path,
    clone_dir: &Path,
    branch: &str,
    creator: &str,
) -> Result<IsolatedCloneOutcome, String> {
    // Fork issue #595: resolve the real repo toplevel ONCE, up front, and
    // use it everywhere `source_dir` would otherwise have been passed to a
    // path-based `git clone`/`git fetch` below — see `resolve_git_toplevel`'s
    // doc comment for why a nested project directory needs this at all.
    let resolved_source_dir = resolve_git_toplevel(source_dir)
        .map(|(toplevel, _prefix)| toplevel)
        .unwrap_or_else(|| source_dir.to_path_buf());
    // Fork issue #766 fix round 2 (audit A1/A2): this entry point's own
    // callers are all test fixtures that never claim to reproduce a real
    // orchestration pick's identity, so the legacy-creator-format fallback
    // stays structurally `Disabled` here — the same variant
    // `src/dispatch.rs`'s own call site now passes. A test that
    // deliberately wants to exercise the fallback uses
    // [`provision_isolated_clone_sync_with_legacy_fallback`] instead, which
    // supplies a real branch rather than silently inheriting one from this
    // wrapper's default.
    provision_isolated_clone_sync_resolved(
        &resolved_source_dir,
        clone_dir,
        branch,
        creator,
        LegacyFallbackEligibility::Disabled,
    )
}

/// Fork issue #766 fix round 2 test helper: identical to
/// [`provision_isolated_clone_sync`] except it enables the legacy-creator-
/// format fallback for a toplevel pick, supplying `branch` itself as the
/// caller's real identity ([`LegacyFallbackEligibility::ToplevelWithIdentity`])
/// — the same string a genuine `Action::SpawnPane` toplevel pick would pass.
/// Exists so a test that deliberately wants to exercise the fallback opts in
/// explicitly rather than relying on [`provision_isolated_clone_sync`]'s
/// default, which is `Disabled` precisely so ~30 unrelated existing tests
/// using that wrapper never accidentally reach it.
#[cfg(test)]
pub(crate) fn provision_isolated_clone_sync_with_legacy_fallback(
    source_dir: &Path,
    clone_dir: &Path,
    branch: &str,
    creator: &str,
) -> Result<IsolatedCloneOutcome, String> {
    let resolved_source_dir = resolve_git_toplevel(source_dir)
        .map(|(toplevel, _prefix)| toplevel)
        .unwrap_or_else(|| source_dir.to_path_buf());
    provision_isolated_clone_sync_resolved(
        &resolved_source_dir,
        clone_dir,
        branch,
        creator,
        LegacyFallbackEligibility::ToplevelWithIdentity(branch),
    )
}

/// Fork issue #766 fix round 2 (audit A1/A2 — replaces the original fix
/// round's bare `relative_subpath: Option<&Path>` parameter to
/// [`provision_isolated_clone_sync_resolved`]/[`resume_existing_isolated_clone`]).
/// The original parameter conflated two questions that turned out not to be
/// the same question at all: "is this pick nested under its git toplevel"
/// (a structural fact about the CURRENT pick, correctly gates out a nested
/// pick — fork issue #763's collision the digest-fronted creator format
/// exists to prevent) and "may this call accept a pre-fork#760 legacy
/// Name-derived `creator=` marker as a match" (a question about the
/// CALLER's own identity, which the bare boolean never carried at all —
/// audit A1: every legacy-shaped marker was accepted by every toplevel-
/// shaped caller, regardless of whether that caller's own Name/segment had
/// anything to do with the marker on disk). This enum keeps the two
/// questions answerable independently while making it impossible for a
/// caller to enable the fallback without also supplying the real identity
/// [`resume_existing_isolated_clone`] needs to check it against.
pub(crate) enum LegacyFallbackEligibility<'a> {
    /// The picked directory is nested under its git toplevel. The fallback
    /// (adopting/resuming the legacy marker) is never reachable regardless
    /// of anything else — the legacy format never carried subpath
    /// information to compare against, and widening it here would silently
    /// reopen the exact collision fork issue #763 closed.
    ///
    /// `own_segment` (PRD fork#777 fix round, reviewer M1 / auditor's
    /// `workspace_044` reasoning) is NOT an identity offered to the
    /// fallback — [`Self::identity_branch`] still returns `None` for this
    /// variant unconditionally, so a nested pick can never silently ADOPT a
    /// legacy marker, preserving fork issue #766 M3's safety property
    /// intact. It exists only so [`resume_existing_isolated_clone`] can
    /// detect the narrower, genuinely ambiguous sub-case — a legacy-shaped
    /// marker whose stored `name=` happens to equal THIS caller's own
    /// segment — and refuse it distinguishably
    /// ([`ResumeRejection::LegacyNestedIdentityUnverifiable`]) rather than
    /// silently treating it as an ordinary, unambiguous collision with a
    /// genuinely different pick (`ResumeRejection::NameCollision`, which
    /// `provision_isolated_clone_or_status` recovers from by disambiguating
    /// and creating a separate clone). See that variant's own doc comment
    /// for why the two cases must not be conflated: the ambiguous case
    /// might be this exact pick's own workspace from before today's
    /// subpath-aware marker format, in which case silently forking a new,
    /// empty clone would abandon the user's existing workspace — possibly
    /// holding uncommitted work — with nothing surfacing that it happened.
    Nested { own_segment: &'a str },
    /// The picked directory IS its own git toplevel (or isn't inside a git
    /// repository at all), and the caller supplies its own current
    /// branch/segment value for THIS pick — the exact string that would be
    /// written into the marker's `name=` field were this call instead
    /// creating a fresh clone right now (see `write_isolated_clone_provenance`'s
    /// `name` parameter). Only a genuine `Action::SpawnPane` toplevel pick
    /// (or a test that deliberately wants to exercise the fallback via
    /// [`provision_isolated_clone_sync_with_legacy_fallback`]) should ever
    /// construct this.
    ToplevelWithIdentity(&'a str),
    /// The picked directory IS a toplevel pick (or outside git entirely),
    /// but this caller's own creator namespace was never affected by fork
    /// issue #766 in the first place — `src/dispatch.rs`'s `dispatch:`-
    /// prefixed creator predates fork#760/#761's path-derived format change
    /// entirely, so the fallback can only ever widen acceptance for that
    /// path, never help it (audit A2). Structurally disabled rather than
    /// left reachable-but-unhelpful.
    Disabled,
}

impl LegacyFallbackEligibility<'_> {
    /// The caller-supplied identity to check the legacy marker's `name=`
    /// field against, when this variant offers one at all. `Nested` NEVER
    /// offers one here — see its own doc comment; that is the structural
    /// gate that keeps a nested pick from ever silently adopting a legacy
    /// marker.
    fn identity_branch(&self) -> Option<&str> {
        match self {
            Self::ToplevelWithIdentity(branch) => Some(branch),
            Self::Nested { .. } | Self::Disabled => None,
        }
    }

    /// PRD fork#777 fix round (reviewer M1): the segment THIS nested
    /// caller's own pick would produce, offered ONLY for detecting the
    /// specific ambiguous case described on [`Self::Nested`]'s own doc
    /// comment — never for adopting the marker (see [`Self::identity_branch`],
    /// unaffected by this method and still `None` for `Nested`).
    fn nested_own_segment(&self) -> Option<&str> {
        match self {
            Self::Nested { own_segment } => Some(own_segment),
            Self::ToplevelWithIdentity(_) | Self::Disabled => None,
        }
    }
}

/// Fork issue #595 fix round 2: same as [`provision_isolated_clone_sync`],
/// except `resolved_source_dir` is assumed ALREADY resolved to a real git
/// toplevel (or left as-is when it is not inside a git repository at all —
/// unaffected either way). See that function's doc comment for why a
/// second entry point exists.
///
/// `legacy_fallback` (fork issue #766, fix round 2): passed straight
/// through to [`resume_existing_isolated_clone`], which only ever widens
/// its stored-vs-computed `creator` comparison to accept the pre-fork#760
/// legacy Name-derived format for [`LegacyFallbackEligibility::ToplevelWithIdentity`].
/// See that function's own doc comment for why a nested pick, and a caller
/// with no real identity to offer, must never take that fallback.
pub(crate) fn provision_isolated_clone_sync_resolved(
    resolved_source_dir: &Path,
    clone_dir: &Path,
    branch: &str,
    creator: &str,
    legacy_fallback: LegacyFallbackEligibility<'_>,
) -> Result<IsolatedCloneOutcome, String> {
    ensure_worktree_parent_dir(clone_dir)?;

    let lock_path = worktree_attach_lock_path(resolved_source_dir, clone_dir)
        .map_err(|e| format!("failed to resolve isolated-clone attach lock path: {e}"))?;
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
                    "failed to acquire isolated-clone attach lock {}: {e}",
                    lock_path.display()
                )
            })?;

    if clone_dir.exists() {
        // PRD fork#544 M3: a present directory is no longer an unconditional
        // refusal — `resume_existing_isolated_clone` runs the three-part
        // eligibility check plus health probe and either resumes it or
        // reports a distinguishable rejection reason. This REPLACES the
        // flat `AlreadyClaimed` this branch used to return; it does not run
        // alongside it.
        return resume_existing_isolated_clone(
            resolved_source_dir,
            clone_dir,
            creator,
            legacy_fallback,
        );
    }

    // Issue #325 auditor A2: `--` end-of-options separator before both
    // paths, matching `worktree_remove_argv`'s "issue #325 auditor A7"
    // precedent — not reachable today (both paths are deck-derived: an
    // absolute picked directory and an allowlisted slug), but `git clone`'s
    // option surface (`-u/--upload-pack=<cmd>`, `--template=<dir>`, `-c
    // <k>=<v>`) is dramatically more dangerous than `worktree remove`'s, so
    // the same cheap defense belongs here at least as much.
    let clone_result = run_status_sync(
        &[
            "clone".to_string(),
            // Issue #325 reviewer P3-2 (fix round 3): pin the remote's name
            // to `origin` explicitly rather than trusting git's default — a
            // `clone.defaultRemoteName` config (system- or user-level;
            // verified with `git -c clone.defaultRemoteName=upstream clone
            // …`) silently renames it, which defeats
            // `point_isolated_clone_origin`, `remove_isolated_clone_origin_default`,
            // and the P2-A remote-tracking probe all at once — every one of
            // them hardcodes the literal `origin` — reinstating P1-1's
            // dangerous local-path remote under a different name the rest
            // of this function never looks at.
            "--origin".to_string(),
            "origin".to_string(),
            "--".to_string(),
            resolved_source_dir.to_string_lossy().into_owned(),
            clone_dir.to_string_lossy().into_owned(),
        ],
        WORKTREE_GIT_TIMEOUT,
    );
    if let Err(err) = clone_result {
        return handle_isolated_clone_add_error(err, clone_dir, creator);
    }

    // Fork#325 M4b (reviewer P1 / auditor C1): write the isolated-clone-
    // specific provenance artifact now — the `clone_dir.exists()` check
    // above already ruled out a pre-planted directory (auditor C1's
    // residual: a call that hits `AlreadyClaimed` returns before this line
    // and never writes it), and the `git clone` immediately above just
    // succeeded, so `clone_dir` genuinely is a fresh clone this call itself
    // created. Deliberately a separate location/namespace from the
    // attach-lock file above, which `create_worktree_sync` also writes into
    // for an ordinary linked worktree (reviewer P1's residual: that shared
    // namespace let a forged occupant of a since-removed linked worktree's
    // path inherit its leftover lock) — see
    // `ISOLATED_CLONE_PROVENANCE_FILENAME`'s doc comment in
    // `worktree_reclaim.rs` for the full reasoning. Best-effort: a write
    // failure here does not fail the whole provisioning call, since the
    // clone itself is already fully usable — it only means this clone will
    // never report `owned: true` from `worktree list`.
    if let Err(e) = write_isolated_clone_provenance(resolved_source_dir, clone_dir, branch, creator)
    {
        tracing::warn!(
            clone = %clone_dir.display(),
            error = %e,
            "issue-dispatch: could not write isolated-clone provenance artifact; this clone \
             will never report owned: true from `worktree list`"
        );
    }

    // Issue #325 reviewer P1 (fix round 2): read the source's own origin URL
    // now — a pure, side-effect-free read — and, when the source HAS an
    // origin, point the clone's `origin` at it IMMEDIATELY, before the
    // branch probe and the checkout. Round 1 ran the whole origin fixup
    // AFTER the checkout, so a checkout failure (or a WORKTREE_GIT_TIMEOUT
    // kill mid-checkout) left a fully populated, valid-looking clone, on the
    // source's HEAD branch, with the dangerous local-path `origin` a plain
    // `git clone` sets up still in place — P1-1's exact hazard, reinstated
    // one step later in this same function. Doing the set-url this early
    // means no later failure in this function can leave that behind, and
    // (P2-A, below) `set-url` never touches `refs/remotes/origin/*`, so
    // moving it here is free.
    //
    // When the source has NO origin, the fixup is a REMOVAL rather than a
    // repoint — and `git remote remove` deletes `refs/remotes/origin/*`
    // along with it (verified: it is not merely a config change). Removing
    // it THIS early would destroy the very `refs/remotes/origin/<branch>`
    // ref the widened probe below (and `git checkout`'s own DWIM attach)
    // need for a branch that exists on the source only as a remote-tracking
    // ref in this fresh clone — silently reintroducing P2-A while fixing
    // P1. So for this branch alone, the FINAL removal is deferred until
    // AFTER a successful checkout (see the two-part `let source_origin`
    // handling below). A checkout failure in between still can't leave the
    // dangerous default behind in practice: it now routes through
    // `handle_isolated_clone_add_error`, which removes the WHOLE directory
    // (config and all) rather than leaving it partially fixed up — the
    // residual risk is only that `remove_dir_all` itself fails, the same
    // low-probability case P3-E already accepts for this path.
    //
    // PRD fork#325 fix round 3 (reviewer P3-1): the deferral above still
    // left a window — up to two `WORKTREE_GIT_TIMEOUT`-bounded subprocesses,
    // and unbounded if this process dies before reaching the removal — where
    // the clone's `origin` was the plain `git clone` default local-path
    // remote, P1-1's exact hazard. Rather than leave that in place for the
    // whole window, this branch now points `origin` at an unpushable
    // SENTINEL immediately (`ISOLATED_CLONE_NO_ORIGIN_SENTINEL`) using the
    // same `point_isolated_clone_origin` helper the has-origin branch uses.
    // `set-url` never touches `refs/remotes/origin/*` (verified), so this is
    // free with respect to the P2-A probe and the checkout below — the real
    // `remote remove` still runs after a successful checkout, removing the
    // sentinel and the refs together, exactly as before.
    //
    // `git remote get-url`/`set-url`/`remove` are cheap local metadata reads
    // with no network I/O — the same class of call `git_common_dir` already
    // runs unbounded on this synchronous path (fork issue #388). PRD
    // fork#544 review-findings fix round: `read_source_origin_url` is now
    // routed through `run_capture_sync` (the same hardened helper every
    // other git subprocess in this module uses) rather than a raw
    // `std::process::Command`, so it does carry `WORKTREE_GIT_TIMEOUT` now
    // — cheap enough not to matter, and no longer a bare, unbounded spawn.
    let source_origin = read_source_origin_url(resolved_source_dir);
    let mut origin_warning = match source_origin.as_deref() {
        Some(url) => point_isolated_clone_origin(clone_dir, url),
        None => point_isolated_clone_origin(clone_dir, ISOLATED_CLONE_NO_ORIGIN_SENTINEL),
    };

    // Issue #325 reviewer P2-A: `worktree_branch_probe_argv` alone (probing
    // only `refs/heads/<branch>`) is correct for the shared-checkout arm,
    // whose `clone_dir` only ever grows LOCAL branches via earlier `git
    // worktree add -b` calls — but wrong here. A FRESH `git clone` gives
    // `refs/heads/` only the source's checked-out HEAD branch; every other
    // branch the source had arrives as `refs/remotes/origin/<b>` only. So
    // the plain probe always answered ABSENT for a real existing branch,
    // and the code ran `git checkout -b <branch>`, silently creating a NEW
    // branch at the clone's HEAD instead of attaching to the real one —
    // reproduced empirically by the reviewer, discarding committed work.
    // `isolated_clone_remote_branch_probe_argv` covers the second location.
    // These refs are intact at this point in BOTH origin-fixup branches: the
    // set-url case never touched them, and the remove case is deferred past
    // this point (see above).
    //
    // Fix round 3 (reviewer P2-1 / auditor C3): round 2 folded BOTH probes
    // succeeding into one `branch_exists` bool, relying on git's own
    // checkout DWIM to correctly attach a remote-only match — a dependency
    // the reviewer found unsafe (see `BranchLocation::RemoteOnly`'s doc
    // comment). The two probes are now kept apart as a `BranchLocation` so
    // `isolated_clone_checkout_argv` can give the remote-only case its own,
    // DWIM-free checkout form.
    //
    // Fork#325 M4c fix round: read the clone's pre-checkout default branch
    // NOW, before either probe or the checkout itself ever moves `HEAD` —
    // [`remove_stray_default_branch`] below needs this name to clean up the
    // leftover ref checkout leaves behind in the `Absent`/`RemoteOnly`
    // cases.
    let default_branch = resolve_clone_default_branch(clone_dir);
    let branch_location = if run_status_sync(
        &crate::issue_dispatch::worktree_branch_probe_argv(clone_dir, branch),
        WORKTREE_GIT_TIMEOUT,
    )
    .is_ok()
    {
        crate::issue_dispatch::BranchLocation::Local
    } else if run_status_sync(
        &crate::issue_dispatch::isolated_clone_remote_branch_probe_argv(clone_dir, branch),
        WORKTREE_GIT_TIMEOUT,
    )
    .is_ok()
    {
        crate::issue_dispatch::BranchLocation::RemoteOnly
    } else {
        crate::issue_dispatch::BranchLocation::Absent
    };
    let checkout_result = run_status_sync(
        &crate::issue_dispatch::isolated_clone_checkout_argv(clone_dir, branch, branch_location),
        WORKTREE_GIT_TIMEOUT,
    );
    if let Err(err) = checkout_result {
        // Issue #325 reviewer P1: give the checkout the same
        // cleanup-and-recovery treatment the clone step already has (via
        // `handle_isolated_clone_add_error`), rather than the plain `Err`
        // return round 1 left it with — that returned before this point
        // with no cleanup. It also permanently wedged the slug: every retry
        // saw `clone_dir.exists()` and reported "clone already exists",
        // naming no recovery command.
        return handle_isolated_clone_add_error(err, clone_dir, creator);
    }

    // Fork#325 M4c fix round: `Local` means `branch` WAS the clone's
    // pre-checkout default branch — nothing to clean up, and
    // `default_branch.as_deref() == Some(branch)` guards the same case a
    // second time in case `Local` was ever reachable for some other reason
    // (belt-and-suspenders: `git branch -d` on the currently checked-out
    // branch fails loudly anyway, but there is no reason to invoke it at
    // all when there is nothing stray to remove).
    if !matches!(
        branch_location,
        crate::issue_dispatch::BranchLocation::Local
    ) && let Some(stray) = default_branch.as_deref()
        && stray != branch
    {
        remove_stray_default_branch(clone_dir, stray);
    }

    if source_origin.is_none() {
        // Only now, after checkout has succeeded and nothing downstream
        // still needs `refs/remotes/origin/*`, remove the clone's default
        // local-path origin (reviewer P1-1's other branch — see the long
        // comment above for why this is deferred rather than run
        // immediately after the clone).
        origin_warning = remove_isolated_clone_origin_default(clone_dir);
    } else {
        // PRD fork#544 M2 Decisions table: a plain local `git clone` only
        // ever contains whatever `source_dir`'s own refs happened to hold
        // at clone time — it is not, on its own, guaranteed fresh against
        // the real `origin/main`. Now that `origin` is pointed at the
        // source's real URL (the `if` branch above only fires when there
        // was none to point at), fetch it once, updating only this NEW
        // clone's own `origin/*` remote-tracking refs — the source/root
        // checkout is never touched. Best-effort and read-only: a failure
        // here (offline, auth) does not fail provisioning, since the clone
        // is already fully usable from its local-clone snapshot; it only
        // means this workspace starts from whatever `main` looked like at
        // clone time instead of the true HEAD.
        if let Err(err) = run_status_sync(
            &[
                "-C".to_string(),
                clone_dir.to_string_lossy().into_owned(),
                "fetch".to_string(),
                "origin".to_string(),
            ],
            WORKTREE_GIT_TIMEOUT,
        ) {
            let e = match err {
                AddError::TimedOut(e) | AddError::Failed(e) => e,
            };
            tracing::warn!(
                clone = %clone_dir.display(),
                error = %e,
                "issue-dispatch: could not fetch origin for freshness after isolated clone; \
                 workspace starts from the clone-time snapshot instead of the true HEAD"
            );
        }
    }

    let marker_warning = mark_worktree_owned_best_effort(clone_dir, creator);
    // PRD fork#325 fix round 3 (reviewer C0 / auditor P2-3): round 2 folded
    // `origin_warning` and `marker_warning` into a single string and reported
    // it through the `Created::marker_warning` field, which `ui.rs` renders
    // via `format_marker_warning` — a fixed template written for the
    // ownership-marker failure ("the ownership marker for X could not be
    // written … a later `reclaim` of it will need `--yes`"). Applied to an
    // origin-fixup failure, that template names the wrong hazard entirely:
    // the actual risk is that the clone still points `origin` at the user's
    // OWN root checkout, so `git push origin` from inside it lands there
    // silently instead of reaching the real remote. Keeping the two warnings
    // in separate fields lets the caller (`ui.rs`) render each with its own,
    // accurate message instead of forcing both through one template built
    // for only one of them.
    Ok(IsolatedCloneOutcome::Created {
        marker_warning,
        origin_warning,
    })
}

/// Shared failure handling for [`provision_isolated_clone_sync`]'s `git
/// clone` and `git checkout` steps (PRD fork#325 fix round 2, reviewer
/// P1/P2-B): both steps can leave `clone_dir` present after a failure —
/// `git clone`'s own `Clone succeeded, but checkout failed` case, a
/// `WORKTREE_GIT_TIMEOUT` kill mid-checkout, or a killed clone — and in
/// every one of those cases `clone_dir`'s presence is (usually) OURS, not
/// evidence that another actor claimed the slug: unlike `git worktree add`,
/// `clone_dir.exists()` is checked INSIDE this function's attach lock,
/// immediately before the clone runs (see the lock acquisition above), so a
/// concurrent claim through any deck code path is impossible by
/// construction here. Reporting every such case as `AlreadyClaimed` (round
/// 1's treatment of a non-timeout `Failed`, borrowed from
/// `classify_worktree_add_result`'s reasoning for `git worktree add`, which
/// does not transfer) discarded the real git error and wedged the slug
/// permanently.
///
/// PRD fork#325 fix round 3 (reviewer C1/C2, auditor C1/C2): round 2's fix —
/// treat every `Failed`-with-directory-present case exactly like `TimedOut`,
/// clean it up and report a generic "timed out" message — went too far in
/// two ways this round corrects.
///
/// First (C2): the generic "isolated clone timed out … try again" wording
/// was shown for EVERY `Failed` outcome, not just genuine timeouts,
/// discarding git's actual error text (visible only in this function's own
/// `tracing::warn!` log). `Failed` is now its own [`IsolatedCloneOutcome`]
/// variant carrying that text verbatim, so the caller can show the user what
/// git actually said. `TimedOut`'s wording is reserved for a genuine
/// [`AddError::TimedOut`].
///
/// Second, and more serious (C1): "`clone_dir.exists()` is checked inside
/// the attach lock" only rules out a concurrent claim through OTHER DECK
/// code — it says nothing about a human running `git worktree add
/// ../<repo>-<slug>` manually (the exact convention CLAUDE.md rule 18 and
/// `/worktree-prd` document) into this precise destination path during the
/// window between that existence check and this function's own `git clone`
/// call; that command takes no lock here at all. `git clone` then refuses
/// with "destination path … already exists and is not an empty directory" —
/// git created NOTHING — and the directory is the human's real worktree, not
/// a half-created clone of ours. [`attempt_isolated_clone_cleanup`]'s
/// `.git`-presence guard (P3-E) does not catch this: `git worktree add`
/// writes `.git` as a plain FILE, which still satisfies a bare existence
/// check. `clone_destination_predates_attempt` recognizes this shape from
/// the error text `run_status_sync` already captured and routes it to
/// `AlreadyClaimed` — round 1's non-destructive treatment — leaving the
/// directory untouched instead of deleting it. Every OTHER `Failed`/
/// `TimedOut` case (a killed clone, "Clone succeeded, but checkout failed",
/// a killed checkout) still means `clone_dir` is ours, and gets the
/// cleanup-and-recovery treatment as before.
fn handle_isolated_clone_add_error(
    err: AddError,
    clone_dir: &Path,
    creator: &str,
) -> Result<IsolatedCloneOutcome, String> {
    let (e, is_timeout) = match err {
        AddError::TimedOut(e) => (e, true),
        AddError::Failed(e) => (e, false),
    };
    if !clone_dir.exists() {
        return Err(e);
    }
    if !is_timeout && clone_destination_predates_attempt(&e) {
        tracing::warn!(
            clone = %clone_dir.display(),
            error = %e,
            "issue-dispatch: isolated clone destination already existed before this attempt \
             (not ours to create); leaving it untouched and reporting AlreadyClaimed"
        );
        return Ok(IsolatedCloneOutcome::AlreadyClaimed);
    }
    tracing::warn!(
        clone = %clone_dir.display(),
        error = %e,
        "issue-dispatch: isolated clone left a partially-populated directory behind; cleaning up"
    );
    let cleaned_up_by = attempt_isolated_clone_cleanup(clone_dir, creator);
    if is_timeout {
        Ok(IsolatedCloneOutcome::TimedOut { cleaned_up_by })
    } else {
        Ok(IsolatedCloneOutcome::Failed {
            error: e,
            cleaned_up_by,
        })
    }
}

/// Whether `error_text` — `git clone`'s own captured stderr — indicates the
/// destination directory PREDATES this clone attempt (git refused to write
/// into an already-existing, non-empty directory) rather than one this
/// attempt itself half-created before failing (PRD fork#325 fix round 3,
/// reviewer C1 / auditor C1). Git's own message for the first case is
/// stable and distinctive: `fatal: destination path '<dir>' already exists
/// and is not an empty directory.` — matched on the two substrings rather
/// than the whole sentence so a quoted path doesn't defeat it. Only this
/// case must never be `remove_dir_all`'d — see
/// [`handle_isolated_clone_add_error`]'s doc comment for why (a manual `git
/// worktree add` racing into the same path is exactly this shape, and
/// deleting it would be strictly worse than round 1's non-destructive
/// `AlreadyClaimed`).
///
/// PRD fork#325 fix round 4 (auditor D1): a *locale*-shifted message DOES
/// defeat this — git's translation catalogs (installed for German, French,
/// and others on a stock Debian/Ubuntu `git` package) replace the whole
/// sentence, and neither translated wording contains either English
/// substring, so this predicate returned `false` (the destructive answer)
/// for a non-English user hitting exactly the collision it exists to
/// recognize. This is intentionally NOT fixed here by widening the match to
/// more translations — that only ever covers the languages someone thought
/// to add. It is fixed one layer up, in [`spawn_git_status_child`]'s
/// `.env("LC_ALL", "C")`: that pins every git child this predicate's input
/// can come from to the untranslated English locale, so matching hardcoded
/// English substrings here is safe again. If a caller is ever added that
/// feeds this predicate text from a `git` invocation NOT spawned through
/// `spawn_git_status_child`, that caller inherits this same locale hazard
/// and needs the same override.
fn clone_destination_predates_attempt(error_text: &str) -> bool {
    error_text.contains("destination path") && error_text.contains("already exists")
}

/// Read `source_dir`'s own `origin` URL, if it has one — a pure,
/// side-effect-free lookup, safe to call at any point in
/// [`provision_isolated_clone_sync`] regardless of clone/checkout state
/// (PRD fork#325 fix round 2, reviewer P1-1). `None` covers both "no
/// `origin` remote configured" and "configured but empty", which
/// [`provision_isolated_clone_sync`] treats identically: nothing better to
/// point the clone's `origin` at.
fn read_source_origin_url(source_dir: &Path) -> Option<String> {
    run_capture_sync(
        &[
            "-C".to_string(),
            source_dir.to_string_lossy().into_owned(),
            "remote".to_string(),
            "get-url".to_string(),
            "origin".to_string(),
        ],
        WORKTREE_GIT_TIMEOUT,
    )
    .ok()
    .map(|s| s.trim().to_string())
    .filter(|url| !url.is_empty())
}

/// Unpushable placeholder [`point_isolated_clone_origin`] points a fresh
/// clone's `origin` at immediately when `source_dir` itself has no origin
/// configured (PRD fork#325 fix round 3, reviewer P3-1) — closing the
/// residual window between the clone and the deferred
/// [`remove_isolated_clone_origin_default`] call after checkout, during
/// which a plain `git clone`'s own dangerous local-path default (P1-1's
/// exact hazard) would otherwise sit in place for up to two
/// `WORKTREE_GIT_TIMEOUT`-bounded subprocesses, or indefinitely if this
/// process dies mid-window. Deliberately not a real host or path: `git push
/// origin` against it fails LOUDLY ("does not appear to be a git
/// repository") instead of landing anywhere, the same safety property the
/// eventual `remote remove` gives, just available one step earlier.
///
/// PRD fork#325 fix round 4 (auditor D3): that safety property held only
/// contingently. The original value, `dot-agent-deck-no-origin-configured`,
/// carries no URL scheme, so git resolves it as a RELATIVE LOCAL PATH
/// against the invoking directory rather than an unreachable remote —
/// verified: `git push origin` against it fails loudly only while nothing
/// exists at that relative path, and SILENTLY SUCCEEDS into a bare repo
/// created there (`git init --bare
/// dot-agent-deck-no-origin-configured`). The window this sentinel exists to
/// cover is precisely the one where such a repo could persist (this process
/// dying before the deferred real removal runs), so the failure mode is
/// exactly backwards from what the doc comment above promises: silent
/// success into an unintended nearby repo instead of a loud, obvious
/// failure. Fixed by giving it a scheme no git transport implements —
/// `dot-agent-deck://…` — which fails unconditionally regardless of what
/// exists on disk (verified: `remote helper 'dot-agent-deck' aborted
/// session`), making the doc comment's claim true rather than contingent.
const ISOLATED_CLONE_NO_ORIGIN_SENTINEL: &str = "dot-agent-deck://no-origin-configured";

/// Point a freshly cloned `clone_dir`'s `origin` at `url` — either
/// `source_dir`'s own origin URL, or (fix round 3, reviewer P3-1)
/// [`ISOLATED_CLONE_NO_ORIGIN_SENTINEL`] when the source has none — instead
/// of the local filesystem path `git clone` defaults it to (reviewer P1-1).
/// Never touches `refs/remotes/origin/*` (only the remote's config entry),
/// so it is safe to call before the branch probe/checkout — see
/// `provision_isolated_clone_sync`'s doc comment for why that placement
/// matters and why the eventual real removal
/// ([`remove_isolated_clone_origin_default`]) still cannot run at the same
/// point.
///
/// PRD fork#325 fix round 2 (reviewer P2-C): this call used to discard its
/// result with `let _ = …` — if it failed, the clone silently kept the
/// dangerous local-path `origin`, P1-1's exact condition, with no record
/// anywhere. Returns `Some(warning)` on failure instead, so the caller can
/// surface it — as its own, distinctly-rendered `origin_warning` field
/// (fix round 3, reviewer C0/P2-3; see [`IsolatedCloneOutcome::Created`]),
/// never folded into the ownership-marker warning
/// [`mark_worktree_owned_best_effort`] uses. The `--` end-of-options
/// separator (matching auditor A2's hardening of the `git clone` argv)
/// guards against a source origin URL beginning with `-` being read as an
/// option.
fn point_isolated_clone_origin(clone_dir: &Path, url: &str) -> Option<String> {
    match crate::git_env::git_at(clone_dir)
        .args(["remote", "set-url", "--", "origin", url])
        .output()
    {
        Ok(out) if out.status.success() => None,
        Ok(out) => Some(format!(
            "failed to point the clone's origin at the source's own origin ({url}): {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(e) => Some(format!(
            "failed to point the clone's origin at the source's own origin ({url}): {e}"
        )),
    }
}

/// Remove `clone_dir`'s default local-path `origin` entirely — the OTHER
/// branch of reviewer P1-1, when `source_dir` itself has no origin
/// configured and there is nothing better to point at — rather than leave a
/// push able to land silently back in the source checkout.
///
/// Unlike [`point_isolated_clone_origin`], `git remote remove` deletes
/// `refs/remotes/origin/*` along with the remote's config (verified
/// empirically, not merely a config change) — so
/// `provision_isolated_clone_sync` deliberately calls this only AFTER a
/// successful checkout, once nothing downstream still needs those refs
/// (reviewer P2-A's widened branch probe, and `git checkout`'s own DWIM
/// attach, both depend on them for a branch that exists on the source only
/// as a remote-tracking ref in a fresh clone). See that function's doc
/// comment for the full reasoning. `remove` takes no attacker-influenced
/// argument, so unlike `set-url` it needs no `--` hardening.
///
/// PRD fork#325 fix round 2 (reviewer P2-C): as with the set-url branch,
/// this used to discard its result with `let _ = …`; returns
/// `Some(warning)` on failure instead.
fn remove_isolated_clone_origin_default(clone_dir: &Path) -> Option<String> {
    match crate::git_env::git_at(clone_dir)
        .args(["remote", "remove", "origin"])
        .output()
    {
        Ok(out) if out.status.success() => None,
        Ok(out) => Some(format!(
            "failed to remove the clone's default local-path origin: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(e) => Some(format!(
            "failed to remove the clone's default local-path origin: {e}"
        )),
    }
}

/// The branch a fresh `git clone` checked out by default — the source
/// repo's own current HEAD branch — via `git symbolic-ref --short -q HEAD`,
/// read BEFORE the checkout below ever moves `clone_dir`'s `HEAD` off of it.
/// `None` on any spawn/exit failure or empty output (e.g. a detached-HEAD
/// source), mirroring [`read_source_origin_url`]'s fail-open-to-`None`
/// shape: an unresolvable default branch just means
/// [`remove_stray_default_branch`] below has nothing it can safely name to
/// clean up, never a hard failure of provisioning itself.
fn resolve_clone_default_branch(clone_dir: &Path) -> Option<String> {
    let out = crate::git_env::git_at(clone_dir)
        .args(["symbolic-ref", "--short", "-q", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if name.is_empty() { None } else { Some(name) }
}

/// Delete `clone_dir`'s leftover default branch once the checkout onto the
/// dispatched `branch` has moved `HEAD` off of it (fork#325 M4c fix round).
///
/// A fresh `git clone` checks out the source's own default branch
/// (typically `main`) as a genuine local `refs/heads/` ref.
/// `isolated_clone_checkout_argv`'s `Absent` (`checkout -b <branch>`) and
/// `RemoteOnly` (`checkout -b <branch> --track origin/<branch>`) forms both
/// move `HEAD` onto the dispatched work branch WITHOUT touching that
/// original ref — so it stayed behind as a second, permanent local branch,
/// which meant `isolated_clone_report`'s tightened `single_local_branch`
/// gate (exactly one local branch: the resolved current one) could never be
/// satisfied by ANY real dispatched clone, not merely the deliberately
/// broken fixtures the tightened rule's own negative tests construct —
/// `worktree_reclaim_062`/`068` are the only two fixtures that exercise
/// every gate positively, and both caught this: reclaim eligibility was
/// unreachable for a genuinely matching clone before this cleanup existed.
///
/// Safe to delete with a plain `-d` (never `-D`): at the point this runs,
/// the leftover branch's tip is exactly the commit the dispatched branch
/// was just cut from (or fast-forwarded from, in the `RemoteOnly` case) —
/// no dispatched work has been committed yet — so it is by construction
/// fully merged into `HEAD` and `-d` cannot refuse it. Best-effort, and
/// deliberately never propagated as a hard failure of provisioning: a
/// failure here (a `branch.<name>` config `git branch -d` itself refuses to
/// touch, a `pre-branch`-style hook, or any other local misconfiguration)
/// just means this clone keeps an extra branch and stays on the
/// permanently-conservative `isolated_clone` verdict on the next `reclaim`
/// sweep — the exact same safe fallback an unresolvable gate anywhere else
/// in the tightened rule already produces.
fn remove_stray_default_branch(clone_dir: &Path, original_branch: &str) {
    let out = crate::git_env::git_at(clone_dir)
        .args(["branch", "-d", "--", original_branch])
        .output();
    match out {
        Ok(o) if o.status.success() => {}
        Ok(o) => tracing::warn!(
            clone = %clone_dir.display(),
            branch = %original_branch,
            stderr = %String::from_utf8_lossy(&o.stderr).trim(),
            "issue-dispatch: could not delete the isolated clone's leftover default branch; \
             it will keep reporting more than one local branch to `worktree list`/`reclaim`"
        ),
        Err(e) => tracing::warn!(
            clone = %clone_dir.display(),
            branch = %original_branch,
            error = %e,
            "issue-dispatch: could not delete the isolated clone's leftover default branch; \
             it will keep reporting more than one local branch to `worktree list`/`reclaim`"
        ),
    }
}

/// Best-effort cleanup for an isolated `git clone` killed by
/// [`provision_isolated_clone_sync`]'s [`WORKTREE_GIT_TIMEOUT`] bound —
/// mirrors [`attempt_worktree_cleanup`]'s reasoning exactly (fork #122/#123
/// P2, extended to this path by issue #325's fix round, auditor A4): a
/// killed clone leaves a half-created directory behind that would otherwise
/// wedge this slug permanently, since every later attempt sees the directory
/// present and reports `AlreadyClaimed`. Unlike `attempt_worktree_cleanup`,
/// `clone_dir` is NOT a linked worktree of any repo here — it is its own
/// independent clone — so cleanup is a plain recursive directory removal,
/// never `git worktree remove`.
///
/// "Confirmed" means the same thing it means for the worktree twin: removal
/// is reported only when the directory is actually gone afterward, so the
/// caller can tell the user either "try again" or name the manual `rm -rf`
/// command, rather than assuming success it cannot back up.
///
/// Deliberately unbounded, unlike `attempt_worktree_cleanup`'s
/// [`WORKTREE_CLEANUP_TIMEOUT`]-bounded `git worktree remove`: there is no
/// `git` subprocess here to bound, only a plain recursive filesystem walk,
/// and `std::fs::remove_dir_all` has no timeout knob to give it short of
/// spawning a thread to race — not worth the complexity for a best-effort
/// cleanup whose failure already degrades gracefully to a named manual
/// command.
pub(crate) fn attempt_isolated_clone_cleanup(clone_dir: &Path, remover: &str) -> Option<String> {
    // Issue #325 reviewer P3-E (widened by fix round 2, auditor R5): a
    // one-line guard that makes this destructive, unconditional
    // `remove_dir_all` self-evidently safe to a future reader — this
    // function is only ever called on a path we just created via `git
    // clone`, under this function's own attach lock, so `.git` is always
    // present as a DIRECTORY in practice; the check costs nothing and means
    // a future caller mistake (a wrong path passed in) removes nothing
    // instead of silently deleting an unrelated directory. Deliberately
    // `is_dir()`, not `exists()`: a linked worktree's `.git` is a FILE (it
    // points back at the main repo's `.git/worktrees/<name>`), so a bare
    // existence check would wave one through — `is_dir()` is strictly
    // stronger and correct for every path this function's two callers can
    // produce (plain `git clone`, never `--separate-git-dir`).
    if !clone_dir.join(".git").is_dir() {
        return None;
    }
    let removed = std::fs::remove_dir_all(clone_dir).is_ok();
    // Auditor A3 (PRD fork#544 review-findings fix round): the M4b
    // provenance artifact lives OUTSIDE `clone_dir` entirely (under
    // `state_dir()`, keyed by `clone_dir`'s canonical path), so removing
    // `clone_dir` above never touches it -- a checkout failure here left it
    // orphaned, "evidence" for a directory that no longer exists, which
    // `resume_existing_isolated_clone`'s Stranger gate (evidence PRESENCE
    // alone) would then wrongly vouch for on a later attempt at this same
    // canonical path. Best-effort, same as the directory removal above: a
    // failure here doesn't change whether this cleanup is reported as
    // successful, since the directory itself (the thing a caller actually
    // interacts with) is what `remover`'s presence in the return value
    // promises was removed.
    let _ = std::fs::remove_file(isolated_clone_provenance_path(clone_dir));
    if removed && !clone_dir.exists() {
        Some(remover.to_string())
    } else {
        None
    }
}

/// L3 (fail-closed): the check a clone directory this fire did not create must
/// pass before it is used — a readable `origin` consistent with `repo`.
async fn verify_clone_origin(clone_dir: &Path, repo: &str) -> Result<(), String> {
    let clone = clone_dir.to_string_lossy();
    let origin = run_git_capture(&["-C", &clone, "remote", "get-url", "origin"])
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
    Ok(())
}

/// How [`clone_via_staging`] ended when it did not fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StagedClone {
    /// This call's clone is now at `clone_dir`.
    Placed,
    /// Another attempt put a directory at `clone_dir` first; this call's copy
    /// was discarded, and the caller must vet what is there before using it.
    LostRace,
}

/// Clone into a staging sibling of `clone_dir`, and move it into place only
/// once the clone has succeeded.
///
/// A clone that stops partway — killed at [`SUBPROCESS_TIMEOUT`] (issue #692),
/// or cut short by the daemon itself dying — would otherwise leave `clone_dir`
/// on disk, and every later fire takes an existing directory for a finished
/// clone: it checks the origin, tries a refresh that can only fail, and never
/// clones again, so the schedule could not recover without someone deleting the
/// directory by hand. Staged, `clone_dir` exists only as a complete clone.
/// Raised by Greptile on PR #1304.
///
/// **The staging name is unique to this call** (process id plus a
/// per-process counter), so two attempts at the same clone — two tasks sharing
/// a workspace, or two daemons — never clear or rename each other's work in
/// progress (raised by Qodo on PR #1304). The cost is that a staging directory
/// orphaned by a daemon that died mid-clone is not reclaimed automatically; it
/// is inert, because nothing reads a `.cloning-*` sibling as a clone. If
/// another attempt finished first, `rename` onto its non-empty directory fails;
/// this attempt's copy is discarded and [`StagedClone::LostRace`] tells the
/// caller that what now sits at `clone_dir` was put there by somebody else.
///
/// The staging directory is a sibling so the final `rename` stays on one
/// filesystem and is atomic. Moving a fresh clone is safe because it has no
/// linked worktrees yet (`.worktrees/` is created later), and a linked worktree
/// is what would record the clone's absolute path. The filesystem calls go
/// through `tokio::fs`, which runs them off the async workers, because removing
/// a partial clone walks every file it has.
async fn clone_via_staging<F, Fut>(clone_dir: &Path, clone: F) -> Result<StagedClone, String>
where
    F: FnOnce(PathBuf) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    static ATTEMPT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let name = clone_dir
        .file_name()
        .ok_or_else(|| format!("clone dir {} has no file name", clone_dir.display()))?;
    let mut staging_name = name.to_os_string();
    staging_name.push(format!(
        ".cloning-{}-{}",
        std::process::id(),
        ATTEMPT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let staging = clone_dir.with_file_name(staging_name);
    let discard = |path: PathBuf| async move {
        match tokio::fs::remove_dir_all(&path).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(
                staging = %path.display(),
                error = %e,
                "issue-dispatch: could not remove a partial clone's staging directory"
            ),
        }
    };
    if let Err(e) = clone(staging.clone()).await {
        discard(staging).await;
        return Err(e);
    }
    match tokio::fs::rename(&staging, clone_dir).await {
        Ok(()) => Ok(StagedClone::Placed),
        Err(e) => {
            if tokio::fs::metadata(clone_dir)
                .await
                .is_ok_and(|meta| meta.is_dir())
            {
                discard(staging).await;
                return Ok(StagedClone::LostRace);
            }
            // Discarded here too, because every call stages under a new name:
            // a rename that keeps failing would otherwise leave one complete
            // copy of the repository per fire (Qodo, PR #1304).
            let err = format!(
                "cloned into {} but could not move it to {}: {e}",
                staging.display(),
                clone_dir.display()
            );
            discard(staging).await;
            Err(err)
        }
    }
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
    run_git_status(&["-C", clone, "fetch"]).await?;
    let pulled = run_git_status(&["-C", clone, "pull", "--ff-only"]).await;
    // Auditor A-06 (upstream sync): the bounded run's process-group kill can cut
    // a fast-forward mid-checkout and leave `.git/index.lock`, after which every
    // later `pull` fails "non-fatally" and dispatch keeps cutting worktrees from
    // a stale HEAD. Name the lock, so the caller's warn says why.
    let lock = Path::new(clone).join(".git").join("index.lock");
    match pulled {
        Err(e) if lock.exists() => Err(format!(
            "{e} ({} exists: a git run in this clone was killed mid-checkout; remove it once \
             no git process is using the clone)",
            lock.display()
        )),
        other => other,
    }
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

/// The default bound on ONE `gh` call of the claim (PRD #421 amendment item 9),
/// independent of [`SUBPROCESS_TIMEOUT`]. A claim call is an API request, small
/// next to a clone, and the claim runs between one issue's dispatch and
/// the next issue's, so a hung call here holds up every issue after it: the
/// bound is what keeps a stuck claim from delaying the rest of the run by
/// more than a few of these.
const CLAIM_CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Overrides [`CLAIM_CALL_TIMEOUT`], in milliseconds — what lets a test drive
/// the timeout without a 30-second wait. A value that is not a positive
/// integer is ignored.
const CLAIM_CALL_TIMEOUT_ENV: &str = "DOT_AGENT_DECK_ISSUE_CLAIM_TIMEOUT_MS";

/// The most `gh api --paginate` may print for one issue's comments, or its
/// events, before the call is killed (PRD #421 amendment item 10). Far above
/// what a typical issue's comments print, and low enough that a runaway
/// listing's captured output stays bounded in the daemon's memory.
const CLAIM_COMMENTS_MAX_BYTES: usize = 8 * 1024 * 1024;

/// The most any other claim call may print on stdout before it is killed. What
/// those calls print is one small JSON object (the account, the written
/// comment), a line, or nothing, so this is far above any real answer.
const CLAIM_CALL_MAX_BYTES: usize = 256 * 1024;

/// How much of a claim call's stderr is kept for its diagnostic; the rest is
/// read and discarded (see [`OutputCaps::stderr`]). The diagnostic built from it
/// is cut much shorter still ([`crate::issue_dispatch::sanitize_claim_error`]),
/// so this only bounds what the daemon holds while the call runs.
const CLAIM_STDERR_MAX_BYTES: usize = 64 * 1024;

/// The default total time all the claim calls of one run may take together
/// (PRD #421 amendment item 12). [`CLAIM_CALL_TIMEOUT`] alone bounds each call,
/// not their number, so a run of many issues against a GitHub that hangs every
/// call would hold the scheduler for that bound times the calls; this caps it.
const CLAIM_RUN_BUDGET: Duration = Duration::from_secs(120);

/// Overrides [`CLAIM_RUN_BUDGET`], in milliseconds, for the same reason as
/// [`CLAIM_CALL_TIMEOUT_ENV`]. A value that is not a positive integer is
/// ignored.
const CLAIM_RUN_BUDGET_ENV: &str = "DOT_AGENT_DECK_ISSUE_CLAIM_BUDGET_MS";

/// What a claim step that ran out of [`ClaimBudget`] reports.
const CLAIM_BUDGET_SPENT: &str = "the run's claim time budget was used up";

/// A positive number of milliseconds, or `None` for anything else.
fn positive_millis(value: Option<&str>) -> Option<Duration> {
    value
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|&ms| ms > 0)
        .map(Duration::from_millis)
}

/// [`CLAIM_CALL_TIMEOUT`], or the [`CLAIM_CALL_TIMEOUT_ENV`] override when it
/// holds a positive number of milliseconds.
fn claim_call_timeout_from(value: Option<&str>) -> Duration {
    positive_millis(value).unwrap_or(CLAIM_CALL_TIMEOUT)
}

/// [`CLAIM_RUN_BUDGET`], or the [`CLAIM_RUN_BUDGET_ENV`] override when it holds
/// a positive number of milliseconds.
fn claim_run_budget_from(value: Option<&str>) -> Duration {
    positive_millis(value).unwrap_or(CLAIM_RUN_BUDGET)
}

/// The time the claim calls of one run share (PRD #421 amendment item 12):
/// each call is bounded by the per-call bound or by what is left, whichever is
/// shorter, and spends the time it actually took. Time between calls — the
/// dispatching itself — spends none of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClaimBudget {
    per_call: Duration,
    remaining: Duration,
}

impl ClaimBudget {
    /// The bound for the next call, or `None` once the budget is spent and no
    /// further claim call may be made.
    fn next_bound(&self) -> Option<Duration> {
        (!self.remaining.is_zero()).then(|| self.per_call.min(self.remaining))
    }

    fn spend(&mut self, elapsed: Duration) {
        self.remaining = self.remaining.saturating_sub(elapsed);
    }

    fn is_spent(&self) -> bool {
        self.remaining.is_zero()
    }
}

/// This machine's clock `local_now` moved onto GitHub's by `offset` (GitHub's
/// clock minus this machine's, measured once per run), or left as it is when
/// no offset was measured.
fn clock_now(
    local_now: chrono::DateTime<chrono::Utc>,
    offset: Option<chrono::Duration>,
) -> chrono::DateTime<chrono::Utc> {
    offset.map_or(local_now, |offset| local_now + offset)
}

/// What the claims of one run share: the machine id the records name, the
/// time budget every claim call draws from, and two lookups — the label
/// provisioning (PRD #421 M2.6(a)) and the signed-in account (amendment item
/// 6), which also yields the offset between GitHub's clock and this machine's
/// (PRD #421 M3.1). Each is made lazily, on the first claim call that needs
/// it, so a run that neither dispatches nor reads or refreshes a claim makes
/// no claim call. Provisioning is attempted
/// once per run, plus at most one re-attempt when a later label add fails while
/// provisioning has not succeeded ([`ClaimRun::should_reprovision`], amendment
/// item 14); its outcome otherwise stands for the rest of the run. Only a
/// successful account lookup is cached, so a failed one is retried by the next
/// claim that needs a record (amendment item 13).
struct ClaimRun {
    host_id: String,
    budget: ClaimBudget,
    provisioned: Option<Result<(), String>>,
    reprovisioned: bool,
    account: Option<u64>,
    /// GitHub's clock minus this machine's, from the account lookup's `Date`;
    /// `None` until a lookup carried a readable one, and then the local clock
    /// stands in.
    clock_offset: Option<chrono::Duration>,
}

impl ClaimRun {
    fn new(hostname: &str) -> Self {
        Self::with_budget(
            hostname,
            ClaimBudget {
                per_call: claim_call_timeout_from(
                    std::env::var(CLAIM_CALL_TIMEOUT_ENV).ok().as_deref(),
                ),
                remaining: claim_run_budget_from(
                    std::env::var(CLAIM_RUN_BUDGET_ENV).ok().as_deref(),
                ),
            },
        )
    }

    fn with_budget(hostname: &str, budget: ClaimBudget) -> Self {
        Self {
            host_id: crate::issue_dispatch::claim_host_id(hostname),
            budget,
            provisioned: None,
            reprovisioned: false,
            account: None,
            clock_offset: None,
        }
    }

    /// Whether a failed label add is worth provisioning the label once more and
    /// retrying the add (amendment item 14): only while provisioning has not
    /// succeeded in this run, so the label may be missing, only once per run,
    /// and only with budget left to make the calls.
    fn should_reprovision(&self) -> bool {
        !self.reprovisioned && !matches!(self.provisioned, Some(Ok(()))) && !self.budget.is_spent()
    }

    /// Keep the account a lookup resolved; a failed lookup leaves nothing
    /// behind, so the next claim that needs a record looks it up again.
    fn remember_account(&mut self, lookup: &Result<u64, String>) {
        if let Ok(id) = lookup {
            self.account = Some(*id);
        }
    }

    /// The id of the account `gh` is signed in with, looked up on first need
    /// and kept for the rest of the run once a lookup succeeds. The lookup that
    /// succeeds also sets the run's clock ([`ClaimRun::now`]) from GitHub's
    /// `Date`, logging once when it has none.
    async fn account(&mut self) -> Result<u64, String> {
        use crate::issue_dispatch::{
            authenticated_user_argv, parse_authenticated_user, sanitize_claim_error,
        };
        if let Some(id) = self.account {
            return Ok(id);
        }
        let lookup = self
            .call(
                &authenticated_user_argv(),
                "looking up the signed-in GitHub account",
                CLAIM_CALL_MAX_BYTES,
            )
            .await
            .and_then(|output| {
                parse_authenticated_user(&output).map_err(|e| sanitize_claim_error(&e))
            });
        let local_now = chrono::Utc::now();
        if let Ok(user) = &lookup {
            self.clock_offset = user
                .server_date
                .map(|server| server.signed_duration_since(local_now));
            if self.clock_offset.is_none() {
                tracing::warn!(
                    "issue-dispatch: GitHub's answer to the account lookup carried no readable \
                     Date header; claim ages in this run use this machine's clock"
                );
            }
        }
        let lookup = lookup.map(|user| user.id);
        self.remember_account(&lookup);
        lookup
    }

    /// Now on GitHub's clock — this machine's clock moved by the offset the
    /// account lookup measured — which is what every claim record's times are
    /// on. This machine's clock when no offset was measured.
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        clock_now(chrono::Utc::now(), self.clock_offset)
    }

    /// Every claim record on `issue`, whoever wrote it (PRD #421 M3.2).
    async fn claim_records(
        &mut self,
        repo: &str,
        issue: u64,
    ) -> Result<Vec<crate::issue_dispatch::IssueClaimRecord>, String> {
        let comments = self
            .call(
                &crate::issue_dispatch::issue_comments_argv(repo, issue),
                "reading the issue's comments",
                CLAIM_COMMENTS_MAX_BYTES,
            )
            .await?;
        crate::issue_dispatch::parse_issue_claim_records(&comments)
            .map_err(|e| crate::issue_dispatch::sanitize_claim_error(&e))
    }

    /// Whether `issue` carries the claim label and when it was first added,
    /// from its events (PRD #421 M3.1).
    async fn claim_label(
        &mut self,
        repo: &str,
        issue: u64,
    ) -> Result<crate::issue_dispatch::ClaimLabelState, String> {
        let events = self
            .call(
                &crate::issue_dispatch::issue_events_argv(repo, issue),
                "reading the issue's events",
                CLAIM_COMMENTS_MAX_BYTES,
            )
            .await?;
        crate::issue_dispatch::parse_claim_label_state(&events)
            .map_err(|e| crate::issue_dispatch::sanitize_claim_error(&e))
    }

    /// One claim call within the run's budget ([`gh_claim_call`]), or the
    /// step reported as skipped when the budget is already spent. A call that
    /// fails as it spends the last of the budget says so as well, since a
    /// timeout shortened by the budget is not the per-call bound's.
    async fn call(
        &mut self,
        argv: &[String],
        what: &str,
        max_stdout: usize,
    ) -> Result<String, String> {
        let Some(bound) = self.budget.next_bound() else {
            return Err(format!("{what} was skipped: {CLAIM_BUDGET_SPENT}"));
        };
        let started = std::time::Instant::now();
        let result = gh_claim_call(argv, what, bound, max_stdout).await;
        self.budget.spend(started.elapsed());
        match result {
            Err(error) if self.budget.is_spent() => Err(format!("{error}; {CLAIM_BUDGET_SPENT}")),
            result => result,
        }
    }

    /// Provision the claim label, logging a failure.
    async fn provision(&mut self, task_name: &str, repo: &str) {
        let outcome = self
            .call(
                &crate::issue_dispatch::claim_label_provision_argv(repo),
                "creating the label",
                CLAIM_CALL_MAX_BYTES,
            )
            .await
            .map(drop);
        if let Err(error) = &outcome {
            tracing::warn!(
                task = task_name,
                repo,
                error = %error,
                "issue-dispatch: could not provision the claim label"
            );
        }
        self.provisioned = Some(outcome);
    }

    /// Add the claim label to `issue`.
    async fn add_label(&mut self, repo: &str, issue: u64) -> Result<(), String> {
        self.call(
            &crate::issue_dispatch::claim_issue_argv(repo, issue),
            "adding the label",
            CLAIM_CALL_MAX_BYTES,
        )
        .await
        .map(drop)
    }
}

/// PRD #421 M2.2 — claim a just-dispatched issue on GitHub: provision the
/// label (before the first claim write of the run, and at most once more if a
/// label add fails while provisioning has not succeeded), add it to the issue,
/// then write this deck's claimant record — editing its existing one if the
/// issue already carries it, posting a new one otherwise.
///
/// Called only after `spawn` returned `Ok`, so a failed dispatch never leaves
/// a claim. Every step is best-effort in the way [`ensure_worktrees_excluded`]
/// is: a failure never fails the dispatch, which genuinely succeeded. Unlike
/// that helper, a failure is not left to a WARN alone: a claim that did not
/// fully succeed raises exactly one [`NotifyEvent::IssueClaimFailed`] naming
/// both outcomes (M2.6(b)), so a read-only token does not pass as a run of
/// clean claims. Every call draws on `run.budget`, so a call that hangs is
/// reported as its step's failure and the run moves on to the next issue, and
/// once the run's budget is spent a claim makes no call and is reported as
/// skipped.
///
/// A failed provisioning does not stop the label write: a token that may apply
/// labels without managing them (GitHub's triage role) still succeeds at the
/// add when the label already exists. When the add fails as well, the label
/// may be missing because provisioning failed transiently, so the label is
/// provisioned once more and the add retried ([`ClaimRun::should_reprovision`]).
async fn claim_issue_record(
    task_name: &str,
    cfg: &IssueDispatchConfig,
    issue: u64,
    run: &mut ClaimRun,
    notifier: &dyn Notifier,
) {
    let repo = cfg.repo.as_str();
    if run.budget.is_spent() {
        let skipped = format!("the claim was skipped: {CLAIM_BUDGET_SPENT}");
        tracing::warn!(
            task = task_name,
            repo,
            issue,
            "issue-dispatch: the issue was dispatched but {skipped}"
        );
        notifier.notify(NotifyEvent::IssueClaimFailed {
            task: task_name.to_string(),
            repo: repo.to_string(),
            issue,
            label_error: Some(skipped.clone()),
            record_error: Some(skipped),
        });
        return;
    }

    if run.provisioned.is_none() {
        run.provision(task_name, repo).await;
    }

    let mut label = run.add_label(repo, issue).await;
    if label.is_err() && run.should_reprovision() {
        run.reprovisioned = true;
        run.provision(task_name, repo).await;
        label = run.add_label(repo, issue).await;
    }
    let label_error = label.err().map(|error| match &run.provisioned {
        Some(Err(provision)) => format!("{error}; earlier, {provision}"),
        _ => error,
    });
    if label_error.is_none() {
        tracing::info!(
            task = task_name,
            repo,
            issue,
            "issue-dispatch: claim label added"
        );
    }

    let record_error = write_claim_record(task_name, repo, issue, cfg.claim.ttl_hours, run)
        .await
        .err();

    if label_error.is_some() || record_error.is_some() {
        tracing::warn!(
            task = task_name,
            repo,
            issue,
            label_error = ?label_error,
            record_error = ?record_error,
            "issue-dispatch: the issue was dispatched but its claim did not fully succeed"
        );
        notifier.notify(NotifyEvent::IssueClaimFailed {
            task: task_name.to_string(),
            repo: repo.to_string(),
            issue,
            label_error,
            record_error,
        });
    }
}

/// Write this deck's claimant record on `issue`: read the comments to find a
/// record this deck wrote — same task, same machine id, authored by the account
/// `gh` is signed in with — and edit it if there is one, so a re-claim never
/// appends a second record. If the account or the comments cannot be read,
/// nothing is posted: a blind post is how a duplicate record would appear.
/// The record states `ttl_hours`, the task's `claim_ttl_hours`, as its
/// lifetime.
async fn write_claim_record(
    task_name: &str,
    repo: &str,
    issue: u64,
    ttl_hours: u32,
    run: &mut ClaimRun,
) -> Result<(), String> {
    use crate::issue_dispatch::{
        create_claim_record_argv, edit_claim_record_argv, find_own_claim_record,
        issue_comments_argv, next_claim_time, render_claim_record_with_ttl, sanitize_claim_error,
    };
    let user_id = run.account().await?;
    let comments = run
        .call(
            &issue_comments_argv(repo, issue),
            "reading the issue's comments",
            CLAIM_COMMENTS_MAX_BYTES,
        )
        .await?;
    let own = find_own_claim_record(&comments, task_name, &run.host_id, user_id)
        .map_err(|e| sanitize_claim_error(&e))?;
    let claimed_at = next_claim_time(run.now(), own.as_ref().and_then(|record| record.claimed_at));
    let body = render_claim_record_with_ttl(task_name, &run.host_id, claimed_at, ttl_hours);
    match own {
        Some(record) => {
            let id = record.id;
            // Guaranteed by `next_claim_time`: an edit that repeated the body
            // would leave `updated_at`, the claim's clock, where it was.
            debug_assert_ne!(body, record.body, "a re-claim must change the record");
            run.call(
                &edit_claim_record_argv(repo, id, &body),
                "editing the claim comment",
                CLAIM_CALL_MAX_BYTES,
            )
            .await?;
            tracing::info!(
                task = task_name,
                repo,
                issue,
                comment_id = id,
                "issue-dispatch: claim record edited"
            );
        }
        None => {
            let created = run
                .call(
                    &create_claim_record_argv(repo, issue, &body),
                    "posting the claim comment",
                    CLAIM_CALL_MAX_BYTES,
                )
                .await?;
            // The id is for the log line only: a response it cannot be read
            // from does not make the posted record a failure.
            let comment_id = serde_json::from_str::<serde_json::Value>(&created)
                .ok()
                .and_then(|comment| comment["id"].as_u64());
            tracing::info!(
                task = task_name,
                repo,
                issue,
                comment_id = ?comment_id,
                "issue-dispatch: claim record created"
            );
        }
    }
    Ok(())
}

/// Run one `gh` call of the claim within `bound` and return its stdout, killing
/// the call if it prints more than `max_stdout` bytes; at most
/// [`CLAIM_STDERR_MAX_BYTES`] of its stderr are kept. Its error names the
/// step (`what`) instead of echoing the argv, which for a comment write carries
/// the whole multi-line body, is folded onto one line so the notification
/// built from it stays a single line, and is sanitised
/// ([`crate::issue_dispatch::sanitize_claim_error`]) because it quotes `gh`'s
/// stderr.
async fn gh_claim_call(
    argv: &[String],
    what: &str,
    bound: Duration,
    max_stdout: usize,
) -> Result<String, String> {
    use crate::issue_dispatch::sanitize_claim_error;
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    let one_line = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    match output_within_capped(
        tokio::process::Command::new("gh"),
        "gh",
        &args,
        bound,
        OutputCaps {
            stdout: Some(max_stdout),
            stderr: Some(CLAIM_STDERR_MAX_BYTES),
        },
    )
    .await
    {
        Ok(output) if output.status.success() => {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        }
        Ok(output) => Err(sanitize_claim_error(&format!(
            "{what} failed ({}): {}",
            output.status,
            one_line(&String::from_utf8_lossy(&output.stderr))
        ))),
        Err(error) if is_subprocess_timeout(&error) => Err(format!(
            "{what} timed out after {}s and was killed",
            bound.as_secs_f64()
        )),
        Err(error) if is_subprocess_overflow(&error) => Err(format!(
            "{what} failed: its output passed {max_stdout} bytes and the call was killed"
        )),
        Err(error) => Err(sanitize_claim_error(&format!(
            "{what} failed: {}",
            one_line(&error)
        ))),
    }
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
/// [`classify_worktree_add_result`]). `cleaned_up_by` records whether the
/// best-effort `git worktree remove --force` attempt
/// ([`create_worktree_sync`]) confirmed the half-created directory was
/// actually removed, so the caller can tell the user either "try again" or
/// give them the exact manual command — issue #325: `Some(remover)` names
/// who ran the cleanup (this is always a self-cleanup of the creator's own
/// half-created directory, so `remover` is that same `creator`), `None`
/// when nothing was actually removed.
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
        cleaned_up_by: Option<String>,
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

/// Attempts (the first included) at `git worktree add` when it fails because a
/// concurrent add's administrative directory was only half written — issue
/// #541's residual case. The [`worktree_attach_lock_path`] exclusive lock
/// already serializes every add this deck itself starts, sync or async, but
/// an add started by the user or by another tool takes no lock at all and
/// still races us; the bounded retry below is the only thing that helps
/// against that case. Bounded on purpose: the window is microseconds wide in
/// the wild, so a handful of tries covers a genuine race by orders of
/// magnitude, while a `commondir` that is permanently unreadable (a crashed
/// add left an empty one behind) still surfaces as an error instead of being
/// retried forever.
const WORKTREE_ADD_ATTEMPTS: u32 = 5;

/// Backoff before retry `attempt` (1-based): 100ms, 200ms, 400ms, 800ms — 1.5s
/// of cover in total, matching upstream issue #541's fix.
fn worktree_add_backoff(attempt: u32) -> Duration {
    // Saturating and capped so the arithmetic stays total: at five attempts
    // the shift never exceeds 3, but raising [`WORKTREE_ADD_ATTEMPTS`] must
    // not be able to turn a backoff into an overflow panic.
    Duration::from_millis(100u64 << attempt.saturating_sub(1).min(10))
}

/// How long to wait for the per-repository worktree lock before giving up on
/// serialization and creating the worktree unserialized. A stuck holder must
/// slow a dispatch down, never wedge it forever.
const WORKTREE_LOCK_WAIT: Duration = Duration::from_secs(60);

/// Issue #541: does this `git worktree add` failure look like the reader
/// side of a concurrent add rather than a real problem? `git worktree add`
/// scans the repo's worktree list before creating its own entry, reading
/// every entry's `commondir`; an add that has created its entry but not yet
/// written that file makes the read come back short, and git turns that into
/// `fatal: failed to read '<…>/worktrees/<name>/commondir': Success` —
/// `strerror(errno)` for an errno nothing ever set. Keyed on the file name,
/// not git's sentence: both the `die_errno` format string and `strerror` are
/// localized, so the surrounding words disappear under a non-English locale
/// while `commondir` — a path component — does not.
fn is_worktree_scan_short_read(err: &str) -> bool {
    err.contains("commondir")
}

/// Path of the lock that serializes worktree creation for one repository.
///
/// Derived from `git rev-parse --git-common-dir` and placed inside it, because
/// that directory is exactly the contended resource — `worktrees/<name>` lives
/// there — and because `clone_dir` may itself be a linked worktree, whose `.git`
/// is a FILE and whose per-worktree admin dir is private. Asking git means every
/// spelling of one repository (the main checkout, any of its worktrees, a
/// relative path) maps onto a single lock file, which is what makes the
/// exclusion hold between separate deck PROCESSES and not merely between tasks
/// in one.
///
/// Returns `None` when the directory cannot be resolved (not a git repo) — the
/// add itself then fails with git's own message, which is the better error.
async fn worktree_lock_path(clone_dir: &Path) -> Option<PathBuf> {
    let common = run_git_capture(&[
        "-C",
        &clone_dir.to_string_lossy(),
        "rev-parse",
        "--git-common-dir",
    ])
    .await
    .ok()?;
    let common = common.trim();
    if common.is_empty() {
        return None;
    }
    // `--git-common-dir` answers relative to the repository (plain `.git` for an
    // ordinary checkout), and our cwd is the daemon's, not `clone_dir`'s — so a
    // relative answer has to be joined here. Resolved this way rather than with
    // `--path-format=absolute` so the derivation does not depend on git 2.31+.
    let common = Path::new(common);
    let path = if common.is_absolute() {
        common.to_path_buf()
    } else {
        clone_dir.join(common)
    };
    Some(path.join("dot-agent-deck-worktree.lock"))
}

/// Serialize `git worktree add` for one repository across processes (issue
/// #541), so this deck's own concurrent dispatches cannot observe each other's
/// half-created administrative directories.
///
/// Every failure mode degrades to "create the worktree anyway": an unresolvable
/// repo, an unwritable `.git`, or a holder that never lets go all return `None`,
/// which is precisely the pre-#541 behaviour that the bounded retry in
/// [`create_worktree`] already covers. A lock is worth having only while it
/// cannot itself become the reason a dispatch fails or hangs.
///
/// Serialization also does not make the retry redundant, and vice versa: this
/// only binds processes that take the lock, so an add started by the user or by
/// another tool still races us, and only the retry survives that.
async fn acquire_worktree_lock(clone_dir: &Path) -> Option<crate::platform::lock::SpawnLock> {
    let path = worktree_lock_path(clone_dir).await?;
    match tokio::time::timeout(
        WORKTREE_LOCK_WAIT,
        crate::platform::lock::acquire_spawn_lock(&path),
    )
    .await
    {
        Ok(Ok(guard)) => Some(guard),
        Ok(Err(e)) => {
            tracing::warn!(
                lock = %path.display(),
                error = %e,
                "could not take the per-repository worktree lock; creating the worktree \
                 unserialized (the bounded retry still covers a concurrent add)"
            );
            None
        }
        Err(_) => {
            tracing::warn!(
                lock = %path.display(),
                waited_secs = WORKTREE_LOCK_WAIT.as_secs(),
                "timed out waiting for the per-repository worktree lock; creating the \
                 worktree unserialized rather than wedging the dispatch"
            );
            None
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
/// Fix #541 follow-up: a live worktree directory reports `AlreadyClaimed`
/// even when the branch is also present and `reuse_existing_branch` is
/// false — the branch-exists early return only applies while the directory
/// is genuinely absent (see the `worktree_dir.exists()` check below), or a
/// dispatch whose worktree is still visible on disk would be told its
/// worktree is "already gone" and be pointed at `git branch -D` for a tree
/// another dispatch is working in.
///
/// Fork #282 (async half): the whole probe→add sequence runs under the same
/// [`worktree_attach_lock_path`] exclusive lock [`create_worktree_sync`]
/// already holds for its own attach race — see the lock acquisition inside
/// this function for why an unbounded `flock` wait is not safe here.
///
/// Issue #541 (upstream vfarcic/dot-agent-deck#541) reported a second,
/// narrower hazard on the same call — a concurrent `git worktree add`
/// reading another add's half-written `commondir` file mid-scan. Two
/// defences, each covering what the other cannot: the exclusive lock above
/// serializes every add this deck itself starts, sync or async (which
/// upstream's own single-process, async-only lock did not); and the bounded
/// retry ([`WORKTREE_ADD_ATTEMPTS`] / [`is_worktree_scan_short_read`]) is
/// what covers the residual case — an add started by the user or another
/// tool, which takes no lock at all. Neither defence swallows anything: a
/// `commondir` that stays unreadable exhausts the attempts and surfaces as
/// `Err`.
///
/// Issue #425 — `creator`. This is the ONLY `git worktree add` in `src/`, so
/// it is also the only place that can claim a worktree as the deck's own at
/// the moment it comes into existence. On success it writes the ownership
/// marker `worktree_reclaim` later reads, recording `creator` so the claim
/// names the responsible dispatch rather than a bare "the deck". Written on
/// the `Created` arm only, and best-effort — see [`crate::worktree_owner`] for
/// why both of those are load-bearing.
///
/// Issue #791 — the same `Created` arm also WARMS the new worktree's project
/// environment, and being the only `git worktree add` in `src/` is again why
/// this is the place for it: every caller spawns only after awaiting this
/// function, so a warm-up finished before it returns is a warm-up finished
/// before any role starts. That ordering is the entire fix, and
/// [`warm_project_environment`] documents the race it closes and why every way
/// it can fail still returns `Created`.
pub async fn create_worktree(
    clone_dir: &Path,
    worktree_dir: &Path,
    branch: &str,
    reuse_existing_branch: bool,
    creator: Creator,
) -> Result<WorktreeCreation, String> {
    create_worktree_from(
        clone_dir,
        worktree_dir,
        branch,
        reuse_existing_branch,
        creator,
        None,
    )
    .await
}

/// [`create_worktree`] with an explicit start-point for a branch it creates.
///
/// Issue #1643: with no start-point, `git worktree add -b` resolves the
/// checkout's `HEAD` itself, at the moment it runs. A caller that has already
/// read `HEAD` and reported it — `dispatch`'s `cut from <branch> at <sha>` —
/// then names one commit while git may cut the branch from another, if `HEAD`
/// moved in between. Passing the sha it read makes the reported base the
/// branch's base by construction.
///
/// `start_point` applies only when this call CREATES the branch (`-b`). A
/// branch that already exists is attached as it stands, and its own history is
/// its base; that covers `reuse_existing_branch` and a retry after a scan
/// short-read, whose branch our own earlier attempt created from the same
/// start-point. `None` is exactly [`create_worktree`]. This stays the only
/// `git worktree add` in `src/`.
///
/// **Locks, always in this order.** First the per-repository worktree lock
/// ([`acquire_worktree_lock`], issue #541): the removal paths
/// (`settle_failed_issue_spawn`, `reclaim_abandoned_spawn_paced`) take it to
/// exclude a concurrent creation, so creation has to take the same one. Then the
/// per-worktree attach lock shared with the TUI's [`create_worktree_sync`]
/// (fork #282). Nothing takes them the other way round, so they cannot
/// deadlock; both waits are bounded, and both are released on the `Created`
/// arm before the warm-up.
pub async fn create_worktree_from(
    clone_dir: &Path,
    worktree_dir: &Path,
    branch: &str,
    reuse_existing_branch: bool,
    creator: Creator,
    start_point: Option<&str>,
) -> Result<WorktreeCreation, String> {
    ensure_worktree_parent_dir(worktree_dir)?;
    // Issue #541: keep this deck's own concurrent creations off each other's
    // half-created entries. Held across the probe AND the add so the two are
    // atomic with respect to another dispatch of the same name, and DROPPED
    // EXPLICITLY on the `Created` arm below — see the comment there for why the
    // warm-up must not run under it.
    let repo_lock = acquire_worktree_lock(clone_dir).await;

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
    // `create_worktree_sync` (the premise behind `run_status`'s old
    // detach-on-drop behaviour that this changes). Bounded with the same `WORKTREE_GIT_TIMEOUT`
    // the sync twin's equivalent calls already use, so a wedged `git` here
    // cannot hold the shared lock indefinitely and starve every other
    // caller of this same `worktree_dir` — including the TUI.
    // Issue #541: re-probed on every attempt inside the loop below, not
    // hoisted out here — an add that dies on the scan has already CREATED
    // its `-b` branch (the branch survives the exit-128), so passing `-b`
    // again on retry would fail with "a branch named … already exists" and
    // turn a transient race into a hard one.
    let mut attempt: u32 = 1;
    let clone = clone_dir.to_string_lossy();
    let branch_ref = format!("refs/heads/{branch}");
    // Issue #692 / PR #1304: what a timed-out add may be cleaned up against.
    // What the FIRST attempt's branch probe found — later attempts can only see
    // a branch our own failed attempt made — and whether the worktree directory
    // was there before the attempt that ended the loop.
    let mut branch_existed_initially = None;
    let mut dir_existed_before_add;
    let add: Result<(), AddError> = loop {
        // Tri-state probe (upstream #692, Qodo on PR #1304): "absent" and "could
        // not tell" differ, and only a DEFINITE absence licenses deleting the
        // branch after a timed-out add. Bounded by the fork's
        // `WORKTREE_GIT_TIMEOUT` because the attach lock is held (fork #282); a
        // probe that hits it is still reported as an error naming the branch,
        // exactly as before the sync, rather than guessed at.
        let probe = probe_branch(&clone, &branch_ref, WORKTREE_GIT_TIMEOUT).await;
        if probe == BranchProbe::TimedOut {
            return Err(format!(
                "timed out after {WORKTREE_GIT_TIMEOUT:?} probing for branch {branch} in {}",
                clone_dir.display()
            ));
        }
        let branch_exists = probe == BranchProbe::Present;
        branch_existed_initially.get_or_insert(probe);
        // Only attempt 1 can report BranchExists/AlreadyClaimed off the bare
        // branch probe. Reaching attempt 2 means the branch was PROVEN absent
        // moments ago, so anything there now was created either by our own
        // failed attempt or by a dispatch racing us — never the "may hold
        // committed work from an earlier dispatch" case this guard exists for,
        // and not re-checked on retry. (A branch a racing dispatch is really
        // using fails the attach with "already used by worktree at …", and its
        // worktree dir is then present, so the outcome is `AlreadyClaimed`.)
        if branch_exists && !reuse_existing_branch && attempt == 1 {
            // A worktree DIRECTORY that is still present is a live claim,
            // not a leftover branch: the caller can see it, and telling
            // them it is "already gone" (BranchExists's message) sends them
            // to `git branch -D` for a tree another dispatch is working in.
            // Only when the directory is genuinely absent does the branch
            // alone mean "leftover from an earlier dispatch".
            if worktree_dir.exists() {
                return Ok(WorktreeCreation::AlreadyClaimed);
            }
            return Ok(WorktreeCreation::BranchExists);
        }
        dir_existed_before_add = worktree_dir.exists();
        let mut add_argv = crate::issue_dispatch::worktree_add_argv(
            clone_dir,
            worktree_dir,
            branch,
            branch_exists,
        );
        if !branch_exists {
            // Issue #1643 (upstream): the start-point applies only when this call
            // CREATES the branch (`-b <branch>`); an existing branch is attached
            // as it stands. Appended here rather than inside
            // `worktree_add_argv` so the sync twin and its tests keep calling the
            // 4-argument builder.
            add_argv.extend(start_point.map(str::to_string));
        }
        // Bounded by the FORK's `WORKTREE_GIT_TIMEOUT`, not upstream's ten-minute
        // `SUBPROCESS_TIMEOUT`: this add runs under the attach lock shared with
        // the TUI's `create_worktree_sync` (fork #282), so a wedged `git` must
        // not hold it for ten minutes. The runner kills the add's whole PROCESS
        // GROUP at the bound (upstream #692), which also closes the residual
        // fork #133 documented for the old direct-child-only `kill_on_drop` kill.
        let result: Result<(), AddError> =
            match run_git_status_within(&add_argv, WORKTREE_GIT_TIMEOUT).await {
                Ok(()) => Ok(()),
                Err(e) if is_subprocess_timeout(&e) => Err(AddError::TimedOut(e)),
                Err(e) => Err(AddError::Failed(e)),
            };
        match result {
            Err(AddError::Failed(e))
                if attempt < WORKTREE_ADD_ATTEMPTS && is_worktree_scan_short_read(&e) =>
            {
                let backoff = worktree_add_backoff(attempt);
                tracing::warn!(
                    clone = %clone_dir.display(),
                    worktree = %worktree_dir.display(),
                    attempt,
                    backoff_ms = backoff.as_millis() as u64,
                    error = %e,
                    "git worktree add read another add's half-created administrative \
                     directory (issue #541); retrying after a backoff"
                );
                tokio::time::sleep(backoff).await;
                attempt += 1;
            }
            // Success, a non-transient failure, or the last attempt: the
            // retry is bounded, so a `commondir` that is genuinely
            // unreadable (a crashed add left an empty one behind) still
            // surfaces as an error rather than looping or being swallowed.
            other => break other,
        }
    };
    // `AddOutcome::TimedOut` is reachable here (unlike before this bound was
    // added). Fork #282 final-pass F1/A1: the add's runner has already killed
    // the add's whole process group at `WORKTREE_GIT_TIMEOUT` by the time we
    // get here, so — mirroring `create_worktree_sync`'s
    // `attempt_worktree_cleanup` — attempt best-effort cleanup of whatever the
    // add half-registered before it was killed, rather than hardcoding
    // `cleaned_up_by: None` and leaving the slug wedged.
    //
    // Issue #692 (upstream): the destructive extras (force-removing a LOCKED
    // entry, and deleting the branch the add's `-b` made) are licensed only when
    // the directory is provably ours — it was absent when the attempt that timed
    // out started, and the per-repository lock is held, which is what keeps
    // another dispatch of this deck from having created it in between. Without
    // the lock the directory is left to the fork's own single-force cleanup and
    // the timeout is reported as it stands.
    let discard_licensed =
        matches!(add, Err(AddError::TimedOut(_))) && repo_lock.is_some() && !dir_existed_before_add;
    // Only a DEFINITE absence licenses deleting the branch: a probe that could
    // not answer proves nothing, and the branch may hold committed work.
    let created_branch = (branch_existed_initially == Some(BranchProbe::Absent)).then_some(branch);
    let outcome = match classify_worktree_add_result(worktree_dir, add) {
        Ok(outcome) => outcome,
        Err(e) => {
            // A timed-out add that never got as far as creating the directory
            // can still have registered an entry or made its branch.
            if discard_licensed {
                discard_timed_out_worktree(clone_dir, worktree_dir, created_branch).await;
            }
            return Err(e);
        }
    };
    Ok(match outcome {
        AddOutcome::Created => {
            // Issue #425: claim the worktree we just created, HERE, so no
            // window exists in which a deck-created worktree is
            // unrecognisable to `worktree list|reclaim`. Best-effort by
            // construction — issue #164: it RETURNS the warning instead of
            // only logging it, so this caller can surface it via
            // `WorktreeCreation::Created`'s `marker_warning` field.
            let creator_str = crate::worktree_reclaim::sanitize_marker_creator(&format!(
                "{}:{}",
                creator.kind, creator.subject
            ));
            let marker_warning = mark_worktree_owned_best_effort(worktree_dir, &creator_str);
            // Release the shared attach lock before warming. It exists to
            // serialize `git worktree add` (fork #282/#541); the warm-up
            // below is a subprocess that can sit for seconds and is bounded
            // in minutes, and holding this lock across it would push a
            // concurrent attach of ANOTHER worktree into an avoidable wait —
            // reintroducing the hazard the lock is for, to protect two
            // materializations that do not touch each other's `.devbox` in
            // the first place.
            drop(_attach_lock);
            // Likewise the per-repository ADD lock (upstream #541): it exists to
            // serialize `git worktree add` against itself, and holding a
            // 60s-wait lock across the warm-up would push a concurrent dispatch
            // of ANOTHER worktree past `WORKTREE_LOCK_WAIT` into the
            // unserialized path.
            drop(repo_lock);
            // Issue #791: materialize this worktree's project environment
            // ONCE, HERE, before returning — so that by the time any caller
            // spawns a role into it, there is no cold `.devbox` left to race
            // over. The outcome is deliberately dropped: EVERY one of them,
            // warmed or not, still reports `Created`, because a worktree
            // that exists has been created whatever devbox did afterwards.
            // See [`warm_project_environment`] for why none of them is an
            // error.
            warm_project_environment(worktree_dir).await;
            WorktreeCreation::Created { marker_warning }
        }
        AddOutcome::AlreadyClaimed => WorktreeCreation::AlreadyClaimed,
        AddOutcome::TimedOut => {
            let creator_str = crate::worktree_reclaim::sanitize_marker_creator(&format!(
                "{}:{}",
                creator.kind, creator.subject
            ));
            let mut cleaned_up_by =
                attempt_worktree_cleanup_async(clone_dir, worktree_dir, &creator_str).await;
            // Issue #692 (upstream, PR #1304): the fork's cleanup above is a single
            // `git worktree remove --force`, which git REFUSES for an entry a
            // killed add left `locked` ("initializing"), and it never touches the
            // branch the add's `-b` had already made — so the next fire would
            // find the slug wedged or its branch left behind. The escalation runs
            // only when the directory is provably ours to remove (see
            // `discard_licensed`), after the fork's cleanup rather than instead of
            // it: when that one succeeded, the escalation's removal finds nothing
            // and costs three cheap, individually bounded `git` calls, and it is
            // the part that deletes the branch. If the escalation is what finally
            // got the directory out, the cleanup is confirmed by it — same
            // contract, "the directory is gone and git agrees".
            if discard_licensed {
                let discarded =
                    discard_timed_out_worktree(clone_dir, worktree_dir, created_branch).await;
                if cleaned_up_by.is_none() && discarded {
                    cleaned_up_by = Some(creator_str);
                }
            }
            WorktreeCreation::TimedOut { cleaned_up_by }
        }
    })
}

/// What `git rev-parse --verify --quiet <ref>` said about a branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BranchProbe {
    /// Exit 0: the ref resolves.
    Present,
    /// Exit 1, which is how `--verify --quiet` reports a ref that does not
    /// exist.
    Absent,
    /// `git` was killed at the probe's bound. Its own variant (fork) because
    /// [`create_worktree_from`] holds the attach lock shared with the TUI while
    /// it probes, so a probe that hit [`WORKTREE_GIT_TIMEOUT`] is reported as an
    /// error rather than guessed at.
    TimedOut,
    /// Anything else — a `git` that would not start, another exit status.
    /// Treated as absent for choosing `-b`, exactly as the old `is_ok()` probe
    /// did, but never as proof that a branch may be deleted.
    Unknown,
}

/// Probe `branch_ref` in `clone`, telling "absent" apart from "could not tell"
/// (Qodo, PR #1304) — the difference decides whether a timed-out add's cleanup
/// may delete the branch. `bound` is the caller's: the fork bounds it by
/// [`WORKTREE_GIT_TIMEOUT`] because the attach lock is held (fork #282).
///
/// The argv is the one [`crate::issue_dispatch::worktree_branch_probe_argv`]
/// builds for the sync twin (`-C <clone> rev-parse --verify --quiet <ref>`).
async fn probe_branch(clone: &str, branch_ref: &str, bound: Duration) -> BranchProbe {
    let args = ["-C", clone, "rev-parse", "--verify", "--quiet", branch_ref];
    match output_within(
        crate::git_env::git_async(),
        crate::git_env::GIT,
        &args,
        bound,
    )
    .await
    {
        Ok(out) if out.status.success() => BranchProbe::Present,
        Ok(out) if out.status.code() == Some(1) => BranchProbe::Absent,
        Err(e) if is_subprocess_timeout(&e) => BranchProbe::TimedOut,
        _ => BranchProbe::Unknown,
    }
}

/// Undo what a `git worktree add` killed at its bound left behind (issue #692):
/// its half-checked-out directory, its administrative entry, and — when
/// `created_branch` names one — the branch its `-b` created. Returns whether the
/// directory is gone and git agrees nothing is registered there (the removal or
/// the prune succeeded), which is what lets the caller report the cleanup as
/// confirmed.
///
/// Only [`create_worktree_from`] calls this, and only once it has established
/// the worktree is its own (see `discard_licensed` there). Every step is
/// best-effort and logged: the caller is already returning the timeout as the
/// dispatch's result, and a cleanup that fails leaves exactly the state that
/// would have existed without it. `--force` twice, because a killed add leaves
/// its entry `locked` ("initializing") and a single `--force` refuses a locked
/// worktree. The branch is deleted only when it did not exist before the first
/// attempt, so a reused branch that may hold committed work is never touched —
/// and `git branch -D` itself refuses a branch another worktree has checked out.
///
/// Each `git` call is bounded by the fork's [`WORKTREE_CLEANUP_TIMEOUT`], not
/// upstream's ten minutes: this runs while the attach lock shared with the TUI
/// is still held (fork #282), and a cleanup that itself wedges must not freeze
/// the TUI's own attach.
async fn discard_timed_out_worktree(
    clone_dir: &Path,
    worktree_dir: &Path,
    created_branch: Option<&str>,
) -> bool {
    let clone = clone_dir.to_string_lossy();
    let wt = worktree_dir.to_string_lossy();
    let removed = git_within(
        &[
            "-C", &clone, "worktree", "remove", "--force", "--force", &wt,
        ],
        WORKTREE_CLEANUP_TIMEOUT,
    )
    .await;
    if let Err(e) = &removed {
        tracing::debug!(
            worktree = %worktree_dir.display(),
            error = %e,
            "issue-dispatch: git could not remove the timed-out worktree; removing its directory"
        );
    }
    // `tokio::fs` so walking a large partial checkout does not hold an async
    // worker (Qodo, PR #1304).
    match tokio::fs::remove_dir_all(worktree_dir).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(
            worktree = %worktree_dir.display(),
            error = %e,
            "issue-dispatch: could not remove the timed-out worktree's directory"
        ),
    }
    let pruned = git_within(
        &["-C", &clone, "worktree", "prune"],
        WORKTREE_CLEANUP_TIMEOUT,
    )
    .await;
    if let Err(e) = &pruned {
        tracing::warn!(error = %e, "issue-dispatch: git worktree prune after a timed-out add failed");
    }
    if let Some(branch) = created_branch
        && let Err(e) = git_within(
            &["-C", &clone, "branch", "-D", branch],
            WORKTREE_CLEANUP_TIMEOUT,
        )
        .await
    {
        tracing::warn!(
            branch,
            error = %e,
            "issue-dispatch: could not delete the branch a timed-out add created"
        );
    }
    !worktree_dir.exists() && (removed.is_ok() || pruned.is_ok())
}

// ---------------------------------------------------------------------------
// Issue #791 — warming a freshly created worktree's project environment
// ---------------------------------------------------------------------------

/// The file whose presence means "this worktree has a devbox environment".
/// Absent, there is nothing to materialize and no reason to spawn anything.
const DEVBOX_MANIFEST: &str = "devbox.json";

/// How long [`warm_project_environment`] waits for `devbox` before giving up on
/// the warm-up and letting the roles proceed.
///
/// **A deadlock guard, not a latency budget**, and the distinction is what sets
/// the value. Waiting here is very nearly free in the case that matters: while
/// devbox materializes, every role that launches through `devbox run` would be
/// blocked on exactly the same work anyway, so the wait is moved rather than
/// added — and moved somewhere serial, which is the entire point. Expiring
/// early is NOT free, because it is the one path that puts a role back in the
/// race (see [`warm_project_environment`]). So the bound is only worth setting
/// low enough to rescue a dispatch from a `devbox` that will never return at
/// all, which is pathological rather than merely slow.
///
/// Ten minutes, then, rather than the two this shipped with in review: a cold
/// `.devbox` for this repository's 22-package manifest took **5.2s** against an
/// already-populated nix store (measured 2026-09-04), but a first-ever closure
/// fetch is a download whose length nothing here has measured, and two minutes
/// was a guess dressed as a budget. Ten is not measured either — no honest
/// number is available for "slow link, cold store" — it is simply far enough
/// out that expiry means *stuck*.
const DEVBOX_WARMUP_TIMEOUT: Duration = Duration::from_secs(600);

/// What [`warm_project_environment`] did. Every arm returns normally: a
/// warm-up that cannot run is a missed optimisation, never a failed dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DevboxWarmup {
    /// No `devbox.json` in the new worktree, so there is nothing to
    /// materialize and NO process was started.
    NoManifest,
    /// `devbox run -- true` completed successfully; `.devbox` is warm.
    Warmed,
    /// `devbox` could not be executed at all — not on `PATH`, not executable.
    Unavailable(String),
    /// `devbox` ran and exited non-zero.
    Failed(String),
    /// `devbox` was still running when [`DEVBOX_WARMUP_TIMEOUT`] expired. It is
    /// left running — see [`warm_project_environment`] for why that takes a
    /// detached task rather than simply dropping the future.
    TimedOut,
}

/// Issue #791: materialize a freshly created worktree's `.devbox` ONCE and
/// SERIALLY, before its caller spawns anything into it.
///
/// **What this fixes.** devbox's per-project `.devbox` materialization is not
/// safe against concurrent invocations of itself. Six roles of an orchestration
/// starting together in a fresh worktree are six concurrent first invocations,
/// and the loser dies at launch — reported as
/// `remove …/virtenv/bin/setup-corepack.mjs: no such file or directory` or its
/// complementary `symlink …: file exists`, or, with the nodejs plugin disabled,
/// as `compact json for hashing: unexpected end of JSON input` from a
/// half-written state file. The failure is SILENT unless you count roles: the
/// deck reports the orchestration as started and it simply runs with five roles
/// instead of six, twice observed taking `agent-orchestrator` and so leaving
/// workers with nobody to delegate to. Only the FIRST materialization is
/// exposed; once `.devbox` is warm every later invocation succeeds, which is
/// why the main checkout never fails and why a failed orchestration succeeds on
/// restart.
///
/// So one invocation, awaited to completion, is the whole mechanism: after it
/// there is no first-materialization window left for a role to lose.
///
/// **Why it never fails a dispatch.** Every reachable outcome returns normally
/// and the caller goes on to spawn. A repository with no `devbox.json`, a
/// machine with no `devbox`, a `devbox` that exits non-zero for a reason of its
/// own (an `init_hook` that needs a credential this host lacks) — none of those
/// is a reason to refuse a dispatch that would otherwise have worked. They cost
/// the optimisation, and the roles then race exactly as they did before this
/// existed. Failing closed here would convert a devbox problem into a dispatch
/// problem for every user who has no devbox at all.
///
/// **The timeout path is the one that is honestly worse than doing nothing, and
/// it is bounded rather than removed.** If [`DEVBOX_WARMUP_TIMEOUT`] expires the
/// roles start while devbox is still materializing — the original race, now with
/// this warm-up as one more contender in it. Raised by Greptile on PR #881 and
/// accepted as real. The two alternatives are worse: waiting forever wedges the
/// dispatch, and killing the child abandons a half-written `.devbox`, which is
/// the `compact json for hashing: unexpected end of JSON input` failure by
/// construction. So the bound is set where expiry means *stuck* rather than
/// *slow* (see [`DEVBOX_WARMUP_TIMEOUT`]), and what is left is a narrow,
/// documented residual instead of a claim that there is none.
///
/// **Leaving the child running takes a detached task, not merely leaving
/// `kill_on_drop` off.** This code first claimed the child survived on the
/// strength of that default, and it was FALSE — measured 2026-09-04: dropping a
/// timed-out `Command::output()` future closes our ends of the child's stdout
/// and stderr pipes, and devbox (a Go program) dies on `SIGPIPE` at its next
/// write. A stand-in that logged progress and then wrote a marker never wrote
/// it. So the run is `tokio::spawn`ed and the timeout is applied to the
/// `JoinHandle`: dropping a handle DETACHES its task, so the pipes stay drained
/// and the child finishes. `warm_up_leaves_a_slow_devbox_running_rather_than_
/// killing_it` is the regression test, and it fails against the dropped-future
/// version.
async fn warm_project_environment(worktree_dir: &Path) -> DevboxWarmup {
    warm_project_environment_with(worktree_dir, &devbox_program(), DEVBOX_WARMUP_TIMEOUT).await
}

/// The program the warm-up runs. `devbox`, resolved from `PATH` exactly as the
/// `git` / `gh` calls in this module are.
///
/// The `cfg(test)` override exists for one assertion — that `create_worktree`
/// really does await the warm-up before reporting `Created` — which cannot be
/// made against the real `devbox` without materializing a nix environment
/// inside a unit test. Production has no override and no way to reach one.
fn devbox_program() -> String {
    #[cfg(test)]
    {
        if let Some(p) = tests::devbox_program_override() {
            return p;
        }
    }
    "devbox".to_string()
}

/// The body of [`warm_project_environment`], with the program and the bound
/// passed in so the degradation paths can be driven without a real `devbox`.
async fn warm_project_environment_with(
    worktree_dir: &Path,
    program: &str,
    bound: Duration,
) -> DevboxWarmup {
    // Probed BEFORE anything is spawned: the great majority of repositories
    // have no devbox environment at all, and they must pay nothing — not a
    // failed `exec`, not a `PATH` walk — for a fix that is not about them.
    //
    // The MANIFEST is the gate, deliberately, rather than "do this dispatch's
    // commands actually invoke devbox?". Greptile asked for the narrower test on
    // PR #881 and it is the right instinct with the wrong trade here, for two
    // reasons. The command set is not knowable at this point: `dispatch_one_issue`
    // passes `resolved_target: None` precisely because the shape is derived later
    // from the worktree's own config, and a single agent's command comes from the
    // DECK's `default_command`, which this module never sees. So the narrower
    // test would have to guess — and its failure direction is to skip the warm-up
    // for a dispatch that needed it, which is the silent dropped role this whole
    // change exists to stop, arrived at by a subtler route. The cost it would
    // save, meanwhile, is mostly not a cost: a repository that ships a
    // `devbox.json` materializes it on first use by SOMEBODY, so warming here
    // usually moves that work rather than adding it. It is genuinely wasted only
    // for a worktree in which nothing ever enters the devbox environment, and
    // there it is one `devbox run -- true` — 5.2s against a populated store.
    if !worktree_dir.join(DEVBOX_MANIFEST).is_file() {
        return DevboxWarmup::NoManifest;
    }

    let started = std::time::Instant::now();
    // Owned, because the run has to outlive this call: see the doc comment on
    // why the bound is applied to a DETACHED task rather than to the future.
    let program = program.to_string();
    let dir = worktree_dir.to_path_buf();
    let run = tokio::spawn(async move {
        tokio::process::Command::new(&program)
            .args(["run", "--", "true"])
            .current_dir(&dir)
            .output()
            .await
    });

    match tokio::time::timeout(bound, run).await {
        Ok(Ok(Ok(out))) if out.status.success() => {
            tracing::debug!(
                worktree = %worktree_dir.display(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "warmed the worktree's devbox environment before spawning (issue #791)"
            );
            DevboxWarmup::Warmed
        }
        Ok(Ok(Ok(out))) => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            tracing::warn!(
                worktree = %worktree_dir.display(),
                status = %out.status,
                stderr = %stderr,
                "devbox warm-up failed; spawning anyway, so concurrent roles may \
                 race the first materialization (issue #791)"
            );
            DevboxWarmup::Failed(stderr)
        }
        Ok(Ok(Err(e))) => {
            tracing::debug!(
                worktree = %worktree_dir.display(),
                error = %e,
                "no usable `devbox` for the warm-up; spawning without one (issue #791)"
            );
            DevboxWarmup::Unavailable(e.to_string())
        }
        // The task itself panicked or was cancelled. Nothing here can panic, so
        // this is unreachable in practice — reported as unavailable rather than
        // unwrapped, because a panic in a warm-up must not take a dispatch down.
        Ok(Err(join)) => {
            tracing::warn!(
                worktree = %worktree_dir.display(),
                error = %join,
                "the devbox warm-up task did not run to completion; spawning anyway \
                 (issue #791)"
            );
            DevboxWarmup::Unavailable(join.to_string())
        }
        Err(_) => {
            tracing::warn!(
                worktree = %worktree_dir.display(),
                waited_secs = bound.as_secs(),
                "devbox warm-up did not finish within its bound; it is left running \
                 and the roles now start alongside it, so they can still race the \
                 first materialization (issue #791)"
            );
            DevboxWarmup::TimedOut
        }
    }
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

/// Filesystem location of fork#325 M4b's isolated-clone-specific provenance
/// artifact ([`crate::worktree_reclaim::ISOLATED_CLONE_PROVENANCE_FILENAME`]'s
/// doc comment). M4b's first attempt put this inside `clone_dir`'s own
/// `.git` directory — reviewer R1 / auditor D1 (blocker, PR #515) reopened
/// auditor A1/B1's forgery on that: a same-uid attacker who can plant a
/// sibling directory at all can, by definition, write into a `.git` it
/// created itself, so evidence living there is forgeable with a bare
/// `touch`. This fix moves the artifact OUTSIDE every candidate entirely,
/// into [`crate::platform::paths::state_dir`] — a directory only this
/// process's own uid can write, never a directory any candidate controls —
/// keyed by [`crate::platform::lock::fnv1a64`] of the canonical clone path,
/// the exact same keying scheme [`worktree_attach_lock_path_from_common_dir`]
/// already uses for the (unrelated) cross-process attach lock; reused here
/// rather than reinvented.
///
/// Resolves identically at write time ([`write_isolated_clone_provenance`],
/// called once `clone_dir` genuinely exists) and at check time
/// ([`crate::worktree_reclaim::candidate_has_attach_lock`], called on a
/// directory `discover_isolated_clones` already found on disk), because
/// [`canonicalize_best_effort`] always resolves through the PARENT and
/// rejoins the file name regardless of whether the full path exists yet —
/// see that function's own doc comment for the write-time/check-time
/// divergence this exact pattern once caused on Windows (fork #331 audit
/// B2), deliberately avoided here too rather than reintroduced.
///
/// This also serves reviewer F2 (`discover_isolated_clones` invoked from
/// inside an isolated clone itself) better than the candidate-local design
/// did: `state_dir()` resolves identically regardless of where the caller
/// is rooted — the root checkout, a subdirectory of it, or another isolated
/// clone entirely — so no `common_dir` resolution is needed at all, and no
/// trust is placed in the candidate to find it.
pub(crate) fn isolated_clone_provenance_path(clone_dir: &Path) -> PathBuf {
    let canonical_clone_dir = canonicalize_best_effort(clone_dir);
    let hash = crate::platform::lock::fnv1a64(canonical_clone_dir.to_string_lossy().as_bytes());
    let basename = canonical_clone_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("clone");
    crate::platform::paths::state_dir()
        .join(crate::worktree_reclaim::ISOLATED_CLONE_PROVENANCE_FILENAME)
        .join(format!("{basename}-{hash:016x}"))
}

/// Read one `key=value` field out of a provenance artifact's raw `content`,
/// but only when the artifact carries a recognized `schema=2` or `schema=3`
/// tag — a pre-M4 `b"deck\n"` artifact (or anything else that doesn't parse
/// as either format) has no such fields to trust, and every caller of this
/// function must treat that as "no evidence to contradict," never as a
/// literal empty string (PRD fork#544 review-findings fix round; shared by
/// [`resume_existing_isolated_clone`]'s creator check and
/// [`forget_isolated_workspace`]'s `path=` verification rather than each
/// growing its own parser). `schema=3` ([`pin_isolated_clone`]/
/// [`unpin_isolated_clone`], fork issue #546) is additive on `schema=2` —
/// the same `root-hash=`/`name=`/`creator=`/`path=` fields plus a
/// `pinned=` line — so widening this check to accept it changes nothing
/// for an existing `schema=2` artifact and every field already read from
/// one (`path=`, `creator=`, …) resolves identically; it only stops a
/// `pinned=` read (or any other field) on a `schema=3` artifact from
/// silently returning `None` as if the artifact carried no evidence at
/// all.
pub(crate) fn isolated_clone_provenance_field(content: &str, key: &str) -> Option<String> {
    let recognized_schema = content
        .lines()
        .any(|line| matches!(line.trim(), "schema=2" | "schema=3"));
    if !recognized_schema {
        return None;
    }
    let prefix = format!("{key}=");
    content
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .map(str::to_string)
}

/// Write fork#325 M4b's isolated-clone-specific provenance artifact at
/// [`isolated_clone_provenance_path`]'s resolved location. Called only from
/// [`provision_isolated_clone_sync`], only once its own `clone_dir.exists()`
/// check and the `git clone` immediately above have both already
/// succeeded, so this genuinely vouches for a clone this call itself just
/// created.
///
/// PRD fork#544 M4: the artifact is no longer the content-free `b"deck\n"`
/// bytes — it now carries plain `key=value` lines: a `schema=` tag for a
/// future consumer to branch on, a `root-hash=` of `source_dir`'s own
/// canonical path (hashed with the same [`crate::platform::lock::fnv1a64`]
/// keying scheme [`isolated_clone_provenance_path`] already uses for the
/// clone path, rather than inventing a second hash), the orchestration
/// `name` typed for this workspace, the clone's own canonical `path`, and
/// (review-findings fix round, reviewer B2) the `creator` identity that
/// opened it. This is additive evidence for future tooling, not a new
/// requirement: [`resume_existing_isolated_clone`]'s eligibility check (b)
/// below still only tests file presence, so a pre-M4 `b"deck\n"` artifact
/// keeps resuming exactly as before (`orchestration/workspace/012`).
///
/// Both `name` and `creator` are caller-supplied strings, not values this
/// module controls the shape of — sanitized via
/// [`crate::worktree_reclaim::sanitize_marker_creator`] (reused rather than
/// inventing a second sanitizer) before being interpolated into the raw
/// `key=value` content below, so a `name`/`creator` containing an embedded
/// `\n` can't forge additional records into the artifact. `path`, by
/// contrast, is NOT sanitized -- it is the canonicalized `clone_dir` itself,
/// not free text -- so PRD fork#544 review-findings fix round 3 (N3) writes
/// `creator=` (and `name=`) BEFORE `path=` in the content below, not after:
/// [`isolated_clone_provenance_field`]'s read side returns the FIRST line
/// matching a given key, so an unsanitized `path` value that somehow
/// embedded its own `\ncreator=...` line could otherwise inject a forged
/// `creator=` record ahead of the real one. With `creator=` written first,
/// any such embedded line would land on a LATER line instead and never be
/// the one returned. Same-uid, self-directed severity either way (a
/// malformed `path` would already have failed the checkout that produces
/// `clone_dir` in the first place), but this ordering costs nothing to get
/// right.
///
/// Atomic write-then-rename, mirroring [`mark_worktree_owned`]'s own
/// pattern in `worktree_reclaim.rs` for the identical reason: on ENOSPC or
/// a process kill mid-write, a plain `std::fs::write` could leave a
/// partially-written file at the final path, which `candidate_has_attach_lock`
/// checks via presence alone (`Path::is_file`) — a half-written file would
/// resolve exactly as a complete one. The pid-suffixed temp name lives in
/// the same directory (same filesystem, so `rename(2)` is atomic) and is
/// cleaned up on every error path, so the final path is only ever created
/// by a rename of a fully-written file.
///
/// The containing directory is created owner-only (`ensure_owner_only_dir`,
/// the same helper [`worktree_attach_lock_path`]'s own callers already use
/// for the sibling lock directory) — this is now a directory outside any
/// candidate's control, so nothing else needs to defend it, but an
/// attacker with same-uid access to this process's own `state_dir()` was
/// never in scope for any check on this path (the honest same-uid ceiling
/// every mechanism here shares with [`crate::worktree_reclaim::owned_git_dir`]).
///
/// [`mark_worktree_owned`]: crate::worktree_reclaim::mark_worktree_owned
fn write_isolated_clone_provenance(
    source_dir: &Path,
    clone_dir: &Path,
    name: &str,
    creator: &str,
) -> Result<(), String> {
    let marker_path = isolated_clone_provenance_path(clone_dir);
    let parent = marker_path.parent().expect(
        "isolated_clone_provenance_path always nests under state_dir(), which has a parent",
    );
    crate::platform::fsperm::ensure_owner_only_dir(parent).map_err(|e| {
        format!(
            "failed to prepare isolated-clone provenance directory {}: {e}",
            parent.display()
        )
    })?;
    let file_name = marker_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("provenance");
    let tmp_path = parent.join(format!("{file_name}.{}.tmp", std::process::id()));

    let root_hash = crate::platform::lock::fnv1a64(
        canonicalize_best_effort(source_dir)
            .to_string_lossy()
            .as_bytes(),
    );
    let canonical_clone_dir = canonicalize_best_effort(clone_dir);
    let safe_name = crate::worktree_reclaim::sanitize_marker_creator(name);
    let safe_creator = crate::worktree_reclaim::sanitize_marker_creator(creator);
    let content = format!(
        "schema=2\nroot-hash={root_hash:016x}\nname={safe_name}\ncreator={safe_creator}\npath={}\n",
        canonical_clone_dir.display()
    );

    std::fs::write(&tmp_path, content.as_bytes()).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        format!("failed to write isolated-clone provenance artifact: {e}")
    })?;
    std::fs::rename(&tmp_path, &marker_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        format!("failed to finalize isolated-clone provenance artifact: {e}")
    })
}

/// Fork issue #766: rewrites an isolated clone's provenance marker's
/// `creator=` line in place once [`resume_existing_isolated_clone`]'s
/// legacy-format fallback has accepted a resume — so a workspace opened
/// under a pre-fork#760 build only ever needs that fallback once, not on
/// every subsequent resume. Reads the CURRENT on-disk content itself
/// (rather than trusting a caller-supplied copy that may have gone stale
/// between the read that fed the comparison and this write) and replaces
/// ONLY the line beginning `creator=`, leaving `schema=`/`root-hash=`/
/// `name=`/`pinned=`/`path=` (present or not, in whatever order a future
/// schema adds them) byte-for-byte untouched — deliberately not a full
/// reconstruction like [`write_isolated_clone_provenance`]/
/// [`set_isolated_clone_pinned`], which would need to already know every
/// field name that can appear.
///
/// Same atomic write-then-rename idiom as its siblings above, and reuses
/// [`pin_temp_disambiguator`] for the temp-file suffix rather than adding a
/// third per-process nonce/seq pair for the identical concurrent-rewrite
/// hazard that helper's own doc comment already describes.
fn migrate_legacy_isolated_clone_creator(
    marker_path: &Path,
    new_creator: &str,
) -> Result<(), String> {
    let content = std::fs::read_to_string(marker_path)
        .map_err(|e| format!("failed to read provenance artifact for migration: {e}"))?;
    let safe_creator = crate::worktree_reclaim::sanitize_marker_creator(new_creator);
    let rewritten: String = content
        .lines()
        .map(|line| {
            if line.starts_with("creator=") {
                format!("creator={safe_creator}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";

    let parent = marker_path.parent().expect(
        "isolated_clone_provenance_path always nests under state_dir(), which has a parent",
    );
    let file_name = marker_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("provenance");
    let tmp_path = parent.join(format!("{file_name}.{}.tmp", pin_temp_disambiguator()));
    std::fs::write(&tmp_path, rewritten.as_bytes()).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        format!("failed to write migrated provenance artifact: {e}")
    })?;
    std::fs::rename(&tmp_path, marker_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        format!("failed to finalize migrated provenance artifact: {e}")
    })
}

/// Per-process disambiguator for [`set_isolated_clone_pinned`]'s temp-file
/// suffix (fork issue #597): before this helper existed that suffix was
/// `std::process::id()` alone, which two concurrent pin/unpin calls against
/// the same clone within one process collide on identically, writing to the
/// same temp path and publishing a torn write. Shares
/// [`crate::agent_pty::mint_nonce_seq`]'s recipe but keeps its own
/// `NONCE`/`SEQ` statics, never shared with any other call site's pair
/// (`spawn.rs::PANE_NONCE`/`PANE_SEQ`, `ui.rs::ORCHESTRATION_CLAIM_TOKEN_*`),
/// per that function's own doc comment on why each id space stays separate.
fn pin_temp_disambiguator() -> String {
    static NONCE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let (nonce, seq) = crate::agent_pty::mint_nonce_seq(&NONCE, &SEQ);
    format!("{nonce:016x}-{seq}")
}

/// Fork issue #546 hazard 2 (maintainer-decided design): rewrite an
/// isolated clone's existing M4b provenance artifact in place as
/// `schema=3`, preserving its `root-hash=`/`name=`/`creator=`/`path=`
/// fields exactly as [`isolated_clone_provenance_field`] reads them back
/// (never re-derived from `clone_dir` itself — this is a REWRITE of
/// existing evidence, not a fresh [`write_isolated_clone_provenance`]) and
/// adding a `pinned=` line recording `pinned`. Shared by
/// [`pin_isolated_clone`] (`pinned=true`) and [`unpin_isolated_clone`]
/// (`pinned=false`) rather than each duplicating the read-preserve-rewrite
/// dance.
///
/// Requires an existing provenance artifact — [`std::fs::read_to_string`]
/// surfaces the same `io::Error` (typically `NotFound`) a caller would get
/// reading it directly if `clone_dir` was never provisioned by this deck,
/// or was already forgotten ([`forget_isolated_workspace`]); this function
/// never fabricates a fresh artifact for a clone that doesn't already have
/// one. A missing `root-hash=`/`path=` field on an otherwise-parseable
/// artifact is likewise refused rather than silently written with an
/// empty value, since a widened [`isolated_clone_provenance_field`] schema
/// check (`schema=2` OR `schema=3`) means this can run against a clone
/// this same call already rewrote to `schema=3` on a previous pin/unpin.
///
/// Atomic write-then-rename, the identical pattern
/// [`write_isolated_clone_provenance`] already uses and for the same
/// reason (see that function's own doc comment): a partially-written
/// artifact must never be observable at the final path, since
/// [`crate::worktree_reclaim::candidate_has_attach_lock`] and every reader
/// of this artifact trust presence alone.
///
/// Field order in the rewritten content matters and is not incidental
/// (review-findings fix round, reviewer/auditor L1/F5): `path=` is NOT
/// sanitized (see [`write_isolated_clone_provenance`]'s own doc comment for
/// why), so every field this rewrite trusts is written BEFORE `path=` --
/// `pinned=` included -- exactly mirroring the write path's own
/// `creator=`/`name=` ordering. [`isolated_clone_provenance_field`]'s read
/// side returns the FIRST line matching a given key, so a `pinned=` line
/// appearing after `path=` would let a hypothetically-injected line inside
/// an unsanitized `path` value shadow the real `pinned=` field; writing it
/// first closes that off the same way the original ordering closes it for
/// `creator=`.
///
/// Re-asserts the containing directory owner-only (`ensure_owner_only_dir`,
/// review-findings fix round, reviewer/auditor L2) exactly as
/// [`write_isolated_clone_provenance`] does on every write to this same
/// location -- this call rewrites the artifact in place rather than
/// creating it fresh, but nothing else re-asserts the permission on the
/// rewrite path if this doesn't.
fn set_isolated_clone_pinned(clone_dir: &Path, pinned: bool) -> std::io::Result<()> {
    let marker_path = isolated_clone_provenance_path(clone_dir);
    let content = std::fs::read_to_string(&marker_path)?;
    let root_hash = isolated_clone_provenance_field(&content, "root-hash").ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "provenance artifact at {} has no root-hash= field to preserve",
                marker_path.display()
            ),
        )
    })?;
    let path = isolated_clone_provenance_field(&content, "path").ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "provenance artifact at {} has no path= field to preserve",
                marker_path.display()
            ),
        )
    })?;
    let name = isolated_clone_provenance_field(&content, "name").unwrap_or_default();
    let creator = isolated_clone_provenance_field(&content, "creator").unwrap_or_default();

    let parent = marker_path.parent().expect(
        "isolated_clone_provenance_path always nests under state_dir(), which has a parent",
    );
    crate::platform::fsperm::ensure_owner_only_dir(parent)?;
    let file_name = marker_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("provenance");
    let tmp_path = parent.join(format!("{file_name}.{}.tmp", pin_temp_disambiguator()));
    let new_content = format!(
        "schema=3\nroot-hash={root_hash}\nname={name}\ncreator={creator}\npinned={pinned}\npath={path}\n"
    );

    std::fs::write(&tmp_path, new_content.as_bytes()).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp_path);
    })?;
    std::fs::rename(&tmp_path, &marker_path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp_path);
    })
}

/// Pin an isolated clone against fork#325 M4c's automatic reclaim (fork
/// issue #546 hazard 2): rewrites its provenance artifact to `schema=3`
/// with `pinned=true`, which [`crate::worktree_reclaim::isolated_clone_report`]
/// then AND's into its reclaim-eligibility check alongside the five
/// existing gates. See [`set_isolated_clone_pinned`] for the rewrite
/// itself. Wired to `worktree pin <path>` (fork issue #597, `src/main.rs`).
pub fn pin_isolated_clone(clone_dir: &Path) -> std::io::Result<()> {
    set_isolated_clone_pinned(clone_dir, true)
}

/// Unpin an isolated clone previously pinned via [`pin_isolated_clone`]
/// (fork issue #546 hazard 2), or explicitly record `pinned=false` on one
/// that was never pinned to begin with — both rewrite the provenance
/// artifact to `schema=3` with `pinned=false`, and both must succeed
/// rather than requiring a prior pin call first. See
/// [`set_isolated_clone_pinned`] for the rewrite itself, and that
/// function's own doc comment for why a clone with no existing provenance
/// artifact is refused rather than given a fresh one here. Wired to
/// `worktree unpin <path>` (fork issue #597, `src/main.rs`).
pub fn unpin_isolated_clone(clone_dir: &Path) -> std::io::Result<()> {
    set_isolated_clone_pinned(clone_dir, false)
}

/// PRD fork#544 M5: the only deliberate way a named workspace is ever
/// cleared — modeled on this codebase's existing `issue claim --takeover
/// --confirm-stopped` pattern (refuse by default, require an explicit
/// confirming flag) rather than on
/// [`crate::worktree_reclaim::remove_isolated_clone_dir`]. That function's
/// checks (branch re-resolution, a clean tree, an empty stash list, and —
/// the one that actually rules it out — the clone's HEAD matching a
/// **merged PR's** `headRefOid`) all encode "this was already reclaimed
/// after its PR merged"; a persistent workspace being explicitly forgotten
/// has no such PR, may be mid-work, and may carry uncommitted changes the
/// caller has every right to discard on purpose. Requiring those
/// preconditions here would make `forget` refuse in exactly the cases a
/// caller asked it to handle. It is also private (`fn`, not `pub(crate)`)
/// to `worktree_reclaim.rs`. So this reimplements only the directory
/// removal, not the reclaim-specific revalidation.
///
/// Ownership is proven the same way [`resume_existing_isolated_clone`]
/// already proves it for resume: presence of the M4b provenance artifact
/// at [`isolated_clone_provenance_path`]. A confirming flag alone only
/// proves the caller wants to remove *a* workspace of its own — not that
/// `clone_dir` *is* one, which is exactly the stranger-directory hazard
/// `ResumeRejection::Stranger` already guards against on the resume side.
///
/// `#[allow(dead_code)]`: PRD fork#544 M5 deliberately ships no CLI wiring
/// yet (that is a later milestone) — this function is exercised directly
/// by `orchestration/workspace/014`-`017` and nothing else calls it today,
/// the same honest state [`create_worktree_sync`]'s own `#[allow(dead_code)]`
/// documents rather than papering over with a synthetic caller.
#[allow(dead_code)]
pub(crate) fn forget_isolated_workspace(
    clone_dir: &Path,
    confirmed: bool,
    remover: &str,
) -> Result<(), String> {
    if !confirmed {
        return Err(
            "refusing to forget: pass an explicit confirming flag to acknowledge that this \
             permanently removes the workspace directory and its provenance record -- nothing \
             was touched"
                .to_string(),
        );
    }

    let marker_path = isolated_clone_provenance_path(clone_dir);
    if !marker_path.is_file() {
        return Err(format!(
            "refusing to forget {}: no M4b ownership evidence found for this workspace \
             (expected a provenance artifact at {}) -- it may never have been created by this \
             deck, or may already have been forgotten",
            clone_dir.display(),
            marker_path.display()
        ));
    }

    // Bundled hardening (PRD fork#544 review-findings fix round):
    // `attempt_isolated_clone_cleanup` guards its own `remove_dir_all` with
    // `clone_dir.join(".git").is_dir()`, making that destructive,
    // unconditional removal self-evidently safe to a future reader --
    // this function performed the same unconditional removal with no
    // equivalent guard. Skipped when `clone_dir` doesn't exist at all (the
    // "already gone, tolerated" case immediately below): there is nothing
    // to protect there, and `remove_dir_all`'s own `NotFound` tolerance
    // already covers it.
    if clone_dir.exists() && !clone_dir.join(".git").is_dir() {
        return Err(format!(
            "refusing to forget {}: no .git directory found there -- not something this \
             deck's forget could safely remove",
            clone_dir.display()
        ));
    }

    // Bundled hardening (PRD fork#544 review-findings fix round): today
    // the sole authority for removal is `marker_path.is_file()` above --
    // presence alone, never checking what the artifact actually NAMES. M4
    // writes `path=<canonical clone dir>` into it; read it back (when the
    // artifact carries the `schema=2` tag `isolated_clone_provenance_field`
    // requires -- a pre-M4 `b"deck\n"` artifact has no `path=` to check,
    // and is tolerated exactly as `resume_existing_isolated_clone`'s own
    // evidence-presence check tolerates it) and confirm it names THIS
    // directory before trusting it. Closes the gap where a stale artifact
    // -- one a checkout failure predating this fix round's Fix 4 never
    // cleaned up, or one surviving some other path -- could authorise
    // deleting the wrong directory.
    if let Ok(content) = std::fs::read_to_string(&marker_path)
        && let Some(stored_path) = isolated_clone_provenance_field(&content, "path")
    {
        let canonical_clone_dir = canonicalize_best_effort(clone_dir);
        if stored_path != canonical_clone_dir.to_string_lossy() {
            return Err(format!(
                "refusing to forget {}: the provenance artifact at {} names a different \
                 directory ({stored_path}) -- stale evidence, not trusted",
                clone_dir.display(),
                marker_path.display()
            ));
        }
    }

    // Tolerate the directory already being gone (fork#325/M4b's own
    // `remove_isolated_clone_dir` precedent): the provenance artifact is
    // the durable ownership record, so a caller retrying a forget whose
    // directory removal previously succeeded but whose artifact removal
    // then failed must still be able to finish the job.
    if let Err(e) = std::fs::remove_dir_all(clone_dir)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        return Err(format!(
            "failed to remove workspace directory {} (requested by {remover}): {e}",
            clone_dir.display()
        ));
    }
    std::fs::remove_file(&marker_path).map_err(|e| {
        format!(
            "failed to remove provenance artifact {} (requested by {remover}): {e}",
            marker_path.display()
        )
    })?;

    // Issue #325 / reviewer B1 / auditor F2 precedent, carried to this
    // removal path too: the ONLY durable trace of a confirmed removal.
    // `remover` is an unauthenticated, caller-supplied string, sanitized
    // here exactly as `remove_isolated_clone_dir` sanitizes its own.
    tracing::info!(
        path = %sanitize_path_for_terminal_display(clone_dir),
        remover = %sanitize_for_terminal_display(remover),
        "isolated workspace forgotten"
    );
    Ok(())
}

/// Read-only `git fetch origin` in `clone_dir` — updates only the
/// `origin/*` remote-tracking refs, never the checked-out local branch or
/// working tree. Shared by [`sync_merged_workspace_to_main`] (which needs an
/// up-to-date `origin/<default_branch>` before it can check ancestry) and
/// [`fetch_other_live_workspace`] (for which this fetch IS the entire
/// operation).
fn fetch_origin(clone_dir: &Path) -> Result<(), String> {
    run_status_sync(
        &[
            "-C".to_string(),
            clone_dir.to_string_lossy().into_owned(),
            "fetch".to_string(),
            "--quiet".to_string(),
            "origin".to_string(),
        ],
        WORKTREE_GIT_TIMEOUT,
    )
    .map_err(|e| {
        let msg = match e {
            AddError::TimedOut(m) | AddError::Failed(m) => m,
        };
        format!("git fetch origin failed in {}: {msg}", clone_dir.display())
    })
}

/// PRD fork#544 M7 (Design step 7 / Decisions table row "What happens to
/// other live workspaces, and to this one, when a merge lands?"). Outcome of
/// [`sync_merged_workspace_to_main`] — its own `Debug` output is what
/// `orchestration/workspace/020`-`022` match against.
///
/// Fork issue #744 wired [`sync_merged_workspace_to_main`] into a real call
/// site (`dot-agent-deck worktree sync`, via
/// [`crate::worktree_reclaim::run_sync`]), which reads `reason` off
/// `LeftUntouched` to report it — no `#[allow(dead_code)]` needed here
/// anymore.
#[derive(Debug)]
pub(crate) enum PostMergeSyncOutcome {
    /// Both preconditions held: local `<default_branch>` now sits exactly
    /// at `origin/<default_branch>`'s SHA, and the workspace is checked out
    /// on it.
    SwitchedToMain,
    /// At least one precondition failed — nothing was touched (no checkout,
    /// no merge, no mutation of any kind). `reason` names which one, for a
    /// caller to show the user.
    LeftUntouched { reason: String },
}

/// PRD fork#544 M7: auto-switch a just-merged workspace onto
/// `default_branch` — but only when it is safe to do so without discarding
/// or stranding anything the merge itself did not capture. Two independent
/// preconditions, both checked (per the Risks section: a tree-cleanliness-
/// only check would wrongly pass `orchestration/workspace/022`'s extra-
/// local-commit case, and an ancestry-only check would wrongly pass
/// `021`'s uncommitted-edit case):
///
/// 1. No uncommitted changes (`git status --porcelain` is empty — this one
///    check already covers staged, unstaged, and untracked files).
/// 2. No local commits beyond the merge: after fetching `origin`, `HEAD` is
///    fully contained in `origin/<default_branch>`'s history
///    (`git merge-base --is-ancestor HEAD origin/<default_branch>`).
///
/// If both hold, a THIRD check (reviewer S3, review-findings fix round)
/// covers a gap neither precondition above catches: LOCAL `default_branch`
/// itself may have separately diverged from `origin/<default_branch>` (a
/// maintainer committing directly to their own local main, say) — the two
/// preconditions above are both checked against HEAD (this workspace's own
/// just-merged feature branch), not against `default_branch`, so neither
/// sees this coming. The starting ref is captured before anything below
/// moves HEAD, so a merge failure further down can restore it.
///
/// Switches onto `default_branch` — git's DWIM creates a local tracking
/// branch from `origin/<default_branch>` when none exists yet, the same
/// mechanism `provision_isolated_clone_sync_attaches_branch_that_
/// exists_only_as_remote_tracking_ref` already relies on elsewhere in this
/// module — and then fast-forwards it with `git merge --ff-only
/// origin/<default_branch>`. `--ff-only`, deliberately never `reset --hard`:
/// an incorrect ancestry check then fails loudly instead of silently
/// discarding anything, and the explicit merge step is what actually lands
/// local `default_branch` exactly on `origin/<default_branch>`'s SHA in the
/// (typical) case where a local branch of that name already exists from the
/// clone's own initial checkout, stale, rather than only being created fresh
/// by the DWIM. `git switch`, not `git checkout` (auditor A5, review-
/// findings fix round): `checkout`'s own `--` separator doesn't protect an
/// option-shaped `default_branch` value the way it looks like it should
/// (verified empirically — see the inline comment at the call site), so an
/// adversarial-looking branch name silently detached HEAD instead of
/// landing on it; `switch`'s `--` genuinely separates options from the
/// target ref.
///
/// If the merge fails after a successful switch, the workspace is restored
/// to its starting ref before the error is returned (reviewer S3) — no
/// `Err` result may ever leave the workspace anywhere other than where it
/// started.
///
/// Wired into a real call site by fork issue #744: `dot-agent-deck worktree
/// sync` calls this for every sibling isolated clone whose own branch is
/// confirmed MERGED, via [`crate::worktree_reclaim::run_sync`]. Directly
/// exercised by `orchestration/workspace/020`-`022`/`028`/`030`/`032`, and
/// indirectly by `orchestration/workspace/033`-`035` through that CLI
/// surface.
pub(crate) fn sync_merged_workspace_to_main(
    clone_dir: &Path,
    default_branch: &str,
) -> Result<PostMergeSyncOutcome, String> {
    // Reviewer S4 / auditor F3 (PRD fork#544 review-findings fix round 3):
    // a `default_branch` git itself refuses to CREATE as a real branch is
    // never a valid input to this function. The `update-ref` route further
    // down was chosen specifically BECAUSE it sidesteps `check-ref-format`'s
    // branch-CREATION validation (see that call's own doc comment) -- which
    // means an adversarial-looking `default_branch` (`--detach`; `HEAD`
    // itself, which `git check-ref-format --branch` genuinely also rejects,
    // verified empirically) would otherwise get written as a literal ref
    // via `update-ref` rather than refused, and every later git invocation
    // against this workspace then inherits that as a live hazard --
    // `refs/heads/HEAD` in particular permanently breaks `rev-parse
    // --abbrev-ref HEAD`, this function's own dependency further down.
    // Validate up front, before touching the workspace at all, and refuse
    // outright rather than proceeding to manufacture an illegal ref.
    let clone_dir_str = clone_dir.to_string_lossy().into_owned();

    // Final cleanup round (reviewer): `-C <clone_dir_str>` pins this gate's
    // cwd, so a cwd-dependent input like `@{-1}` is resolved against the
    // workspace being validated rather than against wherever this deck
    // process itself happens to be running from -- not independently
    // exploitable (the `update-ref` route further down still refuses a
    // literal `@{-1}` component the same way it refuses `--detach`), but a
    // determinism gap worth closing on a gate this security-relevant.
    if run_status_sync(
        &[
            "-C".to_string(),
            clone_dir_str.clone(),
            "check-ref-format".to_string(),
            "--branch".to_string(),
            default_branch.to_string(),
        ],
        WORKTREE_GIT_TIMEOUT,
    )
    .is_err()
    {
        return Err(format!(
            "refusing to sync {}: {default_branch:?} is not a valid git branch name",
            clone_dir.display()
        ));
    }

    let status_stdout = run_capture_sync(
        &[
            "-C".to_string(),
            clone_dir_str.clone(),
            "status".to_string(),
            "--porcelain".to_string(),
        ],
        WORKTREE_GIT_TIMEOUT,
    )
    .map_err(|e| {
        let msg = match e {
            AddError::TimedOut(m) | AddError::Failed(m) => m,
        };
        format!(
            "failed to check {} for uncommitted changes: {msg}",
            clone_dir.display()
        )
    })?;
    if !status_stdout.is_empty() {
        return Ok(PostMergeSyncOutcome::LeftUntouched {
            reason: "the workspace has uncommitted changes -- refusing to switch and \
                     potentially discard unprotected work"
                .to_string(),
        });
    }

    fetch_origin(clone_dir)?;

    let remote_ref = format!("origin/{default_branch}");
    let ancestor_status = run_status_sync(
        &[
            "-C".to_string(),
            clone_dir_str.clone(),
            "merge-base".to_string(),
            "--is-ancestor".to_string(),
            "HEAD".to_string(),
            remote_ref.clone(),
        ],
        WORKTREE_GIT_TIMEOUT,
    );
    if ancestor_status.is_err() {
        return Ok(PostMergeSyncOutcome::LeftUntouched {
            reason: format!(
                "the workspace's HEAD is not fully contained in {remote_ref} -- it carries a \
                 local commit the merge never captured, refusing to switch and potentially \
                 strand or discard it"
            ),
        });
    }

    // Reviewer S3 (PRD fork#544 review-findings fix round): the two
    // preconditions above are both checked against HEAD (this workspace's
    // own just-merged feature branch), but the mutation below targets
    // `default_branch` instead -- a LOCAL `default_branch` that has
    // separately diverged from `origin/<default_branch>` (e.g. a
    // maintainer committing directly to their own local main) is caught by
    // NEITHER precondition, so the switch below can succeed while the
    // follow-up `merge --ff-only` fails. Capture the starting ref NOW,
    // before the switch ever moves HEAD, so a merge failure below can
    // restore it -- the property that actually matters is "no `Err` result
    // ever leaves the workspace anywhere but where it started," not
    // catching the divergence as an earlier precondition failure.
    // `--abbrev-ref` reports the literal string "HEAD" when detached (this
    // function's own preconditions above make that unreachable in
    // practice, since a detached HEAD's "branch" can't itself be resumed
    // as a named workspace, but the fallback keeps this correct rather
    // than merely untested for that case): restore via the exact SHA
    // instead of treating "HEAD" as a branch name.
    let starting_ref_name = run_capture_sync(
        &[
            "-C".to_string(),
            clone_dir_str.clone(),
            "rev-parse".to_string(),
            "--abbrev-ref".to_string(),
            "HEAD".to_string(),
        ],
        WORKTREE_GIT_TIMEOUT,
    )
    .map(|s| s.trim().to_string())
    .map_err(|e| {
        let msg = match e {
            AddError::TimedOut(m) | AddError::Failed(m) => m,
        };
        format!(
            "failed to resolve the workspace's own starting branch in {}: {msg}",
            clone_dir.display()
        )
    })?;
    let starting_head_sha = if starting_ref_name == "HEAD" {
        run_capture_sync(
            &[
                "-C".to_string(),
                clone_dir_str.clone(),
                "rev-parse".to_string(),
                "HEAD".to_string(),
            ],
            WORKTREE_GIT_TIMEOUT,
        )
        .map(|s| s.trim().to_string())
        .map_err(|e| {
            let msg = match e {
                AddError::TimedOut(m) | AddError::Failed(m) => m,
            };
            format!(
                "failed to resolve the workspace's own starting commit in {}: {msg}",
                clone_dir.display()
            )
        })?
    } else {
        String::new()
    };

    // Auditor A5 (PRD fork#544 review-findings fix round): `git checkout
    // --quiet {default_branch}` had no `--` end-of-options separator,
    // unlike the `merge` call below -- an option-shaped `default_branch`
    // value (e.g. `--detach`) is then consumed as a FLAG rather than the
    // branch to check out, silently detaching HEAD instead of landing on
    // the named branch. Verified empirically that `checkout`'s own `--`
    // does NOT fix this (its `--` separates a tree-ish from PATHSPECS, not
    // options from the branch itself -- `checkout -- <branch>` treats
    // `<branch>` as a pathspec, and `checkout <branch> --` still consumes
    // an option-shaped `<branch>` as a flag) -- `git switch`'s `--`
    // genuinely does separate options from the target ref, so this uses
    // `switch` instead.
    //
    // `switch`'s (and `checkout`'s) own DWIM -- creating a local tracking
    // branch when `default_branch` exists on `origin` only as a
    // remote-tracking ref -- goes through the SAME `check-ref-format`
    // branch-name validation `git branch` itself applies, which refuses to
    // CREATE any ref component starting with `-` (verified empirically:
    // `git branch --track -- --detach origin/--detach` fails with `fatal:
    // '--detach' is not a valid branch name`, independent of any `--`
    // separator anywhere in the invoking argv) -- so DWIM alone can never
    // land on an adversarial-looking name like `--detach`, regardless of
    // how the outer command's argv is escaped. `git update-ref`, a
    // low-level ref write, is NOT subject to that same branch-name
    // validation, so it is used here to create the local tracking branch
    // directly when one doesn't already exist -- after which `switch`
    // above needs no DWIM at all, just a lookup of a ref that's already
    // there. Only ever creates, never overwrites: an existing local
    // `default_branch` (the ordinary case, and Fix 5's own diverged-local-
    // branch scenario) is left exactly as it is, since overwriting it here
    // would erase the very divergence Fix 5's own restore-on-failure path
    // depends on being able to see.
    let local_branch_ref = format!("refs/heads/{default_branch}");
    let local_branch_exists = spawn_and_wait_sync(
        &[
            "-C",
            &clone_dir_str,
            "rev-parse",
            "--verify",
            "--quiet",
            &local_branch_ref,
        ],
        WORKTREE_GIT_TIMEOUT,
    )
    .is_ok_and(|out| out.status.success());
    if !local_branch_exists {
        let create_local_branch_status = run_status_sync(
            &[
                "-C".to_string(),
                clone_dir_str.clone(),
                "update-ref".to_string(),
                "--".to_string(),
                local_branch_ref,
                remote_ref.clone(),
            ],
            WORKTREE_GIT_TIMEOUT,
        );
        if let Err(e) = create_local_branch_status {
            let msg = match e {
                AddError::TimedOut(m) | AddError::Failed(m) => m,
            };
            return Err(format!(
                "failed to create a local tracking branch for {default_branch} from \
                 {remote_ref} in {}: {msg}",
                clone_dir.display()
            ));
        }

        // Reviewer S3 (PRD fork#544 review-findings fix round 3): `update-ref`
        // above writes only the ref itself -- no tracking configuration at
        // all, unlike ordinary DWIM branch creation -- which directly
        // contradicts this whole mechanism's own stated purpose ("so
        // ahead/behind info stays accurate," which needs an upstream to even
        // compute). `git branch --set-upstream-to` is not subject to
        // `check-ref-format`'s branch-CREATION validation the way `git
        // branch`/`git switch`'s own DWIM is -- only ref CREATION is what
        // `update-ref` was chosen to sidestep, and this sets tracking on a
        // ref that already exists -- so this doesn't reopen the hazard
        // `update-ref` exists to avoid. A hard error, not a best-effort
        // warning: `remote_ref` was just used to create `local_branch_ref`
        // above, so it is already known to resolve, and silently swallowing
        // a failure here would silently reproduce the exact bug this fixes.
        let set_upstream_status = run_status_sync(
            &[
                "-C".to_string(),
                clone_dir_str.clone(),
                "branch".to_string(),
                format!("--set-upstream-to={remote_ref}"),
                "--".to_string(),
                default_branch.to_string(),
            ],
            WORKTREE_GIT_TIMEOUT,
        );
        if let Err(e) = set_upstream_status {
            let msg = match e {
                AddError::TimedOut(m) | AddError::Failed(m) => m,
            };
            return Err(format!(
                "failed to set upstream tracking for the newly created {default_branch} to \
                 {remote_ref} in {}: {msg}",
                clone_dir.display()
            ));
        }
    }

    let switch_status = run_status_sync(
        &[
            "-C".to_string(),
            clone_dir_str.clone(),
            "switch".to_string(),
            "--quiet".to_string(),
            "--".to_string(),
            default_branch.to_string(),
        ],
        WORKTREE_GIT_TIMEOUT,
    );
    if let Err(e) = switch_status {
        let msg = match e {
            AddError::TimedOut(m) | AddError::Failed(m) => m,
        };
        return Err(format!(
            "git switch {default_branch} failed in {}: {msg}",
            clone_dir.display()
        ));
    }

    let merge_status = run_status_sync(
        &[
            "-C".to_string(),
            clone_dir_str.clone(),
            "merge".to_string(),
            "--quiet".to_string(),
            "--ff-only".to_string(),
            "--".to_string(),
            remote_ref.clone(),
        ],
        WORKTREE_GIT_TIMEOUT,
    );
    if let Err(e) = merge_status {
        let msg = match e {
            AddError::TimedOut(m) | AddError::Failed(m) => m,
        };
        // Reviewer S3: restore the workspace to exactly where it started
        // before the switch above moved it -- best-effort, since the merge
        // failure is already the error that matters to the caller; a
        // restore failure is logged, not layered into the returned text.
        // The ordinary case restores onto the SAME NAMED BRANCH it started
        // on (never detached, matching `--abbrev-ref HEAD`'s original
        // report); only a genuinely detached starting HEAD restores via
        // `--detach <sha>`, since there is no branch name to give back.
        let mut restore_args = vec![
            "-C".to_string(),
            clone_dir_str.clone(),
            "switch".to_string(),
            "--quiet".to_string(),
        ];
        let restore_target = if starting_ref_name == "HEAD" {
            restore_args.push("--detach".to_string());
            &starting_head_sha
        } else {
            &starting_ref_name
        };
        restore_args.push("--".to_string());
        restore_args.push(restore_target.clone());
        if let Err(restore_err) = run_status_sync(&restore_args, WORKTREE_GIT_TIMEOUT) {
            tracing::warn!(
                clone = %clone_dir.display(),
                restore_target = %restore_target,
                error = ?restore_err,
                "issue-dispatch: failed to restore the workspace to its starting ref after a \
                 failed post-merge sync; the workspace may be left switched onto \
                 {default_branch} instead of its original branch"
            );
        }
        return Err(format!(
            "git merge --ff-only {remote_ref} failed in {}: {msg}",
            clone_dir.display()
        ));
    }

    Ok(PostMergeSyncOutcome::SwitchedToMain)
}

/// PRD fork#544 M7 (Decisions table row, "other live workspaces" half): on
/// an UNRELATED merge (this workspace's own branch did not just merge),
/// only a proactive, read-only fetch runs — see [`fetch_origin`] for what
/// that does and does not touch.
///
/// Wired into a real call site by fork issue #744: `dot-agent-deck worktree
/// sync` calls this for every sibling isolated clone whose own branch is
/// NOT known to be merged, via [`crate::worktree_reclaim::run_sync`].
/// Directly exercised by `orchestration/workspace/023`, and indirectly by
/// `orchestration/workspace/033`-`035` through that CLI surface.
pub(crate) fn fetch_other_live_workspace(clone_dir: &Path) -> Result<(), String> {
    fetch_origin(clone_dir)
}

/// PRD fork#544 M3: distinguishable refusal reasons for
/// [`resume_existing_isolated_clone`] — this enum's own `Debug` output is
/// the interface `orchestration/workspace/006`-`008` assert substrings
/// against (`"stranger"`, `"ancestry"`, `"unhealthy"`), so the variant
/// names are chosen to read naturally in that form rather than for any
/// other convention.
#[derive(Debug)]
pub(crate) enum ResumeRejection {
    /// No M4b ownership evidence for this canonical path at all — a
    /// directory this deck never created (or wrote evidence for) at this
    /// exact location.
    Stranger,
    /// M4b evidence is present, but `clone_dir`'s origin/shared history is
    /// PROVEN not to match `source_dir` — the same Name typed against a
    /// different underlying project (Problem Statement #4): either the two
    /// sides' `origin` URLs genuinely disagree, or `git merge-base` ran to
    /// completion and reported no common ancestor (exit 1).
    AncestryMismatch,
    /// PRD fork#544 review-findings fix round 3 (reviewer S1): the ancestry
    /// probe could not reach a verdict either way — `git merge-base` never
    /// got to run a conclusive comparison (its own best-effort fetch to
    /// pull in a missing object failed, or the probe itself failed to
    /// spawn), so there is no positive evidence the two sides share history
    /// AND no proof they don't. Refused the same as [`Self::AncestryMismatch`]
    /// (no positive evidence of shared history means no resume), but kept
    /// distinguishable from it: unlike a proven mismatch, this is not
    /// evidence the directory belongs to a different project, so
    /// [`Self::describe`]'s "remove it manually" advice does not apply here.
    AncestryUnverifiable,
    /// The directory's git state could not be read (`git rev-parse HEAD`
    /// failed) — refused only, never auto-deleted or auto-repaired.
    Unhealthy,
    /// Evidence, ancestry and health all passed, but another call already
    /// resumed this exact canonical path first, inside the very attach-lock
    /// window this call also went through — the loser of the race the
    /// PRD's own Design step 3 names.
    Contested,
    /// Evidence, ancestry and health all passed, and this call won the
    /// Contested race, but the provenance artifact's own `creator=` field
    /// (when present) names a DIFFERENT identity than the one making this
    /// call (PRD fork#544 review-findings fix round, reviewer B2). At the
    /// time this variant was introduced, `creator` was derived from the
    /// typed Worktree slug directly, and `sanitize_workspace_segment`/
    /// `resolve_workspace_path` aren't injective — so two distinct typed
    /// slugs (`fix/544` and `fix-544`, say) could sanitize to the identical
    /// derived path while still producing two different `creator` strings,
    /// and this check caught the second one attaching silently to the
    /// first's directory.
    ///
    /// PRD fork#760 fix round (reviewer B2, then auditor F1/N1): `creator`
    /// is now derived from the full resolved `clone_dir`
    /// (`workspace_resolution.worktree_path`) rather than from the bare
    /// slug or the typed Name — see `orchestration_creator_string`'s own
    /// doc. That closes the collision above and, AT THE TIME, made it
    /// unreachable by construction for two slugs colliding to one path,
    /// since `creator` was purely a function of `clone_dir` and reopening
    /// under the IDENTICAL slug with a DIFFERENT Name resumed correctly
    /// rather than tripping this variant.
    ///
    /// Fork issue #763 (auditor A1/A2, reviewer F1/F2, BLOCKER-class):
    /// that "unreachable by construction" claim is now FALSE for one
    /// shape, and this variant is that shape's ENTIRE remaining defense.
    /// A typed Worktree slug on a nested pick deliberately makes
    /// `physical_worktree_path` (`src/ui.rs`) diverge from `creator`'s own
    /// seed, `workspace_resolution.worktree_path` — two DIFFERENT nested
    /// picks sharing one typed slug now provision into the SAME on-disk
    /// `clone_dir` while still being two DIFFERENT `creator` identities
    /// (see that call site's own comment for the digest that keeps them
    /// distinguishable even under `sanitize_marker_creator`'s 200-char
    /// truncation). `stored_creator != sanitize_marker_creator(creator)`
    /// is exactly what now catches the second pick and refuses it here.
    ///
    /// This is NOT "refused earlier and separately, by
    /// `ClaimOrchestrationName`'s own cwd clause, before provisioning is
    /// ever reached" (the claim this doc used to make, and which was true
    /// before #763) — the auditor traced it precisely: the two nested
    /// picks' claim `cwd` values still carry the distinct relative subpath
    /// appended underneath the now-shared clone directory
    /// (`orchestration_claim_cwd_path`, `src/ui.rs`), so the two `cwd`
    /// strings are genuinely DIFFERENT and `claim_orchestration_name`
    /// (`src/agent_pty.rs`) lets BOTH claims through — the cwd clause never
    /// arbitrates this collision class at all. What protects it instead,
    /// end to end: this creator comparison (the load-bearing check), plus
    /// the cross-process `flock` `acquire_worktree_lock_sync` takes on
    /// `worktree_attach_lock_path` inside `provision_isolated_clone_sync_resolved`
    /// for the entire check-then-register sequence, which rules out a
    /// genuine TOCTOU between the two picks racing to create/resume the
    /// shared `clone_dir` concurrently.
    ///
    /// What this variant also still catches, as before #763: the *same*
    /// on-disk `clone_dir` carrying a provenance marker written by a
    /// genuinely DIFFERENT creator producer — e.g. the `dispatch <name>`
    /// CLI path's `dispatch:<name>` marker (`src/dispatch.rs`), or a marker
    /// written by a pre-fork#760 build whose `creator` was still Name- or
    /// bare-segment-derived — being reopened by this path's
    /// `orchestration:<clone_dir>` producer.
    NameCollision,
    /// PRD fork#777 fix round (reviewer M1 / auditor's `workspace_044`
    /// reasoning). Evidence, ancestry and health all passed, and this call
    /// won the `Contested` race, but the provenance artifact's stored
    /// `creator=` is a LEGACY-shaped marker (`"orchestration:" +
    /// sanitize_workspace_segment(suffix) == name=`) whose `name=` field
    /// equals THIS caller's own segment — for a NESTED pick, where the
    /// legacy format never carried subpath information to distinguish "my
    /// own earlier workspace, written before today's subpath-aware marker
    /// format" from "a genuinely different nested pick that merely shares
    /// the same top-level Name/segment". Deliberately distinct from
    /// [`Self::NameCollision`], which `provision_isolated_clone_or_status`
    /// recovers from by disambiguating and creating a separate clone: this
    /// variant is refused OUTRIGHT and never retried, because the
    /// disambiguation-and-create recovery is only safe when the collision
    /// is UNAMBIGUOUSLY a different pick — here it might not be, and
    /// silently forking a brand-new, empty clone in that case would abandon
    /// the user's own existing workspace (possibly holding uncommitted
    /// work) with nothing surfacing that it happened. See
    /// `LegacyFallbackEligibility::Nested`'s own doc comment for the full
    /// reasoning and `crate::ui::provision_isolated_clone_or_status`'s
    /// retry guard for where this is kept out of the retry.
    LegacyNestedIdentityUnverifiable,
}

impl ResumeRejection {
    /// A single, shared human-readable description of the refusal — reused
    /// by both real callers of [`IsolatedCloneOutcome::Rejected`]
    /// (`src/ui.rs`'s `Action::SpawnPane` dispatch and `src/dispatch.rs`'s
    /// ad hoc `dispatch <name>` CLI path) so the wording can't drift
    /// between them.
    pub(crate) fn describe(&self) -> &'static str {
        match self {
            Self::Stranger => {
                "a directory already exists there but was not created by this deck (no \
                 ownership evidence found) — remove it manually, or pick a different Name"
            }
            Self::AncestryMismatch => {
                "the existing directory's history does not match this project (wrong repo, or \
                 stale) — remove it manually, or pick a different Name"
            }
            Self::AncestryUnverifiable => {
                "the existing directory's history could not be compared against this project \
                 right now (a network or permission problem, not a proven mismatch) — try \
                 again"
            }
            Self::Unhealthy => {
                "the existing directory's git state could not be read (it may be corrupt or \
                 mid-delete) — it was left untouched; repair or remove it manually"
            }
            Self::Contested => "another request just resumed it first — try again",
            Self::NameCollision => {
                "a different orchestration already opened the workspace at this location (its \
                 provenance record names a different creator) — pick a different Name instead \
                 of this one. It may still be in use by the orchestration that opened it — do \
                 not remove it"
            }
            Self::LegacyNestedIdentityUnverifiable => {
                "the existing directory's ownership record is in an old format this build \
                 cannot verify for a picked subdirectory — it may be this exact project's own \
                 earlier workspace (written before today's marker format), or it may be a \
                 different one that merely shares this Name. Pick a different Name, or inspect \
                 the directory manually and remove it only once you have confirmed it is stale \
                 — it may still be in use"
            }
        }
    }
}

/// PRD fork#544 M3: process-local record of which isolated-clone canonical
/// paths have already been resumed once. Deliberately IN-PROCESS rather
/// than a persistent on-disk artifact next to the M4b evidence file it sits
/// beside in this source file: both real callers today
/// (`Action::SpawnPane`'s dispatch in `src/ui.rs`, arbitrated by fork#192's
/// `ClaimOrchestrationName` daemon registry; `src/dispatch.rs`'s ad hoc
/// `dispatch <name>` CLI path, arbitrated by its own has-live-sibling
/// daemon query) already run their OWN liveness check/claim before
/// [`provision_isolated_clone_sync`] is ever reached, so a genuine
/// same-Name concurrent resume never reaches this function in production —
/// this registry exists as THIS function's own defense-in-depth for the
/// brief window between that check and this in-process registration (a
/// window a direct test such as `orchestration/workspace/009` can exercise
/// with no daemon involved at all), and it resets for free on every process
/// restart.
///
/// PRD fork#544 M3 fix round: an entry now gets released by
/// [`release_resumed_isolated_clone_registration`], called by both real
/// callers immediately after `provision_isolated_clone_sync` returns —
/// NOT tied to tab close. That is deliberately early rather than late: by
/// the time either caller reaches this function at all, its own liveness
/// check/claim has already durably established that this orchestration is
/// the sole live user of the path, so this registry's job is done the
/// moment provisioning returns to it. Previously nothing released an entry
/// at all, so within one process a given canonical path could only ever
/// win this registration once — a THIRD open of the same Name in the same
/// still-running process (open, close, reopen [1st resume, works], close,
/// reopen again [2nd resume]) incorrectly saw
/// [`ResumeRejection::Contested`] on that second reopen; see
/// `orchestration/workspace/009`'s extension for the regression test.
fn resumed_isolated_clones() -> &'static std::sync::Mutex<std::collections::HashSet<PathBuf>> {
    static REGISTRY: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
        std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

/// PRD fork#544 M3 fix round: releases `clone_dir`'s entry (if any) from
/// [`resumed_isolated_clones`]. Called by both real `provision_isolated_clone_sync`
/// callers (`src/ui.rs`, `src/dispatch.rs`) immediately after provisioning
/// returns, on every outcome — `Resumed`, `Created`, a `Rejected` refusal,
/// or an `Err` — so a caller need not match on the outcome to know whether
/// to call it: idempotent (a no-op `HashSet::remove` when no entry was ever
/// inserted, which is every outcome except `Resumed` and, since PRD
/// fork#544's review-findings fix round, `Rejected(NameCollision)` — the
/// latter is refused only AFTER winning this same claim, and deliberately
/// leaves it claimed rather than releasing it itself; see
/// `resume_existing_isolated_clone`'s own doc comment on that check for
/// why). Deliberately NOT
/// hooked to tab close or any other teardown path — see
/// [`resumed_isolated_clones`]'s doc comment for why releasing this early
/// is correct rather than a weakening of the race protection.
pub(crate) fn release_resumed_isolated_clone_registration(clone_dir: &Path) {
    let canonical = canonicalize_best_effort(clone_dir);
    resumed_isolated_clones()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&canonical);
}

/// PRD fork#544 M3: `provision_isolated_clone_sync`'s resume arm, called in
/// place of the flat `AlreadyClaimed` return the directory-exists branch
/// used to give unconditionally — from INSIDE the same
/// `worktree_attach_lock_path` attach lock that call already holds for its
/// whole duration, so this whole check-then-register sequence is itself
/// serialized against any other racing call for the identical `clone_dir`
/// (PRD's own Design step 3; see `orchestration/workspace/009`).
///
/// Three-part eligibility, checked in an order that keeps a corrupt
/// directory from being misdiagnosed as an ancestry mismatch: (b) M4b
/// ownership evidence for this canonical path; a health probe (`git
/// rev-parse HEAD`) BEFORE the ancestry probe below, since a directory with
/// no `.git` at all would otherwise fail the ancestry probe's own git
/// invocations for the wrong reason; then (c) ancestry — does `clone_dir`'s
/// history genuinely derive from `source_dir`, via
/// [`isolated_clone_ancestry_matches_source`]. Check (a), name-liveness,
/// already ran ahead of this call entirely — see [`IsolatedCloneOutcome`]'s
/// own doc comment; this function never duplicates it.
///
/// A full pass registers this exact canonical path in
/// [`resumed_isolated_clones`] — the FIRST caller to do so (inside the
/// attach lock, so no two callers can race the registration itself) wins
/// and resumes; a caller that finds the path already registered lost the
/// race and is refused as [`ResumeRejection::Contested`] without touching
/// git again.
/// `legacy_fallback` (fork issue #766, fix round 2): see
/// [`LegacyFallbackEligibility`]'s own doc comment. Read only by the
/// legacy-creator-format fallback inside the `creator` comparison below —
/// see that block's own comment.
fn resume_existing_isolated_clone(
    source_dir: &Path,
    clone_dir: &Path,
    creator: &str,
    legacy_fallback: LegacyFallbackEligibility<'_>,
) -> Result<IsolatedCloneOutcome, String> {
    if !isolated_clone_provenance_path(clone_dir).is_file() {
        return Ok(IsolatedCloneOutcome::Rejected(ResumeRejection::Stranger));
    }
    if !isolated_clone_git_state_readable(clone_dir) {
        return Ok(IsolatedCloneOutcome::Rejected(ResumeRejection::Unhealthy));
    }
    match isolated_clone_ancestry_matches_source(source_dir, clone_dir) {
        AncestryProbe::Matches => {}
        AncestryProbe::ProvenMismatch => {
            return Ok(IsolatedCloneOutcome::Rejected(
                ResumeRejection::AncestryMismatch,
            ));
        }
        AncestryProbe::Unverifiable => {
            return Ok(IsolatedCloneOutcome::Rejected(
                ResumeRejection::AncestryUnverifiable,
            ));
        }
    }

    let canonical = canonicalize_best_effort(clone_dir);
    let first_to_claim = resumed_isolated_clones()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(canonical);
    if !first_to_claim {
        return Ok(IsolatedCloneOutcome::Rejected(ResumeRejection::Contested));
    }

    // Reviewer B2 (PRD fork#544 review-findings fix round): won the
    // Contested race, but that only proves no OTHER call is resuming this
    // exact path RIGHT NOW — it says nothing about whether THIS call's own
    // Name is the one that created it. `sanitize_workspace_segment`/
    // `resolve_workspace_path` aren't injective, so a distinct, colliding
    // Name can reach this point never having raced anyone. The provenance
    // artifact's own `creator=` field (when present -- a pre-M4 `b"deck\n"`
    // artifact has none, and this deliberately tolerates that the same way
    // the eligibility checks above do) is the source of truth for who
    // opened it; a mismatch here means a second, distinct Name resolved to
    // the same on-disk directory as a first, unrelated one. Deliberately
    // does NOT release this call's own claim above on refusal — real
    // callers release unconditionally on every outcome regardless (see
    // `release_resumed_isolated_clone_registration`'s doc comment), and
    // leaving it claimed is what keeps two near-simultaneous mismatched
    // resumes distinguishable from each other (one gets refused here as
    // NameCollision for having won the race in the first place; the other
    // never even reaches this check, refused instead as Contested) rather
    // than both racing to the identical outcome.
    let marker_path = isolated_clone_provenance_path(clone_dir);
    let marker_content = std::fs::read_to_string(&marker_path).ok();
    let stored_creator = marker_content
        .as_deref()
        .and_then(|content| isolated_clone_provenance_field(content, "creator"));
    if let Some(stored_creator) = stored_creator.as_deref()
        && stored_creator != crate::worktree_reclaim::sanitize_marker_creator(creator)
    {
        // Fork issue #766, fix round 2 (audit A1/A2/A3): fork#760 (commit
        // 58a9b8aa) changed a TOPLEVEL pick's `creator` from a Name/segment-
        // derived string (`orchestration_creator_string(typed_name)`,
        // pre-#760) to a path-derived one (`orchestration_creator_string(resolved
        // workspace path)`) — so every marker a pre-#760 build ever wrote
        // for a toplevel pick fails the byte-for-byte comparison above
        // unconditionally, even reopening the IDENTICAL repo/segment under
        // today's code.
        //
        // The original fix round recognized that legacy format by
        // recomputing it purely from the marker's OWN `name=` field and
        // comparing the result against the marker's OWN `creator=` field —
        // both sides came from the same file, so the check answered only
        // "is this marker internally self-consistent", never "does the
        // CALLER resuming it actually own it" (audit A1). Fixed here by
        // requiring genuine caller-identity equivalence instead:
        // [`LegacyFallbackEligibility::ToplevelWithIdentity`] carries the
        // CALLER's own current branch/segment value for THIS pick — the
        // exact string that would be written into `name=` were this call
        // instead creating a fresh clone right now — and the fallback only
        // matches when that, run through the same sanitizer the marker
        // format already implies, equals what is actually stored as
        // `name=` for this directory. A legacy marker whose typed Name
        // needed sanitizing to become its own segment (e.g. `fix/766`,
        // audit A3) still requires the caller to retype the
        // already-sanitized form to match — this fix does not attempt to
        // reverse-engineer the original raw typed Name out of the stored
        // `creator=` string, which would still be self-referential and
        // wouldn't close A1 anyway.
        //
        // Also requires the stored creator to genuinely be LEGACY-SHAPED,
        // not merely namespace-prefixed — round 2's own review/audit fix
        // round (reviewer/auditor B1, BLOCKER): a bare
        // `starts_with("orchestration:")` test does not identify the
        // legacy format at all, since TODAY's own path-derived creator
        // carries that same prefix too. (PRD fork#760 Part A, since
        // reverted, briefly added a SECOND current-ish format — a
        // digest-fronted creator for a nested pick with a typed Worktree
        // slug. No build after the revert can ever produce that format
        // again, but a workspace provisioned by a fork#760/#763-era build
        // may still carry one on disk, so the shape test below stays
        // general enough to keep rejecting it rather than assuming it can
        // no longer occur.) Left as a bare prefix check, a toplevel pick
        // could adopt, then irreversibly migrate, a CURRENT-format marker
        // written under a colliding `name=` — silently merging two live
        // orchestrations into one physical clone.
        //
        // The shape test below strips the `"orchestration:"` prefix and
        // runs the remaining suffix through
        // [`crate::ui::sanitize_workspace_segment`] — the exact
        // transform a bare typed segment goes through on its way into
        // `name=` — and requires the result to equal the marker's own
        // stored `name=` exactly. For a genuine legacy marker the suffix
        // IS a bare segment, so this is a no-op fixed point and the check
        // passes. For today's path-derived (or a legacy digest-fronted)
        // suffix, the extra structure it carries (path separators, or a
        // hex digest and a colon) essentially never sanitizes down to
        // match a bare `name=` value — belt-and-suspenders alongside
        // [`LegacyFallbackEligibility::Disabled`]'s structural fix for
        // audit A2 (`src/dispatch.rs`'s own `dispatch:`-namespaced creator
        // can never legitimately match a legacy ORCHESTRATION marker, and
        // that path now can't even construct the variant that would let it
        // try).
        //
        // `Nested`/`Disabled` both yield no identity branch at all — a
        // nested pick's legacy format never carried subpath information to
        // compare against in the first place, and a caller with no real
        // identity to offer has nothing this check can validate.
        const ORCHESTRATION_CREATOR_NAMESPACE: &str = "orchestration:";
        let stored_name = marker_content
            .as_deref()
            .and_then(|content| isolated_clone_provenance_field(content, "name"));
        let legacy_shape_ok = stored_name.as_deref().is_some_and(|stored_name| {
            stored_creator
                .strip_prefix(ORCHESTRATION_CREATOR_NAMESPACE)
                .is_some_and(|suffix| crate::ui::sanitize_workspace_segment(suffix) == stored_name)
        });
        let legacy_match = legacy_shape_ok
            && legacy_fallback.identity_branch().is_some_and(|branch| {
                stored_name.as_deref()
                    == Some(crate::worktree_reclaim::sanitize_marker_creator(branch).as_str())
            });
        if !legacy_match {
            // PRD fork#777 fix round (reviewer M1 / auditor's `workspace_044`
            // reasoning): a NESTED caller whose own segment happens to equal
            // this legacy-shaped marker's stored `name=` is genuinely
            // ambiguous — it might be this exact pick's own workspace from
            // before today's subpath-aware marker format, or it might be an
            // entirely different nested pick that merely shares the same
            // top-level Name/segment (the ordinary fork#607 collision this
            // whole mechanism exists to catch). Nothing on disk can
            // distinguish the two — the legacy format never carried subpath
            // information at all (`LegacyFallbackEligibility::Nested`'s own
            // doc comment) — so treating this as "just an ordinary,
            // unambiguous different pick" and letting
            // `provision_isolated_clone_or_status` silently disambiguate
            // would risk abandoning the user's own existing workspace,
            // possibly holding uncommitted work, with nothing surfacing that
            // it happened. Refuse distinguishably instead of falling through
            // to the ordinary `NameCollision` (which IS safe to
            // disambiguate, since it never carries this ambiguity).
            if legacy_shape_ok
                && legacy_fallback
                    .nested_own_segment()
                    .is_some_and(|own_segment| {
                        stored_name.as_deref()
                            == Some(
                                crate::worktree_reclaim::sanitize_marker_creator(own_segment)
                                    .as_str(),
                            )
                    })
            {
                return Ok(IsolatedCloneOutcome::Rejected(
                    ResumeRejection::LegacyNestedIdentityUnverifiable,
                ));
            }
            return Ok(IsolatedCloneOutcome::Rejected(
                ResumeRejection::NameCollision,
            ));
        }

        // Fork issue #766: this resume is only reachable via the legacy
        // fallback above — migrate the on-disk marker's `creator=` field to
        // today's format now, so this workspace's next resume matches the
        // fast, no-fallback path directly and this equivalence check does
        // not need to keep firing for it indefinitely. Best-effort: a
        // failure here does not fail the resume itself, since the clone is
        // already fully usable either way — it only means this migration
        // is retried on the next resume too.
        if let Err(e) = migrate_legacy_isolated_clone_creator(&marker_path, creator) {
            tracing::warn!(
                clone = %clone_dir.display(),
                error = %e,
                "issue-dispatch: could not migrate legacy provenance creator field to today's \
                 format; this workspace will keep matching via the legacy fallback until it does"
            );
        }
    }

    tracing::info!(
        clone = %clone_dir.display(),
        creator = %creator,
        "issue-dispatch: resuming existing isolated clone"
    );

    // PRD fork#544 M3 Decisions table: read-only fetch only — updates
    // `origin/<default-branch>`'s remote-tracking ref, touches no local
    // branch and no working tree. Best-effort: a source with no `origin`
    // configured (the `orch-clone-gate` fixture's own shape) leaves this
    // clone with no `origin` remote at all after creation (see
    // `remove_isolated_clone_origin_default`) — `git fetch origin` then
    // fails loudly with "No such remote", which is fine here, not a
    // provisioning failure. `--upload-pack=git-upload-pack` (final cleanup
    // round) closes the `remote.origin.uploadpack` code-execution vector a
    // forged value of that setting in this untrusted `clone_dir`'s own
    // `.git/config` would otherwise open, at zero cost against the real
    // `origin` this fetches from.
    let fetch_warning = match run_status_sync(
        &[
            "-C".to_string(),
            clone_dir.to_string_lossy().into_owned(),
            "fetch".to_string(),
            "--upload-pack=git-upload-pack".to_string(),
            "origin".to_string(),
        ],
        WORKTREE_GIT_TIMEOUT,
    ) {
        Ok(()) => None,
        Err(err) => {
            let e = match err {
                AddError::TimedOut(e) | AddError::Failed(e) => e,
            };
            tracing::warn!(
                clone = %clone_dir.display(),
                error = %e,
                "issue-dispatch: could not fetch origin on resume; ahead/behind info may be stale"
            );
            Some(e)
        }
    };

    Ok(IsolatedCloneOutcome::Resumed { fetch_warning })
}

/// [`isolated_clone_ancestry_matches_source`]'s three-way verdict — kept
/// distinct from a plain `bool` (PRD fork#544 review-findings fix round 3,
/// reviewer S1) so its caller can refuse a PROVEN mismatch
/// ([`Self::ProvenMismatch`]) differently from a probe that simply could
/// not reach a verdict ([`Self::Unverifiable`]) — see
/// [`ResumeRejection::AncestryUnverifiable`]'s own doc comment for why that
/// distinction matters to a caller.
enum AncestryProbe {
    /// Positive evidence of shared history (or both sides genuinely have no
    /// `origin` configured, the expected shape for that case).
    Matches,
    /// Positive evidence the two sides do NOT share history: the `origin`
    /// URLs disagree, or `git merge-base` ran to completion and reported no
    /// common ancestor (exit 1) between two commits it could actually
    /// resolve.
    ProvenMismatch,
    /// The probe never reached a conclusive comparison — no positive
    /// evidence either way.
    Unverifiable,
}

/// PRD fork#544 M3 check (c): does `clone_dir`'s history genuinely derive
/// from `source_dir`, rather than an unrelated repository that merely
/// landed on the same canonical path? Two signals, both must hold when
/// applicable:
///
/// - **Origin URL**, when BOTH sides have one configured — reusing
///   [`read_source_origin_url`] rather than inventing a second way to read
///   it. Deliberately not required when either side has none: a plain
///   local `git clone` of a no-origin source ends this call's own `origin`
///   fixup with NO `origin` remote at all (see
///   [`remove_isolated_clone_origin_default`]), so "both absent" is the
///   expected shape for that case, not evidence of anything.
/// - **Shared history**, via a plain `git merge-base <source HEAD> HEAD`
///   run inside `clone_dir` (PRD fork#544 review-findings fix round,
///   reviewer B1 / auditor A1, pinned by `orchestration/workspace/025`):
///   the ORIGINAL form ran `--is-ancestor`, asking whether `source_dir`'s
///   HEAD AT RESUME TIME is contained in `clone_dir`'s history — true only
///   in the instant right after cloning, when a plain `git clone`
///   hardlinks `source_dir`'s object store, and wrongly refusing a
///   healthy, genuinely-derived workspace the moment `source_dir` advances
///   past it (the everyday steady state this PRD exists to support). A
///   plain merge-base, with neither side pinned to "must contain the
///   other," is symmetric: it passes whichever side has since moved on.
///
/// PRD fork#544 review-findings fix round 3 (auditor F1, reviewer S1/S2):
/// the merge-base probe now runs FIRST, against whatever `clone_dir`'s
/// object database already has, with NO fetch beforehand — the fetch used
/// to run unconditionally, which (a) is exactly the untrusted-`clone_dir`
/// code-execution exposure `spawn_git_status_child`'s new `-c
/// core.fsmonitor=` hardening (above) closes the fsmonitor half of, so
/// skipping the fetch whenever it isn't needed reduces exposure to that
/// residual further still; and (b) meant a colliding-Name `NameCollision`
/// resume (checked further down in `resume_existing_isolated_clone`, AFTER
/// this probe) or a wrong-repo `AncestryMismatch` refusal
/// (`orchestration/workspace/007`) both wrote `git fetch` traffic into a
/// directory this call has no business touching before ever discovering
/// that. The common steady-state case — a workspace resumed more than
/// once, where a prior resume's own fetch already pulled `source_dir`'s
/// then-current HEAD in — now resolves via this first probe alone, no
/// fetch at all. Only when this first attempt fails with exit 128
/// (`source_head`'s object is genuinely missing from `clone_dir`'s
/// database — not exit 1, a proven mismatch, which is conclusive on its
/// own) does this fall back to a best-effort `git fetch` directly from
/// `source_dir`'s own path (not `origin`, which may not exist at all when
/// `source_dir` has none configured — see
/// [`remove_isolated_clone_origin_default`]) followed by a retry of the
/// same probe. A failure at any other point (spawn failure, or the fetch
/// itself failing, or the object still missing after the fetch) resolves to
/// [`AncestryProbe::Unverifiable`], never silently reused as
/// [`AncestryProbe::ProvenMismatch`] (that describe()'s "remove it
/// manually" advice does not apply to a probe that was simply unable to
/// run, e.g. a permission error reading the source's own new commit
/// object — `orchestration/workspace/031`).
///
/// Final cleanup round: this fallback fetch, run against a `clone_dir` this
/// process did not create and cannot vouch for, also now carries
/// `--upload-pack=git-upload-pack` — closing the `remote.<name>.uploadpack`
/// code-execution vector a forged value of that setting in the untrusted
/// directory's own `.git/config` would otherwise open, at zero cost: the
/// flag is a no-op for this call's actual local-path fetch and needs no
/// `origin` remote to exist. `spawn_git_status_child`'s own doc comment
/// (above) tracks which residuals from that untrusted-`.git/config` class
/// remain open; this one no longer is.
fn isolated_clone_ancestry_matches_source(source_dir: &Path, clone_dir: &Path) -> AncestryProbe {
    if let (Some(source_url), Some(clone_url)) = (
        read_source_origin_url(source_dir),
        read_source_origin_url(clone_dir),
    ) && source_url != clone_url
    {
        return AncestryProbe::ProvenMismatch;
    }

    let Some(source_head) = isolated_clone_head_sha(source_dir) else {
        return AncestryProbe::Unverifiable;
    };

    let clone_dir_str = clone_dir.to_string_lossy().into_owned();

    let first_attempt = merge_base_attempt(&clone_dir_str, &source_head);
    if let MergeBaseAttempt::MissingObject = first_attempt {
        // Exit 128 only -- source_head's object isn't in clone_dir's
        // database yet. Best-effort: bring source_dir's own objects
        // (including anything it has committed since clone_dir was
        // created) directly into clone_dir's database, then retry once. A
        // failure here isn't fatal on its own -- the retry below just runs
        // against whatever clone_dir's database already has, same as if
        // the fetch had never existed.
        let _ = run_status_sync(
            &[
                "-C".to_string(),
                clone_dir_str.clone(),
                "fetch".to_string(),
                "--quiet".to_string(),
                "--upload-pack=git-upload-pack".to_string(),
                "--".to_string(),
                source_dir.to_string_lossy().into_owned(),
            ],
            WORKTREE_GIT_TIMEOUT,
        );
        return match merge_base_attempt(&clone_dir_str, &source_head) {
            MergeBaseAttempt::Matches => AncestryProbe::Matches,
            MergeBaseAttempt::ProvenMismatch => AncestryProbe::ProvenMismatch,
            MergeBaseAttempt::MissingObject | MergeBaseAttempt::Failed => {
                AncestryProbe::Unverifiable
            }
        };
    }

    match first_attempt {
        MergeBaseAttempt::Matches => AncestryProbe::Matches,
        MergeBaseAttempt::ProvenMismatch => AncestryProbe::ProvenMismatch,
        MergeBaseAttempt::MissingObject | MergeBaseAttempt::Failed => AncestryProbe::Unverifiable,
    }
}

/// The four distinguishable outcomes of a single `git merge-base
/// <source_head> HEAD` attempt inside `clone_dir`, as run by
/// [`isolated_clone_ancestry_matches_source`] both before and (when this
/// first came back [`Self::MissingObject`]) after its best-effort fetch.
/// Kept as its own enum, rather than folding into [`AncestryProbe`]
/// directly, so that caller can retry ONLY on the specific exit-128 case
/// the fetch might actually resolve -- any other failure (a spawn error,
/// or any exit code other than 0/1/128) is not something a fetch from
/// `source_dir` could plausibly fix, so it resolves straight to
/// [`AncestryProbe::Unverifiable`] without a second, pointless attempt.
enum MergeBaseAttempt {
    /// Exit 0: a merge base was found -- source_dir's HEAD and clone_dir's
    /// own current HEAD share history, regardless of which side has since
    /// advanced past the other.
    Matches,
    /// Exit 1: git ran fine and found no common ancestor between two
    /// resolvable commits -- a genuine, conclusive ancestry mismatch
    /// (`orchestration/workspace/007`'s unrelated second source).
    ProvenMismatch,
    /// Exit 128: `source_head`'s object is not yet in `clone_dir`'s
    /// database -- inconclusive, and specifically the one case a fetch
    /// from `source_dir` might resolve.
    MissingObject,
    /// Any other exit code, or the probe failing to spawn at all --
    /// inconclusive, and not something retrying after a fetch would help.
    Failed,
}

fn merge_base_attempt(clone_dir_str: &str, source_head: &str) -> MergeBaseAttempt {
    match spawn_and_wait_sync(
        &["-C", clone_dir_str, "merge-base", source_head, "HEAD"],
        WORKTREE_GIT_TIMEOUT,
    ) {
        Ok(out) if out.status.success() => MergeBaseAttempt::Matches,
        Ok(out) if out.status.code() == Some(1) => MergeBaseAttempt::ProvenMismatch,
        Ok(out) if out.status.code() == Some(128) => MergeBaseAttempt::MissingObject,
        _ => MergeBaseAttempt::Failed,
    }
}

/// `git rev-parse HEAD` in `dir`, `None` on any spawn/exit failure — shared
/// by [`isolated_clone_ancestry_matches_source`] and
/// [`isolated_clone_git_state_readable`].
fn isolated_clone_head_sha(dir: &Path) -> Option<String> {
    run_capture_sync(
        &[
            "-C".to_string(),
            dir.to_string_lossy().into_owned(),
            "rev-parse".to_string(),
            "HEAD".to_string(),
        ],
        WORKTREE_GIT_TIMEOUT,
    )
    .ok()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
}

/// PRD fork#544 M3 health probe: is `clone_dir`'s git state usable at all?
/// `git rev-parse HEAD` failing (no `.git`, corrupt, mid-delete) is refused
/// distinguishably as [`ResumeRejection::Unhealthy`] — the directory is
/// never auto-deleted or auto-repaired on this path, only refused.
fn isolated_clone_git_state_readable(clone_dir: &Path) -> bool {
    isolated_clone_head_sha(clone_dir).is_some()
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
///
/// `pub(crate)`, not private: [`create_worktree`]'s async prologue needs it
/// directly (see the doc comment above). Fork#325 M4a's final round
/// additionally had `crate::worktree_reclaim::candidate_has_attach_lock`
/// recompute this exact path for a discovered isolated-clone candidate, so
/// that discovery's ownership check and this function's own lock-
/// acquisition path could never silently drift apart on the lock's
/// filename — M4b replaced that mechanism with a dedicated provenance
/// artifact under [`crate::platform::paths::state_dir`] instead (see
/// [`isolated_clone_provenance_path`] and
/// [`crate::worktree_reclaim::ISOLATED_CLONE_PROVENANCE_FILENAME`]'s doc
/// comment for why), so `candidate_has_attach_lock` no longer calls this
/// function at all; this remains `pub(crate)` for the async caller alone.
pub(crate) fn worktree_attach_lock_path_from_common_dir(
    common_dir: &Path,
    worktree_dir: &Path,
) -> PathBuf {
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
///
/// `pub(crate)` since PRD fork#325 M3: `src/ui.rs`'s Nth-concurrent-
/// orchestration gate reuses this exact resolution (not a re-derivation) to
/// decide whether a target root checkout already shares its object store
/// with a live orchestration's working directory — the same "ask git, don't
/// assume `.git` is a directory" reasoning applies identically there.
pub(crate) fn git_common_dir(clone_dir: &Path) -> Result<PathBuf, String> {
    let out = crate::git_env::git_at(clone_dir)
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
    let fallback = crate::git_env::git_at(clone_dir)
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
/// pre-2.31 git); keep the two in sync if either changes. Unlike the add and
/// its cleanup (which go through [`run_git_status_within`] and so kill their
/// whole process group at the bound), this reads through a plain
/// `.output()` with no `kill_on_drop`: a probe read like `git rev-parse
/// --git-common-dir` is safe to leave running detached if it is ever actually
/// cut off by the caller's timeout, and it writes nothing that a surviving copy
/// could race.
async fn git_common_dir_async(clone_dir: &Path) -> Result<PathBuf, String> {
    let out = crate::git_env::git_async()
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
    let fallback = crate::git_env::git_async()
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
/// B2): canonicalize `path`'s parent (which must already exist — see the
/// caller) and rejoin the original file name, so a not-yet-created
/// `worktree_dir` still collapses symlinks/relative components in the part
/// of the path that DOES exist. Falls back to `path` unchanged if that
/// fails — never fatal, since this only affects whether two spellings of
/// one target collide onto the same lock file, not whether the lock is
/// taken at all. Logs on the fallback (fork #331 audit F5): the parent that
/// reaches this branch was just created by `ensure_worktree_parent_dir`
/// moments earlier, so failing to canonicalize it is genuinely anomalous,
/// and this is a mutual-exclusion primitive silently under-serializing —
/// worth a greppable trace even though it is not worth making fatal.
///
/// PRD fork#325 M4: this used to try `path.canonicalize()` FIRST — the
/// primary branch below — and fall back to `parent.canonicalize().join(name)`
/// only when the whole path did not yet exist. Those are two genuinely
/// different algorithms, and WHICH one a given call took was decided
/// entirely by the caller's timing, never by anything about the path
/// itself: [`worktree_attach_lock_path`] calls this before `clone_dir`
/// exists (write time, inside `provision_isolated_clone_sync`), so it
/// always took the fallback branch; `worktree_reclaim::candidate_has_attach_lock`
/// calls it on a directory `discover_isolated_clones` just found on disk
/// (check time), so it always took the primary branch. On the GitHub
/// Actions Windows runner the two branches disagreed for the SAME literal
/// `worktree_dir` — an 8.3 short-name component (`RUNNER~1`, the alias
/// Windows itself substitutes into `%TEMP%` for the runner's account) was
/// in the panic output — so the lock file written at provisioning time and
/// the path hashed at discovery time resolved to two different final
/// spellings of the identical clone, and `worktree_reclaim_053_isolated_clone_with_real_attach_lock_reports_owned_true`
/// reproduced it identically across two consecutive CI runs. Always
/// resolving through the parent, never through the full path directly,
/// makes this ONE algorithm regardless of when it runs — write time and
/// check time can no longer diverge on which branch they took, only
/// (unchanged, and already logged) on whether the parent itself fails to
/// canonicalize.
///
/// A second, independent reason not to reintroduce the full-path branch:
/// the two-branch version could make racing callers hash the SAME target
/// to two DIFFERENT lock paths, not only a write-time/check-time caller
/// pair. Two callers racing to provision the identical `clone_dir` both run
/// at write time (`worktree_attach_lock_path`, before `clone_dir` exists),
/// so both would take the fallback branch and hash identically today — but
/// that agreement held only because both callers happened to observe the
/// path in the same not-yet-created state; anything that let one of them
/// observe it as already-existing (a slow racer landing after the other's
/// `AlreadyClaimed` check, for instance) would flip it onto the primary
/// branch while the other stayed on the fallback, hashing the same real
/// directory to two different lock files and defeating the mutual
/// exclusion the lock exists for. Resolving through the parent
/// unconditionally removes that branch entirely, so no ordering of
/// concurrent `AlreadyClaimed` checks can make two callers disagree on
/// which lock file guards a given target.
fn canonicalize_best_effort(path: &Path) -> PathBuf {
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
        // No parent (e.g. a filesystem root) or no file name to rejoin:
        // fall back to canonicalizing the whole path if it exists, else
        // give up on the raw path. Neither call site above can ever pass a
        // path shaped like this — `clone_dir`/`candidate` are always a
        // sibling of the root checkout, never a root themselves.
        _ => path.canonicalize().unwrap_or_else(|_| path.to_path_buf()),
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
///
/// PRD fork#544 M2b: Model A's own call site (the TUI's `Action::SpawnPane`,
/// `src/ui.rs`) is retired — isolation is now unconditional, so the shared-
/// checkout `git worktree add` sibling this function provisions is no
/// longer reachable from the interactive spawn path at all. Kept rather
/// than deleted (a deliberate scope decision, not an oversight) since it
/// remains exercised directly by this module's and `worktree_reclaim.rs`'s
/// own tests, and nothing about the mechanism itself is wrong — only its
/// one caller went away. `#[allow(dead_code)]` reflects that production-code
/// state honestly rather than papering over it with a synthetic caller.
#[allow(dead_code)]
pub(crate) fn create_worktree_sync(
    clone_dir: &Path,
    worktree_dir: &Path,
    branch: &str,
    creator: &str,
) -> Result<WorktreeCreation, String> {
    // Issue #489 fix round (auditor A2, nit): resolve the lock path — which
    // re-derives `clone_dir`'s `git_common_dir` and fails if `clone_dir`
    // isn't a git repository — BEFORE `ensure_worktree_parent_dir` creates
    // anything on disk. A non-git `clone_dir` now refuses cleanly with no
    // stray directory left behind, instead of creating `worktree_dir`'s
    // parent first and only then hitting this same failure.
    let lock_path = worktree_attach_lock_path(clone_dir, worktree_dir)
        .map_err(|e| format!("failed to resolve worktree lock path: {e}"))?;

    ensure_worktree_parent_dir(worktree_dir)?;

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
        &crate::issue_dispatch::worktree_branch_probe_argv(clone_dir, branch),
        WORKTREE_GIT_TIMEOUT,
    )
    .is_ok();
    let add = run_status_sync(
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
            let cleaned_up_by = attempt_worktree_cleanup(clone_dir, worktree_dir, creator);
            WorktreeCreation::TimedOut { cleaned_up_by }
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
/// is actually gone afterward; either check failing is reported as `None`
/// so the caller fails loudly with the manual command rather than assuming
/// success it cannot back up. Issue #325: this is always a self-cleanup of
/// `remover`'s own half-created directory (see [`create_worktree_sync`]'s
/// only call site, its `AddOutcome::TimedOut` arm) — `remover` is that same
/// `creator`, forwarded straight through rather than re-resolved, and the
/// return carries it (`Some(remover.to_string())`) only when the removal is
/// actually confirmed, never fabricating an identity for a no-op.
fn attempt_worktree_cleanup(
    clone_dir: &Path,
    worktree_dir: &Path,
    remover: &str,
) -> Option<String> {
    let removed = run_status_sync(
        &crate::issue_dispatch::worktree_remove_argv(clone_dir, worktree_dir),
        WORKTREE_CLEANUP_TIMEOUT,
    )
    .is_ok();
    if removed && !worktree_dir.exists() {
        Some(remover.to_string())
    } else {
        None
    }
}

/// Fork #282 final-pass F1 (reviewer) / A1 (auditor): async twin of
/// [`attempt_worktree_cleanup`] for [`create_worktree_from`]'s `TimedOut` arm.
/// Runs after the add's own runner ([`status_within`], via
/// [`run_git_status_within`]) has already killed the add's whole process group
/// at [`WORKTREE_GIT_TIMEOUT`] — since the upstream sync that is the group, not
/// just the direct child the earlier `kill_on_drop` runner reached, so a hook
/// grandchild of the add no longer outlives it — through the same bounded,
/// non-interactive subprocess path the add itself used (stdin closed,
/// `GIT_TERMINAL_PROMPT=0`, ambient git location env cleared), reusing the
/// identical [`crate::issue_dispatch::worktree_remove_argv`] the sync twin uses
/// so the two argv shapes cannot drift. Bounded by its OWN, shorter
/// [`WORKTREE_CLEANUP_TIMEOUT`], so a stuck cleanup cannot itself hang the
/// scheduler tick it exists to unwedge, and killed at that bound for the same
/// reason the add is: a cleanup `git worktree remove` that itself wedges must
/// not be left to finish detached, or it can race the next attempt at the
/// same slug.
///
/// "Confirmed" means the same thing it means for the sync twin: both the
/// command exited successfully AND the directory is actually gone
/// afterward; either check failing is reported as `None` so the caller
/// fails loudly with the manual `git worktree remove --force` recovery
/// command rather than assuming success it cannot back up. Issue #325: this
/// is always a self-cleanup of `remover`'s own half-created directory (see
/// [`create_worktree_from`]'s only call site, its `AddOutcome::TimedOut` arm)
/// — `remover` is that same `creator`, forwarded straight through rather than
/// re-resolved, and the return carries it (`Some(remover.to_string())`)
/// only when the removal is actually confirmed, never fabricating an
/// identity for a no-op.
async fn attempt_worktree_cleanup_async(
    clone_dir: &Path,
    worktree_dir: &Path,
    remover: &str,
) -> Option<String> {
    let removed = run_git_status_within(
        &crate::issue_dispatch::worktree_remove_argv(clone_dir, worktree_dir),
        WORKTREE_CLEANUP_TIMEOUT,
    )
    .await
    .is_ok();
    if removed && !worktree_dir.exists() {
        // Issue #325 reviewer P2-B / auditor A2: logged HERE rather than at
        // each of the two production consumers of `WorktreeCreation::TimedOut`
        // (`dispatch_one_issue` above and `dispatch.rs`'s currently-unreachable
        // arm) — mirroring the confirmed-removal log the sync twin's caller
        // does in `ui.rs`, but at one site so neither consumer has to
        // remember to log, including the unattended scheduled `issue_dispatch`
        // flow this async twin actually serves.
        tracing::info!(
            path = %crate::terminal_sanitize::sanitize_path_for_terminal_display(worktree_dir),
            remover = %crate::terminal_sanitize::sanitize_for_terminal_display(remover),
            "worktree add timed out; half-created directory removed automatically"
        );
        Some(remover.to_string())
    } else {
        None
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
///
/// NOT for `git` — use [`run_git_status`], which neutralizes the ambient git
/// location environment. `xtask/linkage-check`'s rule 13 fails the build on a
/// `git` program literal in this module's production half, so that is enforced
/// rather than asked for.
pub async fn run_status(program: &str, args: &[&str]) -> Result<(), String> {
    status_of(tokio::process::Command::new(program), program, args).await
}

/// [`run_status`] for `git`, built by [`crate::git_env::git_async`] so an
/// ambient `GIT_DIR` cannot redirect it away from the `-C <dir>` in its argv
/// (issue #1181).
pub(crate) async fn run_git_status(args: &[&str]) -> Result<(), String> {
    status_of(crate::git_env::git_async(), crate::git_env::GIT, args).await
}

/// How long one `gh` / `git` subprocess in the dispatch path may run before it
/// is killed and reported as failed (issue #692).
///
/// **A deadlock guard, not a latency budget** — the same reasoning, and the same
/// value, as [`DEVBOX_WARMUP_TIMEOUT`]. Before this bound the calls awaited
/// `.output()` with nothing around it, so one hung `gh` wedged the fire for the
/// rest of the daemon's life while it held the scheduler's `running` flag, and
/// every later fire of that task was reported as skipped. Expiring early is not
/// free either: it fails a dispatch that would have worked. The longest
/// legitimate call here is `gh repo clone` of a large repository, and nothing
/// has measured how long that takes over a slow link, so the value is set far
/// enough out that expiry means *stuck* rather than *slow*. The small API calls
/// (`gh issue list`, `gh pr list`) and the local `git` probes normally return
/// in well under a second, so for them reaching the bound means something was
/// already wrong.
const SUBPROCESS_TIMEOUT: Duration = Duration::from_secs(600);

/// The shared body of [`run_status`] and [`run_git_status`] — one
/// implementation, two ways of constructing the command, so the neutralized
/// variant cannot drift from the error text or the exit handling.
async fn status_of(
    cmd: tokio::process::Command,
    program: &str,
    args: &[&str],
) -> Result<(), String> {
    status_within(cmd, program, args, SUBPROCESS_TIMEOUT).await
}

/// [`status_of`] with the bound passed in, so the timeout path can be driven
/// by a unit test without a ten-minute wait.
async fn status_within(
    cmd: tokio::process::Command,
    program: &str,
    args: &[&str],
    bound: Duration,
) -> Result<(), String> {
    let output = output_within(cmd, program, args, bound).await?;
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

/// Like [`run_status`] but for `String` args — the `gh` argv helpers produce
/// `Vec<String>` (fork: [`run_gh_write`] passes one straight through).
///
/// Since the upstream sync this is a thin wrapper over upstream's
/// [`run_status`], which kills the command's whole process group at its bound
/// or when the caller stops awaiting. The fork's earlier split between a
/// detaching `run_status` and a `kill_on_drop` `run_status_killable` (fork
/// #282 final-pass F1) is gone with it: the hazard F1 guarded against — a
/// `gh repo clone` killed mid-flight leaving a partial `clone_dir` that wedges
/// every later fire — is closed at its source by `clone_via_staging`, which
/// clones into a staging directory and moves it into place only on success.
async fn run_status_args(program: &str, args: &[String]) -> Result<(), String> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_status(program, &refs).await
}

/// [`run_git_status`] for `String` args and an explicit bound: the `git`
/// worktree argv helpers ([`crate::issue_dispatch::worktree_add_argv`] /
/// [`crate::issue_dispatch::worktree_remove_argv`]) produce `Vec<String>`, and
/// the callers that hold the attach lock shared with the TUI's
/// [`create_worktree_sync`] must bound each `git` by [`WORKTREE_GIT_TIMEOUT`] /
/// [`WORKTREE_CLEANUP_TIMEOUT`] rather than [`SUBPROCESS_TIMEOUT`] (fork #282):
/// ten minutes under a lock the TUI waits on would freeze it. The command is
/// built by [`crate::git_env::git_async`] and, on expiry, its whole process
/// group is killed — which also closes the residual fork #133 documented for
/// the earlier `kill_on_drop` runner (a hook grandchild such as
/// `post-checkout` surviving the direct child's kill).
async fn run_git_status_within(args: &[String], bound: Duration) -> Result<(), String> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    git_within(&refs, bound).await
}

/// [`run_git_status_within`] for a `&[&str]` argv.
async fn git_within(args: &[&str], bound: Duration) -> Result<(), String> {
    status_within(
        crate::git_env::git_async(),
        crate::git_env::GIT,
        args,
        bound,
    )
    .await
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
///
/// PRD fork#325 fix round 4 (auditor D1): `.env("LC_ALL", "C")` pins the
/// child's message locale regardless of what the daemon's own process
/// environment carries. Without it, [`clone_destination_predates_attempt`]'s
/// English-substring match on `git clone`'s stderr fails OPEN — under a
/// non-English `LANG` (verified: German, French — git ships translation
/// catalogs and neither wording contains `"destination path"` or `"already
/// exists"`), the predicate returns `false` for the exact "destination
/// predates this attempt" shape it exists to recognize, and that `false`
/// answer routes straight into `remove_dir_all` — C1's destructive path,
/// reopened for any non-English user. `LC_ALL` takes priority over
/// `LANGUAGE`/`LANG` in gettext's own resolution order, so this defeats
/// either override; verified directly (`LC_ALL=C LANGUAGE=de git clone …`
/// still produces the English wording). Chosen over widening the predicate
/// to recognize translated wordings too: that only ever covers the
/// languages someone thought to add, where normalizing the child's locale
/// closes the whole class at once. See
/// [`clone_destination_predates_attempt`]'s doc comment for why matching
/// hardcoded English substrings is safe given this override.
fn spawn_git_status_child(
    args: &[&str],
) -> std::io::Result<(
    std::process::Child,
    crate::platform::proc::AgentProcessGroup,
)> {
    // PRD fork#544 review-findings fix round 3 (auditor F1/F2): every call
    // through this shared core runs with hooks and fsmonitor switched off and
    // the ambient git location environment cleared. Several of this module's
    // callers (the resume arm's ancestry/fetch probes chief among them) run
    // `git` inside a `clone_dir` this process did not just create and cannot
    // vouch for, and a forged `core.fsmonitor` in that directory's own
    // `.git/config` is a same-uid code-execution vector through a plain `git
    // status`/`git fetch`. The fork closed that vector with its own
    // `-c core.fsmonitor=` here; since the upstream sync the command is built by
    // [`crate::git_env::git_sync`] (issue #1181 / #1888), whose `-c` list is a
    // strict superset (`core.hooksPath=/dev/null` as well) and which also
    // clears `GIT_DIR` and its siblings, so the one constructor now decides
    // what a deck-run `git` may inherit and linkage-check rule 13 keeps the
    // `"git"` literal out of this file. Applied in the one shared spawn core,
    // rather than at each call site, for the reason the fork gave: a caller
    // genuinely operating in a directory it just created (e.g. the initial `git
    // clone` itself) pays a harmless no-op. This blocks the fsmonitor and hook
    // vectors; it does NOT close every same-uid config-driven vector reachable
    // through an untrusted `.git/config` (`filter.<driver>.clean` and
    // `diff.external` chief among the residuals). `remote.<name>.uploadpack` on
    // `fetch` was in this list too until PRD fork#544's final cleanup round
    // closed it — see `isolated_clone_ancestry_matches_source`'s own doc
    // comment for how and why.
    let mut command = crate::git_env::git_sync();
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env("SSH_ASKPASS", "")
        .env("LC_ALL", "C");

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

/// The raw result of [`spawn_and_wait_sync`] — exit status plus fully
/// drained stdout/stderr, so a caller that needs the exit CODE (not just
/// success/failure) or the captured OUTPUT (not just whether there was any)
/// can get at both without a second subprocess spawn.
struct SyncGitOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Shared spawn/poll/timeout/drain core for [`run_status_sync`] and
/// [`run_capture_sync`] — both are thin wrappers that differ only in which
/// half of this result they surface to their own caller. Extracted (PRD
/// fork#544 review-findings fix round, bundled hardening) so a synchronous
/// caller that needs captured stdout (`read_source_origin_url`, the
/// ancestry probe) can get [`spawn_git_status_child`]'s same hardening
/// — stdin closed, non-interactive git env, `LC_ALL=C`, a bounded
/// process-group timeout — without a second, unhardened
/// `std::process::Command` of its own.
fn spawn_and_wait_sync(args: &[&str], timeout: Duration) -> Result<SyncGitOutput, AddError> {
    let program = crate::git_env::GIT;
    let (mut child, group) = spawn_git_status_child(args)
        .map_err(|e| AddError::Failed(format!("failed to run `{program}`: {e}")))?;

    // PRD fork#544 review-findings fix round 3 (auditor F4): drain stdout
    // and stderr on their own reader threads CONCURRENTLY with waiting for
    // the child below, rather than sequentially after `try_wait` reports
    // exit. A command whose own output exceeds the OS pipe buffer (e.g.
    // `git status --porcelain` on a busy workspace, comfortably over the
    // usual ~64KiB) blocks the CHILD writing to a full pipe while nothing
    // is reading it -- and since this whole function runs on the TUI's
    // synchronous render/event loop (fork issue #136's own doc comment
    // below), that block reaches all the way up to the UI, stalling it for
    // up to the full `timeout` before this call fails closed. Taking the
    // pipes now, before the wait loop, and handing each to its own thread
    // means the child is never blocked on a full pipe regardless of how
    // long `try_wait` takes to report exit.
    let stdout_reader = child.stdout.take().map(|mut pipe| {
        std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    });
    let stderr_reader = child.stderr.take().map(|mut pipe| {
        std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    });

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
                    // it and everything it forked. The reader threads above
                    // are left to finish on their own -- killing the direct
                    // `git` child alone would not be enough to unblock them: a
                    // hook grandchild (e.g. `post-checkout`) inherits its own
                    // copy of each pipe's write end, so `read_to_end` would
                    // still see it open even after `git` itself exits. It's
                    // the `killpg` call above, reaching that grandchild too
                    // and closing every inherited copy of each fd, that
                    // actually bounds how long the reader threads can block;
                    // this call already discards their output on the timeout
                    // path below, so there is nothing to join.
                    let boxed: Box<dyn portable_pty::Child + Send + Sync> =
                        Box::new(crate::platform::proc::test_child::StdChild(child));
                    crate::platform::proc::terminate_child_with_grace_and_detached_reap_forcing_group_backstop(
                        boxed,
                        WORKTREE_GIT_KILL_GRACE,
                        &group,
                    );
                    return Err(AddError::TimedOut(format!(
                        "`{program} {}` timed out after {timeout:?} without exiting",
                        args.join(" "),
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

    // The child has exited, so each pipe's write end is closed and its
    // reader thread's `read_to_end` has already returned or is about to --
    // `join` here is bounded by that, not by anything still running.
    let stdout = stdout_reader
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    let stderr = stderr_reader
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    Ok(SyncGitOutput {
        status,
        stdout,
        stderr,
    })
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
fn run_status_sync(args: &[String], timeout: Duration) -> Result<(), AddError> {
    let program = crate::git_env::GIT;
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = spawn_and_wait_sync(&refs, timeout)?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    Err(AddError::Failed(format!(
        "`{program} {}` failed ({}): {}",
        refs.join(" "),
        out.status,
        stderr.trim()
    )))
}

/// Blocking, hardened twin of [`run_capture_args`] for synchronous callers
/// that need the child's actual STDOUT, not just success/failure —
/// [`read_source_origin_url`], `sync_merged_workspace_to_main`'s `git
/// status --porcelain` check, and the resume-arm's ancestry/fetch probes
/// (PRD fork#544 review-findings fix round, bundled hardening): all four
/// used to run a raw, unhardened `std::process::Command` of their own
/// rather than [`spawn_git_status_child`]'s stdin-closed/non-interactive/
/// bounded-timeout environment. Shares [`spawn_and_wait_sync`] with
/// [`run_status_sync`] rather than duplicating the spawn/poll/timeout loop.
fn run_capture_sync(args: &[String], timeout: Duration) -> Result<String, AddError> {
    let program = crate::git_env::GIT;
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = spawn_and_wait_sync(&refs, timeout)?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    Err(AddError::Failed(format!(
        "`{program} {}` failed ({}): {}",
        refs.join(" "),
        out.status,
        stderr.trim()
    )))
}

/// The prefix every [`output_within`] timeout error starts with, so a caller
/// that has to undo what a killed command left half-done can recognise one —
/// see [`is_subprocess_timeout`].
const SUBPROCESS_TIMEOUT_MARKER: &str = "timed out: ";

/// Whether `err` is the error [`output_within`] returns when it killed a
/// command at its bound, as opposed to the command failing on its own.
fn is_subprocess_timeout(err: &str) -> bool {
    err.starts_with(SUBPROCESS_TIMEOUT_MARKER)
}

/// Run `cmd` with `args` to completion, or kill it once `bound` has elapsed
/// (issue #692).
///
/// **Killed rather than left running**, which is the opposite of the choice
/// [`warm_project_environment_with`] makes, for a reason specific to these
/// calls. A timed-out fire releases the per-repository worktree lock and the
/// scheduler's `running` flag, and the next fire then starts the same `gh repo
/// clone` / `git fetch` / `git worktree add` again — so a survivor would be
/// racing its own replacement over the same directory.
///
/// **On Unix the whole process group is killed, and the direct child is reaped
/// before this returns.** Killing only the direct child is not enough: `gh repo
/// clone` runs `git clone` as a child of its own, and with its stderr not a
/// terminal that `git` prints no progress, so it would not even die of the
/// closed pipe — it would go on writing the clone directory the next fire is
/// about to use. So the command gets a process group of its own
/// (`process_group(0)`), and the timeout `SIGKILL`s that group. The group
/// outlives its leader while any member remains, so the signal reaches the
/// grandchildren even though the leader is gone first. A descendant that moved
/// itself to another group or session escapes it; nothing `gh` or `git` runs in
/// this path is known to. A group of its own also means a terminal's `^C` would
/// not reach the command — which costs nothing here, because every production
/// caller runs inside the daemon, which is detached from any terminal. Windows
/// has no process-group kill in this code, so there the direct child alone is
/// killed.
///
/// **The direct child is not reaped until both pipes have been drained.** The
/// run reads stdout and stderr to end-of-file first, and only then waits for
/// the child. So whenever the group is signalled — on a timeout, on an
/// overflow, or because the caller dropped the run part-way — the child is
/// still unreaped, a zombie at worst, and its pid, which is the group's id,
/// cannot have been reissued: the signal reaches this command's group and no
/// other, including when the child exited early and a backgrounded descendant
/// still holds a pipe open. Waiting alongside the drains instead would let the
/// child be reaped first, leaving either a stale id to signal or no id at all
/// and the descendant running.
///
/// The `.output()`-equivalent defaults are set explicitly because `spawn` does
/// not imply them: stdin is `/dev/null`, so nothing can block reading it, and
/// both output streams are captured. The whole run — the two pipes draining
/// and then the exit — sits under the bound, so a descendant that keeps a pipe
/// open after the child exits cannot stretch the wait past it either.
async fn output_within(
    cmd: tokio::process::Command,
    program: &str,
    args: &[&str],
    bound: Duration,
) -> Result<std::process::Output, String> {
    output_within_capped(cmd, program, args, bound, OutputCaps::UNBOUNDED).await
}

/// The prefix of the error [`output_within_capped`] returns when it killed a
/// command for printing more than its stdout cap — see
/// [`is_subprocess_overflow`].
const SUBPROCESS_OVERFLOW_MARKER: &str = "output too large: ";

/// Whether `err` is the error [`output_within_capped`] returns when it killed
/// a command for printing more than its stdout cap.
fn is_subprocess_overflow(err: &str) -> bool {
    err.starts_with(SUBPROCESS_OVERFLOW_MARKER)
}

/// How much of each output stream [`output_within_capped`] keeps. `None` keeps
/// all of it.
#[derive(Clone, Copy)]
struct OutputCaps {
    /// Past this many bytes of stdout the command is killed and reported with
    /// [`SUBPROCESS_OVERFLOW_MARKER`]: stdout is the result, so a truncated
    /// capture must never pass for the whole of it.
    stdout: Option<usize>,
    /// Past this many bytes of stderr the rest is read and discarded, and the
    /// capture ends with a note of how many bytes were dropped. Stderr is only
    /// ever a diagnostic, and a command that is merely chatty there (`gh` with
    /// `GH_DEBUG` set) still succeeds.
    stderr: Option<usize>,
}

impl OutputCaps {
    /// Keep everything — what [`output_within`]'s callers get.
    const UNBOUNDED: Self = Self {
        stdout: None,
        stderr: None,
    };
}

/// The process group of a spawned command, `SIGKILL`ed when this is dropped
/// unless [`GroupKill::disarm`] ran first — see [`output_within`] for why the
/// id stays the command's own while this is armed. A no-op off Unix.
struct GroupKill(Option<u32>);

impl GroupKill {
    fn kill(&self) {
        let Some(pid) = self.0 else {
            return;
        };
        #[cfg(all(test, unix))]
        group_kill_probe::record(pid);
        #[cfg(unix)]
        // SAFETY: kill(2) with a negative pid signals the process group
        // `process_group(0)` created, whose id is the child's pid. SIGKILL has
        // no failure mode beyond ESRCH/EPERM, both ignored: ESRCH means
        // everything in it has already exited. Every holder of an armed guard
        // keeps the child unreaped, so the id cannot name anybody else's group.
        unsafe {
            libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
        }
        #[cfg(not(unix))]
        let _ = pid;
    }

    /// The child has been reaped, so its id is no longer this command's.
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for GroupKill {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Test-only record, per thread, of whether each [`GroupKill::kill`] found the
/// leader still unreaped. A killed descendant cannot show that: a group
/// outlives its reaped leader while any member is left, so a signal sent after
/// the reap still reaches the descendant.
#[cfg(all(test, unix))]
mod group_kill_probe {
    use std::cell::RefCell;

    thread_local! {
        static SIGNALS: RefCell<Vec<bool>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) fn record(pid: u32) {
        let unreaped = loop {
            // SAFETY: an all-zero `siginfo_t` is a valid out-parameter, and
            // `WNOWAIT` leaves the child waitable, so this reaps nothing. It
            // returns 0 for a child that is running or a zombie, and fails
            // with ECHILD once the child has been reaped.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let rc = unsafe {
                libc::waitid(
                    libc::P_PID,
                    libc::id_t::from(pid),
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if rc == 0 {
                break true;
            }
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                break false;
            }
        };
        SIGNALS.with(|signals| signals.borrow_mut().push(unreaped));
    }

    /// The signals recorded on this thread since the last call.
    pub(super) fn take() -> Vec<bool> {
        SIGNALS.with(|signals| std::mem::take(&mut *signals.borrow_mut()))
    }
}

/// [`output_within`] with caps on the bytes captured from each stream (see
/// [`OutputCaps`]). A command that prints more stdout than its cap is killed
/// and reaped the way a timed-out one is.
async fn output_within_capped(
    mut cmd: tokio::process::Command,
    program: &str,
    args: &[&str],
    bound: Duration,
    caps: OutputCaps,
) -> Result<std::process::Output, String> {
    use std::process::Stdio;
    use tokio::io::AsyncReadExt;

    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to run `{program}`: {e}"))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    // Declared after `child`, so it is dropped first: a run dropped part-way
    // signals the group before dropping `child` can reap the leader.
    let mut group = GroupKill(child.id());

    async fn drain(pipe: Option<impl tokio::io::AsyncRead + Unpin>) -> Vec<u8> {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf).await;
        }
        buf
    }

    /// The stdout cap was exceeded.
    struct Overflow;

    /// [`drain`], reading at most one byte past `cap` so an overflow is
    /// detected without buffering the rest of the stream.
    async fn drain_capped(
        pipe: Option<impl tokio::io::AsyncRead + Unpin>,
        cap: Option<usize>,
    ) -> Result<Vec<u8>, Overflow> {
        let Some(cap) = cap else {
            return Ok(drain(pipe).await);
        };
        let mut buf = Vec::new();
        if let Some(pipe) = pipe {
            let limit = u64::try_from(cap).unwrap_or(u64::MAX).saturating_add(1);
            let _ = pipe.take(limit).read_to_end(&mut buf).await;
        }
        if buf.len() > cap {
            return Err(Overflow);
        }
        Ok(buf)
    }

    /// [`drain`], keeping the first `cap` bytes and reading the rest to
    /// end-of-file into nothing, so the command never blocks on a full pipe.
    async fn drain_prefix(
        pipe: Option<impl tokio::io::AsyncRead + Unpin>,
        cap: Option<usize>,
    ) -> Vec<u8> {
        let Some(cap) = cap else {
            return drain(pipe).await;
        };
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let limit = u64::try_from(cap).unwrap_or(u64::MAX);
            let _ = (&mut pipe).take(limit).read_to_end(&mut buf).await;
            let discarded = tokio::io::copy(&mut pipe, &mut tokio::io::sink())
                .await
                .unwrap_or(0);
            if discarded > 0 {
                buf.extend_from_slice(format!(" [{discarded} more bytes discarded]").as_bytes());
            }
        }
        buf
    }

    let outcome = tokio::time::timeout(bound, async {
        let stderr = async { Ok(drain_prefix(stderr, caps.stderr).await) };
        let (stdout, stderr) = tokio::try_join!(drain_capped(stdout, caps.stdout), stderr)?;
        // Only now that both pipes are at end-of-file — see `output_within`.
        Ok::<_, Overflow>((child.wait().await, stdout, stderr))
    })
    .await;
    let reason = match outcome {
        Ok(Ok((status, stdout, stderr))) => {
            // `id()` turns `None` once `wait` has reaped the child. A `wait`
            // that failed may not have, and then the guard still kills the
            // group as this returns.
            if child.id().is_none() {
                group.disarm();
            }
            return Ok(std::process::Output {
                status: status.map_err(|e| format!("failed to run `{program}`: {e}"))?,
                stdout,
                stderr,
            });
        }
        Ok(Err(Overflow)) => format!(
            "{SUBPROCESS_OVERFLOW_MARKER}`{program} {}` printed more than {} bytes and was killed",
            args.join(" "),
            caps.stdout.unwrap_or_default()
        ),
        Err(_) => format!(
            "{SUBPROCESS_TIMEOUT_MARKER}`{program} {}` did not finish within {}s and was \
             killed; it is treated as failed so the task's later fires are not blocked \
             behind it",
            args.join(" "),
            bound.as_secs_f64()
        ),
    };
    // Unreaped on both arms: an overflow is found before `wait` starts, and a
    // timeout cancels a `wait` that had not returned.
    group.kill();
    let _ = child.start_kill();
    let _ = child.wait().await;
    if child.id().is_none() {
        group.disarm();
    }
    Err(reason)
}

/// Run a subprocess that must exit zero and return its captured stdout. Accepts
/// `String` args (the `gh` argv helpers produce `Vec<String>`).
async fn run_capture(program: &str, args: &[String]) -> Result<String, String> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_capture_args(program, &refs).await
}

/// Like [`run_capture`] but for `&str` args.
///
/// NOT for `git` — use [`run_git_capture`], for the reason [`run_status`]
/// gives.
pub(crate) async fn run_capture_args(program: &str, args: &[&str]) -> Result<String, String> {
    capture_of(tokio::process::Command::new(program), program, args).await
}

/// [`run_capture_args`] for `git` — the fixed-shape probes (`remote get-url
/// origin`, `rev-parse --short HEAD`) build their argv inline, and the command
/// comes from [`crate::git_env::git_async`] so the `-C <dir>` in that argv is
/// what decides the repository (issue #1181).
pub(crate) async fn run_git_capture(args: &[&str]) -> Result<String, String> {
    capture_of(crate::git_env::git_async(), crate::git_env::GIT, args).await
}

/// The shared body of [`run_capture_args`] and [`run_git_capture`] — see
/// [`status_of`] for why it is shared.
async fn capture_of(
    cmd: tokio::process::Command,
    program: &str,
    args: &[&str],
) -> Result<String, String> {
    capture_within(cmd, program, args, SUBPROCESS_TIMEOUT).await
}

/// [`capture_of`] with the bound passed in — see [`status_within`].
async fn capture_within(
    cmd: tokio::process::Command,
    program: &str,
    args: &[&str],
    bound: Duration,
) -> Result<String, String> {
    let output = output_within(cmd, program, args, bound).await?;
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
    // Only this test module reads `parse_claim_fields` directly (production
    // code here goes through `parsed_claim_from_comment_json` instead, since
    // it also needs the comment's `author` — round-4 author gate), so the
    // import lives here rather than at the top of the file to avoid an
    // unused-import warning on the non-test build.
    use crate::issue_dispatch::parse_claim_fields;
    use spec::spec;

    /// Scenario: PRD #1589 (auditor S5) — the worktree cleanup a close awaits
    /// is pointed at a directory whose name carries a newline, a CR, an ESC and
    /// a bidi override and runs far past any real path. The status check fails
    /// (nothing is there), and its warning names the path escaped, on one line,
    /// bounded — never with the raw controls that would forge a log line.
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn the_worktree_cleanup_log_escapes_and_bounds_the_path() {
        #[derive(Clone, Default)]
        struct CapturedLog(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for CapturedLog {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLog {
            type Writer = CapturedLog;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let hostile = dir
            .path()
            .join(String::from("wt\nWARN forged line\r\u{1b}[2J\u{202e}") + &"x".repeat(2000));
        let captured = CapturedLog::default();
        let guard = crate::test_isolation::capture_tracing_on_this_thread(
            tracing_subscriber::fmt()
                .with_writer(captured.clone())
                .with_max_level(tracing_subscriber::filter::LevelFilter::TRACE)
                .with_ansi(false)
                .finish(),
        );
        let outcome =
            remove_worktree_outcome(&hostile, dir.path(), RemovalPolicy::KeepIfDirty).await;
        drop(guard);
        assert_eq!(outcome, WorktreeRemoval::KeptCouldNotCheck);
        let logged = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        assert!(
            logged.contains("could not check worktree status"),
            "the warning must be logged: {logged:?}"
        );
        assert_eq!(logged.trim_end().lines().count(), 1, "{logged:?}");
        for raw in ['\r', '\u{1b}', '\u{202e}'] {
            assert!(!logged.contains(raw), "{raw:?} reached the log: {logged:?}");
        }
        assert!(logged.contains("wt\\nWARN"), "{logged:?}");
        assert!(
            logged.chars().count() < 1800,
            "each value is bounded: {} chars",
            logged.chars().count()
        );
    }

    /// Issue #692: a subprocess that outlives its bound is reported as a
    /// failure naming the command and the bound, and the call returns promptly
    /// instead of waiting for the child.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_subprocess_that_outlives_its_bound_fails_with_a_clear_error() {
        let bound = Duration::from_millis(200);
        let started = std::time::Instant::now();
        let err = status_within(
            tokio::process::Command::new("sleep"),
            "sleep",
            &["30"],
            bound,
        )
        .await
        .expect_err("a 30s sleep must not pass a 200ms bound");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the bound must end the wait, took {:?}",
            started.elapsed()
        );
        assert!(
            err.contains("`sleep 30` did not finish within 0.2s and was killed"),
            "the error must name the command and the bound: {err}"
        );

        let err = capture_within(
            tokio::process::Command::new("sleep"),
            "sleep",
            &["30"],
            bound,
        )
        .await
        .expect_err("the capturing variant must be bounded too");
        assert!(err.contains("did not finish within"), "{err}");
    }

    /// Scenario: PRD #421 amendment item 10 — a command capped at 1000 bytes of
    /// stdout prints far more and keeps running; it is killed promptly and
    /// reported as an overflow, never returned as a truncated capture. Output
    /// at the cap itself is returned whole.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_subprocess_past_its_stdout_cap_is_killed_and_reported() {
        let started = std::time::Instant::now();
        let err = output_within_capped(
            tokio::process::Command::new("sh"),
            "sh",
            &["-c", "head -c 5000 /dev/zero; sleep 30"],
            Duration::from_secs(20),
            stdout_cap(1000),
        )
        .await
        .expect_err("5000 bytes must not pass a 1000-byte cap");
        assert!(is_subprocess_overflow(&err), "{err}");
        assert!(!is_subprocess_timeout(&err), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "an overflow must end the call, took {:?}",
            started.elapsed()
        );

        let out = output_within_capped(
            tokio::process::Command::new("sh"),
            "sh",
            &["-c", "head -c 1000 /dev/zero"],
            Duration::from_secs(20),
            stdout_cap(1000),
        )
        .await
        .expect("output at the cap is accepted");
        assert_eq!(out.stdout.len(), 1000);
    }

    fn stdout_cap(bytes: usize) -> OutputCaps {
        OutputCaps {
            stdout: Some(bytes),
            stderr: None,
        }
    }

    /// Scenario: PRD #421 review round 2 — the command's leader exits at once,
    /// leaving a backgrounded descendant that holds the pipes open and would
    /// touch a marker a second later; the bound expires first. The descendant
    /// is killed, and the group is signalled while the leader is still
    /// unreaped — the id was this command's, not a stale one.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_timed_out_subprocess_whose_leader_exited_first_takes_its_descendant_with_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("descendant-survived");
        let script = format!("(sleep 1; touch '{}') & exit 0", marker.display());
        let started = std::time::Instant::now();
        let err = output_within(
            tokio::process::Command::new("sh"),
            "sh",
            &["-c", &script],
            Duration::from_millis(200),
        )
        .await
        .expect_err("the descendant holds the pipes past the bound");
        assert!(is_subprocess_timeout(&err), "{err}");
        assert_eq!(group_kill_probe::take(), [true]);
        assert!(
            started.elapsed() < Duration::from_millis(900),
            "the bound must end the wait, took {:?}",
            started.elapsed()
        );
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(
            !marker.exists(),
            "the descendant of a leader that had already exited survived the timeout"
        );
    }

    /// Scenario: PRD #421 review round 2 — the leader exits at once; its
    /// backgrounded descendant then prints past a 1000-byte stdout cap and
    /// would touch a marker a second later. The call is reported as an
    /// overflow, and the descendant is killed with it rather than left running
    /// behind an error that says the call was killed.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_subprocess_overflowing_after_its_leader_exited_takes_its_descendant_with_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("descendant-survived");
        let script = format!(
            "(sleep 0.3; head -c 5000 /dev/zero; sleep 1; touch '{}') & exit 0",
            marker.display()
        );
        let err = output_within_capped(
            tokio::process::Command::new("sh"),
            "sh",
            &["-c", &script],
            Duration::from_secs(20),
            stdout_cap(1000),
        )
        .await
        .expect_err("5000 bytes must not pass a 1000-byte cap");
        assert!(is_subprocess_overflow(&err), "{err}");
        assert_eq!(group_kill_probe::take(), [true]);
        tokio::time::sleep(Duration::from_millis(2000)).await;
        assert!(
            !marker.exists(),
            "the overflowing descendant of an exited leader was left running"
        );
    }

    /// Scenario: PRD #421 review round 3 — the leader exits at once, leaving a
    /// backgrounded descendant that holds the pipes open, touches a readiness
    /// file and would touch a marker a second later. The caller drops the
    /// unbounded call long before its bound, and the descendant is killed with
    /// it rather than left running behind a run nobody is waiting on. The group
    /// is signalled while the leader is still unreaped, so its id cannot have
    /// been reissued to anybody else.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_dropped_call_whose_leader_exited_first_takes_its_descendant_with_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ready = dir.path().join("descendant-ready");
        let marker = dir.path().join("descendant-survived");
        let script = format!(
            "(sleep 0.2; touch '{}'; sleep 1; touch '{}') & exit 0",
            ready.display(),
            marker.display()
        );
        let args = ["-c", script.as_str()];
        let mut call = Box::pin(output_within(
            tokio::process::Command::new("sh"),
            "sh",
            &args,
            Duration::from_secs(60),
        ));
        let started = std::time::Instant::now();
        while !ready.exists() {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "the descendant never signalled readiness"
            );
            if let Ok(result) = tokio::time::timeout(Duration::from_millis(20), &mut call).await {
                panic!("the call ended while the descendant held its pipes: {result:?}");
            }
        }
        group_kill_probe::take();
        drop(call);
        assert_eq!(
            group_kill_probe::take(),
            [true],
            "dropping the call must signal the group once, before the leader is reaped"
        );
        tokio::time::sleep(Duration::from_millis(2000)).await;
        assert!(
            !marker.exists(),
            "the descendant of an exited leader outlived the dropped call"
        );
    }

    /// Scenario: PRD #421 review round 2 — a command floods stderr with 10 MB
    /// and then succeeds. With a 4 KiB stderr cap it is still a success: its
    /// stdout comes back whole, and its stderr is the first 4 KiB plus a note
    /// of how many bytes were discarded, not the whole flood.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_stderr_flood_keeps_a_bounded_prefix_and_the_call_succeeds() {
        let out = output_within_capped(
            tokio::process::Command::new("sh"),
            "sh",
            &[
                "-c",
                "head -c 10000000 /dev/zero | tr '\\0' e >&2; echo done",
            ],
            Duration::from_secs(20),
            OutputCaps {
                stdout: Some(1000),
                stderr: Some(4096),
            },
        )
        .await
        .expect("a stderr flood is not a failure");
        assert!(out.status.success(), "{:?}", out.status);
        assert_eq!(out.stdout, b"done\n");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.starts_with(&"e".repeat(4096)),
            "the prefix is kept: {:?}",
            &stderr[..64.min(stderr.len())]
        );
        assert!(
            stderr.ends_with(" [9995904 more bytes discarded]"),
            "{:?}",
            &stderr[4096..]
        );
        assert!(
            out.stderr.len() < 4096 + 64,
            "{} bytes kept",
            out.stderr.len()
        );
    }

    /// Scenario: PRD #421 amendment item 9 — the claim-call bound is 30 s
    /// unless the override holds a positive millisecond count.
    #[test]
    fn the_claim_call_timeout_defaults_to_30s_and_takes_an_override() {
        assert_eq!(claim_call_timeout_from(None), Duration::from_secs(30));
        assert_eq!(
            claim_call_timeout_from(Some("1500")),
            Duration::from_millis(1500)
        );
        for ignored in ["", "0", "-5", "abc", "1.5"] {
            assert_eq!(
                claim_call_timeout_from(Some(ignored)),
                Duration::from_secs(30),
                "{ignored:?}"
            );
        }
    }

    /// Scenario: PRD #421 amendment item 12 — the run's claim budget is 120 s
    /// unless the override holds a positive millisecond count.
    #[test]
    fn the_claim_run_budget_defaults_to_120s_and_takes_an_override() {
        assert_eq!(claim_run_budget_from(None), Duration::from_secs(120));
        assert_eq!(
            claim_run_budget_from(Some("3000")),
            Duration::from_millis(3000)
        );
        for ignored in ["", "0", "-5", "abc", "1.5"] {
            assert_eq!(
                claim_run_budget_from(Some(ignored)),
                Duration::from_secs(120),
                "{ignored:?}"
            );
        }
    }

    /// Scenario: PRD #421 amendment item 12 — each call is bounded by the
    /// shorter of the per-call bound and what is left of the run's budget, and
    /// once the calls have spent it, no further call gets a bound.
    #[test]
    fn a_claim_call_is_bounded_by_what_is_left_of_the_run_budget() {
        let mut budget = ClaimBudget {
            per_call: Duration::from_secs(5),
            remaining: Duration::from_secs(12),
        };
        assert_eq!(budget.next_bound(), Some(Duration::from_secs(5)));
        budget.spend(Duration::from_secs(5));
        budget.spend(Duration::from_secs(5));
        assert_eq!(budget.next_bound(), Some(Duration::from_secs(2)));
        assert!(!budget.is_spent());
        // A call overruns its bound slightly before it is reaped.
        budget.spend(Duration::from_millis(2_050));
        assert!(budget.is_spent());
        assert_eq!(budget.next_bound(), None);
    }

    fn claim_run_with(remaining: Duration) -> ClaimRun {
        ClaimRun::with_budget(
            "host",
            ClaimBudget {
                per_call: Duration::from_secs(30),
                remaining,
            },
        )
    }

    /// Scenario: PRD #421 amendment item 14 — a failed label add provisions
    /// the label once more only while provisioning has not succeeded in the
    /// run, only once per run, and only with budget left.
    #[test]
    fn a_failed_label_add_reprovisions_once_while_the_label_may_be_missing() {
        let mut run = claim_run_with(Duration::from_secs(120));
        run.provisioned = Some(Err("HTTP 502".into()));
        assert!(run.should_reprovision(), "transient provisioning failure");
        run.reprovisioned = true;
        assert!(!run.should_reprovision(), "at most once per run");

        let mut run = claim_run_with(Duration::from_secs(120));
        run.provisioned = Some(Ok(()));
        assert!(!run.should_reprovision(), "the label exists");

        let mut run = claim_run_with(Duration::ZERO);
        run.provisioned = Some(Err("HTTP 502".into()));
        assert!(!run.should_reprovision(), "no budget left for the calls");
    }

    /// Scenario: PRD #421 amendment item 13 — only a successful account
    /// lookup is kept for the rest of the run; a failed one is not, so the
    /// next claim looks the account up again.
    #[test]
    fn only_a_successful_account_lookup_is_kept() {
        let mut run = claim_run_with(Duration::from_secs(120));
        run.remember_account(&Err("HTTP 502".into()));
        assert_eq!(run.account, None);
        run.remember_account(&Ok(54321));
        assert_eq!(run.account, Some(54321));
    }

    /// Scenario: PRD #421 M3.1 — a run ages claim records on GitHub's clock,
    /// read from the account lookup's `Date`, so a machine whose clock is an
    /// hour fast or slow reaches the same decisions as one in step with
    /// GitHub; without a `Date` the local clock stands in.
    #[test]
    fn claim_decisions_use_githubs_clock_whatever_the_local_clock_says() {
        use crate::issue_dispatch::{
            ClaimIdentity, IssueClaimRecord, claim_refresh_due, live_claimant,
            parse_authenticated_user, parse_issue_claim_records, render_claim_record,
        };
        let github = |rfc3339: &str| {
            chrono::DateTime::parse_from_rfc3339(rfc3339)
                .unwrap()
                .with_timezone(&chrono::Utc)
        };
        let server_now = github("2026-10-23T12:00:00Z");
        let lookup = parse_authenticated_user(
            "HTTP/2.0 200 OK\nDate: Fri, 23 Oct 2026 12:00:00 GMT\r\n\r\n{\"id\":1}",
        )
        .unwrap();
        let ttl = chrono::Duration::hours(1);
        let me = ClaimIdentity {
            user_id: 1,
            task: "mine",
            host_id: "h1-aaaa",
        };
        let record = |user: u64, task: &str, updated_at: &str| -> IssueClaimRecord {
            let json = serde_json::json!([{
                "id": 9, "user": {"id": user, "login": "teammate"},
                "author_association": "COLLABORATOR",
                "body": render_claim_record(task, "h1-aaaa", server_now),
                "updated_at": updated_at,
            }]);
            parse_issue_claim_records(&json.to_string())
                .unwrap()
                .remove(0)
        };
        // A foreign record edited 50 minutes ago on GitHub's clock is live
        // under a one-hour TTL; the deck's own record, edited just now, is
        // not due for a refresh.
        let foreign = record(2, "theirs", "2026-10-23T11:10:00Z");
        let own = record(1, "mine", "2026-10-23T12:00:00Z");
        for skew in [
            chrono::Duration::zero(),
            chrono::Duration::hours(1),
            -chrono::Duration::hours(1),
        ] {
            let local_now = server_now + skew;
            let offset = lookup
                .server_date
                .map(|server| server.signed_duration_since(local_now));
            let now = clock_now(local_now, offset);
            assert_eq!(now, server_now, "skew {skew}");
            assert!(
                live_claimant(std::slice::from_ref(&foreign), &me, false, None, now, ttl).is_some(),
                "skew {skew}"
            );
            assert!(!claim_refresh_due(now, own.updated_at, ttl), "skew {skew}");
        }
        // What the local clock alone would decide: an hour fast reads the
        // live foreign record as expired and edits the fresh own record on
        // every fire; an hour slow reads the foreign record as dated ahead.
        let fast = clock_now(server_now + chrono::Duration::hours(1), None);
        assert!(
            live_claimant(std::slice::from_ref(&foreign), &me, false, None, fast, ttl).is_none()
        );
        assert!(claim_refresh_due(fast, own.updated_at, ttl));
        let slow = clock_now(server_now - chrono::Duration::hours(1), None);
        assert!(
            live_claimant(std::slice::from_ref(&foreign), &me, false, None, slow, ttl).is_none()
        );
    }

    /// Scenario: PRD #421 M3.1 — a run starts on this machine's clock and
    /// moves onto GitHub's only once the account lookup measured an offset.
    #[test]
    fn a_claim_run_uses_the_local_clock_until_an_offset_is_measured() {
        let mut run = claim_run_with(Duration::from_secs(120));
        let before = chrono::Utc::now();
        let now = run.now();
        assert!(now >= before && now <= chrono::Utc::now());
        run.clock_offset = Some(chrono::Duration::hours(-3));
        let now = run.now();
        assert!(now < chrono::Utc::now() - chrono::Duration::minutes(170));
    }

    /// Scenario: PRD #421 amendment item 12 — once the budget is spent, a
    /// claim call is reported as skipped for the budget without running `gh`.
    #[tokio::test]
    async fn a_claim_call_after_the_budget_is_spent_is_skipped() {
        let mut run = claim_run_with(Duration::ZERO);
        let error = run
            .call(&["--version".to_string()], "adding the label", 1024)
            .await
            .unwrap_err();
        assert_eq!(
            error,
            "adding the label was skipped: the run's claim time budget was used up"
        );
    }

    /// Issue #692: the timed-out child is killed, not left running to race the
    /// next fire's attempt at the same clone or worktree. The child would write
    /// a marker one second in; the bound expires long before that, so a marker
    /// that appears means the child survived.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_timed_out_subprocess_is_killed_rather_than_left_running() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("survived");
        let script = format!("sleep 1; touch '{}'", marker.display());
        let err = status_within(
            tokio::process::Command::new("sh"),
            "sh",
            &["-c", &script],
            Duration::from_millis(100),
        )
        .await
        .expect_err("the bound must expire before the script finishes");
        assert!(err.contains("was killed"), "{err}");
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(
            !marker.exists(),
            "the timed-out child ran to completion, so it was left running"
        );
    }

    /// PR #1304 (Qodo): the kill reaches the command's DESCENDANTS, not only
    /// the direct child — the shape of `gh repo clone`, whose `git clone` would
    /// otherwise go on writing the clone directory the next fire uses. The
    /// marker is written by a backgrounded grandchild one second in; the bound
    /// expires long before that.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_timed_out_subprocess_takes_its_descendants_with_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("grandchild-survived");
        let script = format!("(sleep 1; touch '{}') & wait", marker.display());
        let err = status_within(
            tokio::process::Command::new("sh"),
            "sh",
            &["-c", &script],
            Duration::from_millis(100),
        )
        .await
        .expect_err("the bound must expire before the grandchild finishes");
        assert!(is_subprocess_timeout(&err), "{err}");
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(
            !marker.exists(),
            "the timed-out command's grandchild ran to completion, so only the \
             direct child was killed"
        );
    }

    /// Only the timeout is recognised as one: a command that fails on its own
    /// must not trigger the cleanup a killed command needs.
    #[cfg(unix)]
    #[tokio::test]
    async fn only_a_timeout_is_classified_as_one() {
        let err = status_within(
            tokio::process::Command::new("sh"),
            "sh",
            &["-c", "echo 'did not finish within' >&2; exit 1"],
            Duration::from_secs(30),
        )
        .await
        .expect_err("a non-zero exit must fail");
        assert!(!is_subprocess_timeout(&err), "{err}");
    }

    /// PR #1304 (Greptile, Qodo): a clone that fails partway leaves no
    /// `clone_dir` and no staging directory, so the next fire clones again
    /// instead of taking a half-written directory for a finished clone; each
    /// call stages under its own name, so a concurrent attempt's work in
    /// progress is untouched; a clone that succeeds lands at `clone_dir`; and
    /// one that loses the race to an attempt that already finished leaves the
    /// winner's clone in place and says so, so the caller vets its origin.
    #[tokio::test]
    async fn a_failed_clone_leaves_nothing_a_later_fire_would_mistake_for_a_clone() {
        let ws = tempfile::tempdir().expect("tempdir");
        let clone_dir = ws.path().join("repo");
        let entries = || -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(ws.path())
                .expect("read workspace")
                .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        };

        let err = clone_via_staging(&clone_dir, |staging| async move {
            tokio::fs::create_dir_all(staging.join(".git"))
                .await
                .expect("start a partial clone");
            Err("timed out: `gh repo clone` did not finish".to_string())
        })
        .await
        .expect_err("a failed clone must fail");
        assert!(err.contains("did not finish"), "{err}");
        assert!(
            entries().is_empty(),
            "a failed clone must leave neither clone_dir nor its staging dir: {:?}",
            entries()
        );

        let concurrent = ws.path().join("repo.cloning-concurrent");
        tokio::fs::create_dir_all(concurrent.join(".git"))
            .await
            .expect("another attempt, mid-clone");
        let placed = clone_via_staging(&clone_dir, |staging| async move {
            tokio::fs::create_dir_all(staging.join(".git"))
                .await
                .expect("clone");
            Ok(())
        })
        .await;
        assert_eq!(placed, Ok(StagedClone::Placed));
        assert!(
            clone_dir.join(".git").is_dir(),
            "the clone lands at clone_dir"
        );
        assert!(
            concurrent.join(".git").is_dir(),
            "another attempt's staging directory must be left alone"
        );

        tokio::fs::write(clone_dir.join("winner"), "")
            .await
            .expect("mark the finished clone");
        let lost = clone_via_staging(&clone_dir, |staging| async move {
            tokio::fs::create_dir_all(staging.join(".git"))
                .await
                .expect("clone");
            tokio::fs::write(staging.join("loser"), "")
                .await
                .expect("mark this copy");
            Ok(())
        })
        .await;
        assert_eq!(
            lost,
            Ok(StagedClone::LostRace),
            "a clone somebody else placed first is reported as theirs, for the caller to vet"
        );
        assert!(
            clone_dir.join("winner").exists(),
            "the winner's clone is kept"
        );
        assert!(!clone_dir.join("loser").exists());
        assert_eq!(
            entries(),
            vec!["repo".to_string(), "repo.cloning-concurrent".to_string()],
            "the losing copy's staging directory is discarded"
        );

        // A destination the clone can never be moved onto — a file — fails the
        // placement, and the copy it could not place is discarded rather than
        // left to accumulate one per fire. Unix only: Windows' `rename` replaces
        // an existing FILE with the directory (measured on `build-windows`,
        // PR #1304), so there a file is not a destination that refuses.
        #[cfg(unix)]
        {
            let blocked = ws.path().join("blocked");
            tokio::fs::write(&blocked, "not a directory")
                .await
                .expect("occupy the destination");
            let err = clone_via_staging(&blocked, |staging| async move {
                tokio::fs::create_dir_all(staging.join(".git"))
                    .await
                    .expect("clone");
                Ok(())
            })
            .await
            .expect_err("a clone that cannot be placed must fail");
            assert!(err.contains("could not move it"), "{err}");
            assert_eq!(
                entries(),
                vec![
                    "blocked".to_string(),
                    "repo".to_string(),
                    "repo.cloning-concurrent".to_string()
                ],
                "the unplaceable copy's staging directory is discarded"
            );
        }
    }

    /// PR #1304 (Qodo): the branch probe tells "absent" from "could not
    /// tell", because only a definite absence lets a timed-out add's cleanup
    /// delete the branch. A directory that is not a repository makes `git`
    /// exit 128, which must not read as absent.
    #[tokio::test]
    async fn the_branch_probe_does_not_mistake_a_failed_probe_for_an_absent_branch() {
        let scratch = crate::test_temp::tempdir().expect("scratch tempdir");
        let repo = scratch.path().join("repo");
        init_repo_with_commit(scratch.path(), &repo);
        let clone = repo.to_string_lossy();
        assert_eq!(
            probe_branch(&clone, "refs/heads/main", WORKTREE_GIT_TIMEOUT).await,
            BranchProbe::Present
        );
        assert_eq!(
            probe_branch(
                &clone,
                "refs/heads/agent/no-such-branch",
                WORKTREE_GIT_TIMEOUT
            )
            .await,
            BranchProbe::Absent
        );
        let not_a_repo = scratch.path().join("not-a-repo");
        std::fs::create_dir_all(&not_a_repo).expect("create a plain directory");
        assert_eq!(
            probe_branch(
                &not_a_repo.to_string_lossy(),
                "refs/heads/main",
                WORKTREE_GIT_TIMEOUT
            )
            .await,
            BranchProbe::Unknown
        );
    }

    /// PR #1304 (Greptile): what a killed `git worktree add` leaves — a
    /// directory, an administrative entry still `locked` as initializing, and
    /// the branch its `-b` made — is removed, so a later fire sees the issue as
    /// unclaimed rather than skipping it forever.
    #[tokio::test]
    async fn a_timed_out_worktree_add_is_discarded_with_its_branch() {
        let scratch = crate::test_temp::tempdir().expect("scratch tempdir");
        let repo = scratch.path().join("repo");
        init_repo_with_commit(scratch.path(), &repo);
        let worktree_dir = scratch.path().join("repo-issue-5");
        let branch = "agent/issue-5";
        let outcome = create_worktree(
            &repo,
            &worktree_dir,
            branch,
            false,
            Creator::issue_dispatch("unit", 5),
        )
        .await;
        assert!(
            matches!(outcome, Ok(WorktreeCreation::Created { .. })),
            "the setup creation must succeed, got {outcome:?}"
        );
        // The state an add is in before it finishes: its entry locked.
        std::fs::write(
            repo.join(".git")
                .join("worktrees")
                .join("repo-issue-5")
                .join("locked"),
            "initializing",
        )
        .expect("lock the entry the way an unfinished add leaves it");

        discard_timed_out_worktree(&repo, &worktree_dir, Some(branch)).await;

        assert!(
            !worktree_dir.exists(),
            "the worktree directory must be gone"
        );
        let git = |args: &[&str]| {
            let out = crate::git_env::fixture_git(&repo, scratch.path())
                .args(args)
                .output()
                .expect("run git");
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        let listed = git(&["worktree", "list", "--porcelain"]);
        assert!(
            !listed.contains("repo-issue-5"),
            "the administrative entry must be gone: {listed}"
        );
        assert!(
            git(&["branch", "--list", branch]).trim().is_empty(),
            "the branch the add created must be gone"
        );
    }

    /// The bound changes nothing for a call that finishes inside it: success
    /// and a non-zero exit read exactly as they did before issue #692.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_subprocess_inside_its_bound_reports_as_before() {
        let out = capture_within(
            tokio::process::Command::new("sh"),
            "sh",
            &["-c", "printf ok"],
            Duration::from_secs(30),
        )
        .await
        .expect("a quick success must pass");
        assert_eq!(out, "ok");
        let err = status_within(
            tokio::process::Command::new("sh"),
            "sh",
            &["-c", "echo nope >&2; exit 3"],
            Duration::from_secs(30),
        )
        .await
        .expect_err("a non-zero exit must fail");
        assert!(err.contains("failed (exit status: 3): nope"), "{err}");
    }

    /// Test mutex covering temporary process-global env var mutation
    /// (`std::env::set_var` is process-global) — matches `agent_pty.rs`'s
    /// `ENV_TEST_LOCK` precedent for this exact class of test. Named for its
    /// original use (`GIT_CONFIG_GLOBAL` scoping); [`ScopedEnvVar`] now
    /// also serializes non-`GIT_CONFIG_GLOBAL` mutations (e.g. `LANG`)
    /// through the same lock, since any two tests mutating process env
    /// concurrently — regardless of which var — need to be serialized
    /// against each other under a plain `cargo test`, not just against
    /// other users of the same var.
    static GIT_CONFIG_GLOBAL_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// RAII guard for a scoped `std::env` mutation in tests — restores the
    /// previous value (or removes the var entirely if it was unset) on
    /// drop, including when the guarded call panics (PRD fork#325 fix round
    /// 4, auditor D6): the straight-line set-then-restore code this
    /// replaces left the override in place for the rest of the test binary
    /// process under a plain `cargo test` if anything between the two
    /// panicked. Holds [`GIT_CONFIG_GLOBAL_TEST_LOCK`] for its whole
    /// lifetime, so the mutation stays serialized against any other test in
    /// this module doing the same — inert under nextest, which this project
    /// uses (one process per test, so no sibling test ever shares the
    /// mutation), but real under a bare `cargo test` run.
    struct ScopedEnvVar {
        key: &'static str,
        previous: Option<String>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl ScopedEnvVar {
        fn set(key: &'static str, value: &str) -> Self {
            let lock = GIT_CONFIG_GLOBAL_TEST_LOCK
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let previous = std::env::var(key).ok();
            // SAFETY: serialized by GIT_CONFIG_GLOBAL_TEST_LOCK for this
            // guard's entire lifetime; restored on drop below, including on
            // an unwinding panic, so no other test in this process ever
            // observes the override.
            unsafe {
                std::env::set_var(key, value);
            }
            Self {
                key,
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for ScopedEnvVar {
        fn drop(&mut self) {
            // SAFETY: see `set` above — still holding the lock.
            unsafe {
                match self.previous.take() {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

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
            false,
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
            false,
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
            false,
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

    // --- issue #717: the kept-worktree close preview ---

    /// A real repo with one commit plus a linked worktree at `worktree`, so the
    /// preview's `git status --porcelain` runs against a genuine tree rather
    /// than a stub. Mirrors `dispatch::tests::init_repo`.
    ///
    /// **Every `git` here goes through [`crate::git_env::fixture_git`]**,
    /// which switches off the ambient git environment in both of the groups
    /// issue #834 names: *configuration* (`GIT_CONFIG_GLOBAL`/`SYSTEM` at a
    /// nonexistent path, `HOME` and `XDG_CONFIG_HOME` inside the sandbox) and
    /// *location* (the eight discovery variables cleared, plus
    /// `GIT_CEILING_DIRECTORIES` bounding the upward walk at `sandbox_root`).
    /// Without it these commands ran with only `.current_dir(dir)`, and git's
    /// location discovery **outranks** a process's cwd: with an ambient
    /// `GIT_DIR` — routine under `rebase --exec`, a pre-commit hook or
    /// `bisect run` — `init`, `commit` and `worktree add` are writes against
    /// whatever repository that names, which under `cargo test` can be the
    /// checkout the suite is running in. It also supplies the commit identity
    /// by environment, so the fixture no longer writes `user.name`/`user.email`
    /// into a repository to configure one.
    ///
    /// Reached here from issue #1121 round two: a sibling test in this module
    /// went red once in eight plain `cargo test` runs and could not be
    /// reproduced, and this was the one concrete candidate cause — under
    /// `cargo test` (threads, one process) it shares its environment with
    /// `worktree_owner`'s `with_ambient_git_dir_at`, which sets those very
    /// variables process-globally under a lock this fixture did not take.
    fn init_repo_with_worktree(sandbox_root: &Path, repo: &Path, worktree: &Path) {
        let run = |dir: &Path, args: &[&str]| {
            let out = crate::git_env::fixture_git(dir, sandbox_root)
                .args(args)
                .output()
                .expect("git available");
            assert!(out.status.success(), "git {args:?} failed: {out:?}");
        };
        std::fs::create_dir_all(repo).unwrap();
        run(repo, &["init", "-q", "."]);
        // Immune today only because `a.txt` holds no newline; pinned anyway so
        // that adding one stays a content change rather than a Windows-only
        // failure three modules away (`pin_fixture_eol`).
        crate::worktree_owner::pin_fixture_eol(repo, sandbox_root);
        std::fs::write(repo.join("a.txt"), "hi").unwrap();
        run(repo, &["add", "."]);
        run(repo, &["commit", "-qm", "init"]);
        run(
            repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "wt",
                &worktree.to_string_lossy(),
            ],
        );
    }

    /// One registered pane whose cwd is `worktree`.
    fn pane_in(pane_id: &str, worktree: &Path) -> AgentRecord {
        let mut r = record(Some(&worktree.to_string_lossy()), None);
        r.pane_id_env = Some(pane_id.to_string());
        r
    }

    const PROBE: Duration = Duration::from_secs(5);

    /// Scenario: an issue's worktree has just been created when its agent is
    /// refused because the daemon is restarting. The worktree is removed again
    /// and the issue deferred — not failed — so the next fire, here or on the
    /// successor, does not find the issue claimed by an empty worktree. Any
    /// other spawn failure keeps the old behaviour: reported, tree left on disk
    /// (PRD #1487 re-check, reviewer R3).
    #[tokio::test]
    async fn a_restart_refusal_releases_the_issue_worktree_instead_of_claiming_it() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("repo-issue-7");
        init_repo_with_worktree(tmp.path(), &repo, &wt);
        let reg = new_worktree_registry();
        record_worktree(&reg, &wt, &repo, RemovalPolicy::Force);

        let outcome = settle_failed_issue_spawn(
            crate::spawn::SpawnError::DaemonRestarting,
            &reg,
            &wt,
            Creator::issue_dispatch("nightly", 7),
        )
        .await;
        assert_eq!(outcome, Ok(IssueOutcome::DeferredForRestart));
        assert!(
            !wt.exists(),
            "the refused issue's worktree was removed again"
        );
        assert_eq!(take_worktree(&reg, &wt), None, "and its record dropped");
        assert_eq!(
            dispatch_decision(wt.exists(), false, false, None),
            DispatchDecision::Dispatch,
            "so the next fire dispatches the issue"
        );

        let wt2 = tmp.path().join("repo-issue-8");
        crate::git_env::fixture_git(&repo, tmp.path())
            .args(["worktree", "add", "-q", "-b", "wt2", &wt2.to_string_lossy()])
            .output()
            .expect("git available");
        record_worktree(&reg, &wt2, &repo, RemovalPolicy::Force);
        let failed = settle_failed_issue_spawn(
            crate::spawn::SpawnError::Agent("no such command".into()),
            &reg,
            &wt2,
            Creator::issue_dispatch("nightly", 8),
        )
        .await;
        assert!(failed.is_err(), "a real failure is still reported");
        assert!(wt2.exists(), "and leaves its tree as before");
    }

    /// Scenario: a restart refuses the issue's spawn, and the repository's
    /// worktree lock that guards removing its new worktree cannot be taken.
    /// Nothing is removed: the worktree is marked as abandoned and the issue
    /// reports a recoverable failure; the next attempt, which can take the
    /// lock, reclaims it and frees the issue (PRD #1487 review).
    #[tokio::test]
    async fn a_restart_refusal_without_the_worktree_lock_keeps_the_worktree_for_a_retry() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("repo-issue-7");
        init_repo_with_worktree(tmp.path(), &repo, &wt);
        let creator = || Creator::issue_dispatch("nightly", 7);
        crate::worktree_owner::write_marker(&wt, "wt", &creator()).unwrap();
        let reg = new_worktree_registry();
        // A clone whose lock path cannot be resolved: no lock can be taken.
        record_worktree(
            &reg,
            &wt,
            &tmp.path().join("no-such-clone"),
            RemovalPolicy::Force,
        );

        let outcome = settle_failed_issue_spawn(
            crate::spawn::SpawnError::DaemonRestarting,
            &reg,
            &wt,
            creator(),
        )
        .await;
        let message = outcome.expect_err("an unremoved worktree is not a clean deferral");
        assert!(
            message.contains("lock")
                && message.contains(&wt.display().to_string())
                && message.contains("retries"),
            "the report names the lock, the leftover, and the retry: {message}"
        );
        assert!(wt.exists(), "nothing is removed without the lock");
        assert!(
            crate::worktree_owner::is_abandoned_spawn_of(&wt, &creator()),
            "the leftover is marked for the retry"
        );
        assert_eq!(take_worktree(&reg, &wt), None, "and its record dropped");

        assert_eq!(
            reclaim_abandoned_spawn(&wt, &repo, creator(), &reg, &AgentPtyRegistry::new()).await,
            Reclaim::Removed,
            "the next attempt reclaims it under the lock"
        );
        assert!(!wt.exists());
    }

    /// Scenario: the same restart refusal, but `git worktree remove --force`
    /// fails (the worktree is locked). The issue is reported as failed — not a
    /// clean deferral — and the leftover stays recoverable: the next attempt
    /// retries the removal before the worktree-exists check can read it as a
    /// claim, refusing while an agent is rooted there, a live dispatch records
    /// it, or the mark names another issue; and once the lock is gone the
    /// retry removes it and frees the issue (PRD #1487 final audit F2).
    #[tokio::test]
    async fn a_failed_removal_after_a_restart_refusal_is_reported_and_retried_next_attempt() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("repo-issue-7");
        init_repo_with_worktree(tmp.path(), &repo, &wt);
        let creator = || Creator::issue_dispatch("nightly", 7);
        // What `create_worktree`'s success arm leaves: the deck's own claim.
        crate::worktree_owner::write_marker(&wt, "wt", &creator()).unwrap();
        let git = |args: &[&str]| {
            let out = crate::git_env::fixture_git(&repo, tmp.path())
                .args(args)
                .output()
                .expect("git available");
            assert!(out.status.success(), "git {args:?} failed: {out:?}");
        };
        // A locked worktree refuses a single `--force`.
        git(&["worktree", "lock", &wt.to_string_lossy()]);
        let reg = new_worktree_registry();
        record_worktree(&reg, &wt, &repo, RemovalPolicy::Force);

        let outcome = settle_failed_issue_spawn(
            crate::spawn::SpawnError::DaemonRestarting,
            &reg,
            &wt,
            creator(),
        )
        .await;
        let message = outcome.expect_err("a failed removal is not a clean deferral");
        assert!(
            message.contains(&wt.display().to_string()) && message.contains("retries"),
            "the report names the leftover and says it will be retried: {message}"
        );
        assert!(wt.exists(), "the removal really did fail");
        assert!(
            crate::worktree_owner::is_abandoned_spawn_of(&wt, &creator()),
            "the leftover is marked as abandoned by this task for this issue"
        );

        // Still locked: the retry runs, fails again, and says so.
        assert!(matches!(
            reclaim_abandoned_spawn(&wt, &repo, creator(), &reg, &AgentPtyRegistry::new()).await,
            Reclaim::Failed(_)
        ));
        git(&["worktree", "unlock", &wt.to_string_lossy()]);

        // Never someone else's: an agent starting in it, a live dispatch's
        // record, or a mark naming another issue each keep the claim.
        let agents = AgentPtyRegistry::new();
        let starting = agents.reserve_spawn_in_for_test(&wt).unwrap();
        assert_eq!(
            reclaim_abandoned_spawn(&wt, &repo, creator(), &reg, &agents).await,
            Reclaim::NotAbandoned
        );
        agents.release_spawn_for_test(&starting);
        record_worktree(&reg, &wt, &repo, RemovalPolicy::Force);
        assert_eq!(
            reclaim_abandoned_spawn(&wt, &repo, creator(), &reg, &AgentPtyRegistry::new()).await,
            Reclaim::NotAbandoned
        );
        take_worktree(&reg, &wt);
        assert_eq!(
            reclaim_abandoned_spawn(
                &wt,
                &repo,
                Creator::issue_dispatch("nightly", 8),
                &reg,
                &AgentPtyRegistry::new()
            )
            .await,
            Reclaim::NotAbandoned
        );
        assert!(wt.exists(), "none of those removed anything");

        assert_eq!(
            reclaim_abandoned_spawn(&wt, &repo, creator(), &reg, &AgentPtyRegistry::new()).await,
            Reclaim::Removed
        );
        assert!(!wt.exists(), "the retry removed the leftover");
        assert_eq!(
            dispatch_decision(wt.exists(), false, false, None),
            DispatchDecision::Dispatch,
            "so the issue is dispatched rather than skipped as claimed"
        );
    }

    /// Scenario: a worktree the deck created and an agent is (or was) using
    /// carries the ownership marker but no abandoned-spawn mark. The retry
    /// path leaves it alone, so the worktree-exists check still reads it as
    /// the issue being claimed (PRD #1487 final audit F2).
    #[tokio::test]
    async fn the_abandoned_spawn_retry_never_removes_a_worktree_no_failed_spawn_left() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("repo-issue-7");
        init_repo_with_worktree(tmp.path(), &repo, &wt);
        let creator = Creator::issue_dispatch("nightly", 7);
        crate::worktree_owner::write_marker(&wt, "wt", &creator).unwrap();
        let reg = new_worktree_registry();
        assert_eq!(
            reclaim_abandoned_spawn(&wt, &repo, creator, &reg, &AgentPtyRegistry::new()).await,
            Reclaim::NotAbandoned
        );
        assert!(wt.exists());
    }

    /// Scenario: two reclaimers (an old daemon and its successor, say) both
    /// validate the same abandoned worktree. One is held just before it takes
    /// the repository's worktree lock while the other removes the leftover and
    /// re-creates the issue's worktree — first left unmarked, as a fresh
    /// dispatch leaves it, then abandoned again by the same task under a new
    /// generation. Released, the delayed reclaimer re-validates under the lock
    /// and leaves the re-created worktree on disk both times (PRD #1487, the
    /// F2 reclaim race).
    #[tokio::test]
    async fn a_delayed_reclaimer_never_removes_a_worktree_recreated_while_it_waited() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("repo-issue-7");
        init_repo_with_worktree(tmp.path(), &repo, &wt);
        let creator = || Creator::issue_dispatch("nightly", 7);
        crate::worktree_owner::write_marker(&wt, "wt", &creator()).unwrap();
        let reg = new_worktree_registry();

        for abandoned_again in [false, true] {
            let stale = crate::worktree_owner::mark_abandoned_spawn(&wt, &creator()).unwrap();
            let (reached_tx, reached_rx) = tokio::sync::oneshot::channel::<()>();
            let (resume_tx, resume_rx) = tokio::sync::oneshot::channel::<()>();
            let agents = AgentPtyRegistry::new();
            let delayed = reclaim_abandoned_spawn_paced(
                &wt,
                &repo,
                creator(),
                &reg,
                &agents,
                async move {
                    reached_tx.send(()).unwrap();
                    resume_rx.await.unwrap();
                },
                std::future::ready(()),
            );
            let (wt_ref, repo_ref, reg_ref, agents) = (&wt, &repo, &reg, &agents);
            let other = async move {
                reached_rx.await.unwrap();
                assert_eq!(
                    reclaim_abandoned_spawn(wt_ref, repo_ref, creator(), reg_ref, agents).await,
                    Reclaim::Removed,
                    "the first reclaimer removes the leftover"
                );
                assert!(!wt_ref.exists());
                let recreated = create_worktree(repo_ref, wt_ref, "wt", true, creator()).await;
                assert!(
                    matches!(recreated, Ok(WorktreeCreation::Created { .. })),
                    "and a fresh dispatch re-creates the issue's worktree: {recreated:?}"
                );
                if abandoned_again {
                    let fresh =
                        crate::worktree_owner::mark_abandoned_spawn(wt_ref, &creator()).unwrap();
                    assert_ne!(fresh, stale, "each abandonment has its own generation");
                }
                resume_tx.send(()).unwrap();
            };
            let (outcome, ()) = tokio::join!(delayed, other);
            assert_eq!(
                outcome,
                Reclaim::NotAbandoned,
                "the delayed reclaimer validated a generation that no longer exists \
                 (abandoned again: {abandoned_again})"
            );
            assert!(
                wt.exists() && crate::worktree_owner::is_marked(&wt),
                "the re-created worktree survives the delayed reclaimer \
                 (abandoned again: {abandoned_again})"
            );
        }
    }

    /// Scenario: an agent is started in an abandoned worktree while it is being
    /// reclaimed. Started after the reclaim's first pass and still unpublished
    /// when its last check runs, it keeps the worktree. Started after that check
    /// and before the removal, it is refused, so the removal deletes no
    /// directory an agent runs in. Once the reclaim has finished, starts are
    /// accepted again (issue #1612).
    #[cfg(unix)]
    #[tokio::test]
    async fn an_agent_started_in_an_abandoned_worktree_never_has_it_removed_under_it() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("repo-issue-7");
        init_repo_with_worktree(tmp.path(), &repo, &wt);
        let creator = || Creator::issue_dispatch("nightly", 7);
        crate::worktree_owner::write_marker(&wt, "wt", &creator()).unwrap();
        crate::worktree_owner::mark_abandoned_spawn(&wt, &creator()).unwrap();
        let reg = new_worktree_registry();
        let agents = Arc::new(AgentPtyRegistry::new());

        // A start reserved after the first pass and unpublished at the last
        // check: a removal that asked only the published records deleted it.
        let starting = std::sync::Mutex::new(None);
        let outcome = reclaim_abandoned_spawn_paced(
            &wt,
            &repo,
            creator(),
            &reg,
            &agents,
            async {
                *starting.lock().unwrap() = Some(agents.reserve_spawn_in_for_test(&wt).unwrap());
            },
            std::future::ready(()),
        )
        .await;
        assert_eq!(
            outcome,
            Reclaim::NotAbandoned,
            "the starting agent keeps it"
        );
        assert!(wt.exists(), "and nothing was removed under it");
        agents.release_spawn_for_test(&starting.into_inner().unwrap().unwrap());

        // A start that arrives after the last check, before the removal.
        let late_start = std::sync::Mutex::new(None);
        let outcome = reclaim_abandoned_spawn_paced(
            &wt,
            &repo,
            creator(),
            &reg,
            &agents,
            std::future::ready(()),
            async {
                assert!(wt.exists(), "the worktree is still there to start in");
                let wt = wt.to_string_lossy();
                *late_start.lock().unwrap() =
                    Some(agents.spawn_agent(crate::agent_pty::SpawnOptions {
                        command: Some("sleep 30"),
                        cwd: Some(&wt),
                        ..crate::agent_pty::SpawnOptions::default()
                    }));
            },
        )
        .await;
        let late_start = late_start.into_inner().unwrap().unwrap();
        if let Ok(id) = &late_start {
            agents.close_agent(id).unwrap();
        }
        assert!(
            matches!(
                late_start,
                Err(crate::agent_pty::AgentPtyError::DirBeingRemoved(_))
            ),
            "a start in a worktree being removed is refused: {late_start:?}"
        );
        assert_eq!(outcome, Reclaim::Removed);
        assert!(!wt.exists());
        assert!(agents.agent_records().is_empty(), "no agent was started");

        // The removal is over: starts in that path are refused no longer.
        let after = agents
            .reserve_spawn_in_for_test(&wt)
            .expect("the reclaim's hold ends with it");
        agents.release_spawn_for_test(&after);
        agents.shutdown_all();
    }

    /// Scenario: another deck process holds the repository's worktree lock —
    /// the one worktree creation takes — while an abandoned leftover is
    /// reclaimed. The reclaim waits and removes nothing until the lock is
    /// released, then removes the leftover (PRD #1487, the F2 reclaim race).
    #[tokio::test]
    async fn the_abandoned_spawn_retry_removes_only_under_the_repository_worktree_lock() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("repo-issue-7");
        init_repo_with_worktree(tmp.path(), &repo, &wt);
        let creator = Creator::issue_dispatch("nightly", 7);
        crate::worktree_owner::write_marker(&wt, "wt", &creator).unwrap();
        crate::worktree_owner::mark_abandoned_spawn(&wt, &creator).unwrap();

        let lock_path = worktree_lock_path(&repo).await.expect("a lock path");
        let held = crate::platform::lock::acquire_spawn_lock(&lock_path)
            .await
            .expect("hold the repository's worktree lock, as another deck process would");
        let (wt_task, repo_task) = (wt.clone(), repo.clone());
        let reclaiming = tokio::spawn(async move {
            reclaim_abandoned_spawn(
                &wt_task,
                &repo_task,
                creator,
                &new_worktree_registry(),
                &AgentPtyRegistry::new(),
            )
            .await
        });
        // Not a race: while the lock is held the reclaim can never finish.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!reclaiming.is_finished(), "the reclaim waits for the lock");
        assert!(wt.exists(), "and removes nothing while it waits");

        drop(held);
        let outcome = tokio::time::timeout(Duration::from_secs(30), reclaiming)
            .await
            .expect("releasing the lock lets the reclaim proceed")
            .expect("the reclaim task must not panic");
        assert_eq!(outcome, Reclaim::Removed);
        assert!(!wt.exists());
    }

    /// Scenario: an abandoned leftover whose repository's worktree lock cannot
    /// be taken at all (the clone cannot be resolved). The reclaim fails
    /// closed: it reports the issue as failed and leaves the worktree on disk
    /// (PRD #1487, the F2 reclaim race).
    #[tokio::test]
    async fn the_abandoned_spawn_retry_removes_nothing_without_the_lock() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("repo-issue-7");
        init_repo_with_worktree(tmp.path(), &repo, &wt);
        let creator = Creator::issue_dispatch("nightly", 7);
        crate::worktree_owner::write_marker(&wt, "wt", &creator).unwrap();
        crate::worktree_owner::mark_abandoned_spawn(&wt, &creator).unwrap();

        let outcome = reclaim_abandoned_spawn(
            &wt,
            &tmp.path().join("no-such-clone"),
            creator,
            &new_worktree_registry(),
            &AgentPtyRegistry::new(),
        )
        .await;
        assert!(
            matches!(&outcome, Reclaim::Failed(message) if message.contains("lock")),
            "no lock, no removal, and the issue says why: {outcome:?}"
        );
        assert!(wt.exists());
    }

    /// Scenario: an abandoned leftover that was edited between the refused spawn
    /// and the retry — somebody left uncommitted work in it. Upstream removes it
    /// regardless (`RemovalPolicy::Force`); this fork keeps a tree with
    /// uncommitted work (PRD #236, rulings 42/43 of the 2026-11-05 upstream
    /// merge), so the reclaim reports the issue as failed with a message that
    /// says the work was kept and what to do, leaves the file in place, and
    /// never reports the worktree removed. A CLEAN leftover is still removed
    /// (`the_abandoned_spawn_retry_removes_only_under_the_repository_worktree_lock`).
    #[tokio::test]
    async fn a_dirty_abandoned_leftover_is_kept_and_reported_never_discarded() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("repo-issue-7");
        init_repo_with_worktree(tmp.path(), &repo, &wt);
        let creator = Creator::issue_dispatch("nightly", 7);
        crate::worktree_owner::write_marker(&wt, "wt", &creator).unwrap();
        crate::worktree_owner::mark_abandoned_spawn(&wt, &creator).unwrap();
        std::fs::write(wt.join("scratch.txt"), "the user's uncommitted work").unwrap();

        let outcome = reclaim_abandoned_spawn(
            &wt,
            &repo,
            creator,
            &new_worktree_registry(),
            &AgentPtyRegistry::new(),
        )
        .await;
        let Reclaim::Failed(message) = &outcome else {
            panic!("a dirty leftover must be reported, not removed: {outcome:?}");
        };
        assert!(
            message.contains("uncommitted") && message.contains("kept"),
            "the report must say the work was kept: {message}"
        );
        assert_eq!(
            std::fs::read_to_string(wt.join("scratch.txt")).unwrap(),
            "the user's uncommitted work",
            "the uncommitted work is untouched"
        );
    }

    #[tokio::test]
    async fn kept_worktree_preview_reports_a_dirty_keep_if_dirty_tree_with_its_path() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("repo-dispatch-x");
        init_repo_with_worktree(tmp.path(), &repo, &wt);
        std::fs::write(wt.join("scratch.txt"), "the user's uncommitted work").unwrap();

        let reg = new_worktree_registry();
        record_worktree(&reg, &wt, &repo, RemovalPolicy::KeepIfDirty);
        let records = vec![pane_in("pane-1", &wt)];

        let kept = kept_worktree_preview(&records, &reg, &["pane-1".to_string()], PROBE)
            .await
            .expect("a dirty KeepIfDirty tree must be previewed");
        assert_eq!(kept.path, wt.to_string_lossy());
        assert!(
            kept.confirmed_dirty,
            "the probe answered, so the report must say so flatly"
        );
        // The preview must PEEK, never consume: the user has not answered the
        // dialog yet, and a taken entry would strand the directory on Cancel.
        assert!(
            take_worktree(&reg, &wt).is_some(),
            "the preview must leave the registry entry for the close to act on"
        );
    }

    #[tokio::test]
    async fn kept_worktree_preview_is_silent_for_a_clean_tree() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("repo-dispatch-x");
        init_repo_with_worktree(tmp.path(), &repo, &wt);

        let reg = new_worktree_registry();
        record_worktree(&reg, &wt, &repo, RemovalPolicy::KeepIfDirty);
        let records = vec![pane_in("pane-1", &wt)];

        assert_eq!(
            kept_worktree_preview(&records, &reg, &["pane-1".to_string()], PROBE).await,
            None,
            "a clean tree is REMOVED, so warning that it is kept would be false"
        );
    }

    #[tokio::test]
    async fn kept_worktree_preview_is_silent_for_a_force_removed_tree() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("repo-dispatch-x");
        init_repo_with_worktree(tmp.path(), &repo, &wt);
        std::fs::write(wt.join("scratch.txt"), "discarded by design").unwrap();

        let reg = new_worktree_registry();
        // PRD #120 issue-dispatch: dirty or not, this tree goes.
        record_worktree(&reg, &wt, &repo, RemovalPolicy::Force);
        let records = vec![pane_in("pane-1", &wt)];

        assert_eq!(
            kept_worktree_preview(&records, &reg, &["pane-1".to_string()], PROBE).await,
            None,
            "a Force worktree is not kept, so there is nothing to report"
        );
    }

    #[tokio::test]
    async fn kept_worktree_preview_is_silent_for_an_ordinary_pane() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("repo-dispatch-x");
        init_repo_with_worktree(tmp.path(), &repo, &wt);
        std::fs::write(wt.join("scratch.txt"), "dirty, but nobody dispatched here").unwrap();

        // Nothing recorded: this is a pane the user opened themselves, and the
        // deck removes no directory when it closes.
        let reg = new_worktree_registry();
        let records = vec![pane_in("pane-1", &wt)];

        assert_eq!(
            kept_worktree_preview(&records, &reg, &["pane-1".to_string()], PROBE).await,
            None,
            "an unregistered cwd is not a dispatched worktree"
        );
    }

    /// A multi-role orchestration shares ONE worktree across its role panes, so
    /// the preview must resolve it from whichever pane ids it is handed —
    /// including ones it does not recognise, which a tab close mixes in.
    #[tokio::test]
    async fn kept_worktree_preview_resolves_one_tree_from_any_of_a_tabs_panes() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("repo-dispatch-team");
        init_repo_with_worktree(tmp.path(), &repo, &wt);
        std::fs::write(wt.join("scratch.txt"), "work from role 2").unwrap();

        let reg = new_worktree_registry();
        record_worktree(&reg, &wt, &repo, RemovalPolicy::KeepIfDirty);
        let orchestration = |pane_id: &str| {
            let mut r = record(
                Some("/ignored"),
                Some(TabMembership::Orchestration {
                    name: "team".into(),
                    role_index: 0,
                    role_name: "coder".into(),
                    is_start_role: false,
                    orchestration_cwd: Some(wt.to_string_lossy().into_owned()),
                    display_title: None,
                    orchestration_id: None,
                }),
            );
            r.pane_id_env = Some(pane_id.to_string());
            r
        };
        let records = vec![orchestration("role-0"), orchestration("role-1")];

        let panes = vec![
            "not-a-pane-the-daemon-knows".to_string(),
            "role-1".to_string(),
        ];
        let kept = kept_worktree_preview(&records, &reg, &panes, PROBE)
            .await
            .expect("an unknown pane id must not stop the known one from resolving");
        assert_eq!(kept.path, wt.to_string_lossy());
    }

    /// The probe's deadline degrades the WORDING, never the report: the tree is
    /// kept whether or not the status walk finished, and the path is the half
    /// the user actually needs.
    ///
    /// Fork issue #811: the probe here is a stand-in that NEVER answers, so the
    /// deadline is the only way the wait can end. The earlier form raced a real
    /// `git status` against a 1 ns deadline, and `tokio::time::timeout` polls
    /// the inner future before the timer: whenever the runtime was descheduled
    /// long enough for `git` to exit, the probe won and reported the tree dirty.
    ///
    /// Nothing here touches the disk or runs `git`: with the probe stood in,
    /// the preview only matches a pane's cwd against the registry, and both are
    /// plain path values. The paths need not exist.
    #[tokio::test]
    async fn kept_worktree_preview_still_reports_the_path_when_the_probe_times_out() {
        let repo = Path::new("/never-on-disk/repo");
        let wt = Path::new("/never-on-disk/repo-dispatch-x");

        let reg = new_worktree_registry();
        record_worktree(&reg, wt, repo, RemovalPolicy::KeepIfDirty);
        let records = vec![pane_in("pane-1", wt)];

        let probed = std::cell::Cell::new(0usize);
        let never_answers = async |_: &Path| {
            probed.set(probed.get() + 1);
            std::future::pending::<Result<bool, String>>().await
        };

        // The outer guard only matters if the preview ever stops bounding its
        // probe: that regression would otherwise sit on `pending()` until the
        // test runner killed it, instead of failing here with a reason.
        let kept = tokio::time::timeout(
            Duration::from_secs(10),
            kept_worktree_preview_with(
                &records,
                &reg,
                &["pane-1".to_string()],
                Duration::from_millis(1),
                never_answers,
            ),
        )
        .await
        .expect("the preview did not honour its own probe deadline")
        .expect("an unanswered probe must still report the path");
        assert_eq!(
            probed.get(),
            1,
            "the deadline must have been reached by waiting on the probe"
        );
        assert_eq!(kept.path, wt.to_string_lossy());
        assert!(
            !kept.confirmed_dirty,
            "nothing was measured, so nothing may be claimed"
        );
    }

    /// Issue #717 (Greptile P1): the AUTHORITATIVE half of the report. The
    /// close dialog's preview is measured while the agent is still alive, so it
    /// can be overtaken — an agent that commits between the dialog and the close
    /// would make a reused "kept at <path>" claim false about a directory that
    /// was in fact removed. `remove_worktree` returns what it actually did,
    /// measured after the agent is reaped, and that is what reaches the user.
    #[tokio::test]
    async fn remove_worktree_reports_whether_it_kept_the_tree() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");

        // Dirty + KeepIfDirty: kept, and it says so with the path.
        let dirty = tmp.path().join("repo-dispatch-dirty");
        init_repo_with_worktree(tmp.path(), &repo, &dirty);
        std::fs::write(dirty.join("scratch.txt"), "work").unwrap();
        let outcome =
            remove_worktree(&dirty, &repo, RemovalPolicy::KeepIfDirty, "test:remover").await;
        assert_eq!(
            outcome,
            RemoveOutcome::Kept(crate::event::KeptReason::Dirty)
        );
        assert!(dirty.is_dir(), "the tree must actually still be there");

        // Clean + KeepIfDirty: removed, and nothing is reported.
        let clean = tmp.path().join("repo-dispatch-clean");
        let run = |args: &[&str]| {
            let out = crate::git_env::fixture_git(&repo, tmp.path())
                .args(args)
                .output()
                .expect("git available");
            assert!(out.status.success(), "git {args:?} failed: {out:?}");
        };
        run(&[
            "worktree",
            "add",
            "-q",
            "-b",
            "clean",
            &clean.to_string_lossy(),
        ]);
        assert!(
            matches!(
                remove_worktree(&clean, &repo, RemovalPolicy::KeepIfDirty, "test:remover").await,
                RemoveOutcome::Removed(_)
            ),
            "a removed tree must report Removed"
        );
        assert!(!clean.exists());

        // Dirty + Force: removed regardless, and still nothing reported. The
        // report is about what was KEPT, and PRD #120's slot-reclaim model
        // depends on this one going away.
        let forced = tmp.path().join("repo-dispatch-forced");
        run(&[
            "worktree",
            "add",
            "-q",
            "-b",
            "forced",
            &forced.to_string_lossy(),
        ]);
        std::fs::write(forced.join("scratch.txt"), "discarded by design").unwrap();
        assert!(matches!(
            remove_worktree(&forced, &repo, RemovalPolicy::Force, "test:remover").await,
            RemoveOutcome::Removed(_)
        ));
        assert!(!forced.exists());
    }

    /// A removal that FAILS also leaves the directory on disk, so the user is
    /// told for the same reason a deliberate keep is — under the conditional
    /// wording, because nothing measured their edits.
    #[tokio::test]
    async fn remove_worktree_reports_a_tree_left_behind_by_a_failed_removal() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("repo-dispatch-x");
        init_repo_with_worktree(tmp.path(), &repo, &wt);

        // `git -C <clone> worktree remove` against a clone that is not a repo
        // fails, which is the shape of every real failure here (locked, busy,
        // already gone).
        let not_a_repo = tmp.path().join("not-a-repo");
        std::fs::create_dir_all(&not_a_repo).unwrap();
        let outcome = remove_worktree(&wt, &not_a_repo, RemovalPolicy::Force, "test:remover").await;
        assert!(
            matches!(outcome, RemoveOutcome::RemoveFailed(_)),
            "a failed removal must be reported as RemoveFailed, got {outcome:?}"
        );
        assert!(wt.is_dir());
    }

    #[tokio::test]
    async fn worktree_is_dirty_matches_git_status_and_errs_when_it_cannot_ask() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("repo-dispatch-x");
        init_repo_with_worktree(tmp.path(), &repo, &wt);

        assert_eq!(worktree_is_dirty(&wt).await, Ok(false));
        std::fs::write(wt.join("scratch.txt"), "work").unwrap();
        assert_eq!(worktree_is_dirty(&wt).await, Ok(true));

        // Unanswerable is an Err, never a `false` — both callers must be able to
        // tell "clean" from "could not ask", because only one of them is safe
        // to delete on.
        assert!(worktree_is_dirty(&tmp.path().join("gone")).await.is_err());
    }

    /// Issue #1888: a repository whose shared `.git` carries a `post-checkout`
    /// hook and a `core.fsmonitor` command, as a dispatched agent can plant
    /// them. Plain git runs both (the control proves the traps are armed); the
    /// deck's own `worktree add` and `status --porcelain` run neither, and
    /// still answer.
    #[cfg(unix)]
    #[tokio::test]
    async fn planted_hooks_and_fsmonitor_never_run_from_the_decks_git() {
        let tmp = crate::test_temp::tempdir().unwrap();
        let sandbox = tmp.path().to_path_buf();
        let repo = sandbox.join("repo");
        let marker = sandbox.join("MARKER");
        // The fixture is synchronous git and file work, so it runs off the
        // runtime's thread.
        tokio::task::spawn_blocking({
            let (repo, marker) = (repo.clone(), marker.clone());
            move || plant_hook_and_fsmonitor_traps(&sandbox, &repo, &marker)
        })
        .await
        .unwrap();

        let created = tmp.path().join("repo-dispatch-y");
        let outcome = create_worktree_from(
            &repo,
            &created,
            "agent/y",
            false,
            Creator::dispatch("unit"),
            None,
        )
        .await;
        assert!(
            matches!(outcome, Ok(WorktreeCreation::Created { .. })),
            "the setup creation must succeed, got {outcome:?}"
        );
        assert_eq!(worktree_is_dirty(&created).await, Ok(false));
        tokio::fs::write(created.join("scratch.txt"), "work")
            .await
            .unwrap();
        assert_eq!(worktree_is_dirty(&created).await, Ok(true));
        assert!(
            !tokio::fs::try_exists(&marker).await.unwrap(),
            "the deck's git ran a planted program: {}",
            tokio::fs::read_to_string(&marker).await.unwrap_or_default()
        );
    }

    /// The fixture for [`planted_hooks_and_fsmonitor_never_run_from_the_decks_git`]:
    /// a repository with a linked worktree, a `post-checkout` hook and a
    /// `core.fsmonitor` command that each append their name to `marker`, and a
    /// control showing that plain git runs both. Leaves `marker` absent.
    #[cfg(unix)]
    fn plant_hook_and_fsmonitor_traps(sandbox: &Path, repo: &Path, marker: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let wt = sandbox.join("repo-dispatch-x");
        init_repo_with_worktree(sandbox, repo, &wt);
        let trap = |path: &Path| {
            crate::test_isolation::write_script(
                path,
                format!("#!/bin/sh\necho \"$0\" >> '{}'\nexit 1\n", marker.display()),
            )
            .unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        let hooks = repo.join(".git").join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        trap(&hooks.join("post-checkout"));
        let fsmonitor = sandbox.join("fsmonitor.sh");
        trap(&fsmonitor);
        let git = |dir: &Path, args: &[&str]| {
            crate::git_env::fixture_git_with_hooks(dir, sandbox)
                .args(args)
                .output()
                .expect("git available")
        };
        let config = git(
            repo,
            &["config", "core.fsmonitor", &fsmonitor.to_string_lossy()],
        );
        assert!(config.status.success(), "{config:?}");

        // The control: plain git with this repository runs both traps. The
        // add's own status is the hook's, which fails on purpose.
        let control = sandbox.join("repo-control");
        git(
            repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "control",
                &control.to_string_lossy(),
            ],
        );
        let fired = std::fs::read_to_string(marker).unwrap_or_default();
        assert!(
            fired.contains("post-checkout"),
            "hook trap armed: {fired:?}"
        );
        std::fs::remove_file(marker).unwrap();
        git(&wt, &["status", "--porcelain"]);
        let fired = std::fs::read_to_string(marker).unwrap_or_default();
        assert!(
            fired.contains("fsmonitor.sh"),
            "fsmonitor trap armed: {fired:?}"
        );
        std::fs::remove_file(marker).unwrap();
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
            // PRD #745 M11: likewise no spawn instant — this fixture builds a
            // record by hand rather than by spawning anything.
            spawned_at_ms: None,
            cli_name: None,
            prompt_keys: None,
            crashed: None,
            outstanding_delegation: None,
            delegation_commission: None,
            orchestrator_context_path: None,
            authoring_kind: None,
            inbox: None,
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
            let out = crate::git_env::fixture_git(dir, dir)
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
            let out = crate::git_env::fixture_git(dir, dir)
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

        let git_dir_out = crate::git_env::fixture_git(&worktree_dir, &worktree_dir)
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
    ///
    /// Through [`crate::git_env::fixture_git`], for exactly the reasons
    /// [`init_repo_with_worktree`] spells out — this fixture was the other half
    /// of the same module and was missed when that one was isolated (issue
    /// #1121 round three). The three `git config` writes it used to make are
    /// gone with it: the identity now arrives as `GIT_AUTHOR_*`/`GIT_COMMITTER_*`
    /// in the environment, and `commit.gpgsign` no longer needs neutralizing
    /// per-repo because no global or system config reaches the fixture at all.
    fn init_repo_with_commit(sandbox_root: &Path, repo: &Path) {
        std::fs::create_dir_all(repo).expect("create repo dir");
        let git = |args: &[&str]| {
            let out = crate::git_env::fixture_git(repo, sandbox_root)
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
        // The one config a fixture repository must carry itself, because
        // production `create_worktree` checks this repository out with the
        // ambient config while the assertions read it with the neutralized one
        // — see `pin_fixture_eol`, and issue #1121's `build-windows` red.
        crate::worktree_owner::pin_fixture_eol(repo, sandbox_root);
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
        init_repo_with_commit(scratch.path(), &repo);
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
            let head = run_git_capture(&[
                "-C",
                &worktree_dir.to_string_lossy(),
                "branch",
                "--show-current",
            ])
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
        init_repo_with_commit(scratch.path(), &repo);
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

    /// Issue #541, the other half of the fix: worktree creation is serialized
    /// PER REPOSITORY and, because the lock is an `flock(2)` on a file (a named
    /// mutex on Windows), between separate deck processes rather than merely
    /// between tasks in one — concurrent dispatches can come from different
    /// decks, and an in-process mutex would silently not cover them.
    ///
    /// Written the way the platform lock's own contract test is: a held lock
    /// makes the creation block, and releasing it lets the creation complete.
    /// The lock is taken through the SAME derivation production uses, so a
    /// change that moved worktree creation off this key would fail here rather
    /// than quietly stop serializing.
    #[tokio::test]
    async fn create_worktree_serializes_per_repository_across_processes() {
        let scratch = crate::test_temp::tempdir().expect("scratch tempdir");
        let repo = scratch.path().join("repo");
        init_repo_with_commit(scratch.path(), &repo);

        let lock_path = worktree_lock_path(&repo)
            .await
            .expect("a git repo must resolve a worktree lock path");
        assert!(
            lock_path.starts_with(repo.join(".git")),
            "the lock must live in the repository's git common dir (the contended \
             directory), got {}",
            lock_path.display()
        );
        let held = crate::platform::lock::acquire_spawn_lock(&lock_path)
            .await
            .expect("hold the repository's worktree lock, as another deck process would");

        let clone_dir = repo.clone();
        let worktree_dir = scratch.path().join("repo-dispatch-serialized");
        let creating = tokio::spawn(async move {
            create_worktree(
                &clone_dir,
                &worktree_dir,
                "agent/dispatch-serialized",
                false,
                Creator::dispatch("serialized"),
            )
            .await
        });
        // Long enough to reach the blocking wait. Not a race: while the lock is
        // held the creation can NEVER finish, so a slow machine also passes.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            !creating.is_finished(),
            "worktree creation must wait while another process holds this \
             repository's worktree lock; not waiting is how two `git worktree add`s \
             end up reading each other's half-created entries (#541)"
        );

        drop(held);

        let outcome = tokio::time::timeout(Duration::from_secs(30), creating)
            .await
            .expect("releasing the lock must let the creation proceed")
            .expect("the creating task must not panic");
        assert!(
            matches!(outcome, Ok(WorktreeCreation::Created { .. })),
            "once the lock is released the worktree must be created normally, got {outcome:?}"
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
        init_repo_with_commit(scratch.path(), &repo);
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
        init_repo_with_commit(scratch.path(), &repo);
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

        let status = crate::git_env::fixture_git(&worktree_dir, scratch.path())
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
        init_repo_with_commit(scratch.path(), &repo);
        let worktree_dir = scratch.path().join("repo-dispatch-foreign");

        // Somebody else's worktree, on this same repo, at the path our dispatch
        // is about to want: a real linked worktree, so it HAS a git metadata
        // dir a marker could be written into.
        let add = crate::git_env::fixture_git(&repo, scratch.path())
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
            let out = crate::git_env::fixture_git(dir, dir)
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

        let git_dir_out = crate::git_env::fixture_git(&worktree_dir, &worktree_dir)
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

    /// Scenario: `attempt_worktree_cleanup` is the best-effort removal
    /// `create_worktree_sync` runs against a directory IT half-created, after
    /// `git worktree add` timed out (fork #122/#123). Fed a real,
    /// already-registered worktree standing in for that half-created state
    /// and the identity of the creator attempting the cleanup, a confirmed
    /// removal must attribute that identity (issue #325's attribution gap —
    /// today this call site drops it on the floor). A second call against
    /// the now-absent directory removes nothing, and attribution must stay
    /// absent rather than naming an identity for a removal that never
    /// happened.
    #[test]
    fn attempt_worktree_cleanup_records_remover_identity() {
        fn git(dir: &Path, args: &[&str]) {
            let out = crate::git_env::fixture_git(dir, dir)
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

        let worktree_dir = ws.path().join("wt-half-created");
        git(
            &clone_dir,
            &[
                "worktree",
                "add",
                "-b",
                "feat/half-created",
                worktree_dir.to_str().unwrap(),
            ],
        );
        assert!(worktree_dir.exists());

        let remover = "test-creator";
        let removed_by = attempt_worktree_cleanup(&clone_dir, &worktree_dir, remover);
        assert!(
            !worktree_dir.exists(),
            "attempt_worktree_cleanup must actually remove the directory on a confirmed cleanup"
        );
        assert_eq!(
            removed_by.as_deref(),
            Some(remover),
            "a confirmed cleanup removal must attribute the identity attempting it (issue #325) \
             -- got {removed_by:?}"
        );

        let second = attempt_worktree_cleanup(&clone_dir, &worktree_dir, remover);
        assert_eq!(
            second, None,
            "nothing was removed on this call (the directory is already gone) -- attribution \
             must be None, not fabricated, got {second:?}"
        );
    }

    // --- issue #469: `remove_worktree` attribution + the `--` argv gap ---

    /// Scenario: issue #469. Unlike its siblings `attempt_worktree_cleanup(_async)`
    /// and `worktree_reclaim::remove_worktree_dir` (all from PR #458),
    /// `remove_worktree`'s success log carries no identity of the caller.
    /// Mirrors `attempt_worktree_cleanup_records_remover_identity`'s shape: a
    /// real worktree is force-removed, and the returned outcome must carry
    /// the identity that removed it, not just log it.
    #[tokio::test]
    async fn remove_worktree_records_remover_identity() {
        fn git(dir: &Path, args: &[&str]) {
            let out = crate::git_env::fixture_git(dir, dir)
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

        let worktree_dir = ws.path().join("wt-remove-attrib");
        git(
            &clone_dir,
            &[
                "worktree",
                "add",
                "-b",
                "feat/remove-attrib",
                worktree_dir.to_str().unwrap(),
            ],
        );
        assert!(worktree_dir.exists());

        let remover = "test-remover";
        let outcome =
            remove_worktree(&worktree_dir, &clone_dir, RemovalPolicy::Force, remover).await;
        assert!(
            !worktree_dir.exists(),
            "remove_worktree must actually remove the directory"
        );
        match outcome {
            RemoveOutcome::Removed(ref actual) => assert_eq!(
                actual, remover,
                "a removed worktree must attribute the identity that removed it (issue #469)"
            ),
            other => panic!(
                "expected RemoveOutcome::Removed to carry the remover identity, got {other:?}"
            ),
        }
    }

    /// Scenario: issue #469 / #144 finding 4, reviewer F5b. Pure data
    /// assertion against [`remove_worktree_argv`] — no process spawn, no
    /// PATH shadow, no `unsafe`, no platform gate — that the `--`
    /// end-of-options separator sits immediately before the worktree path,
    /// same shape as `issue_dispatch.rs`'s
    /// `worktree_remove_argv_carries_end_of_options_separator_before_path`
    /// (PR #458), which this test is named distinctly from (reviewer F5b:
    /// the previous shell-stub test in this module shared that exact name
    /// across two modules, so a filtered `cargo test` matched both).
    #[test]
    fn remove_worktree_argv_puts_end_of_options_separator_before_path() {
        let argv = remove_worktree_argv("/repo/clone", "/repo/worktrees/agent-issue-7", true);
        let dash_dash = argv
            .iter()
            .position(|a| a == "--")
            .expect("remove_worktree_argv must contain a `--` end-of-options separator");
        assert_eq!(
            argv.get(dash_dash + 1).map(String::as_str),
            Some("/repo/worktrees/agent-issue-7"),
            "the `--` separator must sit IMMEDIATELY before the path argument, got {argv:?}"
        );
    }

    /// Scenario: `remove_worktree_argv`'s `force` parameter is what makes it
    /// diverge from `issue_dispatch.rs`'s always-`--force` sibling — assert
    /// `--force` is present when requested and absent otherwise.
    #[test]
    fn remove_worktree_argv_pushes_force_flag_only_when_requested() {
        let forced = remove_worktree_argv("/repo/clone", "/repo/worktrees/agent-issue-7", true);
        assert!(forced.iter().any(|a| a == "--force"));

        let unforced = remove_worktree_argv("/repo/clone", "/repo/worktrees/agent-issue-7", false);
        assert!(!unforced.iter().any(|a| a == "--force"));
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
            let out = crate::git_env::fixture_git(dir, dir)
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
            let out = crate::git_env::fixture_git(clone_dir, clone_dir)
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
            let out = crate::git_env::fixture_git(dir, dir)
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

    /// Scenario: PRD fork#325 fix round (reviewer P1-1, P1-2). Calls
    /// `provision_isolated_clone_sync` directly against a real source repo
    /// that HAS an `origin` remote configured, with a `branch` that does not
    /// yet exist anywhere. Asserts the resulting clone is checked out on
    /// `branch` (not the source's HEAD branch) and that its `origin` is the
    /// SOURCE's own origin URL — never the local filesystem path a plain
    /// `git clone` defaults `origin` to, which the reviewer reproduced makes
    /// `git push origin` land silently in the source checkout instead of
    /// reaching GitHub.
    #[test]
    fn provision_isolated_clone_sync_sets_origin_and_branch_when_source_has_origin() {
        fn git(dir: &Path, args: &[&str]) {
            let out = crate::git_env::fixture_git(dir, dir)
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
        let source = ws.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        git(&source, &["init", "--initial-branch=main", "--quiet"]);
        git(&source, &["config", "user.email", "test@example.com"]);
        git(&source, &["config", "user.name", "Test"]);
        git(&source, &["config", "commit.gpgsign", "false"]);
        std::fs::write(source.join("README.md"), "seed\n").unwrap();
        git(&source, &["add", "README.md"]);
        git(&source, &["commit", "--quiet", "-m", "seed"]);
        git(
            &source,
            &[
                "remote",
                "add",
                "origin",
                "https://example.invalid/source-repo.git",
            ],
        );

        let clone_dir = ws.path().join("source-my-feature");
        let result = provision_isolated_clone_sync(&source, &clone_dir, "my-feature", "tester");
        assert!(
            matches!(result, Ok(IsolatedCloneOutcome::Created { .. })),
            "isolated clone must succeed, got {result:?}"
        );

        let branch_out = crate::git_env::fixture_git(&clone_dir, &clone_dir)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .expect("git rev-parse must spawn");
        assert_eq!(
            String::from_utf8_lossy(&branch_out.stdout).trim(),
            "my-feature",
            "the isolated clone must be checked out on the typed branch, not the source's HEAD"
        );

        let origin_out = crate::git_env::fixture_git(&clone_dir, &clone_dir)
            .args(["remote", "get-url", "origin"])
            .output()
            .expect("git remote get-url must spawn");
        assert!(
            origin_out.status.success(),
            "clone must have an origin remote configured"
        );
        assert_eq!(
            String::from_utf8_lossy(&origin_out.stdout).trim(),
            "https://example.invalid/source-repo.git",
            "the clone's origin must be the SOURCE's own origin URL, not a local path"
        );
    }

    /// Same fix round, the OTHER half of reviewer P1-1: when the source has
    /// NO `origin` configured at all (the `orch-clone-gate` e2e fixture's own
    /// shape — an ordinary local project with no remote added), there is
    /// nothing better to point the clone's `origin` at. Asserts the clone's
    /// default local-path `origin` (what a plain `git clone` sets up) is
    /// REMOVED rather than left in place — so a later `git push origin`
    /// fails loudly instead of silently landing back in the source checkout.
    #[test]
    fn provision_isolated_clone_sync_removes_origin_when_source_has_none() {
        fn git(dir: &Path, args: &[&str]) {
            let out = crate::git_env::fixture_git(dir, dir)
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
        let source = ws.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        git(&source, &["init", "--initial-branch=main", "--quiet"]);
        git(&source, &["config", "user.email", "test@example.com"]);
        git(&source, &["config", "user.name", "Test"]);
        git(&source, &["config", "commit.gpgsign", "false"]);
        std::fs::write(source.join("README.md"), "seed\n").unwrap();
        git(&source, &["add", "README.md"]);
        git(&source, &["commit", "--quiet", "-m", "seed"]);
        // Deliberately no `git remote add origin` — mirrors the
        // `orch-clone-gate` e2e fixture's shape.

        let clone_dir = ws.path().join("source-my-feature");
        let result = provision_isolated_clone_sync(&source, &clone_dir, "my-feature", "tester");
        assert!(
            matches!(result, Ok(IsolatedCloneOutcome::Created { .. })),
            "isolated clone must succeed, got {result:?}"
        );

        let origin_out = crate::git_env::fixture_git(&clone_dir, &clone_dir)
            .args(["remote", "get-url", "origin"])
            .output()
            .expect("git remote get-url must spawn");
        assert!(
            !origin_out.status.success(),
            "the clone must have NO origin remote when the source had none — a plain \
             `git clone`'s default local-path origin must be removed, not left pointing \
             back at {}",
            source.display()
        );
    }

    /// Scenario: fork issue #595's underlying primitive. Directly exercises
    /// `resolve_git_toplevel` against both shapes it must distinguish: a
    /// directory that already IS the repo root (regression guard — must
    /// report an EMPTY relative prefix, so a caller appends nothing) and a
    /// nested subdirectory (must report the real toplevel, not the
    /// subdirectory, plus the exact relative path back down to it).
    #[test]
    fn resolve_git_toplevel_distinguishes_root_from_nested_subdirectory() {
        fn git(dir: &Path, args: &[&str]) {
            let out = crate::git_env::fixture_git(dir, dir)
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
        let repo_root = ws.path().join("repo");
        std::fs::create_dir_all(&repo_root).unwrap();
        git(&repo_root, &["init", "--initial-branch=main", "--quiet"]);
        git(&repo_root, &["config", "user.email", "test@example.com"]);
        git(&repo_root, &["config", "user.name", "Test"]);
        git(&repo_root, &["config", "commit.gpgsign", "false"]);
        let nested = repo_root.join("baseline").join("intent");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("marker.txt"), "hi\n").unwrap();
        git(&repo_root, &["add", "-A"]);
        git(&repo_root, &["commit", "--quiet", "-m", "seed"]);

        let (root_toplevel, root_prefix) =
            resolve_git_toplevel(&repo_root).expect("repo_root is a real git repo");
        assert_eq!(
            canonicalize_best_effort(&root_toplevel),
            canonicalize_best_effort(&repo_root),
            "toplevel of the root itself must be the root"
        );
        assert!(
            root_prefix.as_os_str().is_empty(),
            "no subpath must be reported when the picked directory already IS the toplevel, \
             got {root_prefix:?}"
        );

        let (nested_toplevel, nested_prefix) =
            resolve_git_toplevel(&nested).expect("nested is inside the same real git repo");
        assert_eq!(
            canonicalize_best_effort(&nested_toplevel),
            canonicalize_best_effort(&repo_root),
            "toplevel of a nested subdirectory must be the repo root, not the subdirectory \
             itself"
        );
        assert_eq!(
            nested_prefix,
            PathBuf::from("baseline/intent"),
            "the relative prefix must be the exact path from the toplevel down to the picked \
             directory, got {nested_prefix:?}"
        );
        // Fork issue #595 fix round 2 (reviewer F5): `PathBuf`'s `PartialEq`
        // compares `components()`, not raw text, so the assertion above
        // would pass just as well with a stray trailing separator
        // (`"baseline/intent/"`) present — it reads as pinning
        // normalization while pinning nothing of the kind. Assert on the
        // raw OS string too, so a regression back to string-based
        // `--show-prefix` parsing (which does carry a trailing `/`) is
        // actually caught here.
        assert_eq!(
            nested_prefix.as_os_str(),
            Path::new("baseline").join("intent").as_os_str(),
            "the relative prefix must carry no trailing separator, got {nested_prefix:?}"
        );
    }

    /// Scenario: fork issue #595 fix round 2 (auditor F1, demonstrated).
    /// `git rev-parse` does not escape its output, and a newline is a legal
    /// path-component byte — so a picked directory whose ANCESTOR name
    /// contains a literal `\n` used to desynchronize the old
    /// `--show-toplevel`/`--show-prefix` two-line parse: the toplevel read
    /// short, pointing at a decoy directory, while the "prefix" absorbed
    /// the real path's remainder (which could come out absolute, letting
    /// `PathBuf::join` discard the clone base entirely). This test builds
    /// exactly that on-disk shape and asserts `resolve_git_toplevel` still
    /// reports the REAL toplevel and a clean relative prefix — proving the
    /// single-value `--show-toplevel` plus Rust-side `strip_prefix`
    /// rewrite has no second line left to desynchronize against. Skipped
    /// on Windows, where `\n` is not a legal path byte at all.
    #[test]
    #[cfg(unix)]
    fn resolve_git_toplevel_survives_a_newline_in_an_ancestor_directory_name() {
        fn git(dir: &Path, args: &[&str]) {
            let out = crate::git_env::fixture_git(dir, dir)
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
        // The parent directory's own name embeds a literal newline byte —
        // exactly the auditor's reproduction shape (`we\nird/repo/sub/dir`).
        let weird_parent = ws.path().join("we\nird");
        let repo_root = weird_parent.join("repo");
        let nested = repo_root.join("sub").join("dir");
        std::fs::create_dir_all(&nested).unwrap();
        git(&repo_root, &["init", "--initial-branch=main", "--quiet"]);
        git(&repo_root, &["config", "user.email", "test@example.com"]);
        git(&repo_root, &["config", "user.name", "Test"]);
        git(&repo_root, &["config", "commit.gpgsign", "false"]);
        std::fs::write(nested.join("marker.txt"), "hi\n").unwrap();
        git(&repo_root, &["add", "-A"]);
        git(&repo_root, &["commit", "--quiet", "-m", "seed"]);

        let (toplevel, prefix) =
            resolve_git_toplevel(&nested).expect("nested is inside a real git repo");
        assert_eq!(
            canonicalize_best_effort(&toplevel),
            canonicalize_best_effort(&repo_root),
            "the toplevel must be the REAL repo root, embedded newline and all — not a decoy \
             directory the old two-line parse would have truncated onto, got {toplevel:?}"
        );
        assert_eq!(
            prefix.as_os_str(),
            std::ffi::OsStr::new("sub/dir"),
            "the relative prefix must be the clean, relative remainder — never absolute (which \
             would let a caller's `PathBuf::join` discard the clone base entirely), got \
             {prefix:?}"
        );
    }

    /// Scenario: fork issue #595. The project directory
    /// `provision_isolated_clone_sync` is asked to isolate is a SUBDIRECTORY
    /// of its git repository (`.dot-agent-deck.toml` living at
    /// `<repo>/baseline/intent`, mirroring the issue's own reproduction),
    /// not the repository's own root. Before the fix, the initial `git
    /// clone -- <source_dir> <clone_dir>` failed instantly with "does not
    /// exist" — `git clone`, unlike ordinary git subcommands, does not walk
    /// up parent directories to find `.git`, so `provision_isolated_clone_sync`
    /// itself returned a top-level `Err`. Asserts provisioning now succeeds
    /// (`Ok(Created { .. })`) and that the resulting clone genuinely
    /// contains the nested subdirectory with the marker file the source
    /// repo committed there — proving the clone is real and correctly
    /// positioned, not just that a path string was computed.
    #[test]
    fn provision_isolated_clone_sync_succeeds_when_source_dir_is_a_repo_subdirectory() {
        fn git(dir: &Path, args: &[&str]) {
            let out = crate::git_env::fixture_git(dir, dir)
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
        let repo_root = ws.path().join("repo");
        std::fs::create_dir_all(&repo_root).unwrap();
        git(&repo_root, &["init", "--initial-branch=main", "--quiet"]);
        git(&repo_root, &["config", "user.email", "test@example.com"]);
        git(&repo_root, &["config", "user.name", "Test"]);
        git(&repo_root, &["config", "commit.gpgsign", "false"]);

        // The nested project directory `.dot-agent-deck.toml` would live in
        // — issue #595's own reproduction shape.
        let nested = repo_root.join("baseline").join("intent");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("marker.txt"), "issue-595-marker\n").unwrap();
        git(&repo_root, &["add", "-A"]);
        git(&repo_root, &["commit", "--quiet", "-m", "seed"]);

        let clone_dir = ws.path().join("repo-my-feature");
        let result = provision_isolated_clone_sync(&nested, &clone_dir, "my-feature", "tester");
        assert!(
            matches!(result, Ok(IsolatedCloneOutcome::Created { .. })),
            "isolated clone must succeed when the picked directory is a repo subdirectory, \
             got {result:?}"
        );

        let marker_in_clone = clone_dir.join("baseline").join("intent").join("marker.txt");
        assert!(
            marker_in_clone.is_file(),
            "the clone must contain the nested subdirectory's own committed content at {}",
            marker_in_clone.display()
        );
        // Normalizes `\r\n` -> `\n` before comparing: on Windows, `git
        // clone`'s destination checkout applies the RUNNER's own (not the
        // source repo's) `core.autocrlf`, which defaults to true and
        // rewrites text files on checkout — a platform difference this
        // assertion has no business caring about, since it exists to prove
        // the clone is real and correctly positioned, not to pin line-ending
        // translation.
        assert_eq!(
            std::fs::read_to_string(&marker_in_clone)
                .unwrap()
                .replace("\r\n", "\n"),
            "issue-595-marker\n",
            "the marker file's content must survive the clone unchanged"
        );
    }

    /// Scenario: fork issue #595, resume path. Same nested-subdirectory
    /// shape as the create-path test above, but the isolated clone already
    /// exists on disk (a re-opened orchestration workspace) —
    /// `resume_existing_isolated_clone`'s ancestry probe has its OWN
    /// path-based `git fetch` fallback (run only when the clone's object
    /// database is missing the source's HEAD commit), sharing the exact
    /// same "does not walk up to find .git" defect the create path has.
    /// Provisions once to create the clone, advances the SOURCE repo with a
    /// second commit the clone's object database does not yet have (forcing
    /// the fallback-fetch branch of the ancestry probe), then provisions
    /// again from the same nested subdirectory and asserts the second call
    /// resumes successfully rather than being refused as unverifiable.
    #[test]
    fn provision_isolated_clone_sync_resumes_when_source_dir_is_a_repo_subdirectory() {
        fn git(dir: &Path, args: &[&str]) {
            let out = crate::git_env::fixture_git(dir, dir)
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
        let state_dir = ws.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let _state_guard =
            ScopedEnvVar::set("DOT_AGENT_DECK_STATE_DIR", state_dir.to_str().unwrap());

        let repo_root = ws.path().join("repo");
        std::fs::create_dir_all(&repo_root).unwrap();
        git(&repo_root, &["init", "--initial-branch=main", "--quiet"]);
        git(&repo_root, &["config", "user.email", "test@example.com"]);
        git(&repo_root, &["config", "user.name", "Test"]);
        git(&repo_root, &["config", "commit.gpgsign", "false"]);

        let nested = repo_root.join("baseline").join("intent");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("marker.txt"), "issue-595-marker\n").unwrap();
        git(&repo_root, &["add", "-A"]);
        git(&repo_root, &["commit", "--quiet", "-m", "seed"]);

        let clone_dir = ws.path().join("repo-my-feature");
        let created = provision_isolated_clone_sync(&nested, &clone_dir, "my-feature", "tester");
        assert!(
            matches!(created, Ok(IsolatedCloneOutcome::Created { .. })),
            "initial isolated clone must succeed, got {created:?}"
        );
        release_resumed_isolated_clone_registration(&clone_dir);

        // Advance the source past what the clone's object database has, so
        // the ancestry probe's fallback fetch (the second path-based git
        // invocation this fix must cover) actually runs.
        std::fs::write(nested.join("marker.txt"), "issue-595-marker-v2\n").unwrap();
        git(&repo_root, &["add", "-A"]);
        git(&repo_root, &["commit", "--quiet", "-m", "advance"]);

        let result = provision_isolated_clone_sync(&nested, &clone_dir, "my-feature", "tester");
        assert!(
            matches!(result, Ok(IsolatedCloneOutcome::Resumed { .. })),
            "resuming from the same repo subdirectory must succeed, got {result:?}"
        );
    }

    /// Scenario: PRD fork#325 fix round 2 (reviewer P2-A), reproducing the
    /// reviewer's own empirical transcript. The source repo has TWO
    /// branches — `main` (checked out) and `clonegate1`, carrying a commit
    /// `main` does not have. `provision_isolated_clone_sync` is called
    /// typing `clonegate1` as the branch. Before this fix, `clonegate1`
    /// arrives in the fresh clone ONLY as `refs/remotes/origin/clonegate1`
    /// (never `refs/heads/clonegate1`), so the old `refs/heads/`-only probe
    /// answered ABSENT and `git checkout -b clonegate1` silently created a
    /// NEW branch at the clone's HEAD (`main`'s tip), discarding
    /// `clonegate1`'s real commit. Asserts the clone attaches to the REAL
    /// branch instead: HEAD is `clonegate1`'s own commit (not `main`'s),
    /// and the file that commit added is present.
    #[test]
    fn provision_isolated_clone_sync_attaches_branch_that_exists_only_as_remote_tracking_ref() {
        fn git(dir: &Path, args: &[&str]) {
            let out = crate::git_env::fixture_git(dir, dir)
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
        let source = ws.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        git(&source, &["init", "--initial-branch=main", "--quiet"]);
        git(&source, &["config", "user.email", "test@example.com"]);
        git(&source, &["config", "user.name", "Test"]);
        git(&source, &["config", "commit.gpgsign", "false"]);
        std::fs::write(source.join("README.md"), "seed\n").unwrap();
        git(&source, &["add", "README.md"]);
        git(&source, &["commit", "--quiet", "-m", "seed"]);

        // A second branch, checked out only long enough to add a commit
        // `main` never gets, then switched back to `main` — so the CLONE
        // (which follows `git clone`'s own default of checking out the
        // source's current HEAD, `main`) never has a local `clonegate1`
        // head of its own to inherit; it only ever sees it via `origin`.
        git(&source, &["checkout", "-b", "clonegate1"]);
        std::fs::write(source.join("WORK.md"), "real work\n").unwrap();
        git(&source, &["add", "WORK.md"]);
        git(&source, &["commit", "--quiet", "-m", "real work"]);
        let clonegate1_sha = crate::git_env::fixture_git(&source, &source)
            .args(["rev-parse", "clonegate1"])
            .output()
            .expect("git rev-parse must spawn");
        let clonegate1_sha = String::from_utf8_lossy(&clonegate1_sha.stdout)
            .trim()
            .to_string();
        git(&source, &["checkout", "main"]);

        let clone_dir = ws.path().join("source-clonegate1");
        let result = provision_isolated_clone_sync(&source, &clone_dir, "clonegate1", "tester");
        assert!(
            matches!(result, Ok(IsolatedCloneOutcome::Created { .. })),
            "isolated clone must succeed, got {result:?}"
        );

        let head_sha = crate::git_env::fixture_git(&clone_dir, &clone_dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("git rev-parse must spawn");
        assert_eq!(
            String::from_utf8_lossy(&head_sha.stdout).trim(),
            clonegate1_sha,
            "the clone must attach to the REAL `clonegate1` (source's commit), not create a \
             fresh branch of the same name at the clone's HEAD (`main`'s commit) — that would \
             silently discard the committed work"
        );
        assert!(
            clone_dir.join("WORK.md").exists(),
            "WORK.md (added on the real `clonegate1`) must be present — its absence means the \
             branch was re-created from `main` instead of attached"
        );
    }

    /// Scenario: PRD fork#325 fix round 3 (reviewer P2-1), reproducing the
    /// reviewer's own empirical transcript: a user with `checkout.guess =
    /// false` in their git config hitting `error: pathspec … did not match`
    /// on round 2's bare, DWIM-dependent `git checkout <branch>` for a
    /// branch that exists in the clone ONLY as a remote-tracking ref (the
    /// same shape the preceding test sets up). Sets `checkout.guess = false`
    /// via a SCOPED `GIT_CONFIG_GLOBAL` override (git >= 2.32; never the
    /// real user's `~/.gitconfig`) for the duration of the
    /// `provision_isolated_clone_sync` call through [`ScopedEnvVar`], which
    /// restores the previous value (or removes the var entirely)
    /// panic-safely on drop (fix round 4, auditor D6 — the straight-line
    /// set/restore this used before left the override in place for the rest
    /// of the process if `provision_isolated_clone_sync` itself panicked)
    /// while holding [`GIT_CONFIG_GLOBAL_TEST_LOCK`] — `std::env::set_var`
    /// mutates PROCESS-global state (matching `agent_pty.rs`'s own
    /// `ENV_TEST_LOCK` precedent for this exact class of test), so without
    /// serializing this override could otherwise leak into a sibling test's
    /// git invocations that happen to run in the same test binary process.
    /// Asserts the clone still succeeds and attaches to the real branch's
    /// commit, proving the round-3 explicit `--track origin/<branch>` form
    /// no longer depends on DWIM at all.
    #[test]
    fn provision_isolated_clone_sync_attaches_remote_only_branch_with_checkout_guess_disabled() {
        fn git(dir: &Path, args: &[&str]) {
            let out = crate::git_env::fixture_git(dir, dir)
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
        let source = ws.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        git(&source, &["init", "--initial-branch=main", "--quiet"]);
        git(&source, &["config", "user.email", "test@example.com"]);
        git(&source, &["config", "user.name", "Test"]);
        git(&source, &["config", "commit.gpgsign", "false"]);
        std::fs::write(source.join("README.md"), "seed\n").unwrap();
        git(&source, &["add", "README.md"]);
        git(&source, &["commit", "--quiet", "-m", "seed"]);

        git(&source, &["checkout", "-b", "clonegate-guess"]);
        std::fs::write(source.join("WORK.md"), "real work\n").unwrap();
        git(&source, &["add", "WORK.md"]);
        git(&source, &["commit", "--quiet", "-m", "real work"]);
        let real_sha = crate::git_env::fixture_git(&source, &source)
            .args(["rev-parse", "clonegate-guess"])
            .output()
            .expect("git rev-parse must spawn");
        let real_sha = String::from_utf8_lossy(&real_sha.stdout).trim().to_string();
        git(&source, &["checkout", "main"]);

        let scoped_global_config = ws.path().join("scoped-gitconfig");
        std::fs::write(&scoped_global_config, "[checkout]\n\tguess = false\n").unwrap();

        let _env_guard =
            ScopedEnvVar::set("GIT_CONFIG_GLOBAL", &scoped_global_config.to_string_lossy());

        let clone_dir = ws.path().join("source-clonegate-guess");
        let result =
            provision_isolated_clone_sync(&source, &clone_dir, "clonegate-guess", "tester");

        drop(_env_guard);

        assert!(
            matches!(result, Ok(IsolatedCloneOutcome::Created { .. })),
            "with checkout.guess=false, the remote-only branch attach must still succeed via \
             the explicit --track form — round 2's bare DWIM-dependent checkout would have \
             failed here with `error: pathspec … did not match`, got {result:?}"
        );

        let head_sha = crate::git_env::fixture_git(&clone_dir, &clone_dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("git rev-parse must spawn");
        assert_eq!(
            String::from_utf8_lossy(&head_sha.stdout).trim(),
            real_sha,
            "the clone must land on the real branch's commit, not a freshly created branch at \
             the clone's HEAD"
        );
    }

    /// Scenario: PRD fork#325 fix round 2 (reviewer P2-E). Two threads call
    /// `provision_isolated_clone_sync` for the SAME `source_dir`/`clone_dir`
    /// pair concurrently — the new attach lock (auditor A3) exists
    /// specifically to serialize this. Asserts exactly one thread reports
    /// `Created` and the other reports a second, distinguishable success,
    /// across many trials, mirroring `worktree/create/001`'s own reasoning
    /// for why a single trial proves nothing.
    ///
    /// PRD fork#544 M3: the loser used to report `AlreadyClaimed`
    /// (round 2's original assertion) — that outcome no longer exists for
    /// this scenario. Once the winner's `git clone` makes `clone_dir`
    /// genuinely present, it is ALSO fully resume-eligible (real M4b
    /// evidence, matching ancestry, healthy), so the loser now correctly
    /// resumes it instead of being refused — this is M3's whole point, not
    /// a regression: see `orchestration/workspace/009` for the same
    /// winner/loser shape applied to a pre-existing (rather than
    /// just-created) directory.
    ///
    /// PRD fork#544 review-findings fix round (reviewer B2): both racers
    /// now share the SAME creator string, not two distinct ones. Reviewer
    /// B2's own fix (`orchestration/workspace/026`) makes a resume refuse
    /// when the provenance artifact's `creator=` names someone else — the
    /// correct behavior for two genuinely DIFFERENT orchestration Names
    /// colliding after sanitization, but wrong for THIS test's own
    /// scenario: two callers racing to open the SAME Name (hence, in real
    /// production, the SAME derived `orchestration:<name>` creator) via two
    /// panes/attempts. Distinct `"racer-a"`/`"racer-b"` strings were never
    /// a realistic stand-in for that — they simulated a same-Name race
    /// using two DIFFERENT identities, which is precisely the shape
    /// `026`'s fix now (correctly) refuses.
    ///
    /// `#[cfg(unix)]` matches `worktree/create/001`'s own precedent — many
    /// trials of real `git` subprocesses (here, full clones, heavier than
    /// that test's `worktree add`) starved `build-windows`'s shared CI
    /// runner.
    #[cfg(unix)]
    #[test]
    fn provision_isolated_clone_sync_concurrent_calls_never_both_create() {
        fn git(dir: &Path, args: &[&str]) {
            let out = crate::git_env::fixture_git(dir, dir)
                .args(args)
                .output()
                .unwrap_or_else(|e| panic!("git {args:?} failed to spawn: {e}"));
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        const TRIALS: usize = 20;

        let ws = tempfile::tempdir().unwrap();
        let source = ws.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        git(&source, &["init", "--initial-branch=main", "--quiet"]);
        git(&source, &["config", "user.email", "test@example.com"]);
        git(&source, &["config", "user.name", "Test"]);
        git(&source, &["config", "commit.gpgsign", "false"]);
        std::fs::write(source.join("README.md"), "seed\n").unwrap();
        git(&source, &["add", "README.md"]);
        git(&source, &["commit", "--quiet", "-m", "seed"]);

        let barriers: Vec<std::sync::Barrier> =
            (0..TRIALS).map(|_| std::sync::Barrier::new(2)).collect();

        let results: Vec<[Result<IsolatedCloneOutcome, String>; 2]> = std::thread::scope(|s| {
            let mut handles = Vec::with_capacity(TRIALS);
            for (i, barrier) in barriers.iter().enumerate() {
                let source = &source;
                let clone_dir = ws.path().join(format!("source-race-{i}"));
                let clone_dir_a = clone_dir.clone();
                let clone_dir_b = clone_dir.clone();
                let h_a = s.spawn(move || {
                    barrier.wait();
                    provision_isolated_clone_sync(source, &clone_dir_a, "race", "racer")
                });
                let h_b = s.spawn(move || {
                    barrier.wait();
                    provision_isolated_clone_sync(source, &clone_dir_b, "race", "racer")
                });
                handles.push((h_a, h_b));
            }
            handles
                .into_iter()
                .map(|(a, b)| [a.join().unwrap(), b.join().unwrap()])
                .collect()
        });

        for (i, pair) in results.iter().enumerate() {
            let created_count = pair
                .iter()
                .filter(|r| matches!(r, Ok(IsolatedCloneOutcome::Created { .. })))
                .count();
            let resumed_count = pair
                .iter()
                .filter(|r| matches!(r, Ok(IsolatedCloneOutcome::Resumed { .. })))
                .count();
            assert_eq!(
                created_count, 1,
                "trial {i}: exactly one caller must report Created, got {pair:?}"
            );
            assert_eq!(
                resumed_count, 1,
                "trial {i}: PRD fork#544 M3 — the loser must now resume the winner's freshly \
                 created (and therefore fully resume-eligible) clone rather than being refused, \
                 got {pair:?}"
            );
        }
    }

    /// Read `dir`'s HEAD commit SHA via `git rev-parse HEAD` — shared by the
    /// PRD fork#544 M3 resume-eligibility tests below, mirroring the
    /// identically-named helper in `tests/e2e_orchestration_worktree.rs`.
    fn head_sha(dir: &Path) -> String {
        let out = crate::git_env::fixture_git(dir, dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("git rev-parse HEAD must spawn");
        assert!(
            out.status.success(),
            "git rev-parse HEAD failed in {dir:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// `git init` + inline-identity-configure + one commit, shared setup for
    /// the PRD fork#544 M3 resume-eligibility tests below — the same
    /// sequence `provision_isolated_clone_sync_sets_origin_and_branch_when_source_has_origin`
    /// and its siblings above already inline per-test; hoisted here since
    /// four more tests need the identical seed.
    fn seed_source_repo(dir: &Path, seed_content: &str) {
        fn git(dir: &Path, args: &[&str]) {
            let out = crate::git_env::fixture_git(dir, dir)
                .args(args)
                .output()
                .unwrap_or_else(|e| panic!("git {args:?} failed to spawn: {e}"));
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "--initial-branch=main", "--quiet"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "Test"]);
        git(dir, &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("README.md"), seed_content).unwrap();
        git(dir, &["add", "README.md"]);
        git(dir, &["commit", "--quiet", "-m", "seed"]);
    }

    /// Run `git <args>` with `dir` as cwd, asserting success — hoisted
    /// module-level plumbing (mirroring `seed_source_repo`'s own hoisting
    /// rationale) for the PRD fork#544 M7 post-merge-sync tests below, which
    /// need to script a real merge landing on a separate "origin" repository
    /// (clone / fetch-a-branch-into-origin / merge --ff-only), not just seed
    /// one initial commit.
    fn git(dir: &Path, args: &[&str]) {
        let out = crate::git_env::fixture_git(dir, dir)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?} failed to spawn: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The capturing twin of [`git`] above — trimmed stdout.
    fn git_output(dir: &Path, args: &[&str]) -> String {
        let out = crate::git_env::fixture_git(dir, dir)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?} failed to spawn: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Scenario: PRD fork#544 M3. `provision_isolated_clone_sync` is called
    /// against a `clone_dir` that already exists on disk but was never
    /// created by this deck — no M4b provenance artifact exists for it (a
    /// directory a human happened to create at the same derived path, with
    /// its own unrelated git history). Asserts the outcome refuses to
    /// silently attach and, once PRD fork#544 M3's eligibility check exists,
    /// names the refusal distinguishably as a stranger directory rather than
    /// folding it into the generic `AlreadyClaimed` outcome every
    /// present-directory case reports pre-M3. `DOT_AGENT_DECK_STATE_DIR` is
    /// pinned so the M4b evidence lookup this test's assertion depends on
    /// resolves deterministically regardless of what else runs in this
    /// process.
    #[spec("orchestration/workspace/006")]
    #[test]
    fn workspace_006_stranger_directory_refused_with_no_evidence() {
        let ws = tempfile::tempdir().unwrap();
        let state_dir = ws.path().join("state");
        let _state_guard =
            ScopedEnvVar::set("DOT_AGENT_DECK_STATE_DIR", state_dir.to_str().unwrap());

        let source = ws.path().join("source");
        seed_source_repo(&source, "seed\n");

        let clone_dir = ws.path().join("source-my-feature");
        // A stranger directory: present on disk with its own unrelated git
        // history, but never created by `provision_isolated_clone_sync` —
        // no M4b provenance artifact was ever written for this canonical
        // path.
        seed_source_repo(&clone_dir, "not the deck's\n");
        let stranger_head_before = head_sha(&clone_dir);

        let result = provision_isolated_clone_sync(&source, &clone_dir, "my-feature", "tester");
        let debug = format!("{result:?}");

        assert!(
            !matches!(result, Ok(IsolatedCloneOutcome::Created { .. })),
            "a stranger directory with no ownership evidence must never be silently (re)created \
             over, got {debug}"
        );
        assert!(
            debug.to_lowercase().contains("stranger"),
            "PRD fork#544 M3: a directory with NO matching M4b ownership evidence must be \
             refused with a distinguishable 'stranger directory' reason (per the PRD's own \
             compatibility table) — pre-M3 code reports the generic AlreadyClaimed outcome for \
             every present-directory case with no such distinction, got {debug}"
        );

        assert_eq!(
            head_sha(&clone_dir),
            stranger_head_before,
            "a refused stranger directory's git state must be completely untouched"
        );
        assert!(
            clone_dir.join("README.md").exists(),
            "a refused stranger directory's working tree must be completely untouched"
        );
    }

    /// Scenario: PRD fork#544 M3. A `clone_dir` that DOES carry genuine M4b
    /// ownership evidence — because it was actually created by a real prior
    /// `provision_isolated_clone_sync` call — is reopened against a SECOND,
    /// entirely unrelated source repository at the same derived path (the
    /// same Name typed against a different underlying project — Problem
    /// Statement #4's exact hazard: content-free evidence alone can't prove
    /// WHICH repository a directory belongs to). Asserts the mismatched
    /// ancestry refuses the resume distinguishably, never silently
    /// attaching the new source's session onto foreign history.
    #[spec("orchestration/workspace/007")]
    #[test]
    fn workspace_007_ancestry_mismatch_refused_as_wrong_repo() {
        let ws = tempfile::tempdir().unwrap();
        let state_dir = ws.path().join("state");
        let _state_guard =
            ScopedEnvVar::set("DOT_AGENT_DECK_STATE_DIR", state_dir.to_str().unwrap());

        let source_a = ws.path().join("source-a");
        seed_source_repo(&source_a, "seed-a\n");

        let clone_dir = ws.path().join("workspace-my-feature");
        let created = provision_isolated_clone_sync(&source_a, &clone_dir, "my-feature", "tester");
        assert!(
            matches!(created, Ok(IsolatedCloneOutcome::Created { .. })),
            "setup: the initial clone from source A must succeed, got {created:?}"
        );
        let clone_head_before = head_sha(&clone_dir);

        // A SECOND, entirely unrelated source repository — no shared
        // history, no shared origin — happening to want the SAME clone_dir.
        let source_b = ws.path().join("source-b");
        seed_source_repo(&source_b, "seed-b-unrelated-history\n");

        let result = provision_isolated_clone_sync(&source_b, &clone_dir, "my-feature", "tester");
        let debug = format!("{result:?}");

        assert!(
            !matches!(result, Ok(IsolatedCloneOutcome::Created { .. })),
            "a directory with REAL M4b evidence but UNRELATED ancestry to the newly-opened \
             source must never be silently created/attached over, got {debug}"
        );
        assert!(
            debug.to_lowercase().contains("ancestry")
                || debug.to_lowercase().contains("wrong repo")
                || debug.to_lowercase().contains("stale")
                || debug.to_lowercase().contains("unrelated"),
            "PRD fork#544 M3: a directory whose origin/ancestry does not match the source being \
             opened must be refused with a distinguishable wrong/stale-repo reason (per the \
             PRD's own compatibility table) — pre-M3 code reports the generic AlreadyClaimed \
             outcome for every present-directory case with no such distinction, got {debug}"
        );

        assert_eq!(
            head_sha(&clone_dir),
            clone_head_before,
            "the existing clone's git state (unrelated to source B) must be untouched by the \
             refused attempt"
        );
    }

    /// Scenario: PRD fork#544 M3. A `clone_dir` that carries genuine M4b
    /// evidence AND matching ancestry (both would pass) is corrupted —
    /// its `.git` directory is removed entirely, simulating a directory
    /// mid-delete or otherwise unhealthy — before a second
    /// `provision_isolated_clone_sync` call against the SAME source.
    /// Asserts the health probe's failure refuses distinguishably, and
    /// critically that the refusal never auto-deletes or silently replaces
    /// the unhealthy directory (the PRD's own explicit "never auto-delete"
    /// decision).
    #[spec("orchestration/workspace/008")]
    #[test]
    fn workspace_008_unhealthy_directory_refused_without_deleting() {
        let ws = tempfile::tempdir().unwrap();
        let state_dir = ws.path().join("state");
        let _state_guard =
            ScopedEnvVar::set("DOT_AGENT_DECK_STATE_DIR", state_dir.to_str().unwrap());

        let source = ws.path().join("source");
        seed_source_repo(&source, "seed\n");

        let clone_dir = ws.path().join("workspace-my-feature");
        let created = provision_isolated_clone_sync(&source, &clone_dir, "my-feature", "tester");
        assert!(
            matches!(created, Ok(IsolatedCloneOutcome::Created { .. })),
            "setup: the initial clone must succeed, got {created:?}"
        );

        // Corrupt the clone's git state — remove `.git` entirely, simulating
        // a directory mid-delete or otherwise unhealthy, while leaving the
        // working-tree files (and the M4b evidence, which lives OUTSIDE the
        // directory under state_dir()) untouched.
        std::fs::remove_dir_all(clone_dir.join(".git")).expect("corrupt clone's .git");
        assert!(
            clone_dir.join("README.md").exists(),
            "sanity: working-tree files survive the corruption, only .git is gone"
        );

        let result = provision_isolated_clone_sync(&source, &clone_dir, "my-feature", "tester");
        let debug = format!("{result:?}");

        assert!(
            !matches!(result, Ok(IsolatedCloneOutcome::Created { .. })),
            "an unhealthy directory (evidence + ancestry would otherwise match) must never be \
             silently treated as fresh/created-over, got {debug}"
        );
        assert!(
            debug.to_lowercase().contains("unhealthy")
                || debug.to_lowercase().contains("corrupt")
                || debug.to_lowercase().contains("health"),
            "PRD fork#544 M3: a directory that fails the health probe (e.g. `git rev-parse \
             HEAD` no longer succeeding) must be refused with a distinguishable 'unhealthy' \
             reason — pre-M3 code reports the generic AlreadyClaimed outcome for every \
             present-directory case with no such distinction, got {debug}"
        );

        // Never auto-deleted, and never silently repaired/replaced either.
        assert!(
            clone_dir.is_dir(),
            "the unhealthy directory must never be auto-deleted on refusal"
        );
        assert!(
            !clone_dir.join(".git").exists(),
            "the directory must still be missing its .git — confirms the refusal did not \
             silently repair or replace it"
        );
        assert!(
            clone_dir.join("README.md").exists(),
            "working-tree files must remain untouched by the refusal"
        );
    }

    /// Scenario: PRD fork#544 M3's resume-race winner/loser split. Mirrors
    /// `provision_isolated_clone_sync_concurrent_calls_never_both_create`'s
    /// own barrier-synchronized two-thread pattern, but races a RESUME
    /// (the directory already exists and is genuinely resume-eligible —
    /// created once via a real prior call, so it carries matching M4b
    /// evidence and ancestry) rather than a fresh CREATE. The setup call
    /// and both racers all share ONE creator string (`"racer"`, matching
    /// that same sibling test's own precedent) rather than three distinct
    /// ones.
    ///
    /// Round-3 review-findings fix (reviewer, "`workspace_009` drift"):
    /// with the original mismatched creators (`"setup"`/`"racer-a"`/
    /// `"racer-b"`), the B2 fix round's new `NameCollision` check makes
    /// even the race's WINNER mismatch the stored `"setup"` creator, so it
    /// is refused as `NameCollision` too — nothing ever resumes. The
    /// surviving `assert_ne!` on the two outcomes' `Debug` strings still
    /// passed, but for a reason unrelated to what this test is meant to
    /// prove: the process-global registry mutex (`resumed_isolated_clones()`)
    /// alone hands one racer `NameCollision` and the other `Contested`, so
    /// the test would stay green even with the (separate, cross-process)
    /// attach lock removed entirely — that mutex is atomic on its own and
    /// fully accounts for the observable split. One shared creator string
    /// removes `NameCollision` from the reachable outcomes here and
    /// restores the race-specific property directly: across all trials,
    /// M3's resume arm yields exactly one `Resumed` winner and exactly one
    /// `Contested` loser, where pre-M3 code yields two identical
    /// `AlreadyClaimed` outcomes with no winner at all — never both, never
    /// neither, never anything else. This test does not, and cannot,
    /// distinguish the registry mutex's contribution from the attach
    /// lock's; see "Does not assert" in `tests/CATALOG.md`.
    ///
    /// `#[cfg(unix)]` matches the precedent this mirrors — many trials of
    /// real `git` subprocesses on a shared CI runner.
    #[cfg(unix)]
    #[spec("orchestration/workspace/009")]
    #[test]
    fn workspace_009_concurrent_resume_attempts_report_distinguishable_outcomes() {
        const TRIALS: usize = 10;

        let ws = tempfile::tempdir().unwrap();
        let state_dir = ws.path().join("state");
        let _state_guard =
            ScopedEnvVar::set("DOT_AGENT_DECK_STATE_DIR", state_dir.to_str().unwrap());

        let source = ws.path().join("source");
        seed_source_repo(&source, "seed\n");

        let barriers: Vec<std::sync::Barrier> =
            (0..TRIALS).map(|_| std::sync::Barrier::new(2)).collect();

        let results: Vec<[Result<IsolatedCloneOutcome, String>; 2]> = std::thread::scope(|s| {
            let mut handles = Vec::with_capacity(TRIALS);
            for (i, barrier) in barriers.iter().enumerate() {
                let source = &source;
                let clone_dir = ws.path().join(format!("source-resume-race-{i}"));
                // Pre-create the resume-eligible directory ONCE, before the
                // race: a real prior clone, carrying genuine M4b evidence
                // and matching ancestry — the race is over RESUMING it, not
                // creating it.
                let setup = provision_isolated_clone_sync(source, &clone_dir, "race", "racer");
                assert!(
                    matches!(setup, Ok(IsolatedCloneOutcome::Created { .. })),
                    "trial {i} setup: the initial clone must succeed, got {setup:?}"
                );

                let clone_dir_a = clone_dir.clone();
                let clone_dir_b = clone_dir.clone();
                let h_a = s.spawn(move || {
                    barrier.wait();
                    provision_isolated_clone_sync(source, &clone_dir_a, "race", "racer")
                });
                let h_b = s.spawn(move || {
                    barrier.wait();
                    provision_isolated_clone_sync(source, &clone_dir_b, "race", "racer")
                });
                handles.push((h_a, h_b));
            }
            handles
                .into_iter()
                .map(|(a, b)| [a.join().unwrap(), b.join().unwrap()])
                .collect()
        });

        for (i, pair) in results.iter().enumerate() {
            let [a, b] = pair;
            let resumed_count = [a, b]
                .iter()
                .filter(|r| matches!(r, Ok(IsolatedCloneOutcome::Resumed { .. })))
                .count();
            let contested_count = [a, b]
                .iter()
                .filter(|r| {
                    matches!(
                        r,
                        Ok(IsolatedCloneOutcome::Rejected(ResumeRejection::Contested))
                    )
                })
                .count();
            assert_eq!(
                resumed_count, 1,
                "trial {i}: PRD fork#544 M3 — two near-simultaneous resume attempts against the \
                 SAME already-eligible directory, opened under the IDENTICAL creator string, \
                 must yield exactly one Resumed winner — pre-M3 code reports the identical \
                 generic AlreadyClaimed for both racers here, with no winner at all, \
                 got {a:?} / {b:?}"
            );
            assert_eq!(
                contested_count, 1,
                "trial {i}: PRD fork#544 M3 — the loser of that same race must be refused as \
                 Contested exactly once, got {a:?} / {b:?}"
            );
        }
    }

    /// Scenario: PRD fork#544 M3 fix round. Resumes the SAME already-eligible
    /// directory TWICE in a row, in one still-running process, releasing
    /// `resumed_isolated_clones()`'s registration in between each resume —
    /// exactly what both real callers (`src/ui.rs`, `src/dispatch.rs`) now do
    /// immediately after `provision_isolated_clone_sync` returns. Before the
    /// fix, nothing ever released an entry, so the SECOND resume in this
    /// sequence incorrectly saw `Rejected(Contested)`; this pins that it now
    /// resumes successfully, matching the PRD's own stated purpose (a named
    /// workspace resumable repeatedly, not once). A trailing THIRD attempt
    /// made WITHOUT releasing in between still correctly loses to
    /// `Contested`, proving the fix didn't accidentally stop the registry
    /// from protecting the still-open race window at all.
    #[spec("orchestration/workspace/010")]
    #[test]
    fn workspace_010_repeated_resume_in_same_process_succeeds_after_registration_release() {
        let ws = tempfile::tempdir().unwrap();
        let state_dir = ws.path().join("state");
        let _state_guard =
            ScopedEnvVar::set("DOT_AGENT_DECK_STATE_DIR", state_dir.to_str().unwrap());

        let source = ws.path().join("source");
        seed_source_repo(&source, "seed\n");

        let clone_dir = ws.path().join("source-repeat-resume");

        // Open #1: a real CREATE, establishing M4b evidence and ancestry.
        let created = provision_isolated_clone_sync(&source, &clone_dir, "repeat", "opener");
        assert!(
            matches!(created, Ok(IsolatedCloneOutcome::Created { .. })),
            "setup: the initial clone must succeed, got {created:?}"
        );

        // Reopen #1 (1st resume): the one legitimate resume every pre-fix
        // test already exercised — must succeed.
        let resume_1 = provision_isolated_clone_sync(&source, &clone_dir, "repeat", "opener");
        assert!(
            matches!(resume_1, Ok(IsolatedCloneOutcome::Resumed { .. })),
            "the 1st resume must succeed, got {resume_1:?}"
        );

        // Mirrors what `src/ui.rs`/`src/dispatch.rs` now do immediately
        // after `provision_isolated_clone_sync` returns, on every outcome —
        // their own liveness check/claim (fork#192's `ClaimOrchestrationName`
        // for the former, the has-live-sibling daemon query for the latter)
        // already ran BEFORE provisioning, so releasing here is safe.
        release_resumed_isolated_clone_registration(&clone_dir);

        // Reopen #2 (2nd resume, the PRD fork#544's own stated purpose — a
        // named workspace resumable repeatedly, not once): before the fix
        // this incorrectly saw `Rejected(Contested)`, since nothing ever
        // released the 1st resume's registration.
        let resume_2 = provision_isolated_clone_sync(&source, &clone_dir, "repeat", "opener");
        assert!(
            matches!(resume_2, Ok(IsolatedCloneOutcome::Resumed { .. })),
            "the 2nd resume, after the 1st resume's registration was released, must succeed — \
             not Rejected(Contested) — got {resume_2:?}"
        );

        // Reopen #3, attempted WITHOUT releasing `resume_2`'s registration
        // first: `009`'s own race-distinguishing property must still hold —
        // the registry must still refuse a resume that finds an
        // unreleased entry already there.
        let resume_3 = provision_isolated_clone_sync(&source, &clone_dir, "repeat", "opener");
        assert!(
            matches!(
                resume_3,
                Ok(IsolatedCloneOutcome::Rejected(ResumeRejection::Contested))
            ),
            "a resume attempted before the prior resume's registration is released must still \
             be refused as Contested — the registry's own race protection must survive the fix, \
             got {resume_3:?}"
        );
    }

    /// Scenario: PRD fork#544 M4. After a real `provision_isolated_clone_sync`
    /// call creates a fresh isolated clone (and, as a side effect, writes the
    /// M4b provenance artifact), the artifact's raw file content is read
    /// directly off disk and must contain a hash of the root checkout's
    /// (`source_dir`'s) own canonical path, the orchestration Name typed for
    /// this workspace, and the clone's own canonical path — all as real,
    /// extractable data, not merely a content-free presence marker. Today the
    /// artifact is still the literal bytes `b"deck\n"`, so this fails on the
    /// very first assertion.
    #[spec("orchestration/workspace/011")]
    #[test]
    fn workspace_011_provenance_artifact_round_trips_schema_hash_name_and_path() {
        let ws = tempfile::tempdir().unwrap();
        let state_dir = ws.path().join("state");
        let _state_guard =
            ScopedEnvVar::set("DOT_AGENT_DECK_STATE_DIR", state_dir.to_str().unwrap());

        let source = ws.path().join("source");
        seed_source_repo(&source, "seed\n");

        let clone_dir = ws.path().join("source-my-feature");
        let created = provision_isolated_clone_sync(&source, &clone_dir, "my-feature", "opener");
        assert!(
            matches!(created, Ok(IsolatedCloneOutcome::Created { .. })),
            "setup: the initial clone must succeed, got {created:?}"
        );

        let marker_path = isolated_clone_provenance_path(&clone_dir);
        let content = std::fs::read_to_string(&marker_path).unwrap_or_else(|e| {
            panic!("provenance artifact must be readable at {marker_path:?}: {e}")
        });

        let expected_root_hash = format!(
            "{:016x}",
            crate::platform::lock::fnv1a64(
                canonicalize_best_effort(&source)
                    .to_string_lossy()
                    .as_bytes()
            )
        );
        let expected_path = canonicalize_best_effort(&clone_dir)
            .to_string_lossy()
            .into_owned();

        assert!(
            content.trim() != "deck",
            "PRD fork#544 M4: the provenance artifact must carry structured data (schema tag, \
             root-checkout-path hash, orchestration Name, canonical workspace path) — today it \
             is still the content-free M4b bytes `b\"deck\\n\"`, got {content:?}"
        );
        assert!(
            content.contains(&expected_root_hash),
            "PRD fork#544 M4: the artifact must record a hash of the root checkout's \
             (source_dir's) canonical path at write time, keyed the same way \
             `isolated_clone_provenance_path` already keys the clone path itself — expected to \
             find {expected_root_hash} somewhere in {content:?}"
        );
        assert!(
            content.contains("my-feature"),
            "PRD fork#544 M4: the artifact must record the orchestration Name as explicit \
             structured data — expected to find the Name \"my-feature\" somewhere in {content:?}"
        );
        assert!(
            content.contains(&expected_path),
            "PRD fork#544 M4: the artifact must record the canonical workspace path as explicit \
             structured data — expected to find {expected_path:?} somewhere in {content:?}"
        );
    }

    /// Scenario: PRD fork#544 M4. A pre-M4 provenance artifact — the literal
    /// bytes `b"deck\n"` written directly to disk, bypassing the writer
    /// entirely, simulating a workspace created by a build that predates the
    /// M4 fields — must still be treated as valid M4b ownership evidence:
    /// reopening the same Name against the same source must still resume
    /// rather than being refused as a stranger directory. This exercises
    /// EXISTING M3 behavior (`resume_existing_isolated_clone`'s eligibility
    /// check (b) only ever tested file presence via `is_file()`, never
    /// content), so unlike `011` this may already be green — reported
    /// honestly either way, not forced.
    #[spec("orchestration/workspace/012")]
    #[test]
    fn workspace_012_old_format_content_free_evidence_still_valid_for_resume() {
        let ws = tempfile::tempdir().unwrap();
        let state_dir = ws.path().join("state");
        let _state_guard =
            ScopedEnvVar::set("DOT_AGENT_DECK_STATE_DIR", state_dir.to_str().unwrap());

        let source = ws.path().join("source");
        seed_source_repo(&source, "seed\n");

        let clone_dir = ws.path().join("source-legacy-workspace");
        let created =
            provision_isolated_clone_sync(&source, &clone_dir, "legacy-workspace", "opener");
        assert!(
            matches!(created, Ok(IsolatedCloneOutcome::Created { .. })),
            "setup: the initial clone must succeed, got {created:?}"
        );

        // Simulate a pre-M4 artifact: overwrite whatever the writer just
        // produced with the OLD content-free bytes, bypassing the writer
        // entirely — this is deliberately NOT calling any writer function,
        // to prove the READ path tolerates a genuinely pre-M4 file rather
        // than merely tolerating today's writer's own output.
        let marker_path = isolated_clone_provenance_path(&clone_dir);
        std::fs::write(&marker_path, b"deck\n").expect("overwrite with pre-M4 format");

        // Release the prior registration so this resume attempt isn't
        // refused as Contested for a reason unrelated to what this test
        // checks.
        release_resumed_isolated_clone_registration(&clone_dir);

        let result =
            provision_isolated_clone_sync(&source, &clone_dir, "legacy-workspace", "opener");
        assert!(
            matches!(result, Ok(IsolatedCloneOutcome::Resumed { .. })),
            "PRD fork#544 M4: an old-format (pre-M4) `b\"deck\\n\"` provenance artifact must \
             still be treated as valid ownership evidence — M3's eligibility check (b) must keep \
             passing for it, not reject it as a stranger directory just because it predates the \
             M4 fields, got {result:?}"
        );
    }

    /// Scenario: fork issue #766 fix round 2 (audit A2). `src/dispatch.rs`'s
    /// own `dispatch <name>` CLI call site now always passes
    /// `LegacyFallbackEligibility::Disabled` — the same variant this test
    /// constructs directly — to `provision_isolated_clone_sync_resolved`.
    /// Proves that variant is structurally incapable of taking the
    /// legacy-creator-format fallback even against a marker that WOULD
    /// satisfy it under `ToplevelWithIdentity` (same repo, same clone_dir,
    /// a legacy `orchestration:`-namespaced creator whose `name=` field
    /// exactly matches the branch this call would otherwise offer as its
    /// identity) — a mismatched-only test could never distinguish "the
    /// fallback correctly refused a mismatch" from "the fallback is
    /// unreachable at all"; this one can, because the marker on disk here
    /// is a genuine match.
    #[spec("orchestration/workspace/045")]
    #[test]
    fn workspace_045_disabled_legacy_fallback_never_matches_even_a_genuine_legacy_marker() {
        let ws = tempfile::tempdir().unwrap();
        let state_dir = ws.path().join("state");
        let _state_guard =
            ScopedEnvVar::set("DOT_AGENT_DECK_STATE_DIR", state_dir.to_str().unwrap());

        let source = ws.path().join("source");
        seed_source_repo(&source, "seed\n");

        let clone_dir = ws.path().join("source-disabled-fallback");
        let created =
            provision_isolated_clone_sync(&source, &clone_dir, "disabled-fallback", "opener");
        assert!(
            matches!(created, Ok(IsolatedCloneOutcome::Created { .. })),
            "setup: the initial clone must succeed, got {created:?}"
        );

        let marker_path = isolated_clone_provenance_path(&clone_dir);
        let content = std::fs::read_to_string(&marker_path).expect("read provenance marker");
        let legacy_creator = "orchestration:disabled-fallback";
        let rewritten: String = content
            .lines()
            .map(|line| {
                if line.starts_with("creator=") {
                    format!("creator={legacy_creator}")
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(&marker_path, &rewritten)
            .expect("overwrite provenance marker with legacy creator");

        release_resumed_isolated_clone_registration(&clone_dir);

        // Exactly the shape `src/dispatch.rs`'s call site constructs:
        // `LegacyFallbackEligibility::Disabled`, with a `creator` argument
        // that deliberately differs from the stored legacy creator (so the
        // primary stored-vs-computed comparison mismatches and the
        // fallback block would run, if it were ever reachable for this
        // variant).
        let result = provision_isolated_clone_sync_resolved(
            &source,
            &clone_dir,
            "disabled-fallback",
            "todays-computed-creator",
            LegacyFallbackEligibility::Disabled,
        );
        assert!(
            matches!(
                result,
                Ok(IsolatedCloneOutcome::Rejected(
                    ResumeRejection::NameCollision
                ))
            ),
            "fork issue #766 audit A2: LegacyFallbackEligibility::Disabled -- the variant \
             src/dispatch.rs's own call site always passes -- must refuse as NameCollision even \
             against a marker that would satisfy the fallback's caller-identity check exactly, \
             got {result:?}"
        );
    }

    /// Scenario: PRD fork#544 M4. After a real `provision_isolated_clone_sync`
    /// call writes today's provenance artifact, the artifact's schema tag
    /// must round-trip as real, readable data — a `schema=<CURRENT>` marker
    /// a future consumer could branch on — not merely be present-but-unread.
    /// No second, genuinely pre-M4 schema value exists yet to compare
    /// against (M4 is the first schema tag this artifact has ever carried),
    /// so this only proves TODAY's tag is real extractable data, per the
    /// PRD's own explicit carve-out ("you don't need a second real 'old
    /// naming scheme' to compare against").
    #[spec("orchestration/workspace/013")]
    #[test]
    fn workspace_013_schema_tag_is_real_readable_data_not_merely_present() {
        // Tester's proposed encoding for the M4 artifact, since none is
        // specified beyond "content-minimal, no untrusted payload": plain
        // `key=value` lines, one per field. `CURRENT_SCHEMA` is this test's
        // own stand-in for whatever tag value the real implementation picks
        // for "today's schema" — see this test's `work-done` report for the
        // full recommendation.
        const CURRENT_SCHEMA: &str = "2";

        let ws = tempfile::tempdir().unwrap();
        let state_dir = ws.path().join("state");
        let _state_guard =
            ScopedEnvVar::set("DOT_AGENT_DECK_STATE_DIR", state_dir.to_str().unwrap());

        let source = ws.path().join("source");
        seed_source_repo(&source, "seed\n");

        let clone_dir = ws.path().join("source-schema-check");
        let created = provision_isolated_clone_sync(&source, &clone_dir, "schema-check", "opener");
        assert!(
            matches!(created, Ok(IsolatedCloneOutcome::Created { .. })),
            "setup: the initial clone must succeed, got {created:?}"
        );

        let marker_path = isolated_clone_provenance_path(&clone_dir);
        let content = std::fs::read_to_string(&marker_path).unwrap_or_else(|e| {
            panic!("provenance artifact must be readable at {marker_path:?}: {e}")
        });

        let schema_tag = content
            .lines()
            .find_map(|line| line.strip_prefix("schema=").map(str::trim));
        assert_eq!(
            schema_tag,
            Some(CURRENT_SCHEMA),
            "PRD fork#544 M4: reading the artifact back must report today's schema tag as real, \
             extractable data a future consumer could branch on to distinguish it from an older \
             naming-scheme's evidence — expected a `schema={CURRENT_SCHEMA}` line, got {content:?}"
        );
    }

    /// Scenario: PRD fork#544 M5. `forget_isolated_workspace` is the new
    /// explicit "forget this workspace" action, modeled on this codebase's
    /// existing `issue claim --takeover --confirm-stopped` pattern: calling
    /// it against a real, live isolated-clone workspace with `confirmed:
    /// false` must refuse rather than silently no-op, and must leave BOTH
    /// the directory and its M4b provenance artifact completely untouched.
    /// `forget_isolated_workspace` does not exist yet (M5 has not
    /// implemented it), so this is a compile-time RED — the expected RED
    /// reason for this milestone, matching this task's own framing ("the
    /// action doesn't exist yet").
    #[spec("orchestration/workspace/014")]
    #[test]
    fn workspace_014_refusal_without_confirmation_leaves_everything_untouched() {
        let ws = tempfile::tempdir().unwrap();
        let state_dir = ws.path().join("state");
        let _state_guard =
            ScopedEnvVar::set("DOT_AGENT_DECK_STATE_DIR", state_dir.to_str().unwrap());

        let source = ws.path().join("source");
        seed_source_repo(&source, "seed\n");

        let clone_dir = ws.path().join("source-forget-me-not");
        let created = provision_isolated_clone_sync(&source, &clone_dir, "forget-me-not", "opener");
        assert!(
            matches!(created, Ok(IsolatedCloneOutcome::Created { .. })),
            "setup: the initial clone must succeed, got {created:?}"
        );

        let marker_path = isolated_clone_provenance_path(&clone_dir);
        let marker_before = std::fs::read_to_string(&marker_path)
            .unwrap_or_else(|e| panic!("setup: provenance artifact must be readable: {e}"));
        let head_before = head_sha(&clone_dir);

        let result = forget_isolated_workspace(&clone_dir, false, "tester");

        assert!(
            result.is_err(),
            "PRD fork#544 M5: forgetting without an explicit confirming flag must refuse, never \
             silently no-op and never report success, got {result:?}"
        );
        let err = result.unwrap_err();
        assert!(
            err.to_lowercase().contains("confirm"),
            "the refusal reason must name the missing confirmation (mirroring this codebase's \
             `--confirm-stopped` precedent), got {err:?}"
        );

        assert!(
            clone_dir.is_dir(),
            "an unconfirmed forget attempt must never remove the workspace directory"
        );
        assert!(
            clone_dir.join("README.md").exists(),
            "an unconfirmed forget attempt must never touch the workspace's working tree"
        );
        assert_eq!(
            head_sha(&clone_dir),
            head_before,
            "an unconfirmed forget attempt must never touch the workspace's git state"
        );
        assert!(
            marker_path.is_file(),
            "an unconfirmed forget attempt must never remove the M4b provenance artifact"
        );
        assert_eq!(
            std::fs::read_to_string(&marker_path).unwrap(),
            marker_before,
            "an unconfirmed forget attempt must never modify the M4b provenance artifact's \
             content"
        );
    }

    /// Scenario: PRD fork#544 M5. Calling `forget_isolated_workspace` with
    /// `confirmed: true` against a real, live isolated-clone workspace must
    /// remove BOTH the workspace directory and its M4b provenance artifact
    /// — never one without the other. This is the milestone's headline
    /// behavior: the only way a named workspace is ever cleared.
    #[spec("orchestration/workspace/015")]
    #[test]
    fn workspace_015_confirmed_forget_atomically_removes_directory_and_provenance() {
        let ws = tempfile::tempdir().unwrap();
        let state_dir = ws.path().join("state");
        let _state_guard =
            ScopedEnvVar::set("DOT_AGENT_DECK_STATE_DIR", state_dir.to_str().unwrap());

        let source = ws.path().join("source");
        seed_source_repo(&source, "seed\n");

        let clone_dir = ws.path().join("source-forget-me");
        let created = provision_isolated_clone_sync(&source, &clone_dir, "forget-me", "opener");
        assert!(
            matches!(created, Ok(IsolatedCloneOutcome::Created { .. })),
            "setup: the initial clone must succeed, got {created:?}"
        );

        let marker_path = isolated_clone_provenance_path(&clone_dir);
        assert!(
            marker_path.is_file(),
            "setup: the provenance artifact must exist before the forget call"
        );

        let result = forget_isolated_workspace(&clone_dir, true, "tester");
        assert!(
            result.is_ok(),
            "PRD fork#544 M5: a confirmed forget against a real, existing workspace must \
             succeed, got {result:?}"
        );

        assert!(
            !clone_dir.exists(),
            "a confirmed forget must remove the workspace directory — don't just check the \
             provenance artifact and assume the directory followed"
        );
        assert!(
            !marker_path.exists(),
            "a confirmed forget must remove the M4b provenance artifact — don't just check the \
             directory and assume the artifact followed"
        );
    }

    /// Scenario: PRD fork#544 M5's own "consider" coverage. Calling
    /// `forget_isolated_workspace` with `confirmed: true` against a
    /// `clone_dir` that has never existed at all — no directory, no
    /// provenance artifact — must not panic and must not falsely report
    /// success; there is nothing there to forget.
    #[spec("orchestration/workspace/016")]
    #[test]
    fn workspace_016_forgetting_a_nonexistent_workspace_does_not_falsely_succeed() {
        let ws = tempfile::tempdir().unwrap();
        let state_dir = ws.path().join("state");
        let _state_guard =
            ScopedEnvVar::set("DOT_AGENT_DECK_STATE_DIR", state_dir.to_str().unwrap());

        let clone_dir = ws.path().join("source-never-existed");
        assert!(
            !clone_dir.exists(),
            "sanity: this path must genuinely never have been created"
        );

        let result = forget_isolated_workspace(&clone_dir, true, "tester");

        assert!(
            result.is_err(),
            "PRD fork#544 M5: forgetting a workspace that was never created must not falsely \
             report success — there is nothing there to forget, got {result:?}"
        );
        assert!(
            !clone_dir.exists(),
            "a forget attempt against a nonexistent workspace must not create anything either"
        );
    }

    /// Scenario: PRD fork#544 M5's own "consider" coverage, and the
    /// codebase's general never-auto-delete-unowned-content safety property
    /// (the same reasoning `resume_existing_isolated_clone`'s stranger-
    /// directory refusal already applies). A directory sitting at the
    /// derived `clone_dir` path with its own real, independent git history
    /// — but never created by `provision_isolated_clone_sync`, so it
    /// carries no M4b provenance artifact — must be refused, not deleted,
    /// even with `confirmed: true`: a confirming flag proves the CALLER
    /// wants to remove ITS workspace, not that this directory IS one.
    #[spec("orchestration/workspace/017")]
    #[test]
    fn workspace_017_stranger_directory_with_no_provenance_is_refused_not_deleted() {
        let ws = tempfile::tempdir().unwrap();
        let state_dir = ws.path().join("state");
        let _state_guard =
            ScopedEnvVar::set("DOT_AGENT_DECK_STATE_DIR", state_dir.to_str().unwrap());

        let clone_dir = ws.path().join("source-not-ours");
        // A stranger directory: present on disk with its own unrelated git
        // history, but never created by `provision_isolated_clone_sync` —
        // no M4b provenance artifact was ever written for this canonical
        // path.
        seed_source_repo(&clone_dir, "not the deck's\n");
        let head_before = head_sha(&clone_dir);

        let result = forget_isolated_workspace(&clone_dir, true, "tester");

        assert!(
            result.is_err(),
            "PRD fork#544 M5: a confirmed forget against a directory with NO matching M4b \
             ownership evidence must refuse rather than deleting someone else's real content, \
             got {result:?}"
        );
        assert!(
            clone_dir.is_dir(),
            "a stranger directory must never be deleted by forget, confirmed or not"
        );
        assert!(
            clone_dir.join("README.md").exists(),
            "a stranger directory's working tree must remain untouched"
        );
        assert_eq!(
            head_sha(&clone_dir),
            head_before,
            "a stranger directory's git state must remain untouched"
        );
    }

    /// Scenario: PRD fork#544 M6's own tab-close persistence guarantee for a
    /// NAMED isolated-clone workspace (Design step 6: "Pin with a test that
    /// tab-close never deletes a named workspace"). A real workspace,
    /// provisioned via `provision_isolated_clone_sync` under a typed
    /// orchestration Name exactly as Model A creates one, is handed to
    /// `remove_worktree` under its own `RemovalPolicy::IsolatedClone` — the
    /// same policy this milestone's registration fix
    /// (`orchestration/workspace/018`) makes Model A's entries carry. The
    /// outcome must be `Kept(IsolatedClone)`, and the directory and its
    /// checked-out branch must survive untouched. This is a REGRESSION
    /// GUARD, not a fresh RED: `remove_worktree`'s `RemovalPolicy::IsolatedClone`
    /// arm already reports `Kept` unconditionally today (Problem Statement
    /// #3) — this pins that guarantee explicitly, under this milestone's own
    /// catalog family, against a genuinely NAMED workspace rather than
    /// relying solely on `src/dispatch.rs`'s pre-existing
    /// `isolated_clone_is_always_kept_on_tab_close_even_when_clean` (written
    /// for Model C / issue #490) to cover it by proxy.
    #[spec("orchestration/workspace/019")]
    #[tokio::test]
    async fn workspace_019_named_isolated_clone_is_kept_unconditionally_on_tab_close() {
        let ws = tempfile::tempdir().unwrap();
        let state_dir = ws.path().join("state");
        let _state_guard =
            ScopedEnvVar::set("DOT_AGENT_DECK_STATE_DIR", state_dir.to_str().unwrap());

        let source = ws.path().join("source");
        seed_source_repo(&source, "seed\n");

        let clone_dir = ws.path().join("named-workspace-019");
        let created =
            provision_isolated_clone_sync(&source, &clone_dir, "workspace019fixed", "opener");
        assert!(
            matches!(created, Ok(IsolatedCloneOutcome::Created { .. })),
            "setup: provisioning a fresh named workspace must succeed, got {created:?}"
        );
        let head_before = head_sha(&clone_dir);

        let outcome = remove_worktree(
            &clone_dir,
            &source,
            RemovalPolicy::IsolatedClone,
            "tab-close",
        )
        .await;

        assert_eq!(
            outcome,
            RemoveOutcome::Kept(crate::event::KeptReason::IsolatedClone),
            "PRD fork#544 M6: a named isolated-clone workspace must always be reported Kept on \
             tab close — never removed via that path — regardless of dirtiness, got {outcome:?}"
        );
        assert!(
            clone_dir.is_dir(),
            "the named workspace directory must still exist on disk after the tab-close \
             removal attempt"
        );
        assert_eq!(
            head_sha(&clone_dir),
            head_before,
            "the workspace's checked-out branch must be untouched by the tab-close removal \
             attempt"
        );
    }

    /// Scenario: PRD fork#544 M7 (Design step 7 / Decisions table row "What
    /// happens to other live workspaces, and to this one, when a merge
    /// lands?"). The workspace whose OWN branch was just confirmed merged
    /// has a completely clean tree, and its checked-out branch's HEAD is now
    /// fully contained in `origin/main` because a real merge landed it there
    /// (simulated by fetching the feature commit into a real, independent
    /// "origin" repository and fast-forwarding its own main onto it — the
    /// same real-git-state discipline `006`-`017` already use). Calling the
    /// new `sync_merged_workspace_to_main` function must auto-switch: check
    /// out `main` and fast-forward it to match `origin/main` exactly,
    /// reporting `SwitchedToMain`. `sync_merged_workspace_to_main` does not
    /// exist yet, so — mirroring `workspace_014`-`017`'s own precedent (M5
    /// had no existing production entry point either) — this is a
    /// compile-time RED (`cannot find function`/`cannot find type`), not a
    /// runtime one.
    #[spec("orchestration/workspace/020")]
    #[test]
    fn workspace_020_post_merge_switch_to_main_succeeds_on_clean_tree() {
        let ws = tempfile::tempdir().unwrap();

        // "origin" -- a real, independently-fetchable local repository, not
        // just a URL string (unlike `provision_isolated_clone_sync_sets_
        // origin_and_branch_when_source_has_origin`'s fake `.invalid` URL):
        // this test must genuinely fetch a real advance.
        let origin_repo = ws.path().join("origin");
        seed_source_repo(&origin_repo, "seed\n");

        // The just-merged workspace: a real clone of origin, checked out on
        // its own feature branch with one real commit -- the PR's own
        // content.
        let clone_dir = ws.path().join("workspace-020");
        git(
            ws.path(),
            &[
                "clone",
                "--quiet",
                origin_repo.to_str().unwrap(),
                clone_dir.to_str().unwrap(),
            ],
        );
        git(&clone_dir, &["checkout", "--quiet", "-b", "feat-020"]);
        git(&clone_dir, &["config", "user.email", "test@example.com"]);
        git(&clone_dir, &["config", "user.name", "Test"]);
        git(&clone_dir, &["config", "commit.gpgsign", "false"]);
        std::fs::write(clone_dir.join("feature.txt"), "the PR's own content\n").unwrap();
        git(&clone_dir, &["add", "feature.txt"]);
        git(&clone_dir, &["commit", "--quiet", "-m", "feat-020 work"]);
        let feature_head = head_sha(&clone_dir);

        // Simulate the merge landing on origin's own main: fetch the
        // feature branch's commit into `origin_repo` directly (they share
        // history via the clone) and fast-forward origin's main onto it --
        // exactly what a real GitHub merge does to `origin/main` from this
        // workspace's point of view. (A `git push` from the clone straight
        // into `origin_repo`'s checked-out branch is deliberately avoided --
        // that fails by default against a non-bare repo's current branch.)
        git(
            &origin_repo,
            &[
                "fetch",
                "--quiet",
                clone_dir.to_str().unwrap(),
                "feat-020:refs/heads/feat-020",
            ],
        );
        git(&origin_repo, &["merge", "--quiet", "--ff-only", "feat-020"]);
        let merged_main_head = head_sha(&origin_repo);
        assert_eq!(
            merged_main_head, feature_head,
            "setup: origin's main must now BE the feature commit (a real fast-forward merge), \
             not merely contain it"
        );

        let result = sync_merged_workspace_to_main(&clone_dir, "main");

        assert!(
            matches!(result, Ok(PostMergeSyncOutcome::SwitchedToMain)),
            "PRD fork#544 M7: a clean workspace whose HEAD is now fully contained in the \
             just-advanced origin/main must auto-switch, got {result:?}"
        );
        assert_eq!(
            git_output(&clone_dir, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "main",
            "a successful switch must leave the workspace checked out on main, not the old \
             feature branch"
        );
        assert_eq!(
            head_sha(&clone_dir),
            merged_main_head,
            "a successful switch must fast-forward local main to match origin/main exactly"
        );
        assert!(
            clone_dir.join("feature.txt").exists(),
            "the merged content must be present in the working tree after switching"
        );
    }

    /// Scenario: PRD fork#544 M7's Risks section — "the single biggest
    /// silent-data-loss risk in the whole PRD". The just-merged workspace
    /// carries a genuine UNCOMMITTED change the merge never captured (the
    /// tree is dirty). Calling `sync_merged_workspace_to_main` must refuse
    /// the auto-switch entirely — leave the checked-out branch, its HEAD,
    /// and the uncommitted edit itself completely untouched — and report
    /// `LeftUntouched` carrying a non-empty note, never silently discard the
    /// edit by switching/checking-out over it.
    #[spec("orchestration/workspace/021")]
    #[test]
    fn workspace_021_post_merge_switch_refused_when_uncommitted_change_present() {
        let ws = tempfile::tempdir().unwrap();

        let origin_repo = ws.path().join("origin");
        seed_source_repo(&origin_repo, "seed\n");

        let clone_dir = ws.path().join("workspace-021");
        git(
            ws.path(),
            &[
                "clone",
                "--quiet",
                origin_repo.to_str().unwrap(),
                clone_dir.to_str().unwrap(),
            ],
        );
        git(&clone_dir, &["checkout", "--quiet", "-b", "feat-021"]);
        git(&clone_dir, &["config", "user.email", "test@example.com"]);
        git(&clone_dir, &["config", "user.name", "Test"]);
        git(&clone_dir, &["config", "commit.gpgsign", "false"]);
        std::fs::write(clone_dir.join("feature.txt"), "the PR's own content\n").unwrap();
        git(&clone_dir, &["add", "feature.txt"]);
        git(&clone_dir, &["commit", "--quiet", "-m", "feat-021 work"]);
        let feature_head = head_sha(&clone_dir);

        git(
            &origin_repo,
            &[
                "fetch",
                "--quiet",
                clone_dir.to_str().unwrap(),
                "feat-021:refs/heads/feat-021",
            ],
        );
        git(&origin_repo, &["merge", "--quiet", "--ff-only", "feat-021"]);

        // Genuinely unprotected work: a real uncommitted edit the merge
        // never saw.
        std::fs::write(
            clone_dir.join("feature.txt"),
            "an uncommitted edit the merge never saw\n",
        )
        .unwrap();
        let branch_before = git_output(&clone_dir, &["rev-parse", "--abbrev-ref", "HEAD"]);

        let result = sync_merged_workspace_to_main(&clone_dir, "main");

        match &result {
            Ok(PostMergeSyncOutcome::LeftUntouched { reason }) => {
                assert!(
                    !reason.is_empty(),
                    "a refusal must surface a non-empty note the caller can show the user"
                );
            }
            other => panic!(
                "PRD fork#544 M7 -- the single biggest silent-data-loss risk in the whole PRD: \
                 an uncommitted change the merge never captured must refuse the auto-switch and \
                 surface a note, never silently switch/discard it, got {other:?}"
            ),
        }

        assert_eq!(
            git_output(&clone_dir, &["rev-parse", "--abbrev-ref", "HEAD"]),
            branch_before,
            "a refused switch must leave the checked-out branch completely untouched"
        );
        assert_eq!(
            head_sha(&clone_dir),
            feature_head,
            "a refused switch must leave the workspace's committed HEAD completely untouched"
        );
        assert_eq!(
            std::fs::read_to_string(clone_dir.join("feature.txt")).unwrap(),
            "an uncommitted edit the merge never saw\n",
            "a refused switch must leave the uncommitted edit itself completely untouched -- \
             this is the exact content the PRD's Risks section calls out as unprotected work"
        );
    }

    /// Scenario: PRD fork#544 M7's Risks section, the OTHER half of "carries
    /// nothing beyond what was merged" — a real LOCAL COMMIT made after the
    /// content the merge captured (e.g. the orchestration kept working in
    /// this workspace before the merge phase ran), with the tree otherwise
    /// fully clean. Isolates this signal from `workspace_021`'s
    /// uncommitted-change signal: a clean-tree-only check would wrongly
    /// treat this workspace as safe to switch and silently strand (or
    /// worse, discard by hard-resetting to origin/main) the extra commit.
    /// Calling `sync_merged_workspace_to_main` must refuse here too.
    #[spec("orchestration/workspace/022")]
    #[test]
    fn workspace_022_post_merge_switch_refused_when_local_commit_beyond_merge() {
        let ws = tempfile::tempdir().unwrap();

        let origin_repo = ws.path().join("origin");
        seed_source_repo(&origin_repo, "seed\n");

        let clone_dir = ws.path().join("workspace-022");
        git(
            ws.path(),
            &[
                "clone",
                "--quiet",
                origin_repo.to_str().unwrap(),
                clone_dir.to_str().unwrap(),
            ],
        );
        git(&clone_dir, &["checkout", "--quiet", "-b", "feat-022"]);
        git(&clone_dir, &["config", "user.email", "test@example.com"]);
        git(&clone_dir, &["config", "user.name", "Test"]);
        git(&clone_dir, &["config", "commit.gpgsign", "false"]);
        std::fs::write(clone_dir.join("feature.txt"), "the PR's own content\n").unwrap();
        git(&clone_dir, &["add", "feature.txt"]);
        git(&clone_dir, &["commit", "--quiet", "-m", "feat-022 work"]);

        git(
            &origin_repo,
            &[
                "fetch",
                "--quiet",
                clone_dir.to_str().unwrap(),
                "feat-022:refs/heads/feat-022",
            ],
        );
        git(&origin_repo, &["merge", "--quiet", "--ff-only", "feat-022"]);

        // A real local commit made AFTER the content the merge captured.
        // The tree is fully clean (this commit IS committed), so an
        // uncommitted-changes check alone would wrongly pass this case.
        std::fs::write(clone_dir.join("extra.txt"), "local work after the merge\n").unwrap();
        git(&clone_dir, &["add", "extra.txt"]);
        git(
            &clone_dir,
            &["commit", "--quiet", "-m", "local work after the merge"],
        );
        let head_with_extra_commit = head_sha(&clone_dir);
        let branch_before = git_output(&clone_dir, &["rev-parse", "--abbrev-ref", "HEAD"]);

        assert_eq!(
            git_output(&clone_dir, &["status", "--porcelain"]),
            "",
            "setup: the tree must be genuinely clean -- this test isolates the EXTRA-COMMIT \
             signal from the uncommitted-change signal workspace_021 already covers"
        );

        let result = sync_merged_workspace_to_main(&clone_dir, "main");

        match &result {
            Ok(PostMergeSyncOutcome::LeftUntouched { reason }) => {
                assert!(
                    !reason.is_empty(),
                    "a refusal must surface a non-empty note the caller can show the user"
                );
            }
            other => panic!(
                "PRD fork#544 M7: a local commit not yet on origin/main -- committed after the \
                 merge's own content -- must also refuse the auto-switch, even though the tree \
                 is fully clean; a clean-tree-only check would silently strand or discard this \
                 commit, got {other:?}"
            ),
        }

        assert_eq!(
            git_output(&clone_dir, &["rev-parse", "--abbrev-ref", "HEAD"]),
            branch_before,
            "a refused switch must leave the checked-out branch completely untouched"
        );
        assert_eq!(
            head_sha(&clone_dir),
            head_with_extra_commit,
            "a refused switch must never discard or strand the extra local commit"
        );
    }

    /// Scenario: PRD fork#544 M7's Decisions table row, the "other live
    /// workspaces" half — on an UNRELATED merge (this workspace's own
    /// branch did not just merge), only a proactive, read-only `git fetch`
    /// runs: the `origin/main` remote-tracking ref updates, but the
    /// checked-out local branch and working tree are completely untouched.
    /// Simulates a genuine unrelated merge by fast-forwarding `origin`'s own
    /// main from a second, independent clone, then calls the new
    /// `fetch_other_live_workspace` function against a THIRD, unrelated
    /// clone standing in for another live orchestration's own workspace.
    #[spec("orchestration/workspace/023")]
    #[test]
    fn workspace_023_other_live_workspace_gets_read_only_fetch_only() {
        let ws = tempfile::tempdir().unwrap();

        let origin_repo = ws.path().join("origin");
        seed_source_repo(&origin_repo, "seed\n");

        // A SEPARATE, unrelated live workspace -- an ordinary clone/checkout
        // standing in for another orchestration's own workspace that is NOT
        // the one whose branch just merged.
        let other_clone = ws.path().join("workspace-023-other");
        git(
            ws.path(),
            &[
                "clone",
                "--quiet",
                origin_repo.to_str().unwrap(),
                other_clone.to_str().unwrap(),
            ],
        );
        git(
            &other_clone,
            &["checkout", "--quiet", "-b", "unrelated-feature"],
        );
        let branch_before = git_output(&other_clone, &["rev-parse", "--abbrev-ref", "HEAD"]);
        let head_before = head_sha(&other_clone);
        let origin_tracking_ref_before = git_output(&other_clone, &["rev-parse", "origin/main"]);

        // The UNRELATED merge: origin's own main genuinely advances while
        // `other_clone` was untouched -- exactly the Decisions table's "on
        // an unrelated merge" scenario. Landed the same way `020`-`022`
        // simulate a merge: fetch a third clone's commit into `origin_repo`
        // directly and fast-forward main onto it (never a `git push` into
        // origin's own checked-out branch).
        let other_committer = ws.path().join("other-committer");
        git(
            ws.path(),
            &[
                "clone",
                "--quiet",
                origin_repo.to_str().unwrap(),
                other_committer.to_str().unwrap(),
            ],
        );
        git(
            &other_committer,
            &["config", "user.email", "test@example.com"],
        );
        git(&other_committer, &["config", "user.name", "Test"]);
        git(&other_committer, &["config", "commit.gpgsign", "false"]);
        std::fs::write(
            other_committer.join("unrelated.txt"),
            "someone else's merged PR\n",
        )
        .unwrap();
        git(&other_committer, &["add", "unrelated.txt"]);
        git(
            &other_committer,
            &["commit", "--quiet", "-m", "unrelated merge"],
        );
        git(
            &origin_repo,
            &[
                "fetch",
                "--quiet",
                other_committer.to_str().unwrap(),
                "main:refs/heads/incoming-main",
            ],
        );
        git(
            &origin_repo,
            &["merge", "--quiet", "--ff-only", "incoming-main"],
        );
        let advanced_origin_main = head_sha(&origin_repo);
        assert_ne!(
            advanced_origin_main, origin_tracking_ref_before,
            "setup: origin's main must have genuinely advanced past what other_clone's stale \
             origin/main remote-tracking ref still shows"
        );

        let result = fetch_other_live_workspace(&other_clone);

        assert!(
            result.is_ok(),
            "PRD fork#544 M7: a proactive read-only fetch for another live workspace on an \
             unrelated merge must succeed, got {result:?}"
        );
        assert_eq!(
            git_output(&other_clone, &["rev-parse", "--abbrev-ref", "HEAD"]),
            branch_before,
            "a proactive fetch must never touch the checked-out local branch"
        );
        assert_eq!(
            head_sha(&other_clone),
            head_before,
            "a proactive fetch must never touch the working tree/local HEAD"
        );
        assert!(
            !other_clone.join("unrelated.txt").exists(),
            "a proactive fetch must never merge/checkout the advanced content into the working \
             tree"
        );
        assert_eq!(
            git_output(&other_clone, &["rev-parse", "origin/main"]),
            advanced_origin_main,
            "the read-only fetch must bring the origin/main remote-tracking ref itself up to \
             date -- this is the one thing it IS supposed to do"
        );
    }

    /// Scenario: PRD fork#544 M9's own coverage list, "creation-time
    /// freshness against origin" — the Decisions table's "Should a fresh
    /// clone start from up-to-date main? ... needs verification ... whether
    /// today's creation path already fetches from origin at clone time or
    /// inherits the root checkout's own staleness." `source_dir` stands in
    /// for the user's already-open root checkout: a real clone of a
    /// separately-advanceable "true origin" repository, whose OWN knowledge
    /// of `origin/main` goes stale the moment `origin_repo` advances again
    /// behind `source_dir`'s back — exactly what happens when a teammate
    /// pushes to `main` while the user's checkout sits unfetched. Asserts
    /// the freshly created workspace's own `origin/main` remote-tracking
    /// ref reflects that TRUE, current advance — not merely a copy of
    /// `source_dir`'s own stale knowledge, and not merely the clone-time
    /// snapshot of `source_dir`'s local `main` branch (which a plain `git
    /// clone` alone would produce) — proving creation genuinely fetches
    /// from the real origin URL rather than inheriting whatever staleness
    /// the root checkout happened to be carrying. Uses a REAL,
    /// independently-fetchable local repository as `origin`, the same
    /// technique `020`-`023` use, so this test genuinely proves a fetch
    /// happened rather than merely a URL string being copied (unlike
    /// `provision_isolated_clone_sync_sets_origin_and_branch_when_source_
    /// has_origin`'s unreachable `.invalid` URL, which no fetch could ever
    /// reach).
    #[spec("orchestration/workspace/024")]
    #[test]
    fn workspace_024_fresh_creation_fetches_true_origin_not_source_staleness() {
        let ws = tempfile::tempdir().unwrap();

        let origin_repo = ws.path().join("origin");
        seed_source_repo(&origin_repo, "seed\n");

        // `source_dir` stands in for the user's already-open root checkout:
        // a real clone of `origin_repo`, so it has a genuine `origin` URL
        // pointing at it. Its own `origin/main` remote-tracking ref is
        // frozen at clone time and never refreshed again below.
        let source_dir = ws.path().join("source");
        git(
            ws.path(),
            &[
                "clone",
                "--quiet",
                origin_repo.to_str().unwrap(),
                source_dir.to_str().unwrap(),
            ],
        );
        let stale_origin_main = git_output(&source_dir, &["rev-parse", "origin/main"]);

        // Advance the TRUE origin directly -- entirely behind `source_dir`'s
        // back, exactly like a teammate pushing to `main` while the user's
        // root checkout sits unfetched. `source_dir` never re-fetches below,
        // so its own `origin/main` ref stays at `stale_origin_main` for the
        // rest of this test.
        std::fs::write(origin_repo.join("advanced.txt"), "true origin moved on\n").unwrap();
        git(&origin_repo, &["add", "advanced.txt"]);
        git(
            &origin_repo,
            &["commit", "--quiet", "-m", "advance past source_dir"],
        );
        let advanced_origin_head = head_sha(&origin_repo);
        assert_ne!(
            advanced_origin_head, stale_origin_main,
            "setup: true origin must have genuinely advanced past what source_dir's own \
             origin/main remote-tracking ref still shows"
        );
        assert_eq!(
            git_output(&source_dir, &["rev-parse", "origin/main"]),
            stale_origin_main,
            "setup: source_dir must never itself re-fetch -- its origin/main ref must remain \
             frozen at the stale commit throughout"
        );

        let clone_dir = ws.path().join("workspace-024");
        let result =
            provision_isolated_clone_sync(&source_dir, &clone_dir, "my-feature-024", "tester");
        assert!(
            matches!(result, Ok(IsolatedCloneOutcome::Created { .. })),
            "isolated clone must succeed, got {result:?}"
        );

        assert_eq!(
            git_output(&clone_dir, &["remote", "get-url", "origin"]),
            origin_repo.to_str().unwrap(),
            "the new workspace's origin must be the TRUE origin URL, read from source_dir's own \
             origin remote"
        );
        assert_eq!(
            git_output(&clone_dir, &["rev-parse", "origin/main"]),
            advanced_origin_head,
            "PRD fork#544 M2 Decisions table: a freshly created workspace must fetch from the \
             real origin at creation time, so its own origin/main remote-tracking ref reflects \
             origin's TRUE current state -- not merely inherit source_dir's own stale \
             origin/main knowledge (still at {stale_origin_main}), and not merely the \
             clone-time snapshot of source_dir's local main branch either"
        );
    }

    /// Scenario: PRD fork#544 review-findings fix round (reviewer B1 /
    /// auditor A1, both independently reproduced this). Creates a workspace
    /// exactly as `orchestration/workspace/001` does, then advances
    /// `source_dir` itself with a real new commit — the ordinary steady
    /// state of a root checkout that keeps being worked in after a
    /// workspace was cloned from it, e.g. a later `git pull` or a direct
    /// commit — so the workspace's own clone (a snapshot frozen at clone
    /// time) has never seen and does not carry that new commit in its
    /// object database. A second `provision_isolated_clone_sync` call then
    /// attempts to resume the same workspace. Pins the CORRECT behavior
    /// (resume must not be misreported as a wrong/foreign repository) —
    /// not merely today's wrong one — per this test's own doc comment on
    /// `isolated_clone_ancestry_matches_source` below for why today's check
    /// gets this backwards.
    #[spec("orchestration/workspace/025")]
    #[test]
    fn workspace_025_resume_survives_source_dir_advancing_past_the_clones_object_db() {
        let ws = tempfile::tempdir().unwrap();
        let state_dir = ws.path().join("state");
        let _state_guard =
            ScopedEnvVar::set("DOT_AGENT_DECK_STATE_DIR", state_dir.to_str().unwrap());

        let source = ws.path().join("source");
        seed_source_repo(&source, "seed\n");

        let clone_dir = ws.path().join("source-my-feature");
        let created = provision_isolated_clone_sync(&source, &clone_dir, "my-feature", "tester");
        assert!(
            matches!(created, Ok(IsolatedCloneOutcome::Created { .. })),
            "setup: the initial clone from source must succeed, got {created:?}"
        );
        let clone_head_before = head_sha(&clone_dir);

        // The ordinary steady state PRD fork#544 exists to support: the
        // root checkout (source_dir) keeps advancing after the workspace
        // was created next to it. A real new commit made directly in
        // source_dir is a commit the clone's own object database has never
        // fetched or received -- it did not exist yet at clone time, and
        // nothing has re-fetched it into the clone since.
        std::fs::write(source.join("advanced.txt"), "source moved on after clone\n").unwrap();
        git(&source, &["add", "advanced.txt"]);
        git(
            &source,
            &["commit", "--quiet", "-m", "source advances past the clone"],
        );
        let source_head_after = head_sha(&source);
        assert_ne!(
            source_head_after, clone_head_before,
            "setup: source_dir must have genuinely advanced past the commit the clone was made \
             from"
        );

        let result = provision_isolated_clone_sync(&source, &clone_dir, "my-feature", "tester");

        assert!(
            matches!(result, Ok(IsolatedCloneOutcome::Resumed { .. })),
            "reviewer N4 (round-3 tightening) / B1 / auditor A1 (both independently reproduced \
             this): `isolated_clone_ancestry_matches_source` runs `git merge-base --is-ancestor \
             <source_dir's CURRENT HEAD> HEAD` INSIDE the clone -- this asks whether \
             source_dir's HEAD AT RESUME TIME is contained in the clone's own history, which is \
             true only in the instant right after cloning (when a plain `git clone` hardlinks \
             source_dir's object store). The moment source_dir advances -- an ordinary `git \
             pull` or a new commit in the root checkout, the everyday steady state this PRD \
             exists to support, not an edge case -- source_dir's new HEAD is a commit the \
             clone's object database has never seen, so this probe fails and a perfectly \
             healthy, genuinely-derived, resumable workspace is wrongly refused as \
             AncestryMismatch. `resume_existing_isolated_clone`'s caller then tells the user to \
             'remove it manually' -- pointed squarely at destroying the exact uncommitted work \
             this PRD exists to protect. A bare `!matches!(.., AncestryMismatch)` would also \
             pass on Stranger/Unhealthy/Err, which is not what this test claims to pin -- both \
             calls here share creator \"tester\", so the resume genuinely succeeds and this \
             asserts the actual intended outcome. Got {result:?}"
        );
        assert_eq!(
            head_sha(&clone_dir),
            clone_head_before,
            "sanity: whatever the outcome (resumed or refused), an attempt at resuming must \
             never itself mutate the workspace's checked-out HEAD"
        );
    }

    /// Scenario: PRD fork#544 review-findings fix round (reviewer B2's own
    /// sibling issue, verified in the same finding). `'my feature'`,
    /// `'feat:544'`, `'wip~1'` and `'cache*'` all survive
    /// `sanitize_workspace_segment` completely UNCHANGED -- it only
    /// replaces path separators and strips NUL/`".."`/a leading `-`/`.`,
    /// none of which any of these four trigger -- yet every one of them is
    /// rejected deep inside `git check-ref-format` the moment
    /// `provision_isolated_clone_sync` tries to `git checkout -b` it
    /// (verified empirically: `git branch -- 'my feature'` and the other
    /// three all fail with `fatal: '<name>' is not a valid branch name`).
    /// Directly exercises `provision_isolated_clone_sync` with each raw
    /// Name (skipping `sanitize_workspace_segment` itself, which is private
    /// to `src/ui.rs` and a no-op for every one of these four anyway).
    /// Pins that the checkout failure is a CLEAN, typed refusal -- never a
    /// silently-succeeding `Created`, and never a half-provisioned
    /// directory left wedged at the derived path for the next attempt at
    /// the same Name to trip over.
    #[spec("orchestration/workspace/027")]
    #[test]
    fn workspace_027_invalid_git_ref_name_rejected_cleanly_not_left_dangling() {
        let ws = tempfile::tempdir().unwrap();
        let state_dir = ws.path().join("state");
        let _state_guard =
            ScopedEnvVar::set("DOT_AGENT_DECK_STATE_DIR", state_dir.to_str().unwrap());

        let source = ws.path().join("source");
        seed_source_repo(&source, "seed\n");

        for invalid_name in ["my feature", "feat:544", "wip~1", "cache*"] {
            let safe_suffix: String = invalid_name
                .chars()
                .map(|c| if c.is_alphanumeric() { c } else { '_' })
                .collect();
            let clone_dir = ws.path().join(format!("workspace-027-{safe_suffix}"));

            let result = provision_isolated_clone_sync(&source, &clone_dir, invalid_name, "tester");

            assert!(
                !matches!(result, Ok(IsolatedCloneOutcome::Created { .. })),
                "an invalid git ref name must never report Created, got {result:?} for Name \
                 {invalid_name:?}"
            );
            assert!(
                !clone_dir.exists(),
                "reviewer B2 sibling issue: an invalid git ref name's checkout failure must \
                 leave NO half-provisioned directory behind at {} for Name {invalid_name:?}, so \
                 a later attempt at the same Name is not wedged by this attempt's own \
                 leftovers, got {result:?}",
                clone_dir.display()
            );
        }
    }

    /// Scenario: PRD fork#544 review-findings fix round (auditor A5),
    /// RE-SCOPED in fix round 3 (auditor F3 / reviewer S4). The original
    /// form of this test pinned auditor A5's fix -- a missing `--`
    /// end-of-options separator on the checkout call let an option-shaped
    /// `default_branch` (`--detach`) silently detach HEAD instead of
    /// landing on the named branch. Round 3 goes further: rather than only
    /// making the checkout/switch call SAFE for an adversarial name, it
    /// validates `default_branch` via `git check-ref-format --branch`
    /// before touching the workspace at all, and refuses outright -- the
    /// production code runs `git check-ref-format --branch --detach` (no
    /// `--` separator: `check-ref-format` does not accept one at all, and
    /// passing one there exits 129 as a usage error, not a ref-name
    /// rejection -- measured directly), which genuinely rejects `--detach`
    /// with exit 128, so this function no longer needs to be safe AGAINST
    /// that name; it never manufactures a real local branch literally named
    /// `--detach` via `update-ref` in the first place (auditor F3: exactly
    /// that unvalidated `update-ref` was the other place `--detach` could
    /// still land as a real ref, unrelated to A5's checkout/switch fix).
    /// `git branch` (and therefore `git fetch`, which mirrors the source's
    /// own branches) still refuses to ever create a real ref component
    /// starting with '-', so this test still plants the adversarial-looking
    /// remote-tracking ref directly with `git update-ref` to reproduce the
    /// same starting shape the original test did -- not because the
    /// validation needs it (it runs before this ref is ever consulted), but
    /// so this stays the same fixture rather than a weaker one.
    ///
    /// Does not assert: the checkout/switch call's own `--`
    /// end-of-options separator (A5's original finding) -- the up-front
    /// `check-ref-format` validation above makes that code path
    /// unreachable for an adversarial `default_branch` like `--detach`, so
    /// nothing in this test's assertions exercises it any longer.
    #[spec("orchestration/workspace/028")]
    #[test]
    fn workspace_028_adversarial_branch_name_refused_before_touching_workspace() {
        let ws = tempfile::tempdir().unwrap();

        let origin_repo = ws.path().join("origin");
        seed_source_repo(&origin_repo, "seed\n");

        let clone_dir = ws.path().join("workspace-028");
        git(
            ws.path(),
            &[
                "clone",
                "--quiet",
                origin_repo.to_str().unwrap(),
                clone_dir.to_str().unwrap(),
            ],
        );

        // Plant `refs/remotes/origin/--detach` directly -- `git branch` (and
        // therefore `git fetch`, which mirrors the source's own branches)
        // refuses to ever create a real ref component starting with '-',
        // so `update-ref` is the only way to construct this fixture at all.
        // Points at the clone's own current HEAD, so (if validation did not
        // refuse first) the ancestry precondition would trivially hold too.
        let head = head_sha(&clone_dir);
        git(
            &clone_dir,
            &["update-ref", "refs/remotes/origin/--detach", &head],
        );
        let starting_branch = git_output(&clone_dir, &["rev-parse", "--abbrev-ref", "HEAD"]);

        let result = sync_merged_workspace_to_main(&clone_dir, "--detach");

        assert!(
            matches!(result, Err(ref msg) if msg.contains("--detach")),
            "reviewer S4 / auditor F3: a `default_branch` git itself refuses to CREATE as a \
             real branch (here '--detach', which `git check-ref-format --branch` itself \
             rejects) must be refused up front as a clean `Err` naming the offending value, \
             never proceed to manufacture an illegal ref via `update-ref` -- got {result:?}"
        );
        assert_eq!(
            git_output(&clone_dir, &["rev-parse", "--abbrev-ref", "HEAD"]),
            starting_branch,
            "an up-front refusal must leave the workspace completely untouched -- still on \
             whatever branch it started on, not detached and not switched onto anything"
        );
        assert!(
            !clone_dir.join(".git/refs/heads/--detach").exists(),
            "an up-front refusal must never reach the `update-ref` call that would otherwise \
             create a real local branch literally named '--detach'"
        );
    }

    /// Scenario: PRD fork#544 review-findings fix round (auditor A3).
    /// `provision_isolated_clone_sync` writes the M4b provenance artifact
    /// (`write_isolated_clone_provenance`, outside `clone_dir` under
    /// `state_dir()`) BEFORE the branch checkout. When the checkout fails
    /// (here, forced with an invalid git ref Name, the same fixture
    /// `workspace_027` above uses), `attempt_isolated_clone_cleanup` removes
    /// `clone_dir` itself but the cleanup function never touches the
    /// provenance artifact, which lives in a completely separate location.
    /// A later attempt at the same canonical path would then find
    /// "evidence" for a directory that no longer exists -- and M3's
    /// Stranger gate (`resume_existing_isolated_clone`) uses evidence
    /// PRESENCE as its entire test, so this stale evidence would wrongly
    /// vouch for whatever gets created there next.
    #[spec("orchestration/workspace/029")]
    #[test]
    fn workspace_029_checkout_failure_leaves_no_orphaned_provenance_artifact() {
        let ws = tempfile::tempdir().unwrap();
        let state_dir = ws.path().join("state");
        let _state_guard =
            ScopedEnvVar::set("DOT_AGENT_DECK_STATE_DIR", state_dir.to_str().unwrap());

        let source = ws.path().join("source");
        seed_source_repo(&source, "seed\n");

        let clone_dir = ws.path().join("workspace-029");
        // "my feature" survives `sanitize_workspace_segment` unchanged
        // (verified in `workspace_027` above) but is rejected deep inside
        // `git check-ref-format` at the checkout step -- forcing the exact
        // checkout-failure path this test needs, after the provenance
        // artifact has already been written.
        let result = provision_isolated_clone_sync(&source, &clone_dir, "my feature", "tester");

        assert!(
            !matches!(result, Ok(IsolatedCloneOutcome::Created { .. })),
            "setup: an invalid git ref Name must fail the checkout step, not succeed, got \
             {result:?}"
        );
        assert!(
            !clone_dir.exists(),
            "setup: checkout failure must clean up the half-provisioned clone directory itself \
             before this test's real assertion (the provenance artifact) is meaningful, got \
             {result:?}"
        );

        let provenance_path = isolated_clone_provenance_path(&clone_dir);
        assert!(
            !provenance_path.exists(),
            "auditor A3: the M4b provenance artifact must not survive a checkout failure that \
             already removed the clone directory itself -- a later attempt at this same \
             canonical path would find 'evidence' for a directory that no longer exists, and \
             M3's Stranger gate (resume_existing_isolated_clone) uses evidence PRESENCE as its \
             entire test, so this stale evidence would wrongly vouch for whatever gets created \
             there next. Found orphaned at {}",
            provenance_path.display()
        );
    }

    /// Scenario: PRD fork#544 review-findings fix round (reviewer S3).
    /// `sync_merged_workspace_to_main`'s preconditions are checked against
    /// `HEAD` (the workspace's own just-merged feature branch), but the
    /// MUTATION (`checkout` then `merge --ff-only`) targets `default_branch`
    /// instead. Sets up a workspace whose HEAD (a feature branch) passes
    /// both preconditions against the just-advanced `origin/main`, while
    /// LOCAL `main` has separately diverged from `origin/main` (an extra
    /// local commit on `main` that was never pushed) -- so the checkout
    /// onto local `main` succeeds, but the subsequent `merge --ff-only`
    /// fails, and the function returns `Err` having already left the
    /// workspace switched onto diverged `main` instead of back where it
    /// started. Pins the property that actually matters (per the task):
    /// no `Err` result may ever leave the workspace anywhere other than
    /// where it started -- not a specific mechanism (precondition
    /// detection vs. restore-on-failure).
    #[spec("orchestration/workspace/030")]
    #[test]
    fn workspace_030_failed_sync_never_leaves_workspace_switched_away_from_start() {
        let ws = tempfile::tempdir().unwrap();

        let origin_repo = ws.path().join("origin");
        seed_source_repo(&origin_repo, "seed\n");

        let clone_dir = ws.path().join("workspace-030");
        git(
            ws.path(),
            &[
                "clone",
                "--quiet",
                origin_repo.to_str().unwrap(),
                clone_dir.to_str().unwrap(),
            ],
        );
        git(&clone_dir, &["config", "user.email", "test@example.com"]);
        git(&clone_dir, &["config", "user.name", "Test"]);
        git(&clone_dir, &["config", "commit.gpgsign", "false"]);

        // Diverge LOCAL main from origin/main: an extra local commit on
        // main that is never pushed anywhere -- exactly the shape a real
        // orchestration's root-checkout main can pick up independently of
        // any one workspace (e.g. a maintainer commits directly to their
        // own local main).
        git(&clone_dir, &["checkout", "--quiet", "main"]);
        std::fs::write(clone_dir.join("local-only.txt"), "never pushed\n").unwrap();
        git(&clone_dir, &["add", "local-only.txt"]);
        git(
            &clone_dir,
            &["commit", "--quiet", "-m", "diverged local main"],
        );
        let diverged_local_main = head_sha(&clone_dir);

        // The feature branch this workspace is actually on: forked BEFORE
        // the divergence above (from the original seed commit), so it
        // shares no history with the diverged local main beyond that seed.
        git(
            &clone_dir,
            &["checkout", "--quiet", "-b", "feat-030", "main~1"],
        );
        std::fs::write(clone_dir.join("feature.txt"), "the PR's own content\n").unwrap();
        git(&clone_dir, &["add", "feature.txt"]);
        git(&clone_dir, &["commit", "--quiet", "-m", "feat-030 work"]);
        let feature_head = head_sha(&clone_dir);

        // Simulate the merge landing on origin's real main: fetch the
        // feature branch's own commit into origin_repo directly and
        // fast-forward origin's main onto it (the same technique
        // `workspace_020` uses) -- origin/main becomes the feature commit,
        // completely independent of clone_dir's own diverged LOCAL main.
        git(
            &origin_repo,
            &[
                "fetch",
                "--quiet",
                clone_dir.to_str().unwrap(),
                "feat-030:refs/heads/feat-030",
            ],
        );
        git(&origin_repo, &["merge", "--quiet", "--ff-only", "feat-030"]);
        let advanced_origin_main = head_sha(&origin_repo);
        assert_eq!(
            advanced_origin_main, feature_head,
            "setup: origin's main must now BE the feature commit (a real fast-forward merge)"
        );

        let branch_before = git_output(&clone_dir, &["rev-parse", "--abbrev-ref", "HEAD"]);
        let head_before = head_sha(&clone_dir);
        assert_eq!(
            branch_before, "feat-030",
            "setup: the workspace must still be on its own feature branch before calling sync"
        );
        assert_eq!(
            head_before, feature_head,
            "setup: the workspace's HEAD must still be the feature commit before calling sync"
        );

        let result = sync_merged_workspace_to_main(&clone_dir, "main");

        assert!(
            result.is_err(),
            "setup: local main ({diverged_local_main}) must have genuinely diverged from the \
             just-advanced origin/main ({advanced_origin_main}), so the ff-only merge fails and \
             this call returns Err, got {result:?}"
        );
        assert_eq!(
            git_output(&clone_dir, &["rev-parse", "--abbrev-ref", "HEAD"]),
            branch_before,
            "reviewer S3: preconditions are checked against HEAD (the feature branch, which \
             passes both), but the mutation targets `default_branch` (\"main\") instead -- the \
             checkout onto local main succeeds (no precondition guards it), then `merge \
             --ff-only` fails because local main independently diverged from origin/main. The \
             function returns Err having already left the workspace switched onto diverged main \
             instead of restoring it to the feature branch it started on. No Err result may \
             ever leave the workspace anywhere other than where it started"
        );
        assert_eq!(
            head_sha(&clone_dir),
            head_before,
            "reviewer S3: a failed sync must never leave the workspace's checked-out commit \
             anywhere other than where it started, got {result:?}"
        );
    }

    /// Scenario: PRD fork#544 review-findings fix round (reviewer S1).
    /// `isolated_clone_ancestry_matches_source`'s own code comment already
    /// distinguishes "exit 1 = proven mismatch" from "exit 128 =
    /// inconclusive" for the `merge-base` probe, but the `match` collapses
    /// both into the identical `false` -> `Rejected(AncestryMismatch)`,
    /// whose user-facing text tells the user to remove the directory
    /// manually. This forces the inconclusive branch specifically: after a
    /// real clone, `source_dir` advances with a genuine new commit (the
    /// same ordinary steady state `workspace_025` exercises), then the
    /// commit's own loose object file has its read permission stripped
    /// (`#[cfg(unix)]` — verified directly with the real `git` binary
    /// before writing this test that `git rev-parse HEAD` in `source_dir`,
    /// which only resolves a ref to a SHA, does not need to open that
    /// object and still succeeds, while the best-effort `git fetch -- \
    /// source_dir` DOES need to transmit that exact object and fails with
    /// a permission error, leaving the follow-up `merge-base` unable to
    /// find the object either — exit 128, never exit 1). Asserts the
    /// result is NOT the same rejection as a genuine wrong-repo/stranger
    /// mismatch, whatever coder ends up naming the distinguishable variant
    /// — pinning the property, not a specific enum variant.
    #[cfg(unix)]
    #[spec("orchestration/workspace/031")]
    #[test]
    fn workspace_031_ancestry_probe_distinguishes_unverifiable_fetch_failure_from_proven_mismatch()
    {
        let ws = tempfile::tempdir().unwrap();
        let state_dir = ws.path().join("state");
        let _state_guard =
            ScopedEnvVar::set("DOT_AGENT_DECK_STATE_DIR", state_dir.to_str().unwrap());

        let source = ws.path().join("source");
        seed_source_repo(&source, "seed\n");

        let clone_dir = ws.path().join("source-my-feature");
        let created = provision_isolated_clone_sync(&source, &clone_dir, "my-feature", "tester");
        assert!(
            matches!(created, Ok(IsolatedCloneOutcome::Created { .. })),
            "setup: the initial clone from source must succeed, got {created:?}"
        );
        let clone_head_before = head_sha(&clone_dir);

        // Source advances past the clone -- the ordinary steady state
        // `workspace_025` exercises -- so the resume's ancestry probe
        // genuinely needs its best-effort fetch to succeed to find shared
        // history.
        std::fs::write(source.join("advanced.txt"), "source moved on after clone\n").unwrap();
        git(&source, &["add", "advanced.txt"]);
        git(
            &source,
            &["commit", "--quiet", "-m", "source advances past the clone"],
        );
        let source_head = head_sha(&source);

        // Strip read permission from the new commit's own loose object
        // file -- forces the best-effort fetch (which must open and
        // transmit that exact object) to fail, while leaving `git
        // rev-parse HEAD` in `source_dir` (a plain ref resolution, run
        // earlier in the same probe) unaffected.
        let obj_path = source
            .join(".git/objects")
            .join(&source_head[0..2])
            .join(&source_head[2..]);
        assert!(
            obj_path.is_file(),
            "setup: the new commit's own loose object file must exist at {obj_path:?}"
        );
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&obj_path, std::fs::Permissions::from_mode(0o000)).unwrap();
        }

        let result = provision_isolated_clone_sync(&source, &clone_dir, "my-feature", "tester");

        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&obj_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }

        assert!(
            !matches!(
                result,
                Ok(IsolatedCloneOutcome::Rejected(
                    ResumeRejection::AncestryMismatch
                ))
            ),
            "reviewer S1: a best-effort fetch failing for a reason that has NOTHING to do with \
             the two repositories being unrelated (here, a permission error reading the \
             source's own new commit object) must not be reported the identical way as a \
             genuine wrong-repo/stranger mismatch. Today's code collapses every non-zero \
             merge-base exit code -- both the proven exit-1 mismatch AND an inconclusive exit-128 \
             ('object not found' because the fetch that would have supplied it just failed) -- \
             into the same `false` -> AncestryMismatch, whose describe() text tells the user to \
             'remove it manually' -- destructive advice for a workspace that is provably \
             genuinely derived from source, just transiently unable to prove it right now. Got \
             {result:?}"
        );
        assert_eq!(
            head_sha(&clone_dir),
            clone_head_before,
            "sanity: whatever the outcome, an attempt at resuming must never itself mutate the \
             workspace's checked-out HEAD"
        );
    }

    /// Scenario: PRD fork#544 review-findings fix round (reviewer S3).
    /// `sync_merged_workspace_to_main` creates a missing local
    /// `default_branch` via a bare `git update-ref -- refs/heads/<b> \
    /// origin/<b>` (Fix 3's own chosen route, specifically BECAUSE it
    /// sidesteps `check-ref-format`'s DWIM-creation validation -- see
    /// `workspace_028`). Unlike ordinary DWIM checkout (which sets
    /// `branch.<b>.remote`/`branch.<b>.merge` automatically when creating a
    /// new local branch from a remote-tracking ref), a bare `update-ref`
    /// writes only the ref itself -- no tracking configuration at all.
    /// Mirrors `workspace_020`'s three-repo real-fast-forward-merge
    /// fixture, but additionally deletes the local `main` branch the clone
    /// starts with (`git branch -D main`, while checked out on the feature
    /// branch) so `local_branch_exists` is genuinely false going into the
    /// sync call -- per the reviewer, the same precondition
    /// `remove_stray_default_branch` normally produces at creation time in
    /// real use, not an edge case.
    #[spec("orchestration/workspace/032")]
    #[test]
    fn workspace_032_update_ref_created_default_branch_still_tracks_its_remote() {
        let ws = tempfile::tempdir().unwrap();

        let origin_repo = ws.path().join("origin");
        seed_source_repo(&origin_repo, "seed\n");

        let clone_dir = ws.path().join("workspace-032");
        git(
            ws.path(),
            &[
                "clone",
                "--quiet",
                origin_repo.to_str().unwrap(),
                clone_dir.to_str().unwrap(),
            ],
        );
        git(&clone_dir, &["checkout", "--quiet", "-b", "feat-032"]);
        git(&clone_dir, &["config", "user.email", "test@example.com"]);
        git(&clone_dir, &["config", "user.name", "Test"]);
        git(&clone_dir, &["config", "commit.gpgsign", "false"]);
        std::fs::write(clone_dir.join("feature.txt"), "the PR's own content\n").unwrap();
        git(&clone_dir, &["add", "feature.txt"]);
        git(&clone_dir, &["commit", "--quiet", "-m", "feat-032 work"]);
        let feature_head = head_sha(&clone_dir);

        // Delete the local `main` the clone started with -- the same
        // `local_branch_exists == false` precondition `remove_stray_
        // default_branch` produces at creation time in real use (reviewer
        // S3: "this is NOT an edge case ... local_branch_exists is
        // normally false").
        git(&clone_dir, &["branch", "-D", "main"]);
        let local_branch_check = crate::git_env::fixture_git(&clone_dir, &clone_dir)
            .args(["rev-parse", "--verify", "--quiet", "refs/heads/main"])
            .output()
            .expect("git rev-parse --verify --quiet refs/heads/main must spawn");
        assert!(
            !local_branch_check.status.success(),
            "setup: local main must genuinely be gone before calling sync"
        );

        // Simulate the merge landing on origin's own main, exactly as
        // `workspace_020`.
        git(
            &origin_repo,
            &[
                "fetch",
                "--quiet",
                clone_dir.to_str().unwrap(),
                "feat-032:refs/heads/feat-032",
            ],
        );
        git(&origin_repo, &["merge", "--quiet", "--ff-only", "feat-032"]);
        let merged_main_head = head_sha(&origin_repo);
        assert_eq!(
            merged_main_head, feature_head,
            "setup: origin's main must now BE the feature commit (a real fast-forward merge)"
        );

        let result = sync_merged_workspace_to_main(&clone_dir, "main");

        assert!(
            matches!(result, Ok(PostMergeSyncOutcome::SwitchedToMain)),
            "setup: a clean workspace whose HEAD is now fully contained in the just-advanced \
             origin/main must auto-switch -- this test's real assertion only means something \
             once this reaches SwitchedToMain via the update-ref-created-branch path, got \
             {result:?}"
        );

        let upstream_check = crate::git_env::fixture_git(&clone_dir, &clone_dir)
            .args(["rev-parse", "--abbrev-ref", "@{upstream}"])
            .output()
            .expect("git rev-parse --abbrev-ref @{upstream} must spawn");
        assert!(
            upstream_check.status.success(),
            "reviewer S3: `sync_merged_workspace_to_main` creates the missing local `main` via \
             a bare `git update-ref -- refs/heads/main origin/main`, which -- unlike ordinary \
             DWIM checkout -- writes no tracking configuration at all, so `main` ends up with no \
             upstream (`git config --get branch.main.remote` unset). That directly contradicts \
             the PRD's own Decisions table, whose stated reason for the read-only fetch this \
             whole mechanism depends on is 'so ahead/behind info stays accurate' -- info that \
             requires an upstream to even compute. `git rev-parse --abbrev-ref @{{upstream}}` \
             for the branch this call just switched to and created must resolve cleanly, not \
             error. stderr: {}",
            String::from_utf8_lossy(&upstream_check.stderr)
        );
        // Final cleanup round (nit 3): tighten past "resolves cleanly" to the
        // exact value, so an upstream pointed at the wrong ref (e.g. a stray
        // `origin/feat-032` left over from setup) is also caught, not just an
        // outright-unset one.
        assert_eq!(
            String::from_utf8_lossy(&upstream_check.stdout).trim(),
            "origin/main",
            "the newly-created `main`'s upstream must be `origin/main` specifically, not merely \
             present"
        );
    }

    /// Scenario: PRD fork#325 fix round 2 (reviewer P2-E, P2-B), extended by
    /// round 3 (reviewer C1/C2, auditor C1/C2). Unit tests
    /// `handle_isolated_clone_add_error` directly rather than inducing a
    /// real `WORKTREE_GIT_TIMEOUT` (30s) wait through `provision_isolated_clone_sync`
    /// — the function is a pure classifier over an `AddError` plus whatever
    /// is actually on disk at `clone_dir`, so a real timeout is unnecessary
    /// to exercise every branch. Covers: (1) `TimedOut` with the directory
    /// present — the original timeout-cleanup path, unchanged; (2) `Failed`
    /// with the directory present, for a genuinely-ours failure ("Clone
    /// succeeded, but checkout failed") — cleaned up, and (round 3) reported
    /// as its own `Failed` variant carrying git's real error text, never
    /// folded into `TimedOut`'s generic wording; (3) either variant with the
    /// directory absent — the error propagates unchanged, since there is
    /// nothing to clean up.
    #[test]
    fn handle_isolated_clone_add_error_covers_timed_out_failed_and_absent() {
        let ws = tempfile::tempdir().unwrap();

        // Case 1: TimedOut, directory present (with a `.git` marker so
        // P3-E's cleanup guard does not skip it) -> cleaned up, TimedOut.
        let timed_out_dir = ws.path().join("timed-out");
        std::fs::create_dir_all(timed_out_dir.join(".git")).unwrap();
        let result = handle_isolated_clone_add_error(
            AddError::TimedOut("simulated timeout".to_string()),
            &timed_out_dir,
            "tester",
        );
        assert!(
            matches!(
                result,
                Ok(IsolatedCloneOutcome::TimedOut {
                    cleaned_up_by: Some(ref who)
                }) if who == "tester"
            ),
            "TimedOut + present must clean up and report TimedOut, got {result:?}"
        );
        assert!(
            !timed_out_dir.exists(),
            "the partially-populated directory must actually be removed"
        );

        // Case 2 (P2-B, refined by round 3 C2): Failed, directory present,
        // ours (not the "destination already exists" shape) -> cleaned up,
        // and reported as `Failed` carrying git's real error text verbatim
        // — never folded into `TimedOut`'s generic "timed out" wording, and
        // never `AlreadyClaimed`.
        let failed_dir = ws.path().join("failed-present");
        std::fs::create_dir_all(failed_dir.join(".git")).unwrap();
        let result = handle_isolated_clone_add_error(
            AddError::Failed("simulated: Clone succeeded, but checkout failed".to_string()),
            &failed_dir,
            "tester",
        );
        assert!(
            matches!(
                result,
                Ok(IsolatedCloneOutcome::Failed {
                    ref error,
                    cleaned_up_by: Some(ref who)
                }) if who == "tester"
                    && error == "simulated: Clone succeeded, but checkout failed"
            ),
            "Failed + present + ours must clean up and report Failed with the real error text, \
             never TimedOut's generic wording or AlreadyClaimed, got {result:?}"
        );
        assert!(
            !failed_dir.exists(),
            "the partially-populated directory must actually be removed"
        );

        // Case 3: either variant, directory absent -> the original git
        // error propagates unchanged (nothing to clean up, nothing to
        // wedge).
        let absent_dir = ws.path().join("never-existed");
        let result = handle_isolated_clone_add_error(
            AddError::Failed("genuine git failure".to_string()),
            &absent_dir,
            "tester",
        );
        match result {
            Err(ref e) if e == "genuine git failure" => {}
            other => panic!(
                "an absent directory means a genuine failure, not ours to clean up — the \
                 original error must propagate, got {other:?}"
            ),
        }
    }

    /// Scenario: PRD fork#325 fix round 3 (reviewer C1 / auditor C1) — the
    /// destructive-behavior regression this round exists to fix. A `git
    /// clone` failure whose error text says the destination path already
    /// existed (git created NOTHING) must NEVER be cleaned up, even though
    /// the directory is present: that shape is exactly what a human's manual
    /// `git worktree add` racing into the same destination path produces
    /// (CLAUDE.md rule 1 / `/worktree-prd`'s own convention), and
    /// `remove_dir_all`ing it would delete their real worktree. Simulates
    /// that race directly — fail with git's actual "destination path
    /// already exists" wording against a directory carrying `.git` as a
    /// PLAIN FILE (fix round 4, auditor D5: `git worktree add`'s real
    /// on-disk shape, and the exact reason `attempt_isolated_clone_cleanup`'s
    /// P3-E `.git`-presence guard does NOT catch this case — a directory
    /// engineered so P3-E alone can't save it, so the assertions below
    /// actually exercise `clone_destination_predates_attempt` rather than
    /// being made to pass by the unrelated guard) — and assert the
    /// directory survives untouched and the outcome is the non-destructive
    /// `AlreadyClaimed`, round 1's original behavior for this shape.
    #[test]
    fn handle_isolated_clone_add_error_never_deletes_a_preexisting_destination() {
        let ws = tempfile::tempdir().unwrap();
        let preexisting_dir = ws.path().join("someone-elses-worktree");
        std::fs::create_dir_all(&preexisting_dir).unwrap();
        // `git worktree add` writes `.git` as a plain FILE (`gitdir: …`),
        // not a directory — without this, P3-E's `.git`-presence guard in
        // `attempt_isolated_clone_cleanup` would preserve the directory on
        // its own even if `clone_destination_predates_attempt` regressed,
        // making the two assertions below inert (auditor D5).
        std::fs::write(
            preexisting_dir.join(".git"),
            format!(
                "gitdir: {}/.git/worktrees/someone-elses-worktree\n",
                ws.path().display()
            ),
        )
        .unwrap();
        let sentinel = preexisting_dir.join("sentinel.txt");
        std::fs::write(&sentinel, b"do not delete me").unwrap();

        let result = handle_isolated_clone_add_error(
            AddError::Failed(format!(
                "fatal: destination path '{}' already exists and is not an empty directory.",
                preexisting_dir.display()
            )),
            &preexisting_dir,
            "tester",
        );

        assert!(
            matches!(result, Ok(IsolatedCloneOutcome::AlreadyClaimed)),
            "a destination that predates this clone attempt must be reported as \
             AlreadyClaimed, never cleaned up, got {result:?}"
        );
        assert!(
            preexisting_dir.exists(),
            "the pre-existing directory must survive untouched"
        );
        assert_eq!(
            std::fs::read(&sentinel).unwrap(),
            b"do not delete me",
            "the pre-existing directory's contents must be untouched"
        );
    }

    /// Scenario: PRD fork#325 fix round 4 (auditor D1) — the locale
    /// regression this round exists to fix, reproducing the auditor's own
    /// empirical measurement against the installed git (2.55.0): under a
    /// non-English `LANG`, `git clone`'s stderr for a pre-existing,
    /// non-empty destination is entirely translated and contains neither
    /// English substring `clone_destination_predates_attempt` matches on.
    /// Runs a REAL `git clone` (not a hand-written error string, unlike the
    /// preceding test) against a genuinely non-empty destination with the
    /// TEST PROCESS's `LANG` set to German, and asserts the captured stderr
    /// still satisfies `clone_destination_predates_attempt` — proving
    /// `spawn_git_status_child`'s `.env("LC_ALL", "C")` override reaches the
    /// child and defeats the parent's locale, rather than merely asserting
    /// the predicate accepts hand-picked English text. Deliberately does
    /// NOT assert on the literal translated wording: a machine with no
    /// German git catalog installed would make git fall back to English
    /// regardless of this fix, which would make an assertion on the
    /// TRANSLATED text itself flaky across machines — the property this
    /// test actually needs (and gets, on any machine, catalog or not) is
    /// that the captured text is the ENGLISH wording, which is exactly what
    /// `LC_ALL=C` guarantees unconditionally.
    #[test]
    fn run_status_sync_clone_error_text_stays_english_under_non_english_locale() {
        fn git(dir: &Path, args: &[&str]) {
            let out = crate::git_env::fixture_git(dir, dir)
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
        let source = ws.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        git(&source, &["init", "--initial-branch=main", "--quiet"]);
        git(&source, &["config", "user.email", "test@example.com"]);
        git(&source, &["config", "user.name", "Test"]);
        git(&source, &["config", "commit.gpgsign", "false"]);
        std::fs::write(source.join("README.md"), "seed\n").unwrap();
        git(&source, &["add", "README.md"]);
        git(&source, &["commit", "--quiet", "-m", "seed"]);

        let dest = ws.path().join("dest");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("sentinel.txt"), b"not empty").unwrap();

        let _lang_guard = ScopedEnvVar::set("LANG", "de_DE.UTF-8");
        let result = run_status_sync(
            &[
                "clone".to_string(),
                "--".to_string(),
                source.to_string_lossy().into_owned(),
                dest.to_string_lossy().into_owned(),
            ],
            WORKTREE_GIT_TIMEOUT,
        );
        drop(_lang_guard);

        let text = match result {
            Err(AddError::Failed(e)) => e,
            other => panic!(
                "cloning into a non-empty destination must fail without timing out, got {other:?}"
            ),
        };
        assert!(
            clone_destination_predates_attempt(&text),
            "spawn_git_status_child's LC_ALL=C override must keep git's stderr in English \
             regardless of the parent process's LANG — auditor D1's non-English repro, got: \
             {text:?}"
        );
    }

    /// Scenario: PRD fork#325 fix round 4 (auditor D3) — the no-origin
    /// sentinel `ISOLATED_CLONE_NO_ORIGIN_SENTINEL` must fail `git push`
    /// unconditionally, not merely while nothing happens to exist at its
    /// old bare-relative-path spelling. Reproduces the auditor's own
    /// measurement: pointing `origin` at the sentinel and pushing must fail
    /// even when a real bare repo sits at the exact relative path the OLD
    /// (pre-fix) sentinel value would have resolved to — proving the fix is
    /// the sentinel's scheme, not an accident of what happened to be absent
    /// from the filesystem.
    #[test]
    fn isolated_clone_no_origin_sentinel_rejects_push_unconditionally() {
        fn git(dir: &Path, args: &[&str]) -> std::process::Output {
            crate::git_env::fixture_git(dir, dir)
                .args(args)
                .output()
                .unwrap_or_else(|e| panic!("git {args:?} failed to spawn: {e}"))
        }

        let ws = tempfile::tempdir().unwrap();
        let repo = ws.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let out = git(&repo, &["init", "--initial-branch=main", "--quiet"]);
        assert!(out.status.success());
        assert!(
            git(&repo, &["config", "user.email", "test@example.com"])
                .status
                .success()
        );
        assert!(
            git(&repo, &["config", "user.name", "Test"])
                .status
                .success()
        );
        assert!(
            git(&repo, &["config", "commit.gpgsign", "false"])
                .status
                .success()
        );
        std::fs::write(repo.join("README.md"), "seed\n").unwrap();
        assert!(git(&repo, &["add", "README.md"]).status.success());
        assert!(
            git(&repo, &["commit", "--quiet", "-m", "seed"])
                .status
                .success()
        );

        // Simulate the OLD sentinel value's exact hazard: a bare repo
        // sitting at the plain-relative-path spelling. Even with this
        // present, the current (scheme-qualified) sentinel must still fail.
        let old_style_relative_path = repo.join("dot-agent-deck-no-origin-configured");
        let out = git(
            &repo,
            &[
                "init",
                "--bare",
                "--quiet",
                old_style_relative_path.to_string_lossy().as_ref(),
            ],
        );
        assert!(out.status.success());

        assert!(
            git(
                &repo,
                &["remote", "add", "origin", ISOLATED_CLONE_NO_ORIGIN_SENTINEL],
            )
            .status
            .success()
        );

        let push = git(&repo, &["push", "origin", "HEAD"]);
        assert!(
            !push.status.success(),
            "git push against ISOLATED_CLONE_NO_ORIGIN_SENTINEL must fail unconditionally — \
             the old bare-relative-path spelling could silently succeed into a repo sitting at \
             that path (auditor D3); this one must not, even with such a repo present, stderr: \
             {}",
            String::from_utf8_lossy(&push.stderr)
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
            let out = crate::git_env::fixture_git(dir, dir)
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
            let out = crate::git_env::fixture_git(clone_dir, clone_dir)
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

    /// Scenario: fork #282 final-pass F1 (reviewer) / A1 (auditor), re-pointed
    /// at the upstream runner by the upstream sync. Pins that a caller that
    /// STOPS AWAITING a [`run_status`] call — the seam [`create_worktree_from`]
    /// itself relies on (an external `tokio::time::timeout` wrapping the call)
    /// — leaves no orphaned child behind, rather than driving the real
    /// [`WORKTREE_GIT_TIMEOUT`] (30s, not injectable), which would make this a
    /// 30s-plus test for no added coverage; `run_status_sync`'s own tests take
    /// the same shortcut by calling it with a short explicit `timeout`.
    ///
    /// This used to pin the fork's separate `run_status_killable`
    /// (`kill_on_drop(true)`), because the plain `run_status` then DETACHED a
    /// dropped child. Upstream's runner (issue #692, `output_within`) kills the
    /// command's whole process group when it is dropped or its bound expires,
    /// for every caller, so the killable variant and the test that pinned the
    /// detaching one are gone; the dropped-call behaviour this test asserts is
    /// now the shared one. Upstream's own
    /// `a_dropped_call_whose_leader_exited_first_takes_its_descendant_with_it`
    /// covers the descendant half; this keeps the fork's direct-child, plain
    /// script seam.
    ///
    /// A fake `git` is a shell script that touches a `started` marker
    /// immediately, sleeps a full second, then touches a `finished` marker.
    /// `run_status` is called wrapped in a 100ms `tokio::time::timeout`, so the
    /// sleep has not elapsed when the timeout fires. Dropping that timed-out
    /// future must kill the script, so `finished` must never appear even after
    /// waiting well past the script's own sleep duration.
    #[cfg(unix)]
    #[test]
    fn run_status_kills_the_child_instead_of_orphaning_it() {
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

            // Generous headroom well past the script's own 1s sleep: if the
            // dropped call did not kill the child, the script would finish and
            // write `finished` somewhere in this window.
            tokio::time::sleep(Duration::from_millis(1500)).await;
        });

        assert!(
            started_marker.exists(),
            "the script must have actually started running for this test to mean anything"
        );
        assert!(
            !finished_marker.exists(),
            "run_status should have killed the child when its caller stopped awaiting, but the \
             finished marker exists -- the child ran to completion in the background instead, \
             reproducing fork #282 final-pass F1/A1"
        );
    }

    // Retired at the upstream sync (#344, named): the fork's
    // `run_status_leaves_the_child_to_finish_detached_instead_of_killing_it`
    // pinned that plain `run_status` DETACHES a child its caller stopped
    // awaiting (fork #282 final-pass F1's correction: a killed `gh repo clone`
    // used to leave a partial `clone_dir` that wedged every later
    // `ensure_clone`). Upstream's #692 runner deliberately kills the whole
    // process group instead, and closes the same hazard at its source: a clone
    // goes to a staging directory and is moved into place only on success
    // (`clone_via_staging`, pinned by
    // `a_failed_clone_leaves_nothing_a_later_fire_would_mistake_for_a_clone`),
    // so a killed clone leaves nothing to mistake for a finished one. Same
    // guarantee, stronger mechanism (rule 24 case 3).

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

    /// Scenario: fork #282 final-pass F1 (reviewer) / A1 (auditor), extended
    /// by issue #325 for attribution. Pins [`attempt_worktree_cleanup_async`]
    /// directly — the cleanup path [`create_worktree`]'s `TimedOut` arm now
    /// runs instead of hardcoding `cleaned_up_by: None`. Creates a REAL
    /// worktree the way a killed `git worktree add` would leave one
    /// (registered via a genuine `git worktree add`, standing in for the
    /// half-finished state a kill leaves behind), then asserts cleanup both
    /// reports `Some(remover)` and actually makes the worktree disappear
    /// from `git worktree list` and the directory itself — matching the
    /// sync twin's "confirmed" contract exactly.
    #[cfg(unix)]
    #[test]
    fn attempt_worktree_cleanup_async_removes_a_registered_worktree_and_confirms_it() {
        fn git(dir: &Path, args: &[&str]) {
            let out = crate::git_env::fixture_git(dir, dir)
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
        let cleaned_up_by = rt.block_on(async {
            attempt_worktree_cleanup_async(&clone_dir, &worktree_dir, "test-remover").await
        });

        assert_eq!(
            cleaned_up_by,
            Some("test-remover".to_string()),
            "attempt_worktree_cleanup_async must report Some(remover) for a plain `git worktree \
             remove --force` against a directory it created"
        );
        assert!(
            !worktree_dir.exists(),
            "the worktree directory must actually be gone after a confirmed cleanup"
        );
        let list = crate::git_env::fixture_git(&clone_dir, &clone_dir)
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

    // --- Issue #791: warming a fresh worktree's devbox environment ---

    /// The program [`devbox_program`] hands back, when a test has set one.
    ///
    /// Exists for [`create_worktree_warms_the_environment_before_it_reports_created`]
    /// alone: the ordering it asserts is a property of `create_worktree`, and
    /// there is no way to observe it against the real `devbox` without
    /// materializing a nix environment inside a unit test. Under `cargo
    /// nextest` — the runner CLAUDE.md rule 5 names — every test is its own
    /// process, so this global is not shared with any other test.
    static DEVBOX_PROGRAM_OVERRIDE: Mutex<Option<String>> = Mutex::new(None);

    pub(super) fn devbox_program_override() -> Option<String> {
        DEVBOX_PROGRAM_OVERRIDE
            .lock()
            .expect("devbox program override lock")
            .clone()
    }

    /// Write an executable stand-in for `devbox` and return its path. `body` is
    /// the shell script after the shebang.
    #[cfg(unix)]
    fn stub_devbox(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("devbox-stub");
        crate::test_isolation::write_script(&path, format!("#!/bin/sh\n{body}\n"))
            .expect("write devbox stand-in");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod devbox stand-in");
        path
    }

    /// A worktree with no `devbox.json` must not spawn anything at all. Proven
    /// by pointing the warm-up at a program that does not exist: reaching the
    /// spawn would report `Unavailable`, so `NoManifest` is evidence the
    /// manifest probe short-circuited before any `exec`.
    #[tokio::test]
    async fn warm_up_starts_no_process_for_a_worktree_with_no_devbox_manifest() {
        let scratch = crate::test_temp::tempdir().expect("scratch tempdir");
        assert_eq!(
            warm_project_environment_with(
                scratch.path(),
                "/nonexistent/dot-agent-deck-devbox",
                Duration::from_secs(5),
            )
            .await,
            DevboxWarmup::NoManifest
        );
    }

    /// A machine with no `devbox` on `PATH` degrades to "no warm-up" rather
    /// than to a failed dispatch. devbox is optional for this project, so this
    /// is the ordinary case for most contributors, not an error path.
    #[tokio::test]
    async fn warm_up_degrades_when_devbox_is_not_installed() {
        let scratch = crate::test_temp::tempdir().expect("scratch tempdir");
        std::fs::write(scratch.path().join(DEVBOX_MANIFEST), "{}\n").expect("write devbox.json");
        let outcome = warm_project_environment_with(
            scratch.path(),
            "/nonexistent/dot-agent-deck-devbox",
            Duration::from_secs(5),
        )
        .await;
        assert!(
            matches!(outcome, DevboxWarmup::Unavailable(_)),
            "an absent devbox must be reported as unavailable, never propagated as a \
             dispatch failure; got {outcome:?}"
        );
    }

    /// A `devbox` that runs and fails — an `init_hook` needing a credential
    /// this host lacks, a manifest referencing a flake that will not build —
    /// is reported and stepped over. The roles then race exactly as they did
    /// before the warm-up existed, which is the pre-#791 status quo and not a
    /// new failure mode.
    #[cfg(unix)]
    #[tokio::test]
    async fn warm_up_degrades_when_devbox_exits_non_zero() {
        let scratch = crate::test_temp::tempdir().expect("scratch tempdir");
        std::fs::write(scratch.path().join(DEVBOX_MANIFEST), "{}\n").expect("write devbox.json");
        let stub = stub_devbox(scratch.path(), "echo 'nix build failed' >&2\nexit 3");
        let outcome = warm_project_environment_with(
            scratch.path(),
            &stub.to_string_lossy(),
            Duration::from_secs(30),
        )
        .await;
        match outcome {
            DevboxWarmup::Failed(stderr) => assert!(
                stderr.contains("nix build failed"),
                "the failure must carry devbox's own words, or the log says nothing \
                 actionable; got {stderr:?}"
            ),
            other => panic!("a non-zero devbox must report Failed, got {other:?}"),
        }
    }

    /// A `devbox` that never returns must cost the dispatch its bound and no
    /// more. Without this the warm-up would be a new way to wedge a dispatch
    /// forever — a strictly worse failure than the race it removes.
    #[cfg(unix)]
    #[tokio::test]
    async fn warm_up_is_bounded_when_devbox_hangs() {
        let scratch = crate::test_temp::tempdir().expect("scratch tempdir");
        std::fs::write(scratch.path().join(DEVBOX_MANIFEST), "{}\n").expect("write devbox.json");
        // Short, because the child now SURVIVES the bound by design (see the
        // test below) — a `sleep 600` here would leave one behind per run.
        let stub = stub_devbox(scratch.path(), "sleep 5");
        let started = std::time::Instant::now();
        let outcome = warm_project_environment_with(
            scratch.path(),
            &stub.to_string_lossy(),
            Duration::from_millis(300),
        )
        .await;
        assert_eq!(outcome, DevboxWarmup::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "the bound must actually bound: a 5s stand-in against a 300ms bound \
             returned after {:?}",
            started.elapsed()
        );
    }

    /// A `devbox` that outlives the bound must be LEFT RUNNING, not killed.
    ///
    /// This is a regression test for a claim that shipped into review as false.
    /// The code asserted the child survived because `kill_on_drop` is off, which
    /// is true and insufficient: dropping a timed-out `Command::output()` future
    /// closes our ends of the child's pipes, and devbox is a Go program that
    /// dies on `SIGPIPE` at its next write to stderr — which it does constantly,
    /// since materializing prints progress. Measured directly: against the
    /// dropped-future version this test fails, because the stand-in below never
    /// reaches its final line.
    ///
    /// It matters because killing it is the one outcome strictly worse than
    /// never having warmed at all: a half-written `.devbox` state file is the
    /// `compact json for hashing: unexpected end of JSON input` half of #791, so
    /// an abandoned warm-up would MANUFACTURE the second failure mode while
    /// trying to prevent the first.
    #[cfg(unix)]
    #[tokio::test]
    async fn warm_up_leaves_a_slow_devbox_running_rather_than_killing_it() {
        let scratch = crate::test_temp::tempdir().expect("scratch tempdir");
        std::fs::write(scratch.path().join(DEVBOX_MANIFEST), "{}\n").expect("write devbox.json");
        let finished = scratch.path().join("finished");
        // Chatty on stderr first — that is what makes a killed child observable
        // at all, since the death is a SIGPIPE on a write.
        let stub = stub_devbox(
            scratch.path(),
            &format!(
                "i=0\nwhile [ $i -lt 8 ]; do echo \"Info: installing $i\" >&2; sleep 0.25; \
                 i=$((i+1)); done\nprintf done > {}",
                finished.to_string_lossy()
            ),
        );
        let outcome = warm_project_environment_with(
            scratch.path(),
            &stub.to_string_lossy(),
            Duration::from_millis(300),
        )
        .await;
        assert_eq!(outcome, DevboxWarmup::TimedOut);
        assert!(
            !finished.is_file(),
            "the stand-in must still have been running when the bound expired, or \
             this proves nothing about what happens to a slow one"
        );

        // Wait for the stand-in to finish on its own. A killed one never writes
        // the marker, so the deadline is the red path; it is a diagnostic bound,
        // not a guess at how long ~2 s of stand-in takes on a loaded runner. A
        // fixed 5 s sleep here read a merely slow stand-in as a killed one.
        let _ = tokio::time::timeout(Duration::from_secs(30), async {
            while !finished.is_file() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        assert!(
            finished.is_file(),
            "a devbox that outlives the bound must be left running — killing it \
             abandons a half-materialized .devbox, which is the second failure \
             mode of #791 rather than a fix for the first"
        );
    }

    /// THE ordering assertion, and the reason the warm-up lives inside
    /// `create_worktree` rather than at its call sites.
    ///
    /// `create_worktree` is the only `git worktree add` in `src/`, so a
    /// worktree cannot come into existence anywhere else; every caller spawns
    /// only after awaiting it. This pins the remaining link: that the warm-up
    /// has already COMPLETED by the time `Created` is reported. The stand-in
    /// writes a marker file, and the marker is asserted the instant
    /// `create_worktree` returns — so a warm-up moved into the background, or
    /// after the return, or dropped altogether, fails here.
    ///
    /// It is a structural claim, deliberately: reproducing the race itself
    /// proves nothing either way (issue #791 measured 6/6 clean and later 1/6
    /// failing on the same code), so what is worth pinning is the ordering.
    #[cfg(unix)]
    #[tokio::test]
    async fn create_worktree_warms_the_environment_before_it_reports_created() {
        let scratch = crate::test_temp::tempdir().expect("scratch tempdir");
        let repo = scratch.path().join("repo");
        init_repo_with_commit(scratch.path(), &repo);
        // Committed, so the fresh worktree checks one out and the warm-up's
        // manifest probe sees it — exactly as a real dispatched worktree does.
        std::fs::write(repo.join(DEVBOX_MANIFEST), "{}\n").expect("write devbox.json");
        let git_add = crate::git_env::fixture_git(&repo, scratch.path())
            .args(["add", DEVBOX_MANIFEST])
            .output()
            .expect("git add devbox.json");
        assert!(git_add.status.success());
        let git_commit = crate::git_env::fixture_git(&repo, scratch.path())
            .args(["commit", "--quiet", "-m", "devbox"])
            .output()
            .expect("git commit devbox.json");
        assert!(git_commit.status.success());

        let evidence = scratch.path().join("warmed");
        let stub = stub_devbox(
            scratch.path(),
            &format!(
                "sleep 0.2\nprintf '%s' \"$PWD\" > {}\n",
                evidence.to_string_lossy()
            ),
        );
        *DEVBOX_PROGRAM_OVERRIDE
            .lock()
            .expect("devbox program override lock") = Some(stub.to_string_lossy().into_owned());

        let worktree_dir = scratch.path().join("repo-dispatch-warm");
        let outcome = create_worktree(
            &repo,
            &worktree_dir,
            "agent/dispatch-warm",
            false,
            Creator::dispatch("warm"),
        )
        .await;
        assert_eq!(
            outcome,
            Ok(WorktreeCreation::Created {
                marker_warning: None
            })
        );
        assert!(
            evidence.is_file(),
            "create_worktree must not report Created until the warm-up has finished — \
             a role spawned against a still-cold .devbox is precisely issue #791"
        );
        // CANONICALIZED on both sides, not compared as strings. On macOS the
        // temp root is `/var/...`, which is a symlink to `/private/var/...`, so
        // the shell's `$PWD` — read from `getcwd()`, already resolved — never
        // equals the path we passed in. Caught by `build-macos` on PR #881,
        // where this assertion was the only failure in 2402 tests. What the
        // assertion is about is directory IDENTITY, so resolving both is the
        // fix rather than a workaround.
        let reported = std::fs::read_to_string(&evidence).expect("read the warm-up's evidence");
        assert_eq!(
            std::fs::canonicalize(reported.trim()).expect("canonicalize the reported cwd"),
            std::fs::canonicalize(&worktree_dir).expect("canonicalize the worktree dir"),
            "the warm-up must run in the NEW worktree; warming any other directory \
             leaves that worktree's .devbox exactly as cold as it was"
        );
    }

    // --- issue #597: isolated-clone pin/unpin CLI wiring ---

    /// Scenario: `set_isolated_clone_pinned`'s temp-file suffix today is
    /// `std::process::id()` alone (fork issue #597 hazard 3) -- two calls
    /// from the SAME process collide on the identical temp path, which is
    /// exactly what a concurrent pin+unpin against the same isolated clone
    /// would do. This calls the not-yet-existing `pin_temp_disambiguator()`
    /// (the fix: reuses `agent_pty::mint_nonce_seq`'s per-process
    /// nonce+monotonic-sequence idiom, the same recipe `spawn.rs::next_pane_id`
    /// and `ui.rs::mint_orchestration_claim_token` already use for this exact
    /// shape) twice in a row and asserts the two returned strings differ --
    /// pinning that the disambiguator, once it exists, can never repeat
    /// within one process the way a bare PID does.
    #[spec("worktree/reclaim/095")]
    #[test]
    fn worktree_reclaim_095_pin_temp_disambiguator_differs_across_same_process_calls() {
        let first = pin_temp_disambiguator();
        let second = pin_temp_disambiguator();
        assert_ne!(
            first, second,
            "two calls to pin_temp_disambiguator() from the same process must never produce \
             the same string -- the PID-only tmp-path suffix set_isolated_clone_pinned used \
             before this helper existed collided on exactly this shape (fork issue #597): a \
             concurrent pin+unpin against the same clone within one process would write to an \
             identical temp path, publishing a torn write. Got the same value {first:?} twice."
        );
    }
}
