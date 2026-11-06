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
// PRD #421 Phase 2 — the visible dispatch claim
// ---------------------------------------------------------------------------
//
// A dispatched issue is labelled [`CLAIM_LABEL`] and carries one claimant
// record: a comment naming the task and the machine id that dispatched it,
// written by the account `gh` is signed in with, and edited rather than
// appended on a re-claim. There is deliberately no builder that removes a label
// or deletes a comment (PRD #421 M2.1/M2.2): the deck never releases a claim.
// `tests/dispatch_claim_never_released.rs` scans `src/` for the spellings of a
// label removal and of a REST `DELETE`.

/// The deck-owned label a dispatched issue carries (PRD #421 chose it over
/// assigning the issue or a bare `in-progress` label). Namespaced to mark it as
/// the deck's, so that a repository's own workflow vocabulary is unlikely to
/// use the same name; `--force` provisioning converges a label that already
/// has the name onto the deck's colour and description.
pub const CLAIM_LABEL: &str = "agent-deck/claimed";

/// The colour [`claim_label_provision_argv`] converges the label onto.
pub const CLAIM_LABEL_COLOR: &str = "1D76DB";

/// The description [`claim_label_provision_argv`] converges the label onto.
pub const CLAIM_LABEL_DESCRIPTION: &str = "An agent was dispatched to this issue by dot-agent-deck";

/// The fixed marker opening every claimant record, which any deck can find.
/// Task and machine id are fields inside the body, not part of the marker.
pub const CLAIM_RECORD_MARKER: &str = "<!-- dot-agent-deck:claim -->";

const CLAIM_RECORD_TASK_FIELD: &str = "- Task: ";
const CLAIM_RECORD_HOST_FIELD: &str = "- Machine: ";
const CLAIM_RECORD_CLAIMED_AT_FIELD: &str = "- Claimed at: ";
/// The record's lifetime: how many hours after its last edit the deck that
/// wrote it holds it live, so a reader judges it by its writer's
/// `claim_ttl_hours` rather than its own. Records written before the field
/// existed (v0.48.0 and earlier) have none. Those builds read a record line
/// by line and skip lines they do not know, so they still parse a record
/// carrying it and still find their own by its task and machine.
const CLAIM_RECORD_TTL_FIELD: &str = "- Lifetime (hours): ";

/// The domain separator [`claim_host_id`] hashes in front of the host name, so
/// the digest differs from a plain SHA-256 of the bare name computed for some
/// other purpose. The `v1` is the scheme's version: a different derivation gets
/// a new separator and a new id prefix rather than silently changing ids.
const CLAIM_HOST_DOMAIN: &[u8] = b"dot-agent-deck/claim-host/v1\0";

/// The machine id a claimant record names instead of the raw host name
/// (PRD #421 amendment item 8): `h1-` and the first 16 hex digits of SHA-256
/// over [`CLAIM_HOST_DOMAIN`] followed by `hostname`. Stable for a machine
/// whose host name does not change, which is what lets a re-claim find its own
/// record.
///
/// It keeps the raw name out of the public comment, but it is NOT
/// anonymisation: the digest is unsalted and host names are low-entropy, so
/// anyone who can guess a name can confirm it by hashing it the same way.
pub fn claim_host_id(hostname: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(CLAIM_HOST_DOMAIN);
    hasher.update(hostname.as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("h1-{hex}")
}

/// Build the `gh api` argv (arguments after `gh`) that reads the account `gh`
/// is signed in with — whose numeric `id` decides which claimant records are
/// this deck's own (PRD #421 amendment item 6). `--include` prints the
/// response headers ahead of the body, so the same call also carries GitHub's
/// `Date`, the clock claim records are aged against (PRD #421 M3.1).
pub fn authenticated_user_argv() -> Vec<String> {
    vec![
        "api".to_string(),
        "--include".to_string(),
        "user".to_string(),
    ]
}

/// What [`parse_authenticated_user`] reads from the account lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthenticatedUser {
    pub id: u64,
    /// The response's `Date` header: GitHub's clock when it answered. `None`
    /// when the output carried no headers, or no readable `Date`.
    pub server_date: Option<chrono::DateTime<chrono::Utc>>,
}

/// Read the output of [`authenticated_user_argv`]: an optional HTTP status
/// line and header block (what `--include` prints, ended by a blank line),
/// then the JSON body with the numeric account id. Output that starts with
/// the body is read as a body with no headers. A body without an id is an
/// error: without the id no record can be proven to be this deck's.
pub fn parse_authenticated_user(output: &str) -> Result<AuthenticatedUser, String> {
    #[derive(serde::Deserialize)]
    struct User {
        id: u64,
    }
    let output = output.trim_start();
    let (server_date, body) = if output.starts_with("HTTP/") {
        let mut server_date = None;
        // No blank line means no body, which the parse below rejects.
        let mut body = "";
        let mut consumed = 0;
        for line in output.split_inclusive('\n') {
            consumed += line.len();
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                body = &output[consumed..];
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.trim().eq_ignore_ascii_case("date")
            {
                server_date = chrono::DateTime::parse_from_rfc2822(value.trim())
                    .ok()
                    .map(|date| date.with_timezone(&chrono::Utc));
            }
        }
        (server_date, body)
    } else {
        (None, output)
    };
    serde_json::from_str::<User>(body)
        .map(|user| AuthenticatedUser {
            id: user.id,
            server_date,
        })
        .map_err(|e| format!("failed to read the signed-in GitHub account: {e}"))
}

/// Build the `gh label create` argv (arguments after `gh`) that provisions
/// [`CLAIM_LABEL`] in `repo`, creating it or, with `--force`, converging an
/// existing one onto the deck's colour and description. Every value after
/// `create` is a constant or the validated `repo` slug, so no element can be
/// read as an unexpected flag.
pub fn claim_label_provision_argv(repo: &str) -> Vec<String> {
    vec![
        "label".to_string(),
        "create".to_string(),
        CLAIM_LABEL.to_string(),
        "--repo".to_string(),
        repo.to_string(),
        "--force".to_string(),
        "--color".to_string(),
        CLAIM_LABEL_COLOR.to_string(),
        "--description".to_string(),
        CLAIM_LABEL_DESCRIPTION.to_string(),
    ]
}

/// Build the `gh issue edit` argv (arguments after `gh`) that adds
/// [`CLAIM_LABEL`] to `issue_number`, with the issue number after the `--`
/// end-of-options marker (see `issue_list_argv`).
pub fn claim_issue_argv(repo: &str, issue_number: u64) -> Vec<String> {
    vec![
        "issue".to_string(),
        "edit".to_string(),
        "--repo".to_string(),
        repo.to_string(),
        "--add-label".to_string(),
        CLAIM_LABEL.to_string(),
        "--".to_string(),
        issue_number.to_string(),
    ]
}

/// Build the `gh api` argv (arguments after `gh`) listing every comment on
/// `issue_number`, which is where an existing claimant record is found. The
/// endpoint starts with `repos/`, so it cannot be read as a flag. It asks for
/// GitHub's largest page, 100 comments, so a long thread costs as few requests
/// as it can.
pub fn issue_comments_argv(repo: &str, issue_number: u64) -> Vec<String> {
    vec![
        "api".to_string(),
        "--paginate".to_string(),
        format!("repos/{repo}/issues/{issue_number}/comments?per_page=100"),
    ]
}

/// Build the `gh api` argv (arguments after `gh`) that posts `body` as a new
/// comment on `issue_number` — used when the issue carries no claimant record
/// of this deck's yet. `-f` sends the body as a raw string, never a file read.
pub fn create_claim_record_argv(repo: &str, issue_number: u64, body: &str) -> Vec<String> {
    vec![
        "api".to_string(),
        "--method".to_string(),
        "POST".to_string(),
        format!("repos/{repo}/issues/{issue_number}/comments"),
        "-f".to_string(),
        format!("body={body}"),
    ]
}

/// Build the `gh api` argv (arguments after `gh`) that replaces the body of
/// the existing claimant record `comment_id` — a re-claim edits the record
/// rather than appending another one.
pub fn edit_claim_record_argv(repo: &str, comment_id: u64, body: &str) -> Vec<String> {
    vec![
        "api".to_string(),
        "--method".to_string(),
        "PATCH".to_string(),
        format!("repos/{repo}/issues/comments/{comment_id}"),
        "-f".to_string(),
        format!("body={body}"),
    ]
}

/// What a claimant record's body says about who wrote it: the scheduled task
/// and the machine id ([`claim_host_id`]) it ran on, and when it was last
/// claimed. Who actually authored the comment is not part of the body —
/// [`find_own_claim_record`] takes that from the comment's `user.id`, which
/// GitHub sets and a body cannot forge.
///
/// `claimed_at` and `ttl_hours` are `None` for a record written before the
/// field existed; such a record is still a record, and gains the field on its
/// next edit by a deck that writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimRecord {
    pub task: String,
    pub host: String,
    pub claimed_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The `claim_ttl_hours` of the deck that wrote the record, as written:
    /// [`ClaimRecord::ttl`] bounds it before anything is judged by it.
    pub ttl_hours: Option<u32>,
}

impl ClaimRecord {
    /// How long after its last edit this record stays live: the lifetime its
    /// writer stated, kept within 1 hour and [`MAX_CLAIM_TTL_HOURS`], or
    /// `fallback` — the reader's own TTL — for a record that states none.
    pub fn ttl(&self, fallback: chrono::Duration) -> chrono::Duration {
        self.ttl_hours.map_or(fallback, |hours| {
            chrono::Duration::hours(i64::from(hours.clamp(1, MAX_CLAIM_TTL_HOURS)))
        })
    }
}

/// Render the body of a claimant record for `task` on the machine `host_id`
/// (a [`claim_host_id`]), claimed at `claimed_at`: the marker on the first
/// line, a sentence for a human reading the issue, then the fields. Each field
/// value is a JSON string, so a task name carrying quotes or a newline still
/// renders on one line and parses back exactly. Only the machine id is
/// repeated in the sentence: it is hex by construction, while the task name is
/// the user's free text.
///
/// The claim time is what makes a re-claim's body differ from the record it
/// edits: GitHub leaves a comment's `updated_at` alone when an edit repeats the
/// same body, and `updated_at` is the claim's clock. It carries milliseconds,
/// and a re-claim takes its time from [`next_claim_time`], which is at least a
/// millisecond past the time the edited record carries, so the two bodies
/// differ even when the wall clock repeats or steps back.
///
/// This is the record without a lifetime, as v0.48.0 and earlier wrote it,
/// which a reader judges by its own TTL; the deck writes
/// [`render_claim_record_with_ttl`].
pub fn render_claim_record(
    task: &str,
    host_id: &str,
    claimed_at: chrono::DateTime<chrono::Utc>,
) -> String {
    render_claim_record_body(task, host_id, claimed_at, None)
}

/// [`render_claim_record`] with the writer's `claim_ttl_hours` as the
/// record's lifetime, on a line after the others, so any deck reading the
/// record ages it by the TTL its writer refreshes it against.
pub fn render_claim_record_with_ttl(
    task: &str,
    host_id: &str,
    claimed_at: chrono::DateTime<chrono::Utc>,
    ttl_hours: u32,
) -> String {
    render_claim_record_body(task, host_id, claimed_at, Some(ttl_hours))
}

fn render_claim_record_body(
    task: &str,
    host_id: &str,
    claimed_at: chrono::DateTime<chrono::Utc>,
    ttl_hours: Option<u32>,
) -> String {
    let quote = |value: &str| serde_json::Value::from(value).to_string();
    let claimed_at = claimed_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let ttl = ttl_hours.map_or_else(String::new, |hours| {
        format!("{CLAIM_RECORD_TTL_FIELD}{hours}\n")
    });
    format!(
        "{CLAIM_RECORD_MARKER}\nAn agent was dispatched to work on this issue by dot-agent-deck \
         on machine `{host_id}`, last claimed at {claimed_at}.\n\n\
         {CLAIM_RECORD_TASK_FIELD}{}\n{CLAIM_RECORD_HOST_FIELD}{}\n{CLAIM_RECORD_CLAIMED_AT_FIELD}{}\n{ttl}",
        quote(task),
        quote(host_id),
        quote(&claimed_at)
    )
}

/// The claim time to write when `previous` is the claim time of the record
/// being edited (`None` for a new record, or one with no readable time):
/// `max(now, previous + 1 ms)` when there is a `previous`, and `now` when
/// there is not. So `now` is used only when it is at least one millisecond
/// past `previous` — a `now` that is past `previous` by less than that still
/// yields `previous + 1 ms`. The record renders its time to the millisecond,
/// so the result always renders differently from the time the edited record
/// holds — a wall clock that repeats a millisecond, or is corrected
/// backwards, cannot re-render the same body. A record whose time is in the future therefore
/// carries each re-claim one millisecond further until the clock catches up.
pub fn next_claim_time(
    now: chrono::DateTime<chrono::Utc>,
    previous: Option<chrono::DateTime<chrono::Utc>>,
) -> chrono::DateTime<chrono::Utc> {
    previous
        .and_then(|previous| previous.checked_add_signed(chrono::Duration::milliseconds(1)))
        .map_or(now, |floor| now.max(floor))
}

/// Parse a comment body as a claimant record. Only a body whose first line is
/// [`CLAIM_RECORD_MARKER`] counts, so a reply quoting a record (`> <!-- …`) is
/// not mistaken for one; the task and machine fields must be present. The
/// claim time and the lifetime are optional — records written before they
/// existed have none — and one that does not parse (an RFC 3339 time; a whole
/// number of hours) reads as absent. Lines it does not know are skipped, which
/// is what lets a later build add a field that this one can still read past.
pub fn parse_claim_record(body: &str) -> Option<ClaimRecord> {
    let mut lines = body.trim_start().lines();
    if lines.next()?.trim_end() != CLAIM_RECORD_MARKER {
        return None;
    }
    let mut task = None;
    let mut host = None;
    let mut claimed_at = None;
    let mut ttl_hours = None;
    for line in lines {
        let line = line.trim_end();
        if let Some(value) = line.strip_prefix(CLAIM_RECORD_TASK_FIELD) {
            task = task.or_else(|| serde_json::from_str::<String>(value).ok());
        } else if let Some(value) = line.strip_prefix(CLAIM_RECORD_HOST_FIELD) {
            host = host.or_else(|| serde_json::from_str::<String>(value).ok());
        } else if let Some(value) = line.strip_prefix(CLAIM_RECORD_CLAIMED_AT_FIELD) {
            claimed_at = claimed_at.or_else(|| {
                serde_json::from_str::<String>(value)
                    .ok()
                    .and_then(|time| chrono::DateTime::parse_from_rfc3339(&time).ok())
                    .map(|time| time.with_timezone(&chrono::Utc))
            });
        } else if let Some(value) = line.strip_prefix(CLAIM_RECORD_TTL_FIELD) {
            ttl_hours = ttl_hours.or_else(|| value.parse::<u32>().ok());
        }
    }
    Some(ClaimRecord {
        task: task?,
        host: host?,
        claimed_at,
        ttl_hours,
    })
}

/// This deck's own claimant record, as [`find_own_claim_record`] found it: the
/// comment's id, its body as GitHub returned it, and the claim time that body
/// carries, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnClaimRecord {
    pub id: u64,
    pub body: String,
    pub claimed_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Find this deck's own claimant record in the output of
/// [`issue_comments_argv`], returning its comment id, body and claim time. A
/// record is this deck's only when its body names the same `task` AND the
/// same `host_id` AND the comment was written by the account `user_id` (PRD
/// #421 amendment item 7), whatever claim time it carries, or none. A body is
/// anyone's to copy, so a matching body by someone else is skipped and the
/// search goes on.
///
/// `gh api --paginate` prints one JSON array per page back to back, and every
/// page is parsed even after an own record turns up, so a stream that goes bad
/// on a later page is still an error. Malformed output — and output with no
/// page at all, which is what a listing cut short looks like — is an error,
/// never "no record", which would append a duplicate.
pub fn find_own_claim_record(
    comments_json: &str,
    task: &str,
    host_id: &str,
    user_id: u64,
) -> Result<Option<OwnClaimRecord>, String> {
    let me = ClaimIdentity {
        user_id,
        task,
        host_id,
    };
    Ok(parse_issue_claim_records(comments_json)?
        .into_iter()
        .find(|found| found.is_own(&me))
        .map(|found| OwnClaimRecord {
            id: found.comment_id,
            body: found.body,
            claimed_at: found.record.claimed_at,
        }))
}

// ---------------------------------------------------------------------------
// PRD #421 Phase 3 — claim-aware dispatch: reading every claim record
// ---------------------------------------------------------------------------
//
// The claim is derived from live GitHub at decision time, never from anything
// this deck remembers: every claim record on the issue (found by
// [`CLAIM_RECORD_MARKER`], whoever wrote it), each aged on its OWN clock — the
// comment's `updated_at`, floored by when the claim label was first added —
// and judged on its own (M3.2). Own records are judged on the deck's local
// evidence (its worktree, an open PR), foreign ones on the clock, and only
// when GitHub vouches for their author's standing on the repository. Whenever
// the evidence is missing the record does not gate: a wrongly-stale claim
// costs one duplicate dispatch, a wrongly-live one wedges the issue.

/// The default `claim_ttl_hours`: a week.
pub const DEFAULT_CLAIM_TTL_HOURS: u32 = 168;

/// How far ahead [`check_claim_ttl_against_cron`] walks a task's cron: a year,
/// so every day-of-week and day-of-month pattern, and both DST changes, are in
/// the walk.
const CLAIM_CRON_HORIZON_DAYS: i64 = 366;

/// The shortest window [`check_claim_ttl_against_cron`] looks for gaps
/// longer than. Its walk takes about `2 × horizon / window` steps whatever the
/// cron ([`crate::scheduler::fire_gap_over`]), so the floor bounds the walk at
/// about 35,000 steps. It decides nothing on its own: every TTL from 2 hours
/// up looks for gaps of at least this, and a 1-hour TTL fails for any cron.
const CLAIM_CRON_MIN_WINDOW: chrono::Duration = chrono::Duration::minutes(30);

/// The most steps [`check_claim_ttl_against_cron`] lets its walk take before
/// it gives up on computing the gap: a backstop, about three times what
/// [`CLAIM_CRON_MIN_WINDOW`] lets a walk need.
const CLAIM_CRON_MAX_STEPS: usize = 100_000;

/// The longest `claim_ttl_hours`, and the longest lifetime a claim record is
/// read with whatever lifetime it states: 90 days. A record that names a
/// longer one still lapses 90 days after its last edit, so no account can
/// hold an issue back for longer without editing its record again.
pub const MAX_CLAIM_TTL_HOURS: u32 = 2160;

/// What [`check_claim_ttl_against_cron`] adds to twice the longest gap between
/// fires. The owner refreshes from a fire, but its record was written some
/// time AFTER the fire that dispatched it, and the refresh's own edit lands
/// some time after the check that decided it; an hour absorbs both delays and
/// modest clock skew, so a TTL that passes the check is not outlived by the
/// owner's next refresh by a few seconds.
pub const CLAIM_TTL_HEADROOM: chrono::Duration = chrono::Duration::hours(1);

/// How far in the future a foreign record's clock may read before it stops
/// gating. GitHub's own clock is the reference ([`claim_record_age`]), so a
/// record dated later than that is not evidence of a live claim; a few minutes
/// absorb the seconds between reading GitHub's time and deciding.
pub const CLAIM_FUTURE_TOLERANCE: chrono::Duration = chrono::Duration::minutes(5);

/// How many characters of a claim record's schedule name a log line shows.
/// The name is whatever the comment says, so it is bounded like the machine id.
const CLAIM_TASK_LOG_CHARS: usize = 64;

/// The repository standings whose claim records may gate another deck: the
/// `author_association` values GitHub gives the repository's owner, members of
/// the organization that owns it, and its collaborators. Anyone else can
/// comment on a public issue, so their records never gate.
const TRUSTED_AUTHOR_ASSOCIATIONS: [&str; 3] = ["OWNER", "MEMBER", "COLLABORATOR"];

/// Who this deck is when it reads claim records (PRD #421 Phase 2's "own"
/// rule): the numeric id of the account `gh` is signed in with, the schedule
/// name, and the machine id ([`claim_host_id`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimIdentity<'a> {
    pub user_id: u64,
    pub task: &'a str,
    pub host_id: &'a str,
}

/// One claim record on an issue, as any deck reads it: the comment it lives
/// in, that comment's author and body, what the body says, and when GitHub
/// last saw the comment change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueClaimRecord {
    pub comment_id: u64,
    /// The comment author's `user.id`, which GitHub sets and a body cannot
    /// forge. `None` when the listing names no author.
    pub author_id: Option<u64>,
    /// The comment author's `user.login`, which a skip names. `None` when the
    /// listing names no author or no login.
    pub author_login: Option<String>,
    /// The comment's `author_association`: the author's standing on the
    /// repository, as GitHub reports it. `None` when the listing has none.
    pub author_association: Option<String>,
    pub body: String,
    pub record: ClaimRecord,
    /// The comment's `updated_at`: the record's clock. GitHub moves it on an
    /// edit that changes the body, not on one that repeats it (measured for
    /// M3.1), which is why a refresh renders a new claim time. `None` when the
    /// listing carried no readable time.
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl IssueClaimRecord {
    /// Whether this record is `me`'s own: written by the same account, naming
    /// the same schedule and the same machine id. Anything else is foreign.
    pub fn is_own(&self, me: &ClaimIdentity<'_>) -> bool {
        self.author_id == Some(me.user_id)
            && self.record.task == me.task
            && self.record.host == me.host_id
    }

    /// Whether GitHub reports the author as the repository's owner, a member
    /// of its organization or a collaborator ([`TRUSTED_AUTHOR_ASSOCIATIONS`]).
    /// Only such a foreign record can gate: anyone can comment on a public
    /// issue, and a record that anyone can renew would let a stranger hold an
    /// issue back indefinitely. A missing association is untrusted.
    pub fn author_is_trusted(&self) -> bool {
        self.author_association
            .as_deref()
            .is_some_and(|association| TRUSTED_AUTHOR_ASSOCIATIONS.contains(&association))
    }
}

/// Every claim record in the output of [`issue_comments_argv`], in listing
/// order. A comment counts when [`parse_claim_record`] accepts its body,
/// whoever wrote it.
///
/// `gh api --paginate` prints one JSON array per page back to back, and every
/// page is parsed. Malformed output — and output with no page at all, which is
/// what a listing cut short looks like — is an error, never "no records".
pub fn parse_issue_claim_records(comments_json: &str) -> Result<Vec<IssueClaimRecord>, String> {
    #[derive(serde::Deserialize)]
    struct Author {
        id: u64,
        #[serde(default)]
        login: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct Comment {
        id: u64,
        #[serde(default)]
        body: Option<String>,
        #[serde(default)]
        user: Option<Author>,
        #[serde(default)]
        author_association: Option<String>,
        #[serde(default)]
        updated_at: Option<String>,
    }
    if comments_json.trim().is_empty() {
        return Err("the issue's comments came back empty".to_string());
    }
    let mut records = Vec::new();
    for page in serde_json::Deserializer::from_str(comments_json).into_iter::<Vec<Comment>>() {
        let page = page.map_err(|e| format!("failed to parse the issue's comments: {e}"))?;
        records.extend(page.into_iter().filter_map(|comment| {
            let body = comment.body?;
            let record = parse_claim_record(&body)?;
            let (author_id, author_login) = comment
                .user
                .map_or((None, None), |user| (Some(user.id), user.login));
            Some(IssueClaimRecord {
                comment_id: comment.id,
                author_id,
                author_login,
                author_association: comment.author_association,
                body,
                record,
                updated_at: comment.updated_at.as_deref().and_then(parse_github_time),
            })
        }));
    }
    Ok(records)
}

/// Build the `gh api` argv (arguments after `gh`) listing `issue_number`'s
/// events, where the time [`CLAIM_LABEL`] was added is found. The endpoint
/// starts with `repos/`, so it cannot be read as a flag; like
/// [`issue_comments_argv`], it asks for 100 events per page.
pub fn issue_events_argv(repo: &str, issue_number: u64) -> Vec<String> {
    vec![
        "api".to_string(),
        "--paginate".to_string(),
        format!("repos/{repo}/issues/{issue_number}/events?per_page=100"),
    ]
}

/// What the issue's events say about [`CLAIM_LABEL`], from the output of
/// [`issue_events_argv`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimLabelState {
    /// Whether the issue carries the label now: its latest `labeled` or
    /// `unlabeled` event for the label is a `labeled` one. The claim gates
    /// only while it does, so removing the label by hand releases the claim.
    pub present: bool,
    /// When the label was first added: the earliest `labeled` event for it,
    /// `None` when there is none.
    ///
    /// The earliest, because it is the floor of every record's clock: the
    /// claim cannot have existed before it. A later add cannot move it, so one
    /// deck's fresh claim cannot hold another deck's dead record open — and
    /// GitHub records no event for a redundant add of a label already present
    /// (measured for M3.1), so re-claims leave it alone anyway.
    pub added_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Read [`ClaimLabelState`] from the output of [`issue_events_argv`]. Of the
/// label's `labeled` and `unlabeled` events the one with the latest time
/// decides whether it is present, a later one in the listing winning a tie; an
/// event with no readable time is left out. Malformed or empty output is an
/// error, like [`parse_issue_claim_records`].
pub fn parse_claim_label_state(events_json: &str) -> Result<ClaimLabelState, String> {
    #[derive(serde::Deserialize)]
    struct Label {
        name: String,
    }
    #[derive(serde::Deserialize)]
    struct Event {
        #[serde(default)]
        event: Option<String>,
        #[serde(default)]
        label: Option<Label>,
        #[serde(default)]
        created_at: Option<String>,
    }
    if events_json.trim().is_empty() {
        return Err("the issue's events came back empty".to_string());
    }
    let mut added_at: Option<chrono::DateTime<chrono::Utc>> = None;
    let mut latest: Option<(chrono::DateTime<chrono::Utc>, bool)> = None;
    for page in serde_json::Deserializer::from_str(events_json).into_iter::<Vec<Event>>() {
        let page = page.map_err(|e| format!("failed to parse the issue's events: {e}"))?;
        for event in page {
            let labeled = match event.event.as_deref() {
                Some("labeled") => true,
                Some("unlabeled") => false,
                _ => continue,
            };
            if event.label.as_ref().map(|label| label.name.as_str()) != Some(CLAIM_LABEL) {
                continue;
            }
            let Some(time) = event.created_at.as_deref().and_then(parse_github_time) else {
                continue;
            };
            if labeled {
                added_at = Some(added_at.map_or(time, |earliest| earliest.min(time)));
            }
            if latest.is_none_or(|(latest, _)| time >= latest) {
                latest = Some((time, labeled));
            }
        }
    }
    Ok(ClaimLabelState {
        present: latest.is_some_and(|(_, labeled)| labeled),
        added_at,
    })
}

/// Parse a GitHub API timestamp (RFC 3339) as UTC; anything else is `None`.
fn parse_github_time(time: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(time)
        .ok()
        .map(|time| time.with_timezone(&chrono::Utc))
}

/// A record's age at `now`: since the later of its own last edit and the time
/// the claim label was added (the floor). `None` when neither time is known —
/// such a record has no clock, so it cannot be fresh. Both times are GitHub's,
/// so `now` should be GitHub's clock too (the run reads it from the account
/// lookup's `Date`); an age below zero means the record is dated after `now`,
/// which [`claim_record_gates`] does not take as a live claim beyond
/// [`CLAIM_FUTURE_TOLERANCE`].
pub fn claim_record_age(
    now: chrono::DateTime<chrono::Utc>,
    updated_at: Option<chrono::DateTime<chrono::Utc>>,
    label_added_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<chrono::Duration> {
    let last_active = updated_at.max(label_added_at)?;
    Some(now.signed_duration_since(last_active))
}

/// Whether one claim record gates dispatch (PRD #421 M3.2), per record:
///
/// | record | evidence | gates |
/// | --- | --- | --- |
/// | own | `own_work_alive` false (no worktree, no open PR) | no |
/// | own | `own_work_alive` true | yes |
/// | foreign | author not `trusted` | no |
/// | foreign | age unknown, at least `ttl`, or dated more than [`CLAIM_FUTURE_TOLERANCE`] ahead | no |
/// | foreign, `trusted` | age below `ttl` | yes |
///
/// The own-and-alive row never decides anything in practice: a worktree or an
/// open PR skips the issue before claims are read. `trusted` is
/// [`IssueClaimRecord::author_is_trusted`].
pub fn claim_record_gates(
    own: bool,
    own_work_alive: bool,
    trusted: bool,
    age: Option<chrono::Duration>,
    ttl: chrono::Duration,
) -> bool {
    if own {
        own_work_alive
    } else {
        trusted && age.is_some_and(|age| age >= -CLAIM_FUTURE_TOLERANCE && age < ttl)
    }
}

/// The claimant a skip names: the schedule and machine id the live record
/// carries, the GitHub account that wrote it, and its age (`None` when it has
/// no clock — only possible for an own record, which gates on evidence rather
/// than age).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claimant {
    pub task: String,
    pub host: String,
    pub login: Option<String>,
    pub age: Option<chrono::Duration>,
}

impl std::fmt::Display for Claimant {
    /// The schedule and machine come from a comment body its author chose, so
    /// both are escaped and bounded before they reach the log; the login,
    /// which GitHub sets, is escaped and bounded all the same.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use crate::config_validation::{bound_chars, escape_field_for_log};
        write!(
            f,
            "schedule {:?} on machine {} (GitHub account {}), ",
            bound_chars(&self.task, CLAIM_TASK_LOG_CHARS),
            escape_field_for_log(&self.host, 64),
            self.login.as_deref().map_or_else(
                || "unknown".to_string(),
                |login| escape_field_for_log(login, 64)
            )
        )?;
        match self.age {
            // A record dated slightly ahead of GitHub's clock reads as `0s`.
            Some(age) => write!(f, "last active {} ago", format_claim_duration(age)),
            None => write!(f, "age unknown"),
        }
    }
}

/// The claimant whose record keeps the issue claimed, or `None` when no record
/// on it is live (PRD #421 M3.2: the claim gates iff at least ONE record is
/// individually live — each aged on its own clock, never an aggregate — and,
/// when foreign, written by an author GitHub vouches for). Of several live
/// records the most recently active one is named. Each record is judged by
/// the lifetime it states ([`ClaimRecord::ttl`]), so decks with different
/// `claim_ttl_hours` read one another's claims the way their writers refresh
/// them; `ttl`, the reader's own, judges records that state none.
pub fn live_claimant(
    records: &[IssueClaimRecord],
    me: &ClaimIdentity<'_>,
    own_work_alive: bool,
    label_added_at: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
    ttl: chrono::Duration,
) -> Option<Claimant> {
    records
        .iter()
        .filter_map(|found| {
            let age = claim_record_age(now, found.updated_at, label_added_at);
            claim_record_gates(
                found.is_own(me),
                own_work_alive,
                found.author_is_trusted(),
                age,
                found.record.ttl(ttl),
            )
            .then(|| Claimant {
                task: found.record.task.clone(),
                host: found.record.host.clone(),
                login: found.author_login.clone(),
                age,
            })
        })
        .min_by_key(|claimant| claimant.age.unwrap_or(chrono::Duration::MAX))
}

/// Whether the owner's skip should refresh its own record now (PRD #421
/// M3.1): when the record is at least half `ttl` old, so a live claim gets one
/// refresh inside every TTL while a deck firing every few minutes does not edit
/// it on every fire. `last_edit` is the own record's `updated_at`; `None` — no
/// own record, or one with no readable time — is due, so the liveness the
/// worktree proves gets published.
pub fn claim_refresh_due(
    now: chrono::DateTime<chrono::Utc>,
    last_edit: Option<chrono::DateTime<chrono::Utc>>,
    ttl: chrono::Duration,
) -> bool {
    last_edit.is_none_or(|last_edit| now.signed_duration_since(last_edit) >= ttl / 2)
}

/// Whether this deck's own `record` should be rewritten although it is not yet
/// half a TTL old: it states a lifetime other than the deck's `ttl_hours`, or
/// none. Other decks age the record by the lifetime it states, so a TTL raised
/// since the record was written would otherwise let it lapse for them before
/// the owner's next refresh fell due.
pub fn claim_record_ttl_outdated(record: &ClaimRecord, ttl_hours: u32) -> bool {
    record.ttl_hours != Some(ttl_hours)
}

/// Check `claim_ttl_hours` itself: at least 1 and at most
/// [`MAX_CLAIM_TTL_HOURS`].
pub fn validate_claim_ttl_hours(ttl_hours: u32) -> Result<(), String> {
    if ttl_hours < 1 {
        return Err(
            "issue_dispatch claim_ttl_hours must be at least 1 (a claim that is never \
             live gates nothing)"
                .to_string(),
        );
    }
    if ttl_hours > MAX_CLAIM_TTL_HOURS {
        return Err(format!(
            "issue_dispatch claim_ttl_hours must be at most {MAX_CLAIM_TTL_HOURS} (90 days); \
             got {ttl_hours}"
        ));
    }
    Ok(())
}

/// Check that a claim TTL of `ttl_hours` can be kept current by a task firing
/// on `cron` (PRD #421 M3.1): the TTL must be at least twice the longest gap
/// between consecutive fires, walked from `from` over a year, plus
/// [`CLAIM_TTL_HEADROOM`]. The owner refreshes its record only from a fire,
/// and only once the record is half a TTL old, so a gap wider than half the
/// TTL can let a live claim expire; the headroom covers the record being
/// written after its fire and the refresh landing after its check.
/// `Err` says why gating cannot be trusted for the task, naming the TTL the
/// cron needs — including when the gap cannot be computed (an invalid cron or
/// one that stops firing).
///
/// The walk looks only for gaps longer than the TTL allows
/// ([`crate::scheduler::fire_gap_over`]), so a cron firing every minute costs
/// no more to check than one firing daily.
pub fn check_claim_ttl_against_cron<Z: chrono::TimeZone>(
    cron: &str,
    ttl_hours: u32,
    from: &chrono::DateTime<Z>,
) -> Result<(), String> {
    use crate::scheduler::FireGap;
    let schedule = crate::scheduler::parse_cron(cron).map_err(|e| e.to_string())?;
    let ttl = chrono::Duration::hours(i64::from(ttl_hours));
    // The longest gap the TTL allows: `ttl >= 2 × gap + headroom`.
    let allowed = (ttl - CLAIM_TTL_HEADROOM) / 2;
    let window = allowed.max(CLAIM_CRON_MIN_WINDOW);
    let found = crate::scheduler::fire_gap_over(
        &schedule,
        from,
        chrono::Duration::days(CLAIM_CRON_HORIZON_DAYS),
        window,
        CLAIM_CRON_MAX_STEPS,
    )
    .ok_or_else(|| {
        format!(
            "the longest gap between fires of cron {cron:?} cannot be computed \
             (it stops firing within a year)"
        )
    })?;
    let (gap, needed_hours) = match found {
        FireGap::Longest(gap) => {
            let needed = gap * 2 + CLAIM_TTL_HEADROOM;
            if ttl >= needed {
                return Ok(());
            }
            (
                format_claim_duration(gap),
                (needed.num_seconds() + 3599) / 3600,
            )
        }
        FireGap::Within if window == allowed => return Ok(()),
        // Only a TTL under twice the floor plus the headroom gets here, and
        // every gap is at most the floor, so 2 hours is enough.
        FireGap::Within => (
            format!("at most {}", format_claim_duration(window)),
            (window * 2 + CLAIM_TTL_HEADROOM).num_hours(),
        ),
    };
    let remedy = if needed_hours > i64::from(MAX_CLAIM_TTL_HOURS) {
        format!(
            "the {needed_hours} hours it needs are more than the {MAX_CLAIM_TTL_HOURS}-hour \
             maximum, so claim gating needs a cron that fires more often"
        )
    } else {
        format!("set claim_ttl_hours to at least {needed_hours}")
    };
    Err(format!(
        "claim_ttl_hours = {ttl_hours} is less than twice the longest gap between fires \
         of cron {cron:?} ({gap}) plus an hour, so a live claim could expire before its \
         owner's next fire refreshes it; {remedy}"
    ))
}

/// Render a duration for a log line: whole days, hours, minutes and seconds,
/// leaving out zero parts (`3d 2h`, `45m`, `0s`).
fn format_claim_duration(duration: chrono::Duration) -> String {
    let total = duration.num_seconds().max(0);
    let parts = [
        (total / 86_400, "d"),
        (total % 86_400 / 3600, "h"),
        (total % 3600 / 60, "m"),
        (total % 60, "s"),
    ];
    let rendered: Vec<String> = parts
        .iter()
        .filter(|(value, _)| *value > 0)
        .map(|(value, unit)| format!("{value}{unit}"))
        .collect();
    if rendered.is_empty() {
        "0s".to_string()
    } else {
        rendered.join(" ")
    }
}

/// The character budget of a claim diagnostic BEFORE escaping: longer text is
/// cut to this many characters, with a note of the full length. Escaping comes
/// after the cut, and each control or bidi character it escapes grows into a
/// sequence of up to eight characters, so the text that reaches a notification
/// or the log can be longer than this — still bounded, and never split inside
/// an escape sequence.
const MAX_CLAIM_ERROR_CHARS: usize = 500;

/// Make a claim diagnostic safe to put in a notification or the log (PRD #421
/// amendment item 11). Its text comes partly from `gh`'s stderr, which echoes
/// server responses, so before it goes anywhere:
///
/// - token-shaped strings (`ghp_…`, `gho_…`, `ghu_…`, `ghs_…`, `ghr_…`,
///   `github_pat_…`) are replaced with `<redacted>` — first, so that cutting
///   the text short cannot leave half a token behind. A prefix is matched
///   wherever it appears, including glued to the word before it
///   (`token_ghp_…`), so an ordinary word that happens to contain one is
///   redacted too: over-redacting a diagnostic costs less than leaking a token;
/// - the text is cut to [`MAX_CLAIM_ERROR_CHARS`] characters;
/// - control characters (C0 including ESC, BEL and CR, DEL, C1) and bidi
///   overrides are escaped, so the text cannot forge or rewrite a log line.
pub fn sanitize_claim_error(text: &str) -> String {
    static TOKEN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?:gh[pousr]_|github_pat_)[A-Za-z0-9_]+")
            .expect("the token pattern is valid")
    });
    let redacted = TOKEN.replace_all(text, "<redacted>");
    crate::config_validation::escape_field_for_log(&redacted, MAX_CLAIM_ERROR_CHARS)
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
/// separate state file: the signals are the per-issue worktree, an open PR,
/// the `in-progress` label (PRD #421 M1.2) and, when `claim_gating` is on, a
/// live claim record from another deck (PRD #421 M3.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchDecision {
    /// Provision the worktree and spawn an agent for this issue.
    Dispatch,
    /// Skip — the issue is already being worked on, for the reason given.
    Skip(SkipCause),
}

/// Why an issue was skipped, so each skip can say so (PRD #421 M3.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipCause {
    /// The per-issue worktree exists.
    WorktreeExists,
    /// An open PR's head branch is `agent/issue-<n>`.
    OpenPr,
    /// The `in-progress` label is present on the issue, whoever applied it
    /// (PRD #421 M1.2). The payload is the deck's own claim-comment text when
    /// one is discoverable among the issue's comments, `None` when the label
    /// was applied by a human or an external tool or no claim comment can be
    /// found. The pure [`dispatch_decision`] cannot know it and returns `None`;
    /// the caller (`issue_dispatch_run`) fills it from the issue's comments
    /// before the skip is reported. Rendered through
    /// `scheduler::sanitize_claimant_for_render`: the text is an untrusted
    /// GitHub comment body.
    LabelInProgress(Option<String>),
    /// Another deck's claim record on the issue is live.
    Claimed(Claimant),
    /// Another run created the worktree between the checks and this run's
    /// own creation of it.
    ConcurrentCreator,
}

impl std::fmt::Display for SkipCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkipCause::WorktreeExists => write!(f, "its worktree exists"),
            SkipCause::OpenPr => write!(f, "an open pull request targets its branch"),
            SkipCause::LabelInProgress(Some(comment)) => write!(
                f,
                "it carries the `in-progress` label ({})",
                crate::scheduler::sanitize_claimant_for_render(comment)
            ),
            SkipCause::LabelInProgress(None) => {
                write!(
                    f,
                    "it carries the `in-progress` label, no claimant recorded"
                )
            }
            SkipCause::Claimed(claimant) => write!(f, "claimed by {claimant}"),
            SkipCause::ConcurrentCreator => {
                write!(f, "another run created its worktree first")
            }
        }
    }
}

/// Decide dispatch-vs-skip from the idempotency signals, in the order they
/// are evaluated: the per-issue worktree exists (primary), an open PR's HEAD
/// branch is `agent/issue-<n>` (secondary), the `in-progress` label is
/// present (tertiary, PRD #421 M1.2) — honoured regardless of who applied it
/// — and a live claim on GitHub (`claimant`, PRD #421 M3.2), which the caller
/// resolves only when the first three leave the issue open — so a failure to
/// read claims can never turn a clean local skip into a failure — and passes
/// as `None` when gating is off or the claims could not be read. Only when
/// all four are absent do we dispatch.
pub fn dispatch_decision(
    worktree_exists: bool,
    open_pr_with_matching_head: bool,
    label_in_progress: bool,
    claimant: Option<Claimant>,
) -> DispatchDecision {
    if worktree_exists {
        DispatchDecision::Skip(SkipCause::WorktreeExists)
    } else if open_pr_with_matching_head {
        DispatchDecision::Skip(SkipCause::OpenPr)
    } else if label_in_progress {
        DispatchDecision::Skip(SkipCause::LabelInProgress(None))
    } else if let Some(claimant) = claimant {
        DispatchDecision::Skip(SkipCause::Claimed(claimant))
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
/// claimant name is (control characters, backticks) and — mirroring the
/// "forcibly released from" clause immediately above it — wrapped in its
/// own code span before it reaches a public comment body: `reason` was the
/// ONLY untrusted value in this module rendered OUTSIDE a code span, so
/// `--reason "ping @someone"` used to post a live GitHub mention and
/// `--reason "#421"` used to create a real cross-reference (reviewer/auditor
/// fix round, PR #582). A `reason` that is empty, or entirely whitespace
/// once sanitized, is treated as if `--reason` had never been passed at all
/// — otherwise `--reason ""` would render as the meaningless ", reason: ".
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
        let sanitized = sanitize_claimant_name(reason);
        let trimmed = sanitized.trim();
        if !trimmed.is_empty() {
            body.push_str(&format!(", reason: `{trimmed}`"));
        }
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

/// Issue #172 (security audit of PRD #421, finding A11/F11, deliberately kept
/// out of that PR): the deck deliberately never parses a triage agent's
/// response — see [`triage_instruction`]'s doc comment — but that leaves no
/// record of what the agent was even ASKED to do, so a `gh` write the agent
/// was authorised to make is unauditable after the fact. Call this once, at
/// the same call site that appends [`triage_instruction`] to a dispatched
/// prompt, so every dispatch that grants the triage instruction leaves a
/// durable trace behind (`deck.log` via `tracing`, append-only and mode-0600
/// hardened — see `open_deck_log_file` in `main.rs`) — never conditional on
/// whether the agent's own writes succeed or even happen.
///
/// Deliberately narrow: this records what was ASKED, not what happened —
/// observing the agent's actual `gh` calls is the other, out-of-scope half of
/// #172's proposal. Deliberately carries `instruction` (this module's own
/// fixed, deck-authored template text) and never the issue's own body/title/
/// comments — those are third-party text, and folding them into the audit
/// record would just hand it a second injection/leak sink instead of fixing
/// the first one.
pub fn record_triage_instruction_issued(repo: &str, issue: u64) {
    let timestamp = chrono::Utc::now().to_rfc3339();
    let instruction = triage_instruction();
    // `issue` and `repo` are also folded straight into the message text
    // (not just carried as structured fields) so this record's issue/repo
    // are found by a plain substring search of the rendered log line,
    // independent of how any given `tracing-subscriber` formatter happens to
    // render a numeric/string field.
    tracing::info!(
        target: "dot_agent_deck::triage_audit",
        repo = %repo,
        issue,
        timestamp = %timestamp,
        instruction = %instruction,
        "issue-dispatch: triage instruction issued to dispatched agent for issue #{issue} of {repo}"
    );
}

/// Builds the prompt delivered to a dispatched agent: `{{issue_number}}`
/// substitution, plus (when `triage` is on) the appended triage instruction
/// and its audit record — see [`record_triage_instruction_issued`]. Extracted
/// from the dispatch-run call site (issue #172) so the triage gate — append
/// the instruction if and only if an audit record is also fired for it — is
/// unit-testable on its own, without the surrounding `gh`/`git`/spawn
/// machinery `dispatch_one_issue` needs for everything else it does.
pub fn build_dispatch_prompt(
    prompt_template: &str,
    issue: u64,
    repo: &str,
    triage: bool,
) -> String {
    let mut prompt = substitute_issue_number(prompt_template, issue);
    if triage {
        prompt.push_str("\n\n");
        prompt.push_str(&triage_instruction());
        record_triage_instruction_issued(repo, issue);
    }
    prompt
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

    // --- PRD #421 Phase 2: the dispatch claim ---

    #[test]
    fn claim_label_provision_argv_forces_the_deck_owned_label() {
        assert_eq!(
            claim_label_provision_argv("vfarcic/dot-ai"),
            vec![
                "label",
                "create",
                "agent-deck/claimed",
                "--repo",
                "vfarcic/dot-ai",
                "--force",
                "--color",
                CLAIM_LABEL_COLOR,
                "--description",
                CLAIM_LABEL_DESCRIPTION,
            ]
        );
        assert_eq!(CLAIM_LABEL_COLOR.len(), 6);
        assert!(CLAIM_LABEL_COLOR.chars().all(|c| c.is_ascii_hexdigit()));
        // GitHub caps a label description at 100 characters.
        assert!(CLAIM_LABEL_DESCRIPTION.chars().count() <= 100);
    }

    #[test]
    fn claim_issue_argv_puts_the_issue_after_end_of_options() {
        assert_eq!(
            claim_issue_argv("vfarcic/dot-ai", 17),
            vec![
                "issue",
                "edit",
                "--repo",
                "vfarcic/dot-ai",
                "--add-label",
                "agent-deck/claimed",
                "--",
                "17",
            ]
        );
    }

    #[test]
    fn claim_record_argvs_address_the_issue_and_comment_endpoints() {
        assert_eq!(
            issue_comments_argv("o/r", 7),
            vec![
                "api",
                "--paginate",
                "repos/o/r/issues/7/comments?per_page=100"
            ]
        );
        assert_eq!(
            create_claim_record_argv("o/r", 7, "hello\nworld"),
            vec![
                "api",
                "--method",
                "POST",
                "repos/o/r/issues/7/comments",
                "-f",
                "body=hello\nworld",
            ]
        );
        assert_eq!(
            edit_claim_record_argv("o/r", 4242, "hello"),
            vec![
                "api",
                "--method",
                "PATCH",
                "repos/o/r/issues/comments/4242",
                "-f",
                "body=hello",
            ]
        );
    }

    fn at(rfc3339: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    fn t0() -> chrono::DateTime<chrono::Utc> {
        at("2026-10-22T05:00:13.000Z")
    }

    #[test]
    fn claim_record_round_trips_task_host_and_claim_time() {
        let when = at("2026-10-22T05:00:13.250Z");
        let body = render_claim_record("dispatch-task", "build-box", when);
        assert!(body.starts_with(CLAIM_RECORD_MARKER));
        assert!(body.contains("dispatch-task") && body.contains("build-box"));
        assert!(
            body.contains("- Claimed at: \"2026-10-22T05:00:13.250Z\"\n"),
            "{body}"
        );
        assert!(
            body.lines()
                .nth(1)
                .unwrap()
                .contains("2026-10-22T05:00:13.250Z"),
            "the sentence says when it was last claimed: {body}"
        );
        assert_eq!(
            parse_claim_record(&body),
            Some(ClaimRecord {
                task: "dispatch-task".into(),
                host: "build-box".into(),
                claimed_at: Some(when),
                ttl_hours: None,
            })
        );
        // Awkward task names survive the round trip on a single field line.
        let tricky = "Issues vfarcic/dot-ai \"quoted\"\nsecond line";
        assert_eq!(
            parse_claim_record(&render_claim_record(tricky, "h", when)),
            Some(ClaimRecord {
                task: tricky.into(),
                host: "h".into(),
                claimed_at: Some(when),
                ttl_hours: None,
            })
        );
    }

    #[test]
    fn claim_record_round_trips_its_writers_lifetime_on_a_line_of_its_own() {
        let when = at("2026-10-22T05:00:13.250Z");
        let body = render_claim_record_with_ttl("dispatch-task", "build-box", when, 48);
        assert!(
            body.ends_with("- Claimed at: \"2026-10-22T05:00:13.250Z\"\n- Lifetime (hours): 48\n"),
            "{body}"
        );
        assert_eq!(
            parse_claim_record(&body),
            Some(ClaimRecord {
                task: "dispatch-task".into(),
                host: "build-box".into(),
                claimed_at: Some(when),
                ttl_hours: Some(48),
            })
        );
        // Without a lifetime the body is exactly what v0.48.0 wrote.
        assert_eq!(
            render_claim_record("dispatch-task", "build-box", when),
            body.replace("- Lifetime (hours): 48\n", "")
        );
        // A lifetime that is not a whole number of hours reads as absent.
        for garbled in ["\"48\"", "-1", "4.5", "a week", "99999999999"] {
            let body = body.replace(": 48\n", &format!(": {garbled}\n"));
            assert_eq!(
                parse_claim_record(&body).unwrap().ttl_hours,
                None,
                "{garbled}"
            );
        }
    }

    /// `parse_claim_record` and the own-record match exactly as v0.48.0 — the
    /// last release before records carried a lifetime — shipped them, frozen
    /// here so the tolerance below is checked against the rules older decks
    /// actually run.
    fn parse_claim_record_as_v0_48(
        body: &str,
    ) -> Option<(String, String, Option<chrono::DateTime<chrono::Utc>>)> {
        let mut lines = body.trim_start().lines();
        if lines.next()?.trim_end() != "<!-- dot-agent-deck:claim -->" {
            return None;
        }
        let mut task = None;
        let mut host = None;
        let mut claimed_at = None;
        for line in lines {
            let line = line.trim_end();
            if let Some(value) = line.strip_prefix("- Task: ") {
                task = task.or_else(|| serde_json::from_str::<String>(value).ok());
            } else if let Some(value) = line.strip_prefix("- Machine: ") {
                host = host.or_else(|| serde_json::from_str::<String>(value).ok());
            } else if let Some(value) = line.strip_prefix("- Claimed at: ") {
                claimed_at = claimed_at.or_else(|| {
                    serde_json::from_str::<String>(value)
                        .ok()
                        .and_then(|time| chrono::DateTime::parse_from_rfc3339(&time).ok())
                        .map(|time| time.with_timezone(&chrono::Utc))
                });
            }
        }
        Some((task?, host?, claimed_at))
    }

    #[test]
    fn a_record_carrying_a_lifetime_reads_the_same_to_v0_48_rules() {
        // v0.48.0 finds its own record by task and machine (and author), and
        // re-claims from the claim time it reads: a record a newer deck wrote
        // must give it all three, or it would post a duplicate record.
        let when = at("2026-10-22T05:00:13.250Z");
        let tricky = "Issues vfarcic/dot-ai \"quoted\"\nsecond line";
        for task in ["dispatch-task", tricky] {
            let body = render_claim_record_with_ttl(task, "h1-0123456789abcdef", when, 48);
            assert_eq!(
                parse_claim_record_as_v0_48(&body),
                Some((
                    task.to_string(),
                    "h1-0123456789abcdef".to_string(),
                    Some(when)
                )),
                "{body}"
            );
        }
        // And the frozen copy is faithful: it reads today's lifetime-less
        // record the way the current parser does.
        let legacy = render_claim_record("t", "h", when);
        let current = parse_claim_record(&legacy).unwrap();
        assert_eq!(
            parse_claim_record_as_v0_48(&legacy),
            Some((current.task, current.host, current.claimed_at))
        );
    }

    #[test]
    fn a_record_is_read_with_its_writers_lifetime_kept_within_bounds() {
        let record = |ttl_hours| ClaimRecord {
            task: "t".into(),
            host: "h".into(),
            claimed_at: None,
            ttl_hours,
        };
        let reader = hours(24);
        assert_eq!(
            record(None).ttl(reader),
            reader,
            "older records: the reader's own"
        );
        assert_eq!(record(Some(3)).ttl(reader), hours(3));
        assert_eq!(record(Some(168)).ttl(reader), hours(168));
        assert_eq!(record(Some(0)).ttl(reader), hours(1));
        assert_eq!(record(Some(MAX_CLAIM_TTL_HOURS)).ttl(reader), hours(2160));
        assert_eq!(record(Some(u32::MAX)).ttl(reader), hours(2160));
    }

    #[test]
    fn an_own_record_is_rewritten_when_its_lifetime_is_not_the_decks() {
        let record = |ttl_hours| ClaimRecord {
            task: "t".into(),
            host: "h".into(),
            claimed_at: None,
            ttl_hours,
        };
        assert!(!claim_record_ttl_outdated(&record(Some(168)), 168));
        assert!(claim_record_ttl_outdated(&record(Some(24)), 168));
        assert!(claim_record_ttl_outdated(&record(Some(168)), 24));
        assert!(claim_record_ttl_outdated(&record(None), 168));
    }

    #[test]
    fn a_re_claim_renders_a_body_that_differs_from_the_record_it_edits() {
        // GitHub moves `updated_at` only when an edit changes the body. A
        // later millisecond renders a different body; `next_claim_time` is
        // what makes the re-claim's time a later one.
        assert_ne!(
            render_claim_record("t", "h", at("2026-10-22T05:00:13.000Z")),
            render_claim_record("t", "h", at("2026-10-22T05:00:13.001Z")),
        );
    }

    #[test]
    fn parse_claim_record_accepts_a_record_without_a_claim_time() {
        // The shape earlier builds wrote, with no `- Claimed at:` line.
        let legacy = format!(
            "{CLAIM_RECORD_MARKER}\nAn agent was dispatched to work on this issue by \
             dot-agent-deck on machine `h1-0123456789abcdef`.\n\n\
             - Task: \"t\"\n- Machine: \"h1-0123456789abcdef\"\n"
        );
        assert_eq!(
            parse_claim_record(&legacy),
            Some(ClaimRecord {
                task: "t".into(),
                host: "h1-0123456789abcdef".into(),
                claimed_at: None,
                ttl_hours: None,
            })
        );
        // A claim time that is not a time reads as absent, not as no record.
        let garbled = format!("{legacy}- Claimed at: \"yesterday\"\n");
        assert_eq!(parse_claim_record(&garbled).unwrap().claimed_at, None);
    }

    #[test]
    fn parse_claim_record_rejects_non_records() {
        // No marker.
        assert_eq!(parse_claim_record("- Task: \"t\"\n- Host: \"h\""), None);
        // A reply quoting a record is not a record.
        let quoted = render_claim_record("t", "h", t0())
            .lines()
            .map(|l| format!("> {l}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(parse_claim_record(&quoted), None);
        // The marker alone, or a missing field, is not a record.
        assert_eq!(parse_claim_record(CLAIM_RECORD_MARKER), None);
        assert_eq!(
            parse_claim_record(&format!("{CLAIM_RECORD_MARKER}\n- Task: \"t\"")),
            None
        );
    }

    const ME: u64 = 12345;

    #[test]
    fn claim_host_id_is_a_stable_versioned_digest_of_the_host_name() {
        let id = claim_host_id("build-box");
        assert_eq!(id, claim_host_id("build-box"), "stable across calls");
        assert!(id.starts_with("h1-"), "{id}");
        let hex = &id["h1-".len()..];
        assert_eq!(hex.len(), 16, "{id}");
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert!(
            !id.contains("build-box"),
            "the raw name stays out of the id"
        );
        assert_ne!(id, claim_host_id("build-box2"));
        // Domain-separated: not a prefix of the plain SHA-256 of the name.
        use sha2::{Digest, Sha256};
        let plain: String = Sha256::digest(b"build-box")
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert!(!plain.starts_with(hex));
    }

    #[test]
    fn authenticated_user_id_is_read_from_gh_api_user() {
        assert_eq!(authenticated_user_argv(), vec!["api", "--include", "user"]);
        // A body with no headers: the id, and no server clock.
        assert_eq!(
            parse_authenticated_user(r#"{"id":12345,"login":"deck-bot"}"#),
            Ok(AuthenticatedUser {
                id: 12345,
                server_date: None,
            })
        );
        assert!(parse_authenticated_user(r#"{"login":"deck-bot"}"#).is_err());
        assert!(parse_authenticated_user("").is_err());
        assert!(parse_authenticated_user("not json").is_err());
    }

    #[test]
    fn the_account_lookup_reads_githubs_date_from_the_included_headers() {
        // What `gh api --include user` printed (gh 2.102): an LF-ended status
        // line, CRLF-ended headers, a blank CRLF line, then the body.
        let output = "HTTP/2.0 200 OK\nContent-Type: application/json\r\n\
                      Date: Fri, 23 Oct 2026 17:55:59 GMT\r\nX-Ratelimit-Limit: 5000\r\n\r\n\
                      {\"login\":\"deck-bot\",\"id\":12345}";
        assert_eq!(
            parse_authenticated_user(output),
            Ok(AuthenticatedUser {
                id: 12345,
                server_date: Some(at("2026-10-23T17:55:59Z")),
            })
        );
        // Header names are case-insensitive; LF-only line ends work too.
        let lf = "HTTP/1.1 200 OK\ndate: Fri, 23 Oct 2026 17:55:59 GMT\n\n{\"id\":7}";
        assert_eq!(
            parse_authenticated_user(lf).unwrap().server_date,
            Some(at("2026-10-23T17:55:59Z"))
        );
        // A missing or unreadable Date leaves the id readable.
        for headers in ["", "Date: yesterday\r\n"] {
            let output = format!("HTTP/2.0 200 OK\n{headers}\r\n{{\"id\":7}}");
            assert_eq!(
                parse_authenticated_user(&output),
                Ok(AuthenticatedUser {
                    id: 7,
                    server_date: None,
                }),
                "{output:?}"
            );
        }
        // Headers with no body after them are an error, not an account.
        assert!(parse_authenticated_user("HTTP/2.0 200 OK\nDate: x\r\n").is_err());
    }

    #[test]
    fn claim_record_names_the_machine_id_not_the_host_name() {
        let host_id = claim_host_id("secret-laptop");
        let body = render_claim_record("dispatch-task", &host_id, t0());
        assert!(!body.contains("secret-laptop"), "{body}");
        let sentence = body.lines().nth(1).unwrap();
        assert!(sentence.contains(&host_id), "{sentence}");
        assert!(!sentence.contains("dispatch-task"), "{sentence}");
    }

    fn comment(id: u64, user: u64, body: &str) -> serde_json::Value {
        serde_json::json!({"id": id, "user": {"id": user, "login": "x"}, "body": body})
    }

    /// The comment id [`find_own_claim_record`] picks, for the tests that are
    /// about which comment matches rather than what it carries.
    fn own_id(json: &str, task: &str, host: &str, user: u64) -> Result<Option<u64>, String> {
        find_own_claim_record(json, task, host, user).map(|own| own.map(|record| record.id))
    }

    #[test]
    fn find_own_claim_record_requires_same_task_host_and_author() {
        let own = render_claim_record("task", "host", t0());
        let other_host = render_claim_record("task", "elsewhere", t0());
        let other_task = render_claim_record("another", "host", t0());
        let json = serde_json::json!([
            comment(1, ME, "plain comment"),
            comment(2, ME, &other_host),
            comment(3, ME, &other_task),
            comment(4, ME, &own),
            {"id": 5, "body": null, "user": null},
        ])
        .to_string();
        assert_eq!(own_id(&json, "task", "host", ME), Ok(Some(4)));
        assert_eq!(own_id(&json, "task", "nowhere", ME), Ok(None));
        assert_eq!(own_id(&json, "task", "host", 999), Ok(None));
        assert_eq!(own_id("[]", "task", "host", ME), Ok(None));
    }

    #[test]
    fn find_own_claim_record_ignores_the_claim_time() {
        // A record claimed at another time, or written before the field
        // existed, is still this deck's own: the time is not part of the match.
        let earlier = render_claim_record("task", "host", at("2025-01-01T00:00:00.000Z"));
        let json = serde_json::json!([comment(4, ME, &earlier)]).to_string();
        assert_eq!(own_id(&json, "task", "host", ME), Ok(Some(4)));
        let legacy = format!(
            "{CLAIM_RECORD_MARKER}\nAn agent was dispatched.\n\n- Task: \"task\"\n- Machine: \"host\"\n"
        );
        let json = serde_json::json!([comment(6, ME, &legacy)]).to_string();
        assert_eq!(own_id(&json, "task", "host", ME), Ok(Some(6)));
    }

    #[test]
    fn find_own_claim_record_returns_the_record_body_and_claim_time() {
        let when = at("2026-10-22T05:00:13.250Z");
        let own = render_claim_record("task", "host", when);
        let json = serde_json::json!([comment(4, ME, &own)]).to_string();
        assert_eq!(
            find_own_claim_record(&json, "task", "host", ME),
            Ok(Some(OwnClaimRecord {
                id: 4,
                body: own,
                claimed_at: Some(when),
            }))
        );
    }

    #[test]
    fn next_claim_time_is_now_when_the_clock_moved_forward() {
        let previous = at("2026-10-22T05:00:13.000Z");
        let now = at("2026-10-22T05:00:14.500Z");
        assert_eq!(next_claim_time(now, Some(previous)), now);
        // Exactly one millisecond on is already strictly later.
        let now = at("2026-10-22T05:00:13.001Z");
        assert_eq!(next_claim_time(now, Some(previous)), now);
    }

    #[test]
    fn next_claim_time_is_strictly_later_when_the_clock_repeats() {
        let previous = at("2026-10-22T05:00:13.250Z");
        let next = next_claim_time(previous, Some(previous));
        assert_eq!(next, at("2026-10-22T05:00:13.251Z"));
        // Within the same millisecond the clock has not moved either.
        let next = next_claim_time(at("2026-10-22T05:00:13.250900Z"), Some(previous));
        assert!(next > previous, "{next}");
        assert_ne!(
            render_claim_record("t", "h", next),
            render_claim_record("t", "h", previous),
            "the same millisecond twice must still change the body"
        );
    }

    #[test]
    fn next_claim_time_is_previous_plus_one_ms_when_the_clock_stepped_back() {
        let previous = at("2026-10-22T05:00:13.250Z");
        let now = at("2026-10-22T04:59:00.000Z");
        assert_eq!(
            next_claim_time(now, Some(previous)),
            at("2026-10-22T05:00:13.251Z")
        );
        assert_ne!(
            render_claim_record("t", "h", next_claim_time(now, Some(previous))),
            render_claim_record("t", "h", previous),
        );
    }

    #[test]
    fn next_claim_time_is_now_for_a_record_without_a_claim_time() {
        let now = t0();
        assert_eq!(next_claim_time(now, None), now);
        // A legacy record, and one whose time does not parse, carry no time
        // to step past: the re-claim's body differs because it gains the field.
        let legacy = format!(
            "{CLAIM_RECORD_MARKER}\nAn agent was dispatched to work on this issue by \
             dot-agent-deck on machine `h`.\n\n- Task: \"t\"\n- Machine: \"h\"\n"
        );
        let garbled = format!("{legacy}- Claimed at: \"yesterday\"\n");
        for old in [legacy, garbled] {
            let json = serde_json::json!([comment(7, ME, &old)]).to_string();
            let own = find_own_claim_record(&json, "t", "h", ME).unwrap().unwrap();
            assert_eq!(own.claimed_at, None);
            let body = render_claim_record("t", "h", next_claim_time(now, own.claimed_at));
            assert_ne!(body, own.body);
        }
    }

    #[test]
    fn a_re_claim_through_the_lookup_never_repeats_the_body() {
        // The path `write_claim_record` takes: find the own record, step past
        // its time, render. A clock frozen on the record's own millisecond, or
        // set behind it, still yields a different body.
        let previous = at("2026-10-22T05:00:13.250Z");
        let old = render_claim_record("t", "h", previous);
        let json = serde_json::json!([comment(7, ME, &old)]).to_string();
        let own = find_own_claim_record(&json, "t", "h", ME).unwrap().unwrap();
        for now in [
            previous,
            at("2026-10-22T05:00:13.250999Z"),
            at("2020-01-01T00:00:00Z"),
        ] {
            let body = render_claim_record("t", "h", next_claim_time(now, own.claimed_at));
            assert_ne!(body, own.body, "clock at {now}");
        }
    }

    #[test]
    fn find_own_claim_record_skips_a_forged_record_and_keeps_searching() {
        let body = render_claim_record("task", "host", t0());
        let forged_first = serde_json::json!([
            comment(1, 99999, &body),
            {"id": 2, "body": body},
            comment(3, ME, &body),
        ])
        .to_string();
        assert_eq!(
            own_id(&forged_first, "task", "host", ME),
            Ok(Some(3)),
            "the forged and the authorless copies are skipped; the genuine one is found"
        );
        let forged_only = serde_json::json!([comment(1, 99999, &body)]).to_string();
        assert_eq!(own_id(&forged_only, "task", "host", ME), Ok(None));
    }

    #[test]
    fn find_own_claim_record_reads_every_paginated_page() {
        let page1 = serde_json::json!([comment(1, ME, "first page")]);
        let page2 = serde_json::json!([comment(9, ME, &render_claim_record("t", "h", t0()))]);
        let stream = format!("{page1}\n{page2}\n");
        assert_eq!(own_id(&stream, "t", "h", ME), Ok(Some(9)));
    }

    #[test]
    fn find_own_claim_record_fails_on_malformed_output() {
        assert!(find_own_claim_record("not json", "t", "h", ME).is_err());
        assert!(find_own_claim_record("{\"id\": 1}", "t", "h", ME).is_err());
    }

    #[test]
    fn find_own_claim_record_fails_on_empty_output() {
        assert!(find_own_claim_record("", "t", "h", ME).is_err());
        assert!(find_own_claim_record(" \n\t\r\n", "t", "h", ME).is_err());
    }

    #[test]
    fn find_own_claim_record_validates_pages_after_the_own_record() {
        let page1 = serde_json::json!([comment(9, ME, &render_claim_record("t", "h", t0()))]);
        let stream = format!("{page1}\n[{{\"id\": 10, \"body\": ");
        assert!(
            find_own_claim_record(&stream, "t", "h", ME).is_err(),
            "a later page that does not parse fails the lookup even after a match"
        );
        let stream = format!("{page1}\n{{\"message\": \"Server Error\"}}");
        assert!(find_own_claim_record(&stream, "t", "h", ME).is_err());
    }

    #[test]
    fn sanitize_claim_error_escapes_caps_and_redacts() {
        let token = format!("ghp_{}", "A1b2C3d4".repeat(5));
        let pat = format!("github_pat_{}", "x9".repeat(30));
        let raw = format!(
            "HTTP 401\u{1b}[2J\u{7}\rforged line token={token} and {pat}; {}",
            "y".repeat(2000)
        );
        let clean = sanitize_claim_error(&raw);
        for raw_char in ['\u{1b}', '\u{7}', '\r', '\n'] {
            assert!(
                !clean.contains(raw_char),
                "{raw_char:?} survived: {clean:?}"
            );
        }
        assert!(
            clean.contains("\\u{1b}") && clean.contains("\\r"),
            "{clean}"
        );
        assert!(
            !clean.contains(&token) && !clean.contains("ghp_A1b2"),
            "{clean}"
        );
        assert!(!clean.contains("github_pat_x9"), "{clean}");
        assert_eq!(clean.matches("<redacted>").count(), 2, "{clean}");
        assert!(
            clean.chars().count() < 600,
            "capped: {} chars",
            clean.chars().count()
        );
        // Every gh token prefix is redacted.
        for prefix in ["ghp_", "gho_", "ghu_", "ghs_", "ghr_"] {
            let text = format!("x {prefix}abc123 y");
            assert_eq!(sanitize_claim_error(&text), "x <redacted> y");
        }
        // A token glued to the word before it — by an underscore, a letter, a
        // digit or a non-ASCII letter — is redacted all the same; only the
        // prefix word survives.
        for (text, expected) in [
            ("xghp_abc", "x<redacted>"),
            ("token_ghp_abc123 y", "token_<redacted> y"),
            ("prefixgithub_pat_11AB_cd y", "prefix<redacted> y"),
            ("id7gho_abc y", "id7<redacted> y"),
            ("cléghs_abc y", "clé<redacted> y"),
            ("令牌ghr_abc y", "令牌<redacted> y"),
        ] {
            assert_eq!(sanitize_claim_error(text), expected, "{text}");
        }
        assert_eq!(sanitize_claim_error("plain error"), "plain error");
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

    fn claimant() -> Claimant {
        Claimant {
            task: "Issues acme/widgets".to_string(),
            host: "h1-0123456789abcdef".to_string(),
            login: Some("teammate".to_string()),
            age: Some(chrono::Duration::hours(3)),
        }
    }

    #[test]
    fn dispatch_decision_truth_table() {
        use SkipCause::*;
        let skip = DispatchDecision::Skip;
        // No signal → dispatch.
        assert_eq!(
            dispatch_decision(false, false, false, None),
            DispatchDecision::Dispatch
        );
        // Each signal alone skips, naming itself.
        assert_eq!(
            dispatch_decision(true, false, false, None),
            skip(WorktreeExists)
        );
        assert_eq!(dispatch_decision(false, true, false, None), skip(OpenPr));
        assert_eq!(
            dispatch_decision(false, false, true, None),
            skip(LabelInProgress(None))
        );
        assert_eq!(
            dispatch_decision(false, false, false, Some(claimant())),
            skip(Claimed(claimant()))
        );
        // PRD #421 M1.4, extended for the claim: exhaustive over every
        // combination of the four signals (worktree exists, open PR,
        // `in-progress` label, live claim). Any one true means skip, only
        // all-false dispatches, and when several are true the first in
        // evaluation order (worktree, PR, label, claim) is the one named.
        for worktree_exists in [false, true] {
            for open_pr in [false, true] {
                for label in [false, true] {
                    for claim in [None, Some(claimant())] {
                        let expected = if worktree_exists {
                            skip(WorktreeExists)
                        } else if open_pr {
                            skip(OpenPr)
                        } else if label {
                            skip(LabelInProgress(None))
                        } else if let Some(claimant) = claim.clone() {
                            skip(Claimed(claimant))
                        } else {
                            DispatchDecision::Dispatch
                        };
                        assert_eq!(
                            dispatch_decision(worktree_exists, open_pr, label, claim.clone()),
                            expected,
                            "dispatch_decision({worktree_exists}, {open_pr}, {label}, {claim:?})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn skip_causes_read_differently_and_a_claim_names_its_claimant() {
        let lines: Vec<String> = [
            SkipCause::WorktreeExists,
            SkipCause::OpenPr,
            SkipCause::Claimed(claimant()),
            SkipCause::ConcurrentCreator,
            SkipCause::LabelInProgress(None),
            SkipCause::LabelInProgress(Some("Claimed by scheduled task `t`.".to_string())),
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        let distinct: std::collections::HashSet<&String> = lines.iter().collect();
        assert_eq!(distinct.len(), lines.len(), "{lines:?}");
        assert_eq!(
            lines[2],
            "claimed by schedule \"Issues acme/widgets\" on machine h1-0123456789abcdef \
             (GitHub account teammate), last active 3h ago"
        );
        let unknown = Claimant {
            login: None,
            age: None,
            ..claimant()
        };
        assert_eq!(
            unknown.to_string(),
            "schedule \"Issues acme/widgets\" on machine h1-0123456789abcdef \
             (GitHub account unknown), age unknown"
        );
        // A record dated a little ahead of GitHub's clock still renders.
        let ahead = Claimant {
            age: Some(chrono::Duration::seconds(-30)),
            ..claimant()
        };
        assert!(ahead.to_string().ends_with("last active 0s ago"), "{ahead}");
    }

    #[test]
    fn a_claimant_line_bounds_and_escapes_what_the_comment_supplied() {
        // The schedule name is whatever the comment says: a 60 000-character
        // one must not reach the log whole, nor may a control or bidi
        // character in any field act on the terminal.
        let long = Claimant {
            task: format!("{}\u{202e}\n", "x".repeat(60_000)),
            host: format!("h1-\n{}", "y".repeat(500)),
            login: Some("bad\u{1b}[31mlogin".to_string()),
            age: Some(chrono::Duration::hours(1)),
        };
        let line = long.to_string();
        assert!(line.chars().count() < 400, "{} chars", line.chars().count());
        assert!(
            line.contains(&format!("\"{}… (60002 characters total)\"", "x".repeat(64))),
            "{line}"
        );
        assert!(
            !line.contains('\n') && !line.contains('\u{1b}') && !line.contains('\u{202e}'),
            "{line:?}"
        );
        // An ordinary name is shown whole.
        assert!(claimant().to_string().contains("\"Issues acme/widgets\""));
    }

    // --- PRD #421 Phase 3: reading every claim record ---

    fn hours(n: i64) -> chrono::Duration {
        chrono::Duration::hours(n)
    }

    fn record_comment(
        id: u64,
        user: u64,
        task: &str,
        host: &str,
        updated_at: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "user": {"id": user, "login": "x"},
            "author_association": "COLLABORATOR",
            "body": render_claim_record(task, host, t0()),
            "created_at": "2026-10-01T00:00:00Z",
            "updated_at": updated_at,
        })
    }

    #[test]
    fn parse_issue_claim_records_finds_every_record_whoever_wrote_it() {
        let page1 = serde_json::json!([
            comment(1, ME, "plain comment"),
            record_comment(2, ME, "mine", "h1-aaaa", "2026-10-22T07:23:14Z"),
        ]);
        let page2 = serde_json::json!([
            record_comment(3, 4242, "theirs", "h1-bbbb", "2026-10-20T01:02:03Z"),
            {"id": 4, "body": render_claim_record("no author", "h1-cccc", t0()), "user": null},
            {"id": 5, "body": null, "user": null},
        ]);
        let records = parse_issue_claim_records(&format!("{page1}\n{page2}\n")).unwrap();
        let summary: Vec<_> = records
            .iter()
            .map(|r| {
                (
                    r.comment_id,
                    r.author_id,
                    r.author_login.as_deref(),
                    r.author_association.as_deref(),
                    r.record.task.as_str(),
                    r.record.host.as_str(),
                    r.updated_at,
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                (
                    2,
                    Some(ME),
                    Some("x"),
                    Some("COLLABORATOR"),
                    "mine",
                    "h1-aaaa",
                    Some(at("2026-10-22T07:23:14Z"))
                ),
                (
                    3,
                    Some(4242),
                    Some("x"),
                    Some("COLLABORATOR"),
                    "theirs",
                    "h1-bbbb",
                    Some(at("2026-10-20T01:02:03Z"))
                ),
                (4, None, None, None, "no author", "h1-cccc", None),
            ]
        );
        assert_eq!(records[0].record.claimed_at, Some(t0()));
        assert_eq!(
            records[0].body,
            render_claim_record("mine", "h1-aaaa", t0())
        );
    }

    #[test]
    fn parse_issue_claim_records_reads_an_unreadable_edit_time_as_absent() {
        let json = serde_json::json!([record_comment(1, ME, "t", "h", "yesterday")]).to_string();
        assert_eq!(
            parse_issue_claim_records(&json).unwrap()[0].updated_at,
            None
        );
    }

    #[test]
    fn parse_issue_claim_records_fails_on_empty_or_malformed_output() {
        assert!(parse_issue_claim_records("").is_err());
        assert!(parse_issue_claim_records("not json").is_err());
        let good = serde_json::json!([record_comment(1, ME, "t", "h", "2026-10-22T07:23:14Z")]);
        assert!(parse_issue_claim_records(&format!("{good}\n{{\"id\": 1}}")).is_err());
        assert_eq!(parse_issue_claim_records("[]"), Ok(Vec::new()));
    }

    #[test]
    fn a_record_is_own_only_with_the_same_author_task_and_machine() {
        let me = ClaimIdentity {
            user_id: ME,
            task: "t",
            host_id: "h",
        };
        let json = serde_json::json!([
            record_comment(1, ME, "t", "h", "2026-10-22T07:23:14Z"),
            record_comment(2, 999, "t", "h", "2026-10-22T07:23:14Z"),
            record_comment(3, ME, "other", "h", "2026-10-22T07:23:14Z"),
            record_comment(4, ME, "t", "elsewhere", "2026-10-22T07:23:14Z"),
            {"id": 5, "body": render_claim_record("t", "h", t0()), "user": null},
        ])
        .to_string();
        let own: Vec<bool> = parse_issue_claim_records(&json)
            .unwrap()
            .iter()
            .map(|r| r.is_own(&me))
            .collect();
        assert_eq!(own, vec![true, false, false, false, false]);
    }

    #[test]
    fn issue_events_argv_addresses_the_issue_events_endpoint() {
        assert_eq!(
            issue_events_argv("acme/widgets", 7),
            vec![
                "api",
                "--paginate",
                "repos/acme/widgets/issues/7/events?per_page=100"
            ]
        );
    }

    fn label_event(event: &str, name: &str, created_at: &str) -> serde_json::Value {
        serde_json::json!({"event": event, "label": {"name": name}, "created_at": created_at})
    }

    #[test]
    fn claim_label_state_floors_at_the_earliest_add_and_follows_the_latest_change() {
        let page1 = serde_json::json!([
            label_event("labeled", "bug", "2026-10-01T00:00:00Z"),
            label_event("labeled", CLAIM_LABEL, "2026-10-22T05:00:11Z"),
            label_event("unlabeled", CLAIM_LABEL, "2026-10-21T00:00:00Z"),
            {"event": "assigned", "created_at": "2026-10-02T00:00:00Z"},
        ]);
        let page2 =
            serde_json::json!([label_event("labeled", CLAIM_LABEL, "2026-10-20T09:00:00Z")]);
        assert_eq!(
            parse_claim_label_state(&format!("{page1}\n{page2}")),
            Ok(ClaimLabelState {
                present: true,
                added_at: Some(at("2026-10-20T09:00:00Z")),
            })
        );
        assert_eq!(
            parse_claim_label_state(
                &serde_json::json!([label_event("labeled", "bug", "2026-10-01T00:00:00Z")])
                    .to_string()
            ),
            Ok(ClaimLabelState {
                present: false,
                added_at: None,
            })
        );
        assert_eq!(
            parse_claim_label_state("[]"),
            Ok(ClaimLabelState {
                present: false,
                added_at: None,
            })
        );
    }

    #[test]
    fn a_claim_label_removed_after_its_last_add_is_not_present() {
        let removed = serde_json::json!([
            label_event("labeled", CLAIM_LABEL, "2026-10-20T09:00:00Z"),
            label_event("unlabeled", CLAIM_LABEL, "2026-10-21T09:00:00Z"),
            label_event("labeled", "bug", "2026-10-22T09:00:00Z"),
        ]);
        assert_eq!(
            parse_claim_label_state(&removed.to_string()),
            Ok(ClaimLabelState {
                present: false,
                added_at: Some(at("2026-10-20T09:00:00Z")),
            })
        );
        // Removed and re-added in the same second: the later event in the
        // listing decides.
        let same_second = serde_json::json!([
            label_event("unlabeled", CLAIM_LABEL, "2026-10-21T09:00:00Z"),
            label_event("labeled", CLAIM_LABEL, "2026-10-21T09:00:00Z"),
        ]);
        assert!(
            parse_claim_label_state(&same_second.to_string())
                .unwrap()
                .present
        );
    }

    #[test]
    fn claim_label_state_fails_on_empty_or_malformed_output() {
        assert!(parse_claim_label_state("").is_err());
        assert!(parse_claim_label_state("{\"event\": \"labeled\"}").is_err());
    }

    #[test]
    fn claim_record_age_runs_from_the_later_of_its_edit_and_the_label_floor() {
        let now = at("2026-10-23T12:00:00Z");
        let edit = at("2026-10-23T02:00:00Z");
        let label = at("2026-10-23T06:00:00Z");
        assert_eq!(claim_record_age(now, Some(edit), None), Some(hours(10)));
        assert_eq!(claim_record_age(now, None, Some(label)), Some(hours(6)));
        // The floor lifts an edit older than the label …
        assert_eq!(
            claim_record_age(now, Some(edit), Some(label)),
            Some(hours(6))
        );
        // … and never ages a later edit.
        assert_eq!(
            claim_record_age(now, Some(label), Some(edit)),
            Some(hours(6))
        );
        assert_eq!(claim_record_age(now, None, None), None);
        // A record dated after `now` has a negative age, which the gate reads.
        assert_eq!(
            claim_record_age(now, Some(now + hours(1)), None),
            Some(-hours(1))
        );
    }

    #[test]
    fn claim_record_liveness_truth_table() {
        let ttl = hours(168);
        let fresh = Some(hours(167));
        let stale = Some(hours(168));
        let slightly_ahead = Some(-chrono::Duration::minutes(4));
        let far_ahead = Some(-chrono::Duration::minutes(6));
        // own + dead → not gating, however fresh its clock; own + live →
        // gating, whatever its clock. Trust plays no part in an own record.
        for age in [fresh, stale, None, far_ahead] {
            for trusted in [false, true] {
                assert!(!claim_record_gates(true, false, trusted, age, ttl));
                assert!(claim_record_gates(true, true, trusted, age, ttl));
            }
        }
        // foreign + trusted + fresh → gating; stale, clockless, or dated more
        // than the tolerance ahead → not. The owner's local evidence plays no
        // part in a foreign record.
        for own_work_alive in [false, true] {
            assert!(claim_record_gates(false, own_work_alive, true, fresh, ttl));
            assert!(claim_record_gates(
                false,
                own_work_alive,
                true,
                slightly_ahead,
                ttl
            ));
            assert!(!claim_record_gates(false, own_work_alive, true, stale, ttl));
            assert!(!claim_record_gates(false, own_work_alive, true, None, ttl));
            assert!(!claim_record_gates(
                false,
                own_work_alive,
                true,
                far_ahead,
                ttl
            ));
            // An author GitHub does not vouch for never gates, however fresh.
            for age in [fresh, slightly_ahead] {
                assert!(!claim_record_gates(false, own_work_alive, false, age, ttl));
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

    // --- reviewer/auditor fix round, PR #582: reason wrapped in a code span ---

    #[test]
    fn release_comment_body_wraps_reason_in_a_code_span() {
        let identity = Identity::orchestration("orch-B", Path::new("/work/wt-b"), "branch-b");
        let body = release_comment_body(
            &identity,
            "2026-08-09T00:00:00Z",
            None,
            None,
            Some("PR merged"),
        );
        assert!(
            body.contains(", reason: `PR merged`"),
            "the reason must be wrapped in its own code span, mirroring the `forcibly \
             released from `{{prev}}`` clause immediately above it, got {body:?}"
        );
    }

    #[test]
    fn release_comment_body_reason_mention_stays_inert_inside_its_code_span() {
        let identity = Identity::orchestration("orch-B", Path::new("/work/wt-b"), "branch-b");
        let body = release_comment_body(
            &identity,
            "2026-08-09T00:00:00Z",
            None,
            None,
            Some("ping @someone see #421"),
        );
        // `reason` used to be the ONLY untrusted value in this module
        // rendered outside a code span — an even count of backticks
        // preceding the `@` means it sits OUTSIDE any open span (live).
        let at_idx = body.find('@').expect("reason must be present");
        let backticks_before = body[..at_idx].matches('`').count();
        assert!(
            !backticks_before.is_multiple_of(2),
            "an @mention inside the reason must sit inside a code span (odd backtick count \
             before it), got {body:?}"
        );
    }

    #[test]
    fn release_comment_body_omits_empty_reason() {
        let identity = Identity::orchestration("orch-B", Path::new("/work/wt-b"), "branch-b");
        for reason in [Some(""), Some("   "), Some("```")] {
            let body = release_comment_body(&identity, "2026-08-09T00:00:00Z", None, None, reason);
            assert!(
                !body.contains("reason:"),
                "an empty (or all-whitespace, or all-backtick) --reason must be treated the \
                 same as no --reason at all, got {body:?} for input {reason:?}"
            );
        }
    }

    fn found(id: u64, user: u64, task: &str, host: &str, updated_at: &str) -> IssueClaimRecord {
        IssueClaimRecord {
            comment_id: id,
            author_id: Some(user),
            author_login: Some(format!("user-{user}")),
            author_association: Some("COLLABORATOR".to_string()),
            body: render_claim_record(task, host, t0()),
            record: ClaimRecord {
                task: task.to_string(),
                host: host.to_string(),
                claimed_at: Some(t0()),
                ttl_hours: None,
            },
            updated_at: Some(at(updated_at)),
        }
    }

    const NOW: &str = "2026-10-23T12:00:00Z";

    fn me() -> ClaimIdentity<'static> {
        ClaimIdentity {
            user_id: ME,
            task: "mine",
            host_id: "h1-aaaa",
        }
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
    fn live_claimant_ages_each_record_on_its_own_clock() {
        let ttl = hours(24);
        let own_recent = found(1, ME, "mine", "h1-aaaa", "2026-10-23T11:00:00Z");
        let foreign_dead = found(2, 4242, "theirs", "h1-bbbb", "2026-10-20T00:00:00Z");
        let foreign_live = found(3, 4242, "theirs", "h1-cccc", "2026-10-23T09:30:00Z");
        let decide =
            |records: &[IssueClaimRecord]| live_claimant(records, &me(), false, None, at(NOW), ttl);
        // Success Criterion 7: the deck's own dead record never gates it.
        assert_eq!(decide(std::slice::from_ref(&own_recent)), None);
        // A dead foreign record does not hold it either, nor does a fresh own
        // record make the dead foreign one read live (no aggregate clock).
        assert_eq!(decide(&[own_recent.clone(), foreign_dead.clone()]), None);
        // A live foreign record gates, and is the one named.
        assert_eq!(
            decide(&[own_recent, foreign_dead, foreign_live]),
            Some(Claimant {
                task: "theirs".to_string(),
                host: "h1-cccc".to_string(),
                login: Some("user-4242".to_string()),
                age: Some(chrono::Duration::minutes(150)),
            })
        );
        assert_eq!(decide(&[]), None);
    }

    #[test]
    fn live_claimant_treats_the_same_body_by_another_account_as_foreign() {
        let forged = found(1, 999, "mine", "h1-aaaa", "2026-10-23T11:00:00Z");
        assert_eq!(
            live_claimant(&[forged], &me(), false, None, at(NOW), hours(24)).map(|c| c.host),
            Some("h1-aaaa".to_string())
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

    // --- Issue #172: triage instruction audit trail ---

    /// A `MakeWriter` over a shared in-memory buffer, mirroring
    /// `tests/logging_filter.rs`'s hand-rolled capture (an `Arc<Mutex<W>>`
    /// isn't directly usable as a `MakeWriter`).
    #[derive(Clone)]
    struct CaptureWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("capture buffer poisoned")
                .extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
        type Writer = CaptureWriter;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// Runs `f` against a thread-local subscriber that captures every emitted
    /// line (installed through `capture_tracing_on_this_thread`, so this stays
    /// independent of `cargo test`'s threads / `nextest`'s processes and
    /// loses no event to another test's callsite interest) and returns
    /// everything written.
    fn capture_tracing(f: impl FnOnce()) -> String {
        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_writer(CaptureWriter(std::sync::Arc::clone(&buf)))
            .with_max_level(tracing_subscriber::filter::LevelFilter::INFO)
            .with_ansi(false)
            .finish();
        {
            let _capture = crate::test_isolation::capture_tracing_on_this_thread(subscriber);
            f();
        }
        String::from_utf8(buf.lock().expect("capture buffer poisoned").clone())
            .expect("log output is not UTF-8")
    }

    /// Issue #172: when a dispatch includes the triage instruction,
    /// `build_dispatch_prompt` must leave a durable audit record behind
    /// naming the issue, the repo, and the instruction template used — this
    /// is what makes a triage-authorised `gh` write traceable after the
    /// fact, since the deck deliberately never parses the agent's response.
    #[test]
    fn build_dispatch_prompt_triage_on_records_an_audit_event() {
        let mut prompt = String::new();
        let log = capture_tracing(|| {
            prompt = build_dispatch_prompt(
                "Work on issue {{issue_number}}",
                172,
                "prageethw/dot-agent-deck",
                true,
            );
        });

        // Unchanged behaviour: the triage vocabulary still reaches the agent's
        // prompt exactly as before this fix.
        assert!(
            prompt.contains(&triage_instruction()),
            "triage instruction must still be appended to the prompt, got: {prompt:?}"
        );

        // The new behaviour: a durable audit record naming what was asked.
        assert!(
            log.contains("triage instruction issued to dispatched agent for issue #172 of prageethw/dot-agent-deck"),
            "expected an audit record naming the issue and repo, got log: {log:?}"
        );
        assert!(
            log.contains("Triage this issue using the following labels"),
            "audit record must carry the instruction template used, got log: {log:?}"
        );
    }

    /// Issue #172, the negative case: with triage off, `build_dispatch_prompt`
    /// must neither append the instruction nor emit any audit record for it —
    /// the audit trail must not fire for a dispatch the triage vocabulary
    /// never reached.
    #[test]
    fn build_dispatch_prompt_triage_off_records_no_audit_event() {
        let mut prompt = String::new();
        let log = capture_tracing(|| {
            prompt = build_dispatch_prompt(
                "Work on issue {{issue_number}}",
                172,
                "prageethw/dot-agent-deck",
                false,
            );
        });

        assert!(
            !prompt.contains("Triage this issue"),
            "triage instruction must not be appended when triage is off, got: {prompt:?}"
        );
        assert!(
            log.is_empty(),
            "no audit record must be emitted when triage is off, got log: {log:?}"
        );
    }

    #[test]
    fn only_a_foreign_record_by_an_owner_member_or_collaborator_gates() {
        // The same body, equally fresh: what decides is the standing GitHub
        // reports for its author on the repository.
        let record = |association: Option<&str>| IssueClaimRecord {
            author_association: association.map(str::to_string),
            ..found(1, 4242, "theirs", "h1-bbbb", "2026-10-23T11:00:00Z")
        };
        let decide = |association: Option<&str>| {
            live_claimant(
                &[record(association)],
                &me(),
                false,
                None,
                at(NOW),
                hours(24),
            )
        };
        for trusted in ["OWNER", "MEMBER", "COLLABORATOR"] {
            assert_eq!(
                decide(Some(trusted)).and_then(|c| c.login),
                Some("user-4242".to_string()),
                "{trusted}"
            );
        }
        for untrusted in [
            Some("CONTRIBUTOR"),
            Some("FIRST_TIME_CONTRIBUTOR"),
            Some("FIRST_TIMER"),
            Some("MANNEQUIN"),
            Some("NONE"),
            Some("collaborator"),
            Some(""),
            None,
        ] {
            assert_eq!(decide(untrusted), None, "{untrusted:?}");
        }
        // An untrusted record does not hide a trusted live one beside it.
        let trusted = IssueClaimRecord {
            comment_id: 2,
            author_login: Some("teammate".to_string()),
            ..record(Some("MEMBER"))
        };
        assert_eq!(
            live_claimant(
                &[record(Some("NONE")), trusted],
                &me(),
                false,
                None,
                at(NOW),
                hours(24)
            )
            .and_then(|c| c.login),
            Some("teammate".to_string())
        );
    }

    #[test]
    fn the_comments_listing_carries_the_authors_standing() {
        let json = serde_json::json!([
            {"id": 1, "user": {"id": 4242, "login": "teammate"}, "author_association": "MEMBER",
             "body": render_claim_record("theirs", "h1-bbbb", t0()),
             "updated_at": "2026-10-23T11:00:00Z"},
            {"id": 2, "user": {"id": 666, "login": "stranger"}, "author_association": "NONE",
             "body": render_claim_record("theirs", "h1-bbbb", t0()),
             "updated_at": "2026-10-23T11:00:00Z"},
            {"id": 3, "user": {"id": 667, "login": "silent"},
             "body": render_claim_record("theirs", "h1-bbbb", t0()),
             "updated_at": "2026-10-23T11:00:00Z"},
        ])
        .to_string();
        let trusted: Vec<(Option<String>, bool)> = parse_issue_claim_records(&json)
            .unwrap()
            .into_iter()
            .map(|r| (r.author_login.clone(), r.author_is_trusted()))
            .collect();
        assert_eq!(
            trusted,
            vec![
                (Some("teammate".to_string()), true),
                (Some("stranger".to_string()), false),
                (Some("silent".to_string()), false),
            ]
        );
    }

    #[test]
    fn a_foreign_record_dated_ahead_of_githubs_clock_does_not_gate() {
        let ttl = hours(24);
        let decide = |updated_at: &str, label: Option<&str>| {
            live_claimant(
                &[found(1, 4242, "theirs", "h1-bbbb", updated_at)],
                &me(),
                false,
                label.map(at),
                at(NOW),
                ttl,
            )
        };
        // A few seconds ahead: still a live claim, shown as just now.
        assert_eq!(
            decide("2026-10-23T12:00:30Z", None).and_then(|c| c.age),
            Some(-chrono::Duration::seconds(30))
        );
        // More than the tolerance ahead: not evidence of a live claim, rather
        // than read as age zero and held for a whole TTL.
        assert_eq!(decide("2026-10-23T12:06:00Z", None), None);
        assert_eq!(decide("2026-10-24T12:00:00Z", None), None);
        // The label floor counts the same way: a label dated ahead lifts the
        // record's clock ahead too.
        assert_eq!(
            decide("2026-10-23T11:00:00Z", Some("2026-10-23T13:00:00Z")),
            None
        );
    }

    #[test]
    fn live_claimant_floors_every_record_by_the_label_time() {
        let foreign = found(2, 4242, "theirs", "h1-bbbb", "2026-10-01T00:00:00Z");
        let label = Some(at("2026-10-23T10:00:00Z"));
        assert_eq!(
            live_claimant(
                std::slice::from_ref(&foreign),
                &me(),
                false,
                label,
                at(NOW),
                hours(24)
            )
            .and_then(|c| c.age),
            Some(hours(2))
        );
        assert_eq!(
            live_claimant(&[foreign], &me(), false, None, at(NOW), hours(24)),
            None
        );
    }

    #[test]
    fn live_claimant_names_the_most_recently_active_of_several_live_records() {
        let older = found(1, 4242, "a", "h1-bbbb", "2026-10-23T08:00:00Z");
        let newer = found(2, 4343, "b", "h1-cccc", "2026-10-23T11:00:00Z");
        assert_eq!(
            live_claimant(&[older, newer], &me(), false, None, at(NOW), hours(24)).map(|c| c.task),
            Some("b".to_string())
        );
    }

    #[test]
    fn live_claimant_judges_each_record_by_its_writers_lifetime() {
        // The reviewer's case: a reader with a 3-hour TTL and an owner on the
        // 168-hour default, which refreshes only every 84 hours. Ten hours
        // after its last edit the owner's claim is live, whatever the reader's
        // own TTL; a record from an older deck, stating no lifetime, is judged
        // by the reader's.
        let with_ttl = |id, ttl_hours: Option<u32>, updated_at: &str| {
            let mut record = found(id, 4242, "theirs", "h1-bbbb", updated_at);
            record.record.ttl_hours = ttl_hours;
            record
        };
        let ten_hours_ago = "2026-10-23T02:00:00Z";
        let decide = |record: IssueClaimRecord, reader_ttl| {
            live_claimant(&[record], &me(), false, None, at(NOW), reader_ttl).is_some()
        };
        assert!(decide(with_ttl(1, Some(168), ten_hours_ago), hours(3)));
        assert!(!decide(with_ttl(1, None, ten_hours_ago), hours(3)));
        assert!(decide(with_ttl(1, None, ten_hours_ago), hours(168)));
        // A shorter lifetime than the reader's lapses on the writer's clock.
        assert!(!decide(with_ttl(1, Some(3), ten_hours_ago), hours(168)));
        // A lifetime past 90 days still lapses at 90 days; one of 0 at an hour.
        assert!(decide(
            with_ttl(1, Some(u32::MAX), "2026-07-26T12:00:00Z"),
            hours(3)
        ));
        assert!(!decide(
            with_ttl(1, Some(u32::MAX), "2026-07-25T11:00:00Z"),
            hours(3)
        ));
        assert!(!decide(
            with_ttl(1, Some(0), "2026-10-23T10:59:00Z"),
            hours(168)
        ));
        assert!(decide(
            with_ttl(1, Some(0), "2026-10-23T11:01:00Z"),
            hours(168)
        ));
    }

    #[test]
    fn live_claimant_gates_on_an_own_record_only_while_its_work_is_alive() {
        let own_old = found(1, ME, "mine", "h1-aaaa", "2026-01-01T00:00:00Z");
        let own_old = std::slice::from_ref(&own_old);
        assert_eq!(
            live_claimant(own_old, &me(), false, None, at(NOW), hours(24)),
            None
        );
        assert!(live_claimant(own_old, &me(), true, None, at(NOW), hours(24)).is_some());
    }

    #[test]
    fn a_refresh_is_due_once_the_own_record_is_half_a_ttl_old() {
        let now = at(NOW);
        let ttl = hours(168);
        assert!(!claim_refresh_due(now, Some(now), ttl));
        assert!(!claim_refresh_due(now, Some(now - hours(83)), ttl));
        assert!(claim_refresh_due(now, Some(now - hours(84)), ttl));
        assert!(claim_refresh_due(now, Some(now - hours(500)), ttl));
        // No own record (or no readable edit time): write one.
        assert!(claim_refresh_due(now, None, ttl));
    }

    #[test]
    fn claim_ttl_hours_must_be_between_one_hour_and_ninety_days() {
        assert!(validate_claim_ttl_hours(0).is_err());
        assert!(validate_claim_ttl_hours(1).is_ok());
        assert!(validate_claim_ttl_hours(DEFAULT_CLAIM_TTL_HOURS).is_ok());
        assert!(validate_claim_ttl_hours(MAX_CLAIM_TTL_HOURS).is_ok());
        assert_eq!(MAX_CLAIM_TTL_HOURS, 90 * 24);
        let err = validate_claim_ttl_hours(MAX_CLAIM_TTL_HOURS + 1).unwrap_err();
        assert!(err.contains("at most 2160"), "{err}");
    }

    fn utc(y: i32, mo: u32, d: u32) -> chrono::DateTime<chrono::Utc> {
        use chrono::TimeZone;
        chrono::Utc.with_ymd_and_hms(y, mo, d, 0, 0, 0).unwrap()
    }

    #[test]
    fn the_documented_weekday_cron_needs_twice_its_72h_weekend_gap_plus_an_hour() {
        // `docs/scheduled-tasks.md`'s example: 24h apart on weekdays, 72h
        // across a weekend. Evaluated in UTC so no DST hour moves the gap.
        let cron = "0 9 * * MON-FRI";
        let from = utc(2026, 10, 21);
        assert_eq!(check_claim_ttl_against_cron(cron, 145, &from), Ok(()));
        assert_eq!(
            check_claim_ttl_against_cron(cron, DEFAULT_CLAIM_TTL_HOURS, &from),
            Ok(())
        );
        let err = check_claim_ttl_against_cron(cron, 100, &from).unwrap_err();
        assert!(
            err.contains("(3d)") && err.contains("at least 145"),
            "the error names the gap and the TTL it needs: {err}"
        );
        // Exactly twice the gap is not enough any more.
        assert!(check_claim_ttl_against_cron(cron, 144, &from).is_err());
    }

    #[test]
    fn a_ttl_check_that_cannot_compute_the_gap_fails() {
        let from = utc(2026, 10, 21);
        // Invalid cron.
        assert!(check_claim_ttl_against_cron("not a cron", 168, &from).is_err());
        // A schedule that stops firing (7-field form with a past year range).
        assert!(check_claim_ttl_against_cron("0 0 9 * * * 2026", 10_000, &from).is_err());
    }

    #[test]
    fn frequent_crons_keep_gating_on_at_the_default_ttl() {
        // The walk jumps over stretches with no long gap, so a cron firing
        // every minute (or every second) is as checkable as a daily one.
        let from = utc(2026, 10, 21);
        for cron in ["* * * * *", "*/5 * * * *", "*/10 * * * *", "* * * * * *"] {
            assert_eq!(
                check_claim_ttl_against_cron(cron, DEFAULT_CLAIM_TTL_HOURS, &from),
                Ok(()),
                "{cron}"
            );
            // Twice a gap of at most ten minutes plus the hour's headroom
            // fits in two hours, and never in one.
            assert_eq!(
                check_claim_ttl_against_cron(cron, 2, &from),
                Ok(()),
                "{cron}"
            );
            let err = check_claim_ttl_against_cron(cron, 1, &from).unwrap_err();
            assert!(err.contains("at least 2"), "{cron}: {err}");
        }
    }

    #[test]
    fn a_long_gap_late_in_the_year_turns_gating_off() {
        // Dense for most of the year with a long silence somewhere in it: the
        // check must find the silence however many fires come before it.
        let from = utc(2027, 1, 15);
        // Every five minutes on the 1st of the month only: 30 days and five
        // minutes from a 31-day month's 1st at 23:55 to the next 1st.
        let monthly = "*/5 * 1 * *";
        let err =
            check_claim_ttl_against_cron(monthly, DEFAULT_CLAIM_TTL_HOURS, &from).unwrap_err();
        assert!(
            err.contains("(30d 5m)") && err.contains("at least 1442"),
            "{err}"
        );
        assert!(check_claim_ttl_against_cron(monthly, 1441, &from).is_err());
        assert_eq!(check_claim_ttl_against_cron(monthly, 1442, &from), Ok(()));
        // Every five minutes except in December, eleven months after the
        // walk's start: 31 days and five minutes without a fire.
        let not_december = "*/5 * * 1-11 *";
        let err =
            check_claim_ttl_against_cron(not_december, DEFAULT_CLAIM_TTL_HOURS, &from).unwrap_err();
        assert!(
            err.contains("(31d 5m)") && err.contains("at least 1490"),
            "{err}"
        );
        assert_eq!(
            check_claim_ttl_against_cron(not_december, 1490, &from),
            Ok(())
        );
        // Every five minutes in March only: no TTL up to the maximum covers
        // the eleven months between Marches, and the warning says so.
        let err =
            check_claim_ttl_against_cron("*/5 * * 3 *", MAX_CLAIM_TTL_HOURS, &from).unwrap_err();
        assert!(
            err.contains("more than the 2160-hour maximum") && !err.contains("set claim_ttl_hours"),
            "{err}"
        );
    }

    #[test]
    fn a_sparse_cron_needs_a_ttl_twice_its_gap() {
        // Monthly on the 1st: the walk covers a year, so the longest month
        // gap (31 days) decides, wherever the walk starts.
        let from = utc(2026, 10, 21);
        assert!(check_claim_ttl_against_cron("0 9 1 * *", 168, &from).is_err());
        assert_eq!(
            check_claim_ttl_against_cron("0 9 1 * *", 31 * 24 * 2 + 1, &from),
            Ok(())
        );
        assert!(check_claim_ttl_against_cron("0 9 1 * *", 31 * 24 * 2, &from).is_err());
    }

    /// Walk a healthy owner over `weeks` of `cron` fires, starting at the
    /// first fire after `from`: its record is written `write_delay` after the
    /// fire that dispatched it; every later fire checks the record
    /// `check_delay` after the fire and, when [`claim_refresh_due`] says so,
    /// its edit lands `edit_delay` after that. Returns the first moment a
    /// peer would see the record expired (its age reaching `ttl` before the
    /// next edit lands), or `None` when it never lapses.
    fn first_lapse(
        cron: &str,
        ttl_hours: u32,
        from: chrono::DateTime<chrono::Utc>,
        weeks: i64,
        write_delay: chrono::Duration,
        check_delay: chrono::Duration,
        edit_delay: chrono::Duration,
    ) -> Option<chrono::DateTime<chrono::Utc>> {
        let ttl = hours(i64::from(ttl_hours));
        let end = from + chrono::Duration::weeks(weeks);
        let schedule = crate::scheduler::parse_cron(cron).unwrap();
        let mut fires = schedule.after(&from);
        let mut last_edit = fires.next().unwrap() + write_delay;
        for fire in fires {
            if fire > end {
                return None;
            }
            let check = fire + check_delay;
            if check >= last_edit + ttl {
                return Some(last_edit + ttl);
            }
            if claim_refresh_due(check, Some(last_edit), ttl) {
                let landed = check + edit_delay;
                if landed >= last_edit + ttl {
                    return Some(last_edit + ttl);
                }
                last_edit = landed;
            }
        }
        None
    }

    #[test]
    fn a_ttl_that_passes_the_check_outlives_every_refresh_of_a_healthy_owner() {
        // The reviewer's case: a daily cron, the record written 10 seconds
        // after its fire, each check 1 second after its fire and its edit 30
        // seconds after that. The second check after the write sees an age
        // just under half the TTL and does nothing, so the edit falls due at
        // the third — when exactly twice the gap had already expired.
        let from = utc(2026, 10, 21);
        let reviewer = |ttl_hours| {
            first_lapse(
                "0 9 * * *",
                ttl_hours,
                from,
                8,
                chrono::Duration::seconds(10),
                chrono::Duration::seconds(1),
                chrono::Duration::seconds(30),
            )
        };
        assert!(
            reviewer(48).is_some(),
            "twice the gap alone lets the claim lapse, which the headroom exists for"
        );
        assert_eq!(
            check_claim_ttl_against_cron("0 9 * * *", 48, &from).ok(),
            None
        );
        assert_eq!(check_claim_ttl_against_cron("0 9 * * *", 49, &from), Ok(()));
        assert_eq!(reviewer(49), None);

        // And for every cron the docs use, at the smallest TTL the check
        // accepts, with a slower write and edit: the record written ten
        // minutes after its fire, the edit landing ten minutes after a check
        // that runs a minute in.
        for cron in [
            "*/10 * * * *",
            "0 * * * *",
            "0 9 * * *",
            "0 9 * * MON-FRI",
            "0 9 * * MON",
        ] {
            let minimum = (1..=2_000)
                .find(|ttl| check_claim_ttl_against_cron(cron, *ttl, &from).is_ok())
                .unwrap();
            assert_eq!(
                first_lapse(
                    cron,
                    minimum,
                    from,
                    8,
                    chrono::Duration::minutes(10),
                    chrono::Duration::minutes(1),
                    chrono::Duration::minutes(10),
                ),
                None,
                "{cron} at claim_ttl_hours = {minimum}"
            );
        }
        // The 168-hour default passes for the documented weekday cron.
        assert_eq!(
            check_claim_ttl_against_cron("0 9 * * MON-FRI", DEFAULT_CLAIM_TTL_HOURS, &from),
            Ok(())
        );
    }

    #[test]
    fn format_claim_duration_leaves_out_zero_parts() {
        assert_eq!(format_claim_duration(hours(72)), "3d");
        assert_eq!(
            format_claim_duration(hours(26) + chrono::Duration::seconds(65)),
            "1d 2h 1m 5s"
        );
        assert_eq!(format_claim_duration(chrono::Duration::minutes(45)), "45m");
        assert_eq!(format_claim_duration(chrono::Duration::zero()), "0s");
    }
}
