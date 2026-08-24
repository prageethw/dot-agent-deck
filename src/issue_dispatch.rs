//! Pure GitHub-layer helpers for the `issue_dispatch` scheduled-task type
//! (PRD #120). This module is the **foundation / pure-data layer only**: prompt
//! templating, per-issue path & branch derivation, `gh` argv construction, and
//! the idempotency decision. None of it spawns processes, touches the
//! filesystem, or wires the fire-time dispatch callback — those land in a later
//! task that composes #127's `spawn` primitive over the values these functions
//! produce.
//!
//! The config type that carries an issue-dispatch task's GitHub-specific knobs
//! lives next to the rest of the schedules schema as
//! [`crate::config::IssueDispatchConfig`]; the shared scheduler fields (`name`,
//! `cron`, `working_dir`, `prompt`, `enabled`) come from the enclosing
//! [`crate::config::ScheduledTask`]. The functions here take primitives rather
//! than the config struct so they stay decoupled and trivially unit-testable.

use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// U2 — prompt templating + default name
// ---------------------------------------------------------------------------

/// The single placeholder substituted in an issue-dispatch prompt template at
/// fire time. Documented in the PRD as user-owned: the user may change the
/// surrounding prompt freely, but this token is what each issue's number lands
/// in.
pub const ISSUE_NUMBER_PLACEHOLDER: &str = "{{issue_number}}";

/// The default-seed prompt template for a newly-created issue-dispatch task.
/// The user can replace it with anything (e.g. `/prd-full {{issue_number}}`);
/// the agent deduces the repo/URL from the worktree it runs in, so the issue
/// number alone is enough.
pub const DEFAULT_ISSUE_PROMPT_TEMPLATE: &str = "Work on issue {{issue_number}}";

/// Substitute every [`ISSUE_NUMBER_PLACEHOLDER`] occurrence in `template` with
/// `issue_number`. A template with no placeholder is returned unchanged (the
/// user opted out of interpolation) — the prompt is user-owned, so this never
/// errors or appends a context block.
pub fn substitute_issue_number(template: &str, issue_number: u64) -> String {
    template.replace(ISSUE_NUMBER_PLACEHOLDER, &issue_number.to_string())
}

/// The default-seed task name for an issue-dispatch task targeting `repo`:
/// `Issues <repo>`. The name is the reuse key (renames forbidden), so it is
/// resolved once at creation time when the repo is known.
pub fn default_issue_dispatch_name(repo: &str) -> String {
    format!("Issues {repo}")
}

// ---------------------------------------------------------------------------
// U3 — per-issue path & branch derivation
// ---------------------------------------------------------------------------

/// The deterministic filesystem layout + branch for one dispatched issue
/// (PRD #120 locked decisions). Pure data so the fire-time flow can derive it
/// without touching disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuePaths {
    /// The repo clone directory: `<working_dir>/<name>`.
    pub clone_dir: PathBuf,
    /// The per-issue worktree: `<clone_dir>/.worktrees/issue-<n>`.
    pub worktree_dir: PathBuf,
    /// The per-issue branch: `agent/issue-<n>`.
    pub branch: String,
}

/// The deterministic per-issue branch name: `agent/issue-<n>`. This is the
/// idempotency key the secondary PR check (U4 `pr_list_for_issue_argv`) matches
/// on, so it is exposed on its own.
pub fn issue_branch(issue_number: u64) -> String {
    format!("agent/issue-{issue_number}")
}

/// Derive the clone dir, per-issue worktree dir, and branch for `issue_number`,
/// given the task's `working_dir` (the workspace root) and `name` (the reuse
/// key). The clone-dir path component is a SANITIZED single segment of `name`
/// (see [`sanitize_clone_segment`]), so the human-friendly reuse key — including
/// the default seed `Issues <owner>/<repo>`, which carries a `/` — can never nest
/// or escape `<working_dir>/<segment>`. See [`IssuePaths`].
pub fn derive_issue_paths(working_dir: &Path, name: &str, issue_number: u64) -> IssuePaths {
    let clone_dir = working_dir.join(sanitize_clone_segment(name));
    let worktree_dir = clone_dir
        .join(".worktrees")
        .join(format!("issue-{issue_number}"));
    IssuePaths {
        clone_dir,
        worktree_dir,
        branch: issue_branch(issue_number),
    }
}

/// Reduce `name` to a SINGLE filesystem segment safe to join under a workspace
/// root: path separators (`/`, `\`) collapse to `-` and `..`/NUL are stripped, so
/// the result can never contain a separator or a parent reference and therefore
/// can never escape or nest outside `<working_dir>/<segment>` (L2 + S4).
///
/// `name` itself stays the human-friendly reuse key — only the *path component*
/// derived from it is sanitized. A name with no surviving alphanumeric character
/// (empty, or only separators/`..`/punctuation) falls back to a fixed `issues`
/// segment so a path is always derivable. An already-safe single segment (e.g.
/// `dispatch-task`) is returned unchanged.
pub fn sanitize_clone_segment(name: &str) -> String {
    let collapsed = name
        .replace(['/', '\\'], "-")
        .replace('\0', "")
        .replace("..", "");
    let trimmed = collapsed.trim();
    if trimmed.chars().any(char::is_alphanumeric) {
        trimmed.to_string()
    } else {
        "issues".to_string()
    }
}

// ---------------------------------------------------------------------------
// U4 — `gh` argv construction
// ---------------------------------------------------------------------------

/// Build the `gh issue list` argv — the arguments AFTER the `gh` program, i.e.
/// what the fire-time flow passes to `Command::new("gh").args(..)`.
///
/// Always lists OPEN issues as JSON carrying the issue `number` AND `labels`,
/// capped at `max_per_run`. `labels` rides along on this ALREADY-MADE call
/// (PRD #421 M1.2) rather than costing a separate `gh issue view` per
/// candidate — the read-back mechanism the PRD's catalog notes is
/// deliberately left to the coder, and this is the one that costs nothing
/// extra. Appends `--label <label>` when a label filter is set and
/// `--search <query>` when a raw query override is set; both are independent
/// and omitted when `None` (the default = all open issues up to the cap).
pub fn issue_list_argv(
    repo: &str,
    max_per_run: usize,
    label: Option<&str>,
    query: Option<&str>,
) -> Vec<String> {
    let mut argv = vec![
        "issue".to_string(),
        "list".to_string(),
        "--repo".to_string(),
        repo.to_string(),
        "--state".to_string(),
        "open".to_string(),
        "--json".to_string(),
        "number,labels".to_string(),
        "--limit".to_string(),
        max_per_run.to_string(),
    ];
    if let Some(label) = label {
        argv.push("--label".to_string());
        argv.push(label.to_string());
    }
    if let Some(query) = query {
        argv.push("--search".to_string());
        argv.push(query.to_string());
    }
    // M1: end-of-options marker. `gh issue list` takes no positionals, so this is
    // a belt-and-suspenders second layer behind the leading-`-` rejection in
    // [`validate_issue_dispatch_config`] — it guarantees no later argv element can
    // be reinterpreted as a flag.
    argv.push("--".to_string());
    argv
}

/// Build the `gh pr list` argv (arguments after `gh`) for the secondary
/// idempotency check: an OPEN PR whose HEAD branch is `agent/issue-<n>` means
/// the issue is already in flight. Keying on the deterministic head branch is
/// more reliable than parsing `Closes #n` from PR bodies (PRD #120).
pub fn pr_list_for_issue_argv(repo: &str, issue_number: u64) -> Vec<String> {
    vec![
        "pr".to_string(),
        "list".to_string(),
        "--repo".to_string(),
        repo.to_string(),
        "--state".to_string(),
        "open".to_string(),
        "--head".to_string(),
        issue_branch(issue_number),
        "--json".to_string(),
        "number".to_string(),
        // M1: end-of-options marker (see `issue_list_argv`).
        "--".to_string(),
    ]
}

// ---------------------------------------------------------------------------
// Worktree creation argv (fork #122) — shared by the async issue-dispatch
// creator (`issue_dispatch_run::create_worktree`) and the sync TUI creator
// (`issue_dispatch_run::create_worktree_sync`, fork #122's orchestration-tab
// `SpawnPane` path) so the two never drift on WHAT to run, only on how the
// process gets spawned (tokio vs std).
// ---------------------------------------------------------------------------

/// Argv for `git rev-parse --verify --quiet refs/heads/<branch>` — probes
/// whether `branch` already exists in `clone_dir`, so the caller can choose
/// between attaching an existing branch and creating one with `-b` before
/// `git worktree add`.
pub fn worktree_branch_probe_argv(clone_dir: &Path, branch: &str) -> Vec<String> {
    vec![
        "-C".to_string(),
        clone_dir.to_string_lossy().into_owned(),
        "rev-parse".to_string(),
        "--verify".to_string(),
        "--quiet".to_string(),
        format!("refs/heads/{branch}"),
    ]
}

/// Argv for `git rev-parse --verify --quiet refs/remotes/origin/<branch>` —
/// the second half of the isolated-clone branch probe (PRD fork#325 fix
/// round 2, reviewer P2-A). [`worktree_branch_probe_argv`] alone (probing
/// only `refs/heads/<branch>`) answers ABSENT for every branch a FRESH `git
/// clone` did not check out: a clone gives `refs/heads/` only the source's
/// checked-out HEAD branch, and every other branch the source had arrives
/// as a remote-tracking ref only. Probing this ref too lets
/// `provision_isolated_clone_sync` correctly ATTACH to a branch that
/// already exists on the source (git's own checkout DWIM resolves the
/// plain branch name to the remote-tracking tip and sets up tracking) —
/// instead of silently re-creating it at the clone's HEAD, discarding
/// whatever committed work the real branch carried, which the reviewer
/// reproduced end to end. Not used by the shared-checkout arm
/// (`create_worktree`/`create_worktree_sync`): that arm's `clone_dir` only
/// ever grows LOCAL branches, via its own earlier `git worktree add -b`
/// calls, so `worktree_branch_probe_argv` alone is already correct there.
pub fn isolated_clone_remote_branch_probe_argv(clone_dir: &Path, branch: &str) -> Vec<String> {
    vec![
        "-C".to_string(),
        clone_dir.to_string_lossy().into_owned(),
        "rev-parse".to_string(),
        "--verify".to_string(),
        "--quiet".to_string(),
        format!("refs/remotes/origin/{branch}"),
    ]
}

/// Argv for `git worktree add`: attaches `branch` when `branch_exists`,
/// otherwise creates it with `-b`. A branch left behind by an earlier run
/// whose worktree was removed (but branch preserved) must be attached, not
/// re-created — `git worktree add -b` on an existing branch name fails.
pub fn worktree_add_argv(
    clone_dir: &Path,
    worktree_dir: &Path,
    branch: &str,
    branch_exists: bool,
) -> Vec<String> {
    let mut argv = vec![
        "-C".to_string(),
        clone_dir.to_string_lossy().into_owned(),
        "worktree".to_string(),
        "add".to_string(),
        worktree_dir.to_string_lossy().into_owned(),
    ];
    if branch_exists {
        argv.push(branch.to_string());
    } else {
        argv.push("-b".to_string());
        argv.push(branch.to_string());
    }
    argv
}

/// Argv for `git worktree remove --force`: fork #122/#123 re-audit's (P2)
/// best-effort cleanup after a `git worktree add` is killed for exceeding
/// its timeout mid-checkout. `git worktree add` registers the worktree
/// before checkout/hooks finish, so a killed add leaves a half-created
/// directory (and usually its registration) behind; `--force` is required
/// because a plain `worktree remove` refuses a directory whose checkout
/// never completed cleanly. The `--` end-of-options separator before the
/// path (issue #325 auditor A7) matches `worktree_reclaim.rs`'s
/// `remove_worktree_dir` — not reachable today since every path here is
/// deck-derived from a slug, but it costs nothing and removes the
/// assumption that a future path can never begin with `-`.
pub fn worktree_remove_argv(clone_dir: &Path, worktree_dir: &Path) -> Vec<String> {
    vec![
        "-C".to_string(),
        clone_dir.to_string_lossy().into_owned(),
        "worktree".to_string(),
        "remove".to_string(),
        "--force".to_string(),
        "--".to_string(),
        worktree_dir.to_string_lossy().into_owned(),
    ]
}

/// Where [`isolated_clone_checkout_argv`]'s two-probe branch check
/// (`worktree_branch_probe_argv` / `isolated_clone_remote_branch_probe_argv`)
/// found `branch` in a freshly cloned isolated working tree (PRD fork#325 fix
/// round 3, reviewer P2-1 / auditor C3). A fresh `git clone` gives
/// `refs/heads/` only the source's checked-out HEAD branch, so any other
/// branch the source had arrives as a remote-tracking ref ONLY — the two
/// probes tell those cases apart, and `isolated_clone_checkout_argv` needs to
/// know which one matched because the checkout FORM differs between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchLocation {
    /// Absent from the clone entirely — create it fresh with `-b`.
    Absent,
    /// Already a local branch (`refs/heads/<branch>`) — attach with a plain
    /// `git checkout <branch>`, exactly like the shared-checkout arm.
    Local,
    /// Exists only as a remote-tracking ref (`refs/remotes/origin/<branch>`)
    /// — round 2 relied on git's checkout DWIM to resolve a plain `git
    /// checkout <branch>` against this ref. Round 3 (reviewer C3, auditor
    /// C3) found that dependency itself unsafe: DWIM is controlled by
    /// `checkout.guess` (a user's global git config with it set `false`
    /// makes checkout refuse outright with "pathspec … did not match"), and
    /// is shadowed entirely when the repo root holds a file sharing the
    /// branch's name ("could both be a local file and a tracking branch").
    /// Both destroy the clone via `handle_isolated_clone_add_error`'s
    /// cleanup and loop identically on retry, since the same DWIM-dependent
    /// form runs again. Use the explicit, DWIM-free
    /// `--track origin/<branch>` form instead.
    RemoteOnly,
}

/// Argv for `git checkout` inside a freshly cloned isolated working tree
/// (PRD fork#325 M3 fix round, reviewer P1-2): `provision_isolated_clone_sync`'s
/// plain `git clone` lands on the SOURCE's HEAD branch (typically `main`),
/// never the slug the user typed — this is the follow-up step that lands it
/// on `branch` instead, matching what `worktree_add_argv`'s attach-vs-create
/// split already does for the shared-checkout arm: attach `branch` when it
/// already exists locally, otherwise create it fresh with `-b` off the
/// clone's checked-out HEAD.
///
/// PRD fork#325 fix round 3 (reviewer P2-1 / auditor C3): a THIRD case,
/// [`BranchLocation::RemoteOnly`], was folded into "attach" alongside
/// `Local` in round 2, both producing the identical bare `git checkout
/// <branch>` and relying on git's checkout DWIM to make the remote-only case
/// work. See [`BranchLocation::RemoteOnly`]'s doc comment for the two real
/// failures that dependency caused. `RemoteOnly` now gets its own explicit,
/// DWIM-free form: `git checkout -b <branch> --track origin/<branch>`. This
/// still depends on `refs/remotes/origin/<branch>` being present at checkout
/// time — exactly as the DWIM form did — so `provision_isolated_clone_sync`'s
/// deferral of the no-origin-on-source removal until after a successful
/// checkout is unchanged and still required; only the checkout's own argv
/// form changed.
pub fn isolated_clone_checkout_argv(
    clone_dir: &Path,
    branch: &str,
    location: BranchLocation,
) -> Vec<String> {
    let mut argv = vec![
        "-C".to_string(),
        clone_dir.to_string_lossy().into_owned(),
        "checkout".to_string(),
    ];
    match location {
        BranchLocation::Local => {
            argv.push(branch.to_string());
        }
        BranchLocation::RemoteOnly => {
            argv.push("-b".to_string());
            argv.push(branch.to_string());
            argv.push("--track".to_string());
            argv.push(format!("origin/{branch}"));
        }
        BranchLocation::Absent => {
            argv.push("-b".to_string());
            argv.push(branch.to_string());
        }
    }
    argv
}

// ---------------------------------------------------------------------------
// M1 — validate the user-config GitHub knobs that flow into `gh`/`git` argv
// ---------------------------------------------------------------------------

/// Validate the GitHub-specific knobs of an `issue_dispatch` task before they
/// reach `gh`/`git`. `repo`/`label`/`query` come from hand-edited TOML and flow
/// into `gh repo clone <repo>` and `gh issue list --repo <repo> [--label …]
/// [--search …]`; even via `Command::args` (no shell) a value beginning with `-`
/// is parsed as a FLAG, and `repo` is an argument-injection vector (e.g. `ext::`,
/// `file://`, a local repo carrying hooks) run unattended by the daemon.
///
/// `repo` must be a strict GitHub `owner/name` slug — letters, digits, `.`, `_`,
/// `-` in each segment (`^[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$`) — AND must not start
/// with `-` (the regex's character class permits a leading `-`, which `gh` would
/// still read as a flag). `max_per_run` must be at least 1 — a `0` cap makes
/// `gh issue list --limit 0` enumerate (and dispatch) nothing on every fire.
/// `label`/`query` are rejected when they are empty (a meaningless filter that
/// `gh` would still pass through) or start with `-` (parsed as a `gh` flag).
pub fn validate_issue_dispatch_config(
    repo: &str,
    max_per_run: usize,
    label: Option<&str>,
    query: Option<&str>,
) -> Result<(), String> {
    if repo.starts_with('-') || !is_owner_name(repo) {
        return Err(format!(
            "issue_dispatch repo {repo:?} must be a GitHub `owner/name` slug \
             (letters, digits, '.', '_', '-' in each segment; no leading '-')"
        ));
    }
    if max_per_run < 1 {
        return Err(format!(
            "issue_dispatch max_per_run must be at least 1 (a cap of {max_per_run} \
             makes `gh issue list --limit 0` dispatch nothing every fire)"
        ));
    }
    if let Some(label) = label {
        if label.is_empty() {
            return Err("issue_dispatch label must not be empty".to_string());
        }
        if label.starts_with('-') {
            return Err(format!(
                "issue_dispatch label {label:?} must not start with '-' (it would be parsed as a `gh` flag)"
            ));
        }
    }
    if let Some(query) = query {
        if query.is_empty() {
            return Err("issue_dispatch query must not be empty".to_string());
        }
        if query.starts_with('-') {
            return Err(format!(
                "issue_dispatch query {query:?} must not start with '-' (it would be parsed as a `gh` flag)"
            ));
        }
    }
    Ok(())
}

/// Whether `repo` matches `^[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$` — exactly one `/`
/// with a non-empty allowed-char segment on each side.
fn is_owner_name(repo: &str) -> bool {
    let mut parts = repo.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(owner), Some(name), None) => {
            !owner.is_empty()
                && !name.is_empty()
                && owner.chars().all(is_repo_char)
                && name.chars().all(is_repo_char)
        }
        _ => false,
    }
}

fn is_repo_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')
}

// ---------------------------------------------------------------------------
// U5 — idempotency decision
// ---------------------------------------------------------------------------

/// Whether a candidate issue should be dispatched or skipped (PRD #120). No
/// separate state file — three signals, one of them an explicit claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchDecision {
    /// Provision the worktree and spawn an agent for this issue.
    Dispatch,
    /// Skip — the issue is already claimed.
    Skip,
}

/// Decide dispatch-vs-skip from the three idempotency signals: the per-issue
/// worktree already exists (primary), an open PR's HEAD branch is
/// `agent/issue-<n>` (secondary), or the `in-progress` label is present
/// (tertiary, PRD #421 M1.2) — honoured regardless of who applied it. Any one
/// being true means the issue is already claimed → [`DispatchDecision::Skip`];
/// only when all three are false do we dispatch.
pub fn dispatch_decision(
    worktree_exists: bool,
    open_pr_with_matching_head: bool,
    label_in_progress: bool,
) -> DispatchDecision {
    if worktree_exists || open_pr_with_matching_head || label_in_progress {
        DispatchDecision::Skip
    } else {
        DispatchDecision::Dispatch
    }
}

// ---------------------------------------------------------------------------
// PRD #421 M1.0/M1.1/M1.2 — claim label/comment argv + comment body
// ---------------------------------------------------------------------------

/// The label vocabulary's claim label, written on a successful dispatch (M1.0)
/// and read back as the third idempotency signal (M1.2).
pub const IN_PROGRESS_LABEL: &str = "in-progress";

/// [`IN_PROGRESS_LABEL`]'s colour/description (PRD #421 review B1): the label
/// is now ENSURED to exist before `claim_issue` adds it (see
/// `issue_dispatch_run::ensure_claim_label`), unconditionally — unlike the
/// opt-in triage vocabulary — because real `gh issue edit --add-label`
/// resolves the name to an ID client-side and hard-errors before any mutation
/// when the repo has never carried it, which otherwise makes the claim a
/// complete, silent no-op on any such repo (reviewer F1 / auditor F1).
pub const IN_PROGRESS_LABEL_COLOR: &str = "006b75";
pub const IN_PROGRESS_LABEL_DESCRIPTION: &str =
    "Claimed by an issue-dispatch task; do not dispatch again until this label is removed.";

/// Build the `gh issue edit --add-label` argv (arguments after `gh`) that
/// writes `label` onto `issue`. The issue number is a positional argument
/// placed AFTER the `--` end-of-options marker — unlike `issue_list_argv`'s
/// trailing `--` (which guards nothing because no positional follows it),
/// this one genuinely does its job: nothing between `gh` and `--` can ever be
/// reinterpreted as the positional, and nothing after `--` can be
/// reinterpreted as a flag (reviewer F8 / auditor F2).
pub fn issue_edit_add_label_argv(repo: &str, issue: u64, label: &str) -> Vec<String> {
    vec![
        "issue".to_string(),
        "edit".to_string(),
        "--repo".to_string(),
        repo.to_string(),
        "--add-label".to_string(),
        label.to_string(),
        "--".to_string(),
        issue.to_string(),
    ]
}

/// Build the `gh issue edit --remove-label` argv (arguments after `gh`) that
/// strips `label` from `issue` — issue #326's `issue release` uses this to
/// remove [`IN_PROGRESS_LABEL`], the exact mirror of
/// [`issue_edit_add_label_argv`] with the same `--` end-of-options placement
/// (see that function's doc comment for why the placement matters).
pub fn issue_edit_remove_label_argv(repo: &str, issue: u64, label: &str) -> Vec<String> {
    vec![
        "issue".to_string(),
        "edit".to_string(),
        "--repo".to_string(),
        repo.to_string(),
        "--remove-label".to_string(),
        label.to_string(),
        "--".to_string(),
        issue.to_string(),
    ]
}

/// Build the `gh issue comment` argv (arguments after `gh`) that posts `body`
/// as a new comment on `issue` — always APPENDED, never edited in place (PRD
/// #421 M1.1: the only path that re-runs the dispatch-success flow for the
/// same issue is a deliberate un-claim, usually a different claimant taking
/// over, so editing in place would overwrite the previous claimant's record).
/// The issue number sits after the `--` end-of-options marker — see
/// [`issue_edit_add_label_argv`]'s doc comment for why that placement matters.
pub fn issue_comment_argv(repo: &str, issue: u64, body: &str) -> Vec<String> {
    vec![
        "issue".to_string(),
        "comment".to_string(),
        "--repo".to_string(),
        repo.to_string(),
        "--body".to_string(),
        body.to_string(),
        "--".to_string(),
        issue.to_string(),
    ]
}

/// Build the `gh issue view` argv (arguments after `gh`) that reads back an
/// issue's comments and current assignees. Two callers: the M1.3 claimant
/// lookup, called only once the `in-progress` label is already known present
/// (from the `gh issue list` response `issue_list_argv` requests — see its
/// doc comment); and, since PRD fork#235 FINAL round 5, `claim_issue`'s
/// removal-target lookup (`current assignees − {claimant}`), called on
/// EVERY claim regardless of label state. The issue number sits after the
/// `--` end-of-options marker — see [`issue_edit_add_label_argv`]'s doc
/// comment for why that placement matters.
pub fn issue_view_comments_argv(repo: &str, issue: u64) -> Vec<String> {
    vec![
        "issue".to_string(),
        "view".to_string(),
        "--repo".to_string(),
        repo.to_string(),
        "--json".to_string(),
        "comments,assignees".to_string(),
        "--".to_string(),
        issue.to_string(),
    ]
}

/// The two forms of claimant identity (PRD fork#235 round 3). An issue's
/// claim is a LOCK, not merely a record, so identity must name an INSTANCE,
/// never a bare name — fork #201 records that orchestration-name uniqueness
/// is only advisory (two forms open at once are suggested the same name and
/// neither submit is refused — "this is the case #74 is actually about"),
/// and fork #222 adds truncation collisions plus the `unknown` sentinel.
/// Comparing bare names would make two DISTINCT holders compare EQUAL and
/// wave both through in exactly the scenario the lock exists for
/// (`issue/claim/007` pins this).
///
/// Round 3's anchor (CLAUDE.md rule 23, `prds/235-issue-claim-lock.md`'s
/// "Identity, round 2" section) is the worktree an agent is actually running
/// in: its absolute path plus its git branch — both of which CLAUDE.md rule
/// 1 already obliges the orchestrator to create and name, so this invents no
/// new mechanism. Two prior anchors were tried and rejected: the worktree
/// ownership marker (round 1 — almost never present, since rule 1's mandated
/// `git worktree add` flow writes none) and `DOT_AGENT_DECK_PANE_ID` (round 2
/// — a small daemon-scoped integer that recycles across a daemon restart).
///
/// Comparison is always on the WHOLE composed [`Display`](std::fmt::Display)
/// string, never a field in isolation, and — for [`Identity::Worktree`] — the
/// `label` field is NEVER part of it: it is decoration only (see the field's
/// own doc).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identity {
    /// An agent working a worktree — round 3's whole anchor. `path` and
    /// `branch` are the ENTIRE compared identity (see [`Identity`]'s own
    /// doc); `label`, when present, is a human-readable decoration (e.g. "the
    /// orchestration `<name>`") that is rendered into the claim comment for a
    /// reader's benefit but never compared. `None` for a CLI `issue claim`
    /// caller (`src/issue_claim.rs`): round 3 drops the worktree ownership
    /// marker read entirely, so the CLI has no name to decorate with, only
    /// the anchor itself — matching rule 23's own bare example, "the
    /// orchestration working `<path>` on branch `<branch>`". `Some(_)` for
    /// the async dispatch path (`issue_dispatch_run.rs`), which knows its
    /// bound `SpawnKind`'s own name.
    Worktree {
        path: PathBuf,
        branch: String,
        /// The claiming machine's own hostname (round-3 audit A1): without
        /// this, two decks on two DIFFERENT physical machines whose
        /// worktrees happen to share an absolute path (ordinary under
        /// Codespaces/devcontainers' `/workspaces/<repo>` convention) compare
        /// EQUAL and both take the idempotent-refresh row — #74 verbatim.
        /// Always populated fresh (via [`crate::issue_dispatch_run::local_hostname`])
        /// by every constructor below; a [`ParsedClaim`] reconstructed from a
        /// comment with no discoverable host clause (an older-shaped or
        /// hand-typed comment) simply omits it from the compared string,
        /// which can then never equal a freshly-resolved caller's identity —
        /// failing closed rather than assuming same-host.
        host: String,
        label: Option<String>,
    },
    /// A human claiming outside any worktree: `human:<login>@<host>`. `login`
    /// is a validated `gh` login (see [`validate_gh_login`]), not free text,
    /// so it needs no further sanitization here.
    Human { login: String, host: String },
}

/// Best-effort canonicalisation for an [`Identity::Worktree`]'s `path`
/// (round-3 audit R2/A6), applied HERE — the one place every constructor
/// below funnels through — so the CLI's physical `getcwd()`-derived path and
/// the dispatch path's lexical, possibly symlink-containing configured path
/// always converge on the same string without either call site needing to
/// remember to normalise itself. Falls back to the given path unchanged when
/// canonicalization fails (a synthetic/non-existent path in a unit test, or
/// a real filesystem error) — an identity is still buildable, it just can no
/// longer promise the normalized form.
fn canonicalize_identity_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

impl Identity {
    /// Build a bare [`Identity::Worktree`] with no decorative label — the CLI
    /// `issue claim` path (`src/issue_claim.rs`), which resolves purely from
    /// `git` (round 3 reads no marker, so it has no name to decorate with).
    pub fn worktree(path: &Path, branch: &str) -> Self {
        Identity::Worktree {
            path: canonicalize_identity_path(path),
            branch: branch.to_string(),
            host: crate::issue_dispatch_run::local_hostname(),
            label: None,
        }
    }

    /// Build an [`Identity::Worktree`] decorated as an orchestration. `name`
    /// is run through [`sanitize_claimant_name`] before being embedded in its
    /// own backtick-wrapped code span in the rendered label — it comes from
    /// the bound [`crate::spawn::SpawnKind::Orchestration`]'s own typed name,
    /// untrusted text landing in a public GitHub comment.
    pub fn orchestration(name: &str, worktree_path: &Path, branch: &str) -> Self {
        Identity::Worktree {
            path: canonicalize_identity_path(worktree_path),
            branch: branch.to_string(),
            host: crate::issue_dispatch_run::local_hostname(),
            label: Some(format!(
                "the orchestration `{}`",
                sanitize_claimant_name(name)
            )),
        }
    }

    /// Build an [`Identity::Worktree`] decorated as a single-agent
    /// issue-dispatch fire. `task` (`ScheduledTask.name`) is sanitized for
    /// the same reason as [`Identity::orchestration`]'s `name`.
    pub fn issue_dispatch(task: &str, issue: u64, worktree_path: &Path, branch: &str) -> Self {
        Identity::Worktree {
            path: canonicalize_identity_path(worktree_path),
            branch: branch.to_string(),
            host: crate::issue_dispatch_run::local_hostname(),
            label: Some(format!(
                "the issue-dispatch task `{}` (issue #{issue})",
                sanitize_claimant_name(task)
            )),
        }
    }

    /// Build an [`Identity::Human`]. `login` is assumed already validated by
    /// the caller via [`validate_gh_login`] — a `gh` login is not free text.
    pub fn human(login: &str, host: &str) -> Self {
        Identity::Human {
            login: login.to_string(),
            host: host.to_string(),
        }
    }
}

impl std::fmt::Display for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // The compared string names the anchor (path + branch) PLUS the
            // claiming host (round-3 audit A1) — `label` is decoration and
            // must never affect comparison
            // (`identity_comparison_is_on_the_whole_string_not_the_name_alone`
            // pins this: two callers sharing a `label` but rooted in
            // different worktrees must still compare unequal). `|` separates
            // the host from `path@branch` — distinct from the pre-existing
            // `@` separator so a host can never be misread as part of the
            // branch, and not a control character, so
            // [`sanitize_claimant_name`] never mangles it when this string
            // is later echoed into a "taking over from" tail.
            Identity::Worktree {
                path, branch, host, ..
            } => {
                write!(f, "worktree:{}@{branch}|{host}", path.display())
            }
            Identity::Human { login, host } => write!(f, "human:{login}@{host}"),
        }
    }
}

/// Accept only `^[A-Za-z0-9][A-Za-z0-9-]*$` (PRD fork#235 M1). A `gh` login
/// reaches both a public comment body and a `gh` argv (`--add-assignee`,
/// `--remove-assignee`), so unlike an orchestration/task name it is
/// validated rather than merely sanitized — GitHub logins are already
/// restricted to this shape, so a value failing this check is not a real
/// login at all.
pub fn validate_gh_login(login: &str) -> bool {
    let mut chars = login.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Build the `gh api user --jq .login` argv (arguments after `gh`) that
/// resolves the currently-authenticated `gh` login — "whoever `gh` is
/// authenticated as on this host" (PRD fork#235 M1/M2), the human who owns
/// the agent making the claim.
pub fn gh_current_login_argv() -> Vec<String> {
    vec![
        "api".to_string(),
        "user".to_string(),
        "--jq".to_string(),
        ".login".to_string(),
    ]
}

/// Build the `gh issue edit --add-assignee/--remove-assignee` argv
/// (arguments after `gh`) implementing replace-to-one assignment (PRD
/// fork#235 M2, round 5): `add` is the new assignee, `remove` the current
/// assignees being displaced — `current GitHub assignees − {add}` (empty for
/// the very first claim on an issue, or an idempotent refresh where `add` was
/// already the sole assignee). One `--remove-assignee` flag per entry, so a
/// hand-assigned issue carrying more than one prior assignee is fully
/// cleared, not just narrowed to one survivor. Mirrors
/// [`issue_edit_add_label_argv`]'s `--` end-of-options placement.
pub fn issue_edit_assignee_argv(
    repo: &str,
    issue: u64,
    add: Option<&str>,
    remove: &[String],
) -> Vec<String> {
    let mut argv = vec![
        "issue".to_string(),
        "edit".to_string(),
        "--repo".to_string(),
        repo.to_string(),
    ];
    if let Some(a) = add {
        argv.push("--add-assignee".to_string());
        argv.push(a.to_string());
    }
    for r in remove {
        argv.push("--remove-assignee".to_string());
        argv.push(r.clone());
    }
    argv.push("--".to_string());
    argv.push(issue.to_string());
    argv
}

/// The literal prefix [`claim_comment_body`] always renders — the terminal
/// character a claim comment can be recognized by (`.rfind` in
/// `issue_dispatch_run::parse_claim_comment` / [`parse_claim_state`]).
/// **Load-bearing**: a takeover comment MUST keep this exact prefix — wording
/// it e.g. `Taken over by …` would make it invisible to every reader that
/// looks for this string, leaving the system convinced the PREVIOUS holder
/// still holds the issue (a silent regression of the whole feature).
pub const CLAIM_COMMENT_PREFIX: &str = "Claimed by ";

/// The fields [`parse_claim_fields`] extracts from one already-located claim
/// comment (a comment whose body starts with [`CLAIM_COMMENT_PREFIX`]). Feeds
/// the M3 lock decision (`identity` compared against the caller's own,
/// `timestamp` rendered into a refusal). `login` is rendered for a human
/// reader only (PRD fork#235 FINAL round 5) — never read back into a write;
/// the assignee-replacement target comes from `gh issue view`'s own
/// `assignees` field instead, via [`parse_current_assignees`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedClaim {
    /// The composed identity string (an [`Identity`]'s `Display` output) —
    /// compared verbatim, never decomposed, per the whole-string comparison
    /// rule every `Identity` variant's doc already states. Reconstructed here
    /// from the parsed `path`+`branch` (worktree form) or `login`+`host`
    /// (human form) using the exact same shape [`Identity`]'s own `Display`
    /// impl produces, so a freshly-resolved caller identity and a
    /// previously-posted one compare equal whenever they name the same
    /// worktree/branch or the same human.
    pub identity: String,
    pub timestamp: String,
    /// The `@<login>` clause. `Some` for a worktree-form claim that resolved
    /// one, or unconditionally for a human-form claim (the login IS the
    /// identity there). `None` for an older-shaped or best-effort claim
    /// comment that carries no login clause (a fork#235 claim made when `gh
    /// api user` failed).
    pub login: Option<String>,
    /// The full, unparsed comment body — preserved for callers that render
    /// the whole thing (the PRD #421 skip-reason claimant text), so parsing
    /// out structured fields doesn't lose the ability to show the original.
    pub raw: String,
}

/// Parse the structured fields out of one already-located claim comment body
/// (PRD fork#235 M1, re-keyed for round 3's worktree-path-plus-branch
/// anchor). `raw` is assumed to already start with [`CLAIM_COMMENT_PREFIX`]
/// — callers locate it via `.rfind` on that prefix (see
/// `issue_dispatch_run::parse_claim_comment` / [`parse_claim_state`]) before
/// calling this. `None` if the body doesn't match either of the two shapes
/// [`claim_comment_body`] renders (a genuinely malformed/hand-edited comment,
/// or an older-round comment predating this format) — never partial-fills a
/// result an M3 caller could misread as a smaller, but still valid, claim.
///
/// Two shapes, disambiguated by which of two mutually-exclusive markers is
/// present (`" working from \`"` can never also match `" working \`"` — the
/// literal text between `working` and the backtick differs):
///   - Human: ``@<login> working from `<host>` at <ts>[, taking over from
///     <tail>].``
///   - Worktree: ``<decoration>working `<path>` on branch `<branch>`[ on host
///     <host>] at <ts>[, for @<login>][, taking over from <tail>].``
pub fn parse_claim_fields(raw: &str) -> Option<ParsedClaim> {
    let rest = raw.strip_prefix(CLAIM_COMMENT_PREFIX)?;
    let line = claim_line(rest);

    if let Some(after_at) = line.strip_prefix('@') {
        return parse_human_claim(after_at, raw);
    }
    parse_worktree_claim(line, raw)
}

/// Bound one claim's own text (round-3 audit: reviewer/auditor found the
/// SAME "scan the whole remaining body" shape independently in the
/// timestamp search AND the login search — `issue/claim/012`). A forged
/// SECOND `Claimed by` line injected across a raw newline must never be
/// reachable by any field's sub-search, so every sub-search below operates
/// on THIS bounded slice rather than the unbounded rest of the comment.
/// Bounded at whichever comes first: the next newline, or (defence in depth,
/// even absent a newline) the next literal [`CLAIM_COMMENT_PREFIX`]
/// occurrence.
fn claim_line(rest: &str) -> &str {
    let newline_at = rest.find('\n').unwrap_or(rest.len());
    let next_claim_at = rest.find(CLAIM_COMMENT_PREFIX).unwrap_or(rest.len());
    &rest[..newline_at.min(next_claim_at)]
}

/// Parse the human-form claim body (everything after the `Claimed by @`
/// already stripped): `<login> working from \`<host>\` at <ts>[, taking over
/// from <tail>].`.
fn parse_human_claim(after_at: &str, raw: &str) -> Option<ParsedClaim> {
    let marker = " working from `";
    let marker_idx = after_at.find(marker)?;
    let login = after_at[..marker_idx].to_string();
    // Round-4 audit, cause 1 (`issue/claim/025`): validate at the parser
    // boundary, same as `parse_worktree_claim`'s login clause — a
    // hand-typed or hostile human-shaped comment is not restricted to a
    // real login's shape either.
    if !validate_gh_login(&login) {
        return None;
    }
    let after_marker = &after_at[marker_idx + marker.len()..];
    let host_end = after_marker.find('`')?;
    let host = after_marker[..host_end].to_string();
    let after_host = &after_marker[host_end + 1..];
    let (timestamp, _) = extract_timestamp(after_host)?;
    Some(ParsedClaim {
        identity: format!("human:{login}@{host}"),
        timestamp,
        login: Some(login),
        raw: raw.to_string(),
    })
}

/// Parse the worktree-form claim body (everything after `Claimed by `, NOT
/// starting with `@`): `<decoration>working \`<path>\` on branch
/// \`<branch>\` at <ts>[, for @<login>][, taking over from <tail>].`.
/// `decoration` (e.g. `"the orchestration \`orch-A\` "`) is skipped over via
/// the `" working \`"` marker search rather than parsed — it is display-only
/// (see [`Identity::Worktree`]'s `label` field doc) and never feeds the
/// compared identity string.
fn parse_worktree_claim(rest: &str, raw: &str) -> Option<ParsedClaim> {
    let working_marker = " working `";
    let working_idx = rest.find(working_marker)?;
    let after_working = &rest[working_idx + working_marker.len()..];
    let path_end = after_working.find('`')?;
    let path = after_working[..path_end].to_string();
    let after_path = &after_working[path_end + 1..];

    let branch_marker = " on branch `";
    let branch_idx = after_path.find(branch_marker)?;
    let after_branch_marker = &after_path[branch_idx + branch_marker.len()..];
    let branch_end = after_branch_marker.find('`')?;
    let branch = after_branch_marker[..branch_end].to_string();
    let after_branch = &after_branch_marker[branch_end + 1..];

    // The host clause (round-3 audit A1) is OPTIONAL: present only in a
    // comment posted by this round's own writer (`claim_comment_body`),
    // never in an older-shaped or hand-typed comment. When absent, the
    // reconstructed identity carries NO host component at all — it can then
    // never compare equal to a freshly-resolved caller identity (which
    // always knows its own host), refusing rather than assuming same-host
    // (`issue/claim/016`). Not backtick-wrapped: the host is locally
    // resolved, never attacker-influenceable (this PRD's Threat model
    // excludes a hostile local process), so it needs no code-span escaping.
    let host_marker = " on host ";
    let (host_suffix, after_host_or_branch) = match after_branch.strip_prefix(host_marker) {
        Some(after_host) => {
            let host_end = after_host.find(" at ")?;
            (
                format!("|{}", &after_host[..host_end]),
                &after_host[host_end..],
            )
        }
        None => (String::new(), after_branch),
    };

    let (timestamp, after_timestamp) = extract_timestamp(after_host_or_branch)?;
    // Round-4 audit (`issue/claim/024`): bound the START of this search to
    // AFTER the timestamp clause's own span (`after_timestamp`, a suffix of
    // `rest` starting right where `extract_timestamp` stopped), not the
    // whole remaining line from the very start. `claim_line()` already
    // bounds the END; without this, an EARLIER field — the decorative
    // label, the path, or the branch, none of which are restricted from
    // containing the literal substring `, for @` — can shadow a genuine
    // trailing login clause, which can only ever appear after the
    // timestamp in a comment this format actually renders.
    let login = after_timestamp.find(", for @").and_then(|idx| {
        let after = &after_timestamp[idx + ", for @".len()..];
        let end = after.find(',').unwrap_or(after.len());
        let candidate = after[..end].trim_end_matches('.').to_string();
        // Round-4 audit, cause 1 (`issue/claim/025`): validate at the parser
        // boundary — `validate_gh_login`'s two pre-existing call sites both
        // validate the deck's own `gh api user` reply, never a login PARSED
        // out of a comment. `login` is rendered for a human reader only
        // (PRD fork#235 FINAL round 5 — no write path reads it back), but it
        // is still untrusted text reaching operator-facing output, so it is
        // dropped here rather than shown malformed.
        validate_gh_login(&candidate).then_some(candidate)
    });
    Some(ParsedClaim {
        identity: format!("worktree:{path}@{branch}{host_suffix}"),
        timestamp,
        login,
        raw: raw.to_string(),
    })
}

/// Extract the timestamp out of `after_marker`, which is assumed to start
/// with (optionally after other text) the literal `" at "` marker followed by
/// the RFC3339 timestamp, ending at the next `,` or the end of the string
/// (with a trailing `.` trimmed). Returns the timestamp alongside the
/// REMAINDER of `after_marker` starting at that same terminator — a suffix
/// of the caller's own string, not a copy — so a caller can bound any
/// further sub-search within the same claim line to start only after the
/// timestamp clause has consumed its own span (round-4 audit,
/// `issue/claim/024`: [`parse_worktree_claim`]'s login search used to scan
/// from the very start of the line, so an EARLIER field could shadow a
/// genuine trailing login clause — see that function's own doc).
fn extract_timestamp(after_marker: &str) -> Option<(String, &str)> {
    let ts_marker = " at ";
    let ts_start = after_marker.find(ts_marker)? + ts_marker.len();
    let after_ts = &after_marker[ts_start..];
    let ts_end = after_ts.find(',').unwrap_or(after_ts.len());
    let timestamp = after_ts[..ts_end].trim_end_matches('.').to_string();
    Some((timestamp, &after_ts[ts_end..]))
}

/// Build the `gh issue view --json labels,comments,assignees` argv (arguments
/// after `gh`) M3's `issue claim` reads to decide: whether `in-progress` is
/// present, who the newest claim comment names (display only, PRD fork#235
/// FINAL round 5), and the current GitHub assignees (the round-5 removal
/// target: `current assignees − {claimant}`). One call carries all three
/// signals the CLI write path needs.
pub fn issue_view_claim_state_argv(repo: &str, issue: u64) -> Vec<String> {
    vec![
        "issue".to_string(),
        "view".to_string(),
        "--repo".to_string(),
        repo.to_string(),
        "--json".to_string(),
        "labels,comments,assignees".to_string(),
        "--".to_string(),
        issue.to_string(),
    ]
}

/// Parse whatever [`parse_claim_fields`] extracts from `comment`'s `body` —
/// the shared step between [`parse_claim_state`] (the CLI path) and
/// `issue_dispatch_run::parse_claim_comment` (the async dispatch path) so
/// both read "what does the newest claim comment say?" the exact same way.
pub(crate) fn parsed_claim_from_comment_json(comment: &serde_json::Value) -> Option<ParsedClaim> {
    let body = comment.get("body").and_then(serde_json::Value::as_str)?;
    parse_claim_fields(body)
}

/// The current assignee logins out of a `gh issue view` document's own
/// `assignees` field — shared by [`parse_claim_state`] and
/// [`parse_current_assignees`], the PRD fork#235 FINAL round-5 removal
/// target's SOLE source (`current assignees − {claimant}`, never a claim
/// comment). Missing or malformed `assignees` degrades to empty, same
/// discipline as `label_present`/`held` above.
fn assignee_logins(value: &serde_json::Value) -> Vec<String> {
    value
        .get("assignees")
        .and_then(serde_json::Value::as_array)
        .map(|assignees| {
            assignees
                .iter()
                .filter_map(|a| a.get("login").and_then(serde_json::Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Parse a `gh issue view --json labels,comments,assignees` document into
/// (label present, newest claim, current assignees). PRD fork#235 M3/round 5:
/// the pure counterpart to [`issue_view_claim_state_argv`], kept separate
/// from the subprocess call so the JSON-shape logic is unit-testable.
pub fn parse_claim_state(json: &str) -> Result<(bool, Option<ParsedClaim>, Vec<String>), String> {
    let value: serde_json::Value = serde_json::from_str(json.trim())
        .map_err(|e| format!("failed to parse `gh issue view` JSON: {e}"))?;
    let label_present = value
        .get("labels")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|labels| {
            labels.iter().any(|l| {
                l.get("name").and_then(serde_json::Value::as_str) == Some(IN_PROGRESS_LABEL)
            })
        });
    let held = value
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
        });
    let assignees = assignee_logins(&value);
    Ok((label_present, held, assignees))
}

/// Parse a `gh issue view --json ...,assignees` document into the current
/// assignee logins alone — the dispatch path's own entry point onto
/// [`assignee_logins`], for `issue_dispatch_run::claim_issue`'s round-5
/// removal-target lookup, which (unlike the CLI path's [`parse_claim_state`])
/// has no use for `labels` or `comments` at all.
pub fn parse_current_assignees(json: &str) -> Result<Vec<String>, String> {
    let value: serde_json::Value = serde_json::from_str(json.trim())
        .map_err(|e| format!("failed to parse `gh issue view` JSON: {e}"))?;
    Ok(assignee_logins(&value))
}

/// Neutralise a claimant-supplied task name for safe interpolation into a
/// public GitHub comment (PRD #421 review C5 / auditor F3; PRD fork#235
/// round-3 review also applies this to a HELD claim's reconstructed
/// `identity` string before it is echoed back into a new comment's `taking
/// over from …` tail or a refusal message — see
/// `issue_claim::decide_claim`'s call site). `ScheduledTask.name`
/// is hand-edited config with no character restriction, and the un-escaped
/// backtick wrapper around it in `Claimant::describe` lets a name that itself
/// contains a backtick close the code span early — after which a crafted
/// `@`-mention notifies a real GitHub user, or an embedded newline forges a
/// second, fabricated `Claimed by …` line (undermining the newest-claimant
/// lookup this PRD relies on). Backticks are dropped rather than escaped — a
/// backslash-escaped backtick does not render as literal inside a CommonMark
/// code span — newlines/carriage returns collapse to a space, and every other
/// C0/DEL control character is dropped outright.
pub(crate) fn sanitize_claimant_name(name: &str) -> String {
    name.chars()
        .filter_map(|c| match c {
            '`' => None,
            '\n' | '\r' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect()
}

/// Render the "who + where + when" clause shared by [`claim_comment_body`]
/// and [`release_comment_body`] — the identical two-arm match SonarCloud
/// flagged as duplication between those two functions (issue #326 follow-up).
/// `prefix` is the caller's [`CLAIM_COMMENT_PREFIX`] or
/// [`RELEASE_COMMENT_PREFIX`].
///
/// Round-3 audit A3 (`issue/claim/020`): `path` and `branch` can both be
/// attacker-influenceable with NO forged comment involved at all — a
/// scheduled-task NAME reaches `path` via `sanitize_clone_segment`, which
/// strips only `/ \ \0 ..`, not backticks, and a raw git branch name is not
/// restricted from containing one either. Sanitize both, exactly like a
/// claimant NAME, before they go inside their own backtick-wrapped span —
/// otherwise an embedded backtick closes that span early and whatever
/// follows (an `@mention`, a forged `Claimed by`/`Released by` line) renders
/// as LIVE markdown. Sanitizing here (not in the stored `Identity`) leaves
/// the compared identity string untouched. The same reasoning applies
/// identically to the release side, since it shares this rendering.
fn render_identity_clause(prefix: &str, identity: &Identity, timestamp: &str) -> String {
    match identity {
        Identity::Worktree {
            path,
            branch,
            host,
            label,
        } => {
            let label = label.as_deref().unwrap_or("the orchestration");
            let path_str = sanitize_claimant_name(&path.display().to_string());
            let branch_str = sanitize_claimant_name(branch);
            format!(
                "{prefix}{label} working `{path_str}` on branch `{branch_str}` on host {host} \
                 at {timestamp}"
            )
        }
        Identity::Human { login, host } => {
            format!("{prefix}@{login} working from `{host}` at {timestamp}")
        }
    }
}

/// Render the claim-comment body posted on a claim (PRD #421 M1.1; PRD
/// fork#235 round 3 re-keys it onto the worktree-path-plus-branch anchor,
/// CLAUDE.md rule 23): who claimed it, when, for which human (`login`,
/// omitted for a worktree-form claim that resolved none; always present for
/// a human-form claim, since the login IS the identity there), and — on a
/// takeover — who it was taken over from. `dispatch/010` asserts the body
/// names both the claiming task (decoration) and the dispatched worktree's
/// path+branch (the compared anchor); `dispatch/021` asserts it names the
/// orchestration, not the scheduled task, as that decoration.
///
/// **The `Claimed by ` prefix is load-bearing** (see
/// [`CLAIM_COMMENT_PREFIX`]) and must survive every variant of this
/// rendering, including a takeover — provenance goes in the tail
/// (`, taking over from …`), never by changing the verb. The "who + where +
/// when" clause itself, including the round-3 audit A3 sanitization
/// reasoning, is shared with [`release_comment_body`] via
/// [`render_identity_clause`].
///
/// Mirrors [`parse_claim_fields`]'s two shapes exactly — a change to one
/// without the other breaks round-tripping.
pub fn claim_comment_body(
    identity: &Identity,
    timestamp: &str,
    login: Option<&str>,
    takeover_from: Option<&str>,
) -> String {
    let mut body = render_identity_clause(CLAIM_COMMENT_PREFIX, identity, timestamp);
    // The `for @<login>` clause is meaningful only for the worktree form —
    // a human-form claim already names the login as the identity itself
    // (rendered above), so repeating it would be redundant and would make
    // `parse_human_claim`'s single `@` after the prefix ambiguous.
    if matches!(identity, Identity::Worktree { .. })
        && let Some(login) = login
    {
        body.push_str(&format!(", for @{login}"));
    }
    if let Some(prev) = takeover_from {
        body.push_str(&format!(", taking over from `{prev}`"));
    }
    body.push('.');
    body
}

/// The literal prefix [`release_comment_body`] always renders — issue #326's
/// counterpart to [`CLAIM_COMMENT_PREFIX`], recording that a claim was
/// deliberately relinquished rather than merely never made.
pub const RELEASE_COMMENT_PREFIX: &str = "Released by ";

/// Render the release-comment body posted on `issue release` (issue #326) —
/// the release-side mirror of [`claim_comment_body`], same fields and same
/// backtick-wrapping discipline (round-3 audit A3's reasoning applies
/// identically here: `path`/`branch` can carry an attacker-influenceable
/// backtick with no forged comment involved — see [`render_identity_clause`],
/// which both functions share). `forced_from` names the identity a `--force`
/// release displaced — `None` for releasing one's own claim, or for a forced
/// release of an issue whose holder identity was never known. `reason` is
/// the caller's optional free-text `--reason`, sanitized the same way a
/// claimant name is (control characters, backticks) before it reaches a
/// public comment body.
pub fn release_comment_body(
    identity: &Identity,
    timestamp: &str,
    login: Option<&str>,
    forced_from: Option<&str>,
    reason: Option<&str>,
) -> String {
    let mut body = render_identity_clause(RELEASE_COMMENT_PREFIX, identity, timestamp);
    if matches!(identity, Identity::Worktree { .. })
        && let Some(login) = login
    {
        body.push_str(&format!(", for @{login}"));
    }
    if let Some(prev) = forced_from {
        body.push_str(&format!(", forcibly released from `{prev}`"));
    }
    if let Some(reason) = reason {
        body.push_str(&format!(", reason: {}", sanitize_claimant_name(reason)));
    }
    body.push('.');
    body
}

// ---------------------------------------------------------------------------
// PRD #421 M2.0/M2.1/M2.2 — triage label vocabulary + prompt instruction
// ---------------------------------------------------------------------------

/// A label's full canonical shape: name, colour (hex digits, no leading `#`),
/// and description — everything `gh label create --force` needs for the call
/// to be a genuine converge-to-declared-state operation rather than a
/// colour-randomizing write (PRD #421 review B2 / reviewer F2, auditor F1):
/// real `gh` assigns a RANDOM colour whenever `--color` is omitted, and
/// `--force` then PATCHes that random colour onto the label even when it
/// already exists — 96 rewrites/day against a maintainer's own taxonomy on a
/// `*/15` cron.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LabelSpec {
    pub name: &'static str,
    pub color: &'static str,
    pub description: &'static str,
}

/// The triage label vocabulary (M2.0), settling the PRD's own open
/// label-naming question, hyphenated to match house style (`in-progress`,
/// `ci-cd`). The deck ensures these exist (idempotently) and delivers them to
/// a dispatched agent's prompt when [`crate::config::IssueDispatchConfig::triage`]
/// is on; the deck itself never applies one to an issue — that is the spawned
/// agent's job, via its own `gh` calls. Colours form two visually distinct
/// ramps (red→green priority, blue size) plus a third hue for `needs-triage`,
/// so `--force`'s repeated writes converge on something legible rather than
/// re-randomizing (B2).
pub const TRIAGE_LABELS: [LabelSpec; 7] = [
    LabelSpec {
        name: "priority-high",
        color: "b60205",
        description: "High priority — address soon.",
    },
    LabelSpec {
        name: "priority-medium",
        color: "fbca04",
        description: "Medium priority.",
    },
    LabelSpec {
        name: "priority-low",
        color: "0e8a16",
        description: "Low priority — can wait.",
    },
    LabelSpec {
        name: "size-high",
        color: "0052cc",
        description: "Large amount of work.",
    },
    LabelSpec {
        name: "size-medium",
        color: "1d76db",
        description: "Moderate amount of work.",
    },
    LabelSpec {
        name: "size-low",
        color: "c5def5",
        description: "Small amount of work.",
    },
    LabelSpec {
        name: "needs-triage",
        color: "d4c5f9",
        description: "Priority not yet determined.",
    },
];

/// The work-type vocabulary's GitHub-label surface (PRD fork#340 M5) —
/// deliberately **one entry**. `bug`, `documentation` and `enhancement`
/// already exist on GitHub with its own default colour/description, and
/// [`label_create_argv`]'s `--force` PATCHes name/colour/description on
/// every [`ensure_labels`](crate::issue_dispatch_run) run: adding any
/// of those three here with our own wording would silently overwrite the
/// repo-wide default on every dispatch. `chore` is the only work type with
/// no existing label, so it is the only one safe to add. The gate itself
/// (`cargo xtask work-type-check`) never reads labels — this is optional and
/// decoupled by design, which is also why only `chore` needs to exist at
/// all. Colour `d93f0b` (a solid orange) is chosen to sit outside both
/// `TRIAGE_LABELS` ramps (red→green priority, blue size, plus the
/// `needs-triage` purple) and outside GitHub's own `bug`/`documentation`/
/// `enhancement` defaults, so it reads as its own category rather than an
/// accidental near-match.
pub const TYPE_LABELS: [LabelSpec; 1] = [LabelSpec {
    name: "chore",
    color: "d93f0b",
    description: "Maintenance work with no user-facing surface.",
}];

/// Build the `gh label create` argv (arguments after `gh`) that idempotently
/// ensures `label` exists on `repo` with the given `color`/`description`.
/// `--force` updates the label in place if it is already there instead of
/// erroring — genuinely idempotent now that an explicit `color`/`description`
/// are always supplied (B2): without them `gh` assigns a random colour on
/// every call and `--force` writes it over whatever was there.
///
/// The label name is positional and must come immediately after `create` —
/// this shape is load-bearing for the L2 test stub's `gh`, which parses the
/// name as its first argument regardless of what follows. No `--` end-of-
/// options marker: with the positional necessarily BEFORE every flag, a
/// trailing `--` (the pre-fix shape) protected nothing that followed it, and
/// there is no flag-free arrangement that would let one placed correctly
/// still leave the name where the stub (and `gh`'s own `label create <name>
/// [flags]` usage) expects it (reviewer F8 / auditor F2, D5).
pub fn label_create_argv(repo: &str, label: &str, color: &str, description: &str) -> Vec<String> {
    vec![
        "label".to_string(),
        "create".to_string(),
        label.to_string(),
        "--repo".to_string(),
        repo.to_string(),
        "--color".to_string(),
        color.to_string(),
        "--description".to_string(),
        description.to_string(),
        "--force".to_string(),
    ]
}

/// The triage instruction appended to a dispatched issue's prompt when triage
/// is enabled (M2.2): names the full vocabulary and states the uncertainty
/// rule — under uncertainty, apply `needs-triage` and leave priority unset
/// rather than guess, because a wrong priority is indistinguishable from a
/// considered one and so is worse than an absent one. Also notes the
/// human-present bounded-question option from the PRD, while making clear the
/// unattended path must never block a scheduled run on a prompt.
pub fn triage_instruction() -> String {
    format!(
        "Triage this issue using the following labels: {labels}. Apply one size label \
         (`size-high`, `size-medium`, or `size-low`) for how much work it looks like, and one \
         priority label (`priority-high`, `priority-medium`, or `priority-low`) when you are \
         confident in the ranking. If you are uncertain or not confident about the priority, \
         apply `needs-triage` instead and leave priority unset rather than guess — a wrong \
         priority is worse than no priority at all. If a human is present in this session you \
         may instead ask a bounded question, e.g. \"priority for #<N>: high, medium, or low?\" \
         — but never block an unattended, scheduled run on a prompt: apply `needs-triage` and \
         continue.",
        labels = TRIAGE_LABELS
            .iter()
            .map(|l| l.name)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- U2: prompt templating + default name ---

    #[test]
    fn substitute_issue_number_replaces_placeholder() {
        assert_eq!(
            substitute_issue_number("Work on issue {{issue_number}}", 42),
            "Work on issue 42"
        );
        // Multiple occurrences are all substituted.
        assert_eq!(
            substitute_issue_number("#{{issue_number}} -> {{issue_number}}", 7),
            "#7 -> 7"
        );
        // The default seed substitutes as documented.
        assert_eq!(
            substitute_issue_number(DEFAULT_ISSUE_PROMPT_TEMPLATE, 120),
            "Work on issue 120"
        );
    }

    #[test]
    fn substitute_issue_number_leaves_placeholderless_template_unchanged() {
        assert_eq!(
            substitute_issue_number("/prd-full please", 9),
            "/prd-full please"
        );
    }

    #[test]
    fn default_issue_dispatch_name_is_issues_repo() {
        assert_eq!(
            default_issue_dispatch_name("vfarcic/dot-ai"),
            "Issues vfarcic/dot-ai"
        );
    }

    // --- U3: path & branch derivation ---

    #[test]
    fn derive_issue_paths_exact_layout() {
        // A plain single-segment name is used verbatim as the clone dir.
        let paths = derive_issue_paths(Path::new("/work/space"), "dispatch-task", 17);
        assert_eq!(paths.clone_dir, PathBuf::from("/work/space/dispatch-task"));
        assert_eq!(
            paths.worktree_dir,
            PathBuf::from("/work/space/dispatch-task/.worktrees/issue-17")
        );
        assert_eq!(paths.branch, "agent/issue-17");
    }

    #[test]
    fn derive_issue_paths_sanitizes_default_seed_name_with_slash() {
        // The default-seeded name `Issues <owner>/<repo>` carries a `/`; it must
        // collapse to a single clone-dir segment, never nesting `owner/repo`.
        let paths = derive_issue_paths(Path::new("/work/space"), "Issues vfarcic/dot-ai", 17);
        assert_eq!(
            paths.clone_dir,
            PathBuf::from("/work/space/Issues vfarcic-dot-ai")
        );
        assert_eq!(
            paths.worktree_dir,
            PathBuf::from("/work/space/Issues vfarcic-dot-ai/.worktrees/issue-17")
        );
    }

    #[test]
    fn derive_issue_paths_never_escapes_working_dir() {
        // L2 + S4: absolute, `..`, and slash-laden names all map to a single safe
        // segment strictly inside the working dir.
        let wd = Path::new("/work/space");
        for name in [
            "/etc/passwd",
            "../../escape",
            "a/b/c",
            "Issues vfarcic/dot-ai",
            r"..\..\windows",
        ] {
            let clone = derive_issue_paths(wd, name, 1).clone_dir;
            assert!(
                clone.starts_with(wd),
                "clone dir {clone:?} escaped working dir for name {name:?}"
            );
            let rel = clone.strip_prefix(wd).expect("clone dir under working dir");
            assert_eq!(
                rel.components().count(),
                1,
                "clone dir {clone:?} must be ONE segment under the working dir (name {name:?})"
            );
            assert!(
                !clone.to_string_lossy().contains(".."),
                "clone dir {clone:?} must not contain `..` (name {name:?})"
            );
        }
    }

    #[test]
    fn sanitize_clone_segment_passthrough_and_fallback() {
        assert_eq!(sanitize_clone_segment("dispatch-task"), "dispatch-task");
        assert_eq!(
            sanitize_clone_segment("Issues vfarcic/dot-ai"),
            "Issues vfarcic-dot-ai"
        );
        // Reduces-to-nothing inputs fall back to a fixed segment.
        assert_eq!(sanitize_clone_segment(".."), "issues");
        assert_eq!(sanitize_clone_segment("/"), "issues");
        assert_eq!(sanitize_clone_segment(""), "issues");
        assert_eq!(sanitize_clone_segment("."), "issues");
    }

    #[test]
    fn issue_branch_is_deterministic() {
        assert_eq!(issue_branch(1), "agent/issue-1");
        assert_eq!(issue_branch(999), "agent/issue-999");
    }

    // --- U4: gh argv construction ---

    #[test]
    fn issue_list_argv_no_filters() {
        assert_eq!(
            issue_list_argv("vfarcic/dot-ai", 5, None, None),
            vec![
                "issue",
                "list",
                "--repo",
                "vfarcic/dot-ai",
                "--state",
                "open",
                "--json",
                "number,labels",
                "--limit",
                "5",
                "--",
            ]
        );
    }

    #[test]
    fn issue_list_argv_label_only() {
        assert_eq!(
            issue_list_argv("vfarcic/dot-ai", 3, Some("agent-eligible"), None),
            vec![
                "issue",
                "list",
                "--repo",
                "vfarcic/dot-ai",
                "--state",
                "open",
                "--json",
                "number,labels",
                "--limit",
                "3",
                "--label",
                "agent-eligible",
                "--",
            ]
        );
    }

    #[test]
    fn issue_list_argv_query_override() {
        assert_eq!(
            issue_list_argv("vfarcic/dot-ai", 10, None, Some("is:open sort:created-asc")),
            vec![
                "issue",
                "list",
                "--repo",
                "vfarcic/dot-ai",
                "--state",
                "open",
                "--json",
                "number,labels",
                "--limit",
                "10",
                "--search",
                "is:open sort:created-asc",
                "--",
            ]
        );
    }

    #[test]
    fn issue_list_argv_label_and_query_both_present() {
        assert_eq!(
            issue_list_argv("o/r", 2, Some("bug"), Some("milestone:v1")),
            vec![
                "issue",
                "list",
                "--repo",
                "o/r",
                "--state",
                "open",
                "--json",
                "number,labels",
                "--limit",
                "2",
                "--label",
                "bug",
                "--search",
                "milestone:v1",
                "--",
            ]
        );
    }

    #[test]
    fn argv_builders_carry_end_of_options_separator() {
        // M1: both builders terminate with the `--` end-of-options marker.
        assert!(issue_list_argv("o/r", 1, None, None).contains(&"--".to_string()));
        assert!(pr_list_for_issue_argv("o/r", 1).contains(&"--".to_string()));
    }

    /// Scenario: issue #325 auditor A7 — `worktree_remove_argv` (the
    /// `AddOutcome::TimedOut` cleanup twins' argv builder) must carry the
    /// `--` end-of-options separator immediately before the worktree path,
    /// matching `worktree_reclaim.rs`'s `remove_worktree_dir` — otherwise a
    /// worktree path beginning with `-` would be parsed as a `git worktree
    /// remove` flag rather than the target. Pure data assertion, no process
    /// spawn.
    #[test]
    fn worktree_remove_argv_carries_end_of_options_separator_before_path() {
        let clone_dir = Path::new("/repo/clone");
        let worktree_dir = Path::new("/repo/worktrees/agent-issue-7");
        let argv = worktree_remove_argv(clone_dir, worktree_dir);
        let dash_dash = argv
            .iter()
            .position(|a| a == "--")
            .expect("worktree_remove_argv must contain a `--` end-of-options separator");
        assert_eq!(
            argv.get(dash_dash + 1).map(String::as_str),
            Some(worktree_dir.to_string_lossy().as_ref()),
            "the `--` separator must sit IMMEDIATELY before the path argument, got {argv:?}"
        );
    }

    /// Scenario: issue #325 fix round (reviewer P1-2) — `isolated_clone_checkout_argv`
    /// must create a NEW branch (`-b <branch>`) when the typed slug does not
    /// already exist as a branch, so the isolated clone lands on the branch
    /// the user actually typed instead of silently staying on the source's
    /// HEAD branch.
    #[test]
    fn isolated_clone_checkout_argv_creates_new_branch_when_absent() {
        let clone_dir = Path::new("/repo/clone-my-feature");
        assert_eq!(
            isolated_clone_checkout_argv(clone_dir, "my-feature", BranchLocation::Absent),
            vec![
                "-C",
                "/repo/clone-my-feature",
                "checkout",
                "-b",
                "my-feature",
            ]
        );
    }

    /// Same fix round: when the typed slug already exists as a LOCAL branch
    /// in the clone (it carries every ref the source had), the checkout must
    /// ATTACH to it rather than retry `-b`, which `git checkout -b` on an
    /// existing branch name refuses — mirroring `worktree_add_argv`'s
    /// identical attach-vs-create split for the shared-checkout arm.
    #[test]
    fn isolated_clone_checkout_argv_attaches_existing_local_branch() {
        let clone_dir = Path::new("/repo/clone-my-feature");
        assert_eq!(
            isolated_clone_checkout_argv(clone_dir, "my-feature", BranchLocation::Local),
            vec!["-C", "/repo/clone-my-feature", "checkout", "my-feature"]
        );
    }

    /// Scenario: issue #325 fix round 3 (reviewer P2-1 / auditor C3) —
    /// when the typed slug exists ONLY as a remote-tracking ref (a fresh
    /// clone's shape for every branch but the source's checked-out HEAD),
    /// the checkout must use the explicit `-b <branch> --track
    /// origin/<branch>` form rather than a bare `git checkout <branch>`
    /// that depends on git's DWIM to resolve it — DWIM refuses outright
    /// under a `checkout.guess=false` git config, and is shadowed by a
    /// same-named file at the repo root.
    #[test]
    fn isolated_clone_checkout_argv_tracks_remote_only_branch_explicitly() {
        let clone_dir = Path::new("/repo/clone-my-feature");
        assert_eq!(
            isolated_clone_checkout_argv(clone_dir, "my-feature", BranchLocation::RemoteOnly),
            vec![
                "-C",
                "/repo/clone-my-feature",
                "checkout",
                "-b",
                "my-feature",
                "--track",
                "origin/my-feature",
            ]
        );
    }

    /// Scenario: issue #325 fix round 2 (reviewer P2-A) — pure argv
    /// assertion for `isolated_clone_remote_branch_probe_argv`, mirroring
    /// the plain `refs/heads/<branch>` probe but against
    /// `refs/remotes/origin/<branch>`, the location a fresh clone's
    /// non-HEAD branches actually live at.
    #[test]
    fn isolated_clone_remote_branch_probe_argv_probes_remote_tracking_ref() {
        let clone_dir = Path::new("/repo/clone-my-feature");
        assert_eq!(
            isolated_clone_remote_branch_probe_argv(clone_dir, "my-feature"),
            vec![
                "-C",
                "/repo/clone-my-feature",
                "rev-parse",
                "--verify",
                "--quiet",
                "refs/remotes/origin/my-feature",
            ]
        );
    }

    #[test]
    fn pr_list_for_issue_argv_keys_on_head_branch() {
        assert_eq!(
            pr_list_for_issue_argv("vfarcic/dot-ai", 17),
            vec![
                "pr",
                "list",
                "--repo",
                "vfarcic/dot-ai",
                "--state",
                "open",
                "--head",
                "agent/issue-17",
                "--json",
                "number",
                "--",
            ]
        );
    }

    // --- M1: user-config validation ---

    #[test]
    fn validate_issue_dispatch_config_accepts_valid_slug_and_filters() {
        assert!(validate_issue_dispatch_config("vfarcic/dot-ai", 3, None, None).is_ok());
        assert!(
            validate_issue_dispatch_config(
                "acme/widgets.v2",
                5,
                Some("agent-eligible"),
                Some("is:open sort:created-asc")
            )
            .is_ok()
        );
        // The smallest valid cap (1) is accepted.
        assert!(validate_issue_dispatch_config("o/r", 1, None, None).is_ok());
    }

    #[test]
    fn validate_issue_dispatch_config_rejects_bad_repo() {
        // Not an owner/name slug.
        assert!(validate_issue_dispatch_config("not-a-slug", 3, None, None).is_err());
        assert!(validate_issue_dispatch_config("a/b/c", 3, None, None).is_err());
        assert!(validate_issue_dispatch_config("owner/", 3, None, None).is_err());
        assert!(validate_issue_dispatch_config("/name", 3, None, None).is_err());
        // Injection-shaped values.
        assert!(validate_issue_dispatch_config("ext::sh -c id", 3, None, None).is_err());
        assert!(validate_issue_dispatch_config("file:///etc", 3, None, None).is_err());
        // Leading `-` would be read as a `gh` flag even though the char is in the
        // slug character class.
        assert!(validate_issue_dispatch_config("-x/y", 3, None, None).is_err());
    }

    #[test]
    fn validate_issue_dispatch_config_rejects_max_per_run_below_one() {
        // A 0 cap makes `gh issue list --limit 0` dispatch nothing every fire.
        let err = validate_issue_dispatch_config("o/r", 0, None, None).unwrap_err();
        assert!(
            err.contains("max_per_run"),
            "error should name the offending field, got {err:?}"
        );
        // A valid slug with a 0 cap is still rejected (the cap, not the slug).
        assert!(validate_issue_dispatch_config("vfarcic/dot-ai", 0, None, None).is_err());
    }

    #[test]
    fn validate_issue_dispatch_config_rejects_empty_label_or_query() {
        // An empty filter is meaningless and would still be passed through to `gh`.
        let label_err = validate_issue_dispatch_config("o/r", 3, Some(""), None).unwrap_err();
        assert!(
            label_err.contains("label"),
            "error should name the label, got {label_err:?}"
        );
        let query_err = validate_issue_dispatch_config("o/r", 3, None, Some("")).unwrap_err();
        assert!(
            query_err.contains("query"),
            "error should name the query, got {query_err:?}"
        );
    }

    #[test]
    fn validate_issue_dispatch_config_rejects_leading_dash_label_or_query() {
        assert!(validate_issue_dispatch_config("o/r", 3, Some("-rf"), None).is_err());
        assert!(validate_issue_dispatch_config("o/r", 3, None, Some("--owner")).is_err());
        // Non-leading dashes are fine.
        assert!(validate_issue_dispatch_config("o/r", 3, Some("agent-eligible"), None).is_ok());
    }

    // --- U5: idempotency decision (truth table) ---

    #[test]
    fn dispatch_decision_truth_table() {
        // PRD #421 M1.4: exhaustive over all 8 combinations of the 3 signals
        // (worktree exists, open PR, `in-progress` label) — any one true means
        // skip; only all-false dispatches.
        for worktree_exists in [false, true] {
            for open_pr in [false, true] {
                for label in [false, true] {
                    let expected = if worktree_exists || open_pr || label {
                        DispatchDecision::Skip
                    } else {
                        DispatchDecision::Dispatch
                    };
                    assert_eq!(
                        dispatch_decision(worktree_exists, open_pr, label),
                        expected,
                        "dispatch_decision({worktree_exists}, {open_pr}, {label})"
                    );
                }
            }
        }
    }

    // --- PRD #421 M1.0/M1.1/M1.2: claim label/comment argv + comment body ---

    #[test]
    fn issue_edit_add_label_argv_shape() {
        assert_eq!(
            issue_edit_add_label_argv("acme/widgets", 7, IN_PROGRESS_LABEL),
            vec![
                "issue",
                "edit",
                "--repo",
                "acme/widgets",
                "--add-label",
                "in-progress",
                "--",
                "7",
            ]
        );
    }

    #[test]
    fn issue_edit_remove_label_argv_shape() {
        assert_eq!(
            issue_edit_remove_label_argv("acme/widgets", 7, IN_PROGRESS_LABEL),
            vec![
                "issue",
                "edit",
                "--repo",
                "acme/widgets",
                "--remove-label",
                "in-progress",
                "--",
                "7",
            ]
        );
    }

    #[test]
    fn issue_comment_argv_shape() {
        assert_eq!(
            issue_comment_argv(
                "acme/widgets",
                7,
                "Claimed by scheduled task `dispatch-task`."
            ),
            vec![
                "issue",
                "comment",
                "--repo",
                "acme/widgets",
                "--body",
                "Claimed by scheduled task `dispatch-task`.",
                "--",
                "7",
            ]
        );
    }

    #[test]
    fn issue_view_comments_argv_shape() {
        assert_eq!(
            issue_view_comments_argv("acme/widgets", 7),
            vec![
                "issue",
                "view",
                "--repo",
                "acme/widgets",
                "--json",
                "comments,assignees",
                "--",
                "7",
            ]
        );
    }

    #[test]
    fn type_labels_contains_only_chore() {
        // M5: `bug`, `documentation` and `enhancement` already exist on
        // GitHub with their own default colour/description, and
        // `label_create_argv`'s `--force` would silently overwrite them on
        // every dispatch run if they were listed here too. `chore` is the
        // only work type with no pre-existing label.
        assert_eq!(TYPE_LABELS.len(), 1);
        assert_eq!(TYPE_LABELS[0].name, "chore");
    }

    #[test]
    fn label_create_argv_carries_color_and_description() {
        assert_eq!(
            label_create_argv("acme/widgets", IN_PROGRESS_LABEL, "006b75", "claim marker"),
            vec![
                "label",
                "create",
                "in-progress",
                "--repo",
                "acme/widgets",
                "--color",
                "006b75",
                "--description",
                "claim marker",
                "--force",
            ]
        );
    }

    #[test]
    fn claim_comment_body_names_task_identity() {
        let identity = Identity::issue_dispatch(
            "dispatch-task",
            7,
            Path::new("/work/dispatch-task/.worktrees/issue-7"),
            "agent/issue-7",
        );
        let body = claim_comment_body(&identity, "2026-08-09T00:00:00Z", None, None);
        assert!(body.starts_with(CLAIM_COMMENT_PREFIX), "got {body:?}");
        assert!(
            body.contains("dispatch-task"),
            "body must name the claiming task, got {body:?}"
        );
        assert!(body.contains("/work/dispatch-task/.worktrees/issue-7"));
        assert!(body.contains("agent/issue-7"));
        assert!(body.contains("2026-08-09T00:00:00Z"));
        assert!(
            !body.contains("for @"),
            "no login was supplied, so the `for @<login>` clause must be omitted entirely, got {body:?}"
        );
    }

    #[test]
    fn claim_comment_body_neutralizes_backtick_and_newline_in_task_name() {
        // C5 / auditor F3: a backtick would close the surrounding code span
        // early; a raw newline could then start a fabricated second line.
        let identity = Identity::issue_dispatch(
            "x` cc @nobody\ninjected",
            7,
            Path::new("/work/x/.worktrees/issue-7"),
            "agent/issue-7",
        );
        let body = claim_comment_body(&identity, "2026-08-09T00:00:00Z", None, None);
        // Round 3's format wraps THREE fields in their own backtick pair: the
        // decorated task name (`Identity::issue_dispatch`'s `label`), the
        // worktree path, and the branch — reviewer F5: the name must sit
        // inside a code span exactly like the other two, restoring the
        // mention-injection protection PRD #421's C5 fix established and a
        // prior round's bare `orchestration:name@host:wt` rendering dropped
        // (that regression is why this test's expected count was wrongly
        // relaxed from PRD #421's original 4 down to 2 — round 3's shape has
        // one more backtick-wrapped field than PRD #421's did, so the
        // restored count is 6, not 4).
        assert_eq!(
            body.matches('`').count(),
            6,
            "task name must not be able to introduce extra backticks, got {body:?}"
        );
        assert!(
            !body.contains('\n'),
            "task name must not be able to introduce a raw newline, got {body:?}"
        );
        assert!(
            !body.contains("x`"),
            "the name's own backtick must not survive verbatim, got {body:?}"
        );
    }

    #[test]
    fn claim_comment_body_includes_login_and_takeover_clause() {
        let holder =
            Identity::orchestration("orch-A", Path::new("/work/wt-a"), "branch-a").to_string();
        let identity = Identity::orchestration("orch-B", Path::new("/work/wt-b"), "branch-b");
        let body = claim_comment_body(
            &identity,
            "2026-08-09T00:00:00Z",
            Some("bob"),
            Some(&holder),
        );
        assert!(body.starts_with(CLAIM_COMMENT_PREFIX));
        assert!(
            body.contains(", for @bob"),
            "must carry the login clause, got {body:?}"
        );
        assert!(
            body.contains(&format!("taking over from `{holder}`")),
            "must name who it was taken over from, backtick-wrapped, got {body:?}"
        );
        // The takeover clause must never displace the load-bearing prefix.
        assert!(body.starts_with(CLAIM_COMMENT_PREFIX));
    }

    // --- issue #326: release_comment_body ---

    #[test]
    fn release_comment_body_names_releaser_and_reason() {
        let identity = Identity::orchestration("orch-B", Path::new("/work/wt-b"), "branch-b");
        let body = release_comment_body(
            &identity,
            "2026-08-09T00:00:00Z",
            Some("bob"),
            None,
            Some("PR merged"),
        );
        assert!(body.starts_with(RELEASE_COMMENT_PREFIX), "got {body:?}");
        assert!(body.contains(", for @bob"), "got {body:?}");
        assert!(body.contains("PR merged"), "got {body:?}");
        assert!(
            !body.contains("forcibly released"),
            "no forced_from was supplied, so the forced clause must be omitted, got {body:?}"
        );
    }

    #[test]
    fn release_comment_body_names_who_it_was_forced_from() {
        let holder =
            Identity::orchestration("orch-A", Path::new("/work/wt-a"), "branch-a").to_string();
        let identity = Identity::orchestration("orch-B", Path::new("/work/wt-b"), "branch-b");
        let body =
            release_comment_body(&identity, "2026-08-09T00:00:00Z", None, Some(&holder), None);
        assert!(
            body.contains(&format!("forcibly released from `{holder}`")),
            "must name who it was forcibly released from, backtick-wrapped, got {body:?}"
        );
    }

    #[test]
    fn release_comment_body_sanitizes_reason() {
        let identity = Identity::orchestration("orch-B", Path::new("/work/wt-b"), "branch-b");
        let body = release_comment_body(
            &identity,
            "2026-08-09T00:00:00Z",
            None,
            None,
            Some("done`\ninjected"),
        );
        assert!(!body.contains('\n'), "got {body:?}");
        assert!(!body.contains("done`"), "got {body:?}");
    }

    #[test]
    fn human_claim_comment_body_names_login_and_host_not_for_clause() {
        let identity = Identity::human("alice", "host-1");
        let body = claim_comment_body(&identity, "2026-08-09T00:00:00Z", Some("alice"), None);
        assert!(body.starts_with(CLAIM_COMMENT_PREFIX));
        assert!(body.starts_with("Claimed by @alice working from `host-1` at"));
        // The login is already the identity itself for a human claim, so a
        // redundant `, for @alice` clause must never be appended — appending
        // one would also make `parse_human_claim`'s single-`@`-after-prefix
        // shape ambiguous.
        assert!(
            !body.contains(", for @"),
            "a human-form claim must not carry a redundant `for @<login>` clause, got {body:?}"
        );
    }

    // --- PRD fork#235 round 3: Identity composition ---

    #[test]
    fn identity_renders_each_form() {
        // Round-3 audit A1: `Identity::Worktree` always carries the local
        // machine's own host (see the field's doc) as part of the compared
        // string, so the expected strings below reference the SAME
        // `local_hostname()` this process would resolve, rather than a
        // hardcoded value — the assertion must hold on every machine, not
        // just the one it was written on.
        let host = crate::issue_dispatch_run::local_hostname();
        assert_eq!(
            Identity::orchestration("orch-A", Path::new("/work/wt-a"), "branch-a").to_string(),
            format!("worktree:/work/wt-a@branch-a|{host}")
        );
        assert_eq!(
            Identity::issue_dispatch("dispatch-task", 7, Path::new("/work/wt"), "branch-x")
                .to_string(),
            format!("worktree:/work/wt@branch-x|{host}")
        );
        assert_eq!(
            Identity::human("alice", "host-1").to_string(),
            "human:alice@host-1"
        );
    }

    #[test]
    fn identity_comparison_is_on_the_whole_string_not_the_name_alone() {
        // fork #201/#222 + `issue/claim/007`: two orchestrations sharing the
        // exact same typed name but rooted in different worktrees must
        // compose to DIFFERENT identities.
        let a = Identity::orchestration("same-name", Path::new("/work/wt-1"), "branch-1");
        let b = Identity::orchestration("same-name", Path::new("/work/wt-2"), "branch-1");
        assert_ne!(a, b);
        assert_ne!(a.to_string(), b.to_string());
    }

    #[test]
    fn identity_comparison_ignores_the_decorative_label() {
        // Round 3: the SAME worktree+branch compares EQUAL regardless of
        // which decoration (or none) names it — `label` is display-only.
        let bare = Identity::worktree(Path::new("/work/wt-a"), "branch-a");
        let named = Identity::orchestration("orch-A", Path::new("/work/wt-a"), "branch-a");
        assert_eq!(bare.to_string(), named.to_string());
    }

    #[test]
    fn validate_gh_login_accepts_and_rejects() {
        assert!(validate_gh_login("alice"));
        assert!(validate_gh_login("a"));
        assert!(validate_gh_login("alice-bob9"));
        assert!(!validate_gh_login(""));
        assert!(!validate_gh_login("-alice"));
        assert!(!validate_gh_login("alice bob"));
        assert!(!validate_gh_login("alice`"));
        assert!(!validate_gh_login("alice\nbob"));
    }

    #[test]
    fn gh_current_login_argv_shape() {
        assert_eq!(
            gh_current_login_argv(),
            vec!["api", "user", "--jq", ".login"]
        );
    }

    #[test]
    fn issue_edit_assignee_argv_shapes() {
        assert_eq!(
            issue_edit_assignee_argv("acme/widgets", 7, Some("bob"), &["alice".to_string()]),
            vec![
                "issue",
                "edit",
                "--repo",
                "acme/widgets",
                "--add-assignee",
                "bob",
                "--remove-assignee",
                "alice",
                "--",
                "7",
            ]
        );
        // First-ever claim: no prior assignees to remove.
        assert_eq!(
            issue_edit_assignee_argv("acme/widgets", 7, Some("bob"), &[]),
            vec![
                "issue",
                "edit",
                "--repo",
                "acme/widgets",
                "--add-assignee",
                "bob",
                "--",
                "7"
            ]
        );
        // A hand-assigned issue carrying more than one prior assignee: one
        // `--remove-assignee` flag per entry (PRD fork#235 FINAL round 5).
        assert_eq!(
            issue_edit_assignee_argv(
                "acme/widgets",
                7,
                Some("bob"),
                &["alice".to_string(), "carol".to_string()]
            ),
            vec![
                "issue",
                "edit",
                "--repo",
                "acme/widgets",
                "--add-assignee",
                "bob",
                "--remove-assignee",
                "alice",
                "--remove-assignee",
                "carol",
                "--",
                "7",
            ]
        );
    }

    #[test]
    fn issue_view_claim_state_argv_shape() {
        assert_eq!(
            issue_view_claim_state_argv("acme/widgets", 7),
            vec![
                "issue",
                "view",
                "--repo",
                "acme/widgets",
                "--json",
                "labels,comments,assignees",
                "--",
                "7",
            ]
        );
    }

    // --- PRD fork#235 round 3: claim-comment parsing ---

    #[test]
    fn parse_claim_fields_extracts_worktree_identity_timestamp_login() {
        let comment = "Claimed by the orchestration `orch-A` working `/work/wt-a` on branch `branch-a` at 2026-08-09T00:00:00Z, for @alice.";
        let parsed = parse_claim_fields(comment).expect("must parse");
        assert_eq!(parsed.identity, "worktree:/work/wt-a@branch-a");
        assert_eq!(parsed.timestamp, "2026-08-09T00:00:00Z");
        assert_eq!(parsed.login.as_deref(), Some("alice"));
        assert_eq!(parsed.raw, comment);
    }

    #[test]
    fn parse_claim_fields_extracts_human_identity_and_login() {
        let comment = "Claimed by @dave working from `host-1` at 2026-08-09T00:00:00Z.";
        let parsed = parse_claim_fields(comment).expect("must parse");
        assert_eq!(parsed.identity, "human:dave@host-1");
        assert_eq!(parsed.timestamp, "2026-08-09T00:00:00Z");
        // The login IS the identity for a human claim, so it is always
        // reported even though there is no separate `for @<login>` clause.
        assert_eq!(parsed.login.as_deref(), Some("dave"));
    }

    #[test]
    fn parse_claim_fields_no_login_clause() {
        let comment = "Claimed by the orchestration working `/work/wt-bare` on branch `bare-branch` at 2026-08-09T00:00:00Z.";
        let parsed = parse_claim_fields(comment).expect("must parse");
        assert_eq!(parsed.identity, "worktree:/work/wt-bare@bare-branch");
        assert_eq!(parsed.login, None);
    }

    #[test]
    fn parse_claim_fields_takeover_clause_does_not_break_login_parse() {
        let comment = "Claimed by the orchestration `orch-B` working `/work/wt-b` on branch `branch-b` at 2026-08-09T00:00:00Z, for @bob, taking over from `worktree:/work/wt-a@branch-a`.";
        let parsed = parse_claim_fields(comment).expect("must parse");
        assert_eq!(parsed.identity, "worktree:/work/wt-b@branch-b");
        assert_eq!(parsed.login.as_deref(), Some("bob"));
        assert_eq!(parsed.timestamp, "2026-08-09T00:00:00Z");
    }

    #[test]
    fn parse_claim_fields_rejects_unrelated_text() {
        assert_eq!(parse_claim_fields("just a comment"), None);
    }

    #[test]
    fn parse_claim_fields_rejects_old_format_comment() {
        // An older-round (or hostile/hand-edited) comment in the PRE-round-3
        // shape must not be misread as a round-3 claim — `issue/claim/012`
        // relies on this failing closed to `None` rather than partial-fitting.
        let comment = "Claimed by orchestration:orch-A@host-1:a1b2c3d4 on `host-1` at 2026-08-09T00:00:00Z, for @alice.";
        assert_eq!(parse_claim_fields(comment), None);
    }

    #[test]
    fn parse_claim_state_reads_label_and_newest_claim() {
        let json = r#"{"labels":[{"name":"in-progress"}],"comments":[
            {"body":"unrelated"},
            {"body":"Claimed by the orchestration `orch-A` working `/work/wt-a` on branch `branch-a` at 2026-08-01T00:00:00Z, for @alice."},
            {"body":"Claimed by the orchestration `orch-B` working `/work/wt-b` on branch `branch-b` at 2026-08-09T00:00:00Z, for @bob."}
        ],"assignees":[{"login":"alice"}]}"#;
        let (label_present, held, assignees) = parse_claim_state(json).unwrap();
        assert!(label_present);
        let held = held.expect("a claim comment must be found");
        assert_eq!(held.identity, "worktree:/work/wt-b@branch-b");
        assert_eq!(held.login.as_deref(), Some("bob"));
        assert_eq!(assignees, vec!["alice".to_string()]);
    }

    #[test]
    fn parse_claim_state_no_label_no_comments() {
        let (label_present, held, assignees) =
            parse_claim_state(r#"{"labels":[],"comments":[]}"#).unwrap();
        assert!(!label_present);
        assert_eq!(held, None);
        assert_eq!(assignees, Vec::<String>::new());
    }

    #[test]
    fn parse_claim_state_labelled_with_no_claim_comment() {
        let json = r#"{"labels":[{"name":"in-progress"}],"comments":[{"body":"unrelated"}]}"#;
        let (label_present, held, assignees) = parse_claim_state(json).unwrap();
        assert!(label_present);
        assert_eq!(held, None);
        assert_eq!(assignees, Vec::<String>::new());
    }

    #[test]
    fn parse_claim_state_rejects_non_json() {
        assert!(parse_claim_state("not json").is_err());
    }

    #[test]
    fn parse_current_assignees_reads_logins_missing_field_is_empty() {
        let json = r#"{"comments":[],"assignees":[{"login":"alice"},{"login":"bob"}]}"#;
        assert_eq!(
            parse_current_assignees(json).unwrap(),
            vec!["alice".to_string(), "bob".to_string()]
        );
        assert_eq!(
            parse_current_assignees(r#"{"comments":[]}"#).unwrap(),
            Vec::<String>::new()
        );
    }
}
