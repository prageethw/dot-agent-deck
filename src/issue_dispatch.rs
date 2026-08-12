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
/// never completed cleanly.
pub fn worktree_remove_argv(clone_dir: &Path, worktree_dir: &Path) -> Vec<String> {
    vec![
        "-C".to_string(),
        clone_dir.to_string_lossy().into_owned(),
        "worktree".to_string(),
        "remove".to_string(),
        "--force".to_string(),
        worktree_dir.to_string_lossy().into_owned(),
    ]
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
/// issue's comments — the M1.3 claimant lookup, called ONLY once the
/// `in-progress` label is already known present (from the `gh issue list`
/// response `issue_list_argv` requests — see its doc comment), so an
/// unlabelled issue never triggers this call at all. The issue number sits
/// after the `--` end-of-options marker — see [`issue_edit_add_label_argv`]'s
/// doc comment for why that placement matters.
pub fn issue_view_comments_argv(repo: &str, issue: u64) -> Vec<String> {
    vec![
        "issue".to_string(),
        "view".to_string(),
        "--repo".to_string(),
        repo.to_string(),
        "--json".to_string(),
        "comments".to_string(),
        "--".to_string(),
        issue.to_string(),
    ]
}

/// The identity that claims an issue by posting a claim comment (PRD #421
/// M1.1). `run_issue_dispatch` has exactly one caller and claims under
/// [`Claimant::Task`] — `ScheduledTask.name`; that is the only write point
/// that exists today. A second variant for a human orchestration's own claim
/// was previously represented here for completeness even though nothing
/// constructed it — removed (fork #421 review E1) as speculative generality:
/// a `pub` enum's dead variant is never flagged by `dead_code`, so its unit
/// test was coverage over unreachable code that read as reassurance without
/// providing any. When a second write point appears it will come with
/// concrete requirements about what identity it carries; re-adding a variant
/// then is a small change made with better information than exists now. See
/// `prds/421-automatic-issue-labelling.md`'s "Decisions taken during
/// implementation" for the full record of why only one write point exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claimant {
    /// The scheduler-side dispatch: `ScheduledTask.name`.
    Task { name: String },
}

impl Claimant {
    fn describe(&self) -> String {
        match self {
            Claimant::Task { name } => {
                format!("scheduled task `{}`", sanitize_claimant_name(name))
            }
        }
    }
}

/// Neutralise a claimant-supplied task name for safe interpolation into a
/// public GitHub comment (PRD #421 review C5 / auditor F3). `ScheduledTask.name`
/// is hand-edited config with no character restriction, and the un-escaped
/// backtick wrapper around it in `Claimant::describe` lets a name that itself
/// contains a backtick close the code span early — after which a crafted
/// `@`-mention notifies a real GitHub user, or an embedded newline forges a
/// second, fabricated `Claimed by …` line (undermining the newest-claimant
/// lookup this PRD relies on). Backticks are dropped rather than escaped — a
/// backslash-escaped backtick does not render as literal inside a CommonMark
/// code span — newlines/carriage returns collapse to a space, and every other
/// C0/DEL control character is dropped outright.
fn sanitize_claimant_name(name: &str) -> String {
    name.chars()
        .filter_map(|c| match c {
            '`' => None,
            '\n' | '\r' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect()
}

/// Render the claim-comment body posted on a successful dispatch (PRD #421
/// M1.1): who claimed it, on which host, and when. `dispatch/010` asserts only
/// that the body names the claiming task; the exact wording around
/// host/timestamp beyond that is this function's choice.
pub fn claim_comment_body(claimant: &Claimant, host: &str, timestamp: &str) -> String {
    format!(
        "Claimed by {} on `{host}` at {timestamp}.",
        claimant.describe()
    )
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
                "comments",
                "--",
                "7",
            ]
        );
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
    fn claim_comment_body_names_task_claimant() {
        let claimant = Claimant::Task {
            name: "dispatch-task".to_string(),
        };
        let body = claim_comment_body(&claimant, "host-1", "2026-08-09T00:00:00Z");
        assert!(
            body.contains("dispatch-task"),
            "body must name the claiming task, got {body:?}"
        );
        assert!(body.contains("host-1"));
        assert!(body.contains("2026-08-09T00:00:00Z"));
    }

    #[test]
    fn claim_comment_body_neutralizes_backtick_and_newline_in_task_name() {
        // C5 / auditor F3: a backtick would close the surrounding code span
        // early; a raw newline could then start a fabricated second line.
        let claimant = Claimant::Task {
            name: "x` cc @nobody\ninjected".to_string(),
        };
        let body = claim_comment_body(&claimant, "host-1", "2026-08-09T00:00:00Z");
        // Every backtick in the rendered body must be one of the two fixed
        // wrapper pairs (around the host, around the sanitized name) — none
        // contributed by the task name itself.
        assert_eq!(
            body.matches('`').count(),
            4,
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
}
