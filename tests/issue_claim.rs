//! RED tests for PRD fork#235's `dot-agent-deck issue claim <n> [--repo
//! <owner/name>] [--takeover] [--confirm-stopped]` — turning the issue claim
//! from a record (PRD #421) into a lock that refuses.
//!
//! Fast tier on purpose, mirroring `tests/daemon_status.rs` (fork #47) and
//! `tests/worktree_reclaim.rs` (PRD #422): the REAL `dot-agent-deck` binary
//! run as a subprocess against real git repos in tempdirs, with a synthetic
//! `gh` on `PATH`. No PTY, no daemon, no LLM, no `e2e` feature gate — this is
//! the tier CI actually blocks on, and the lock is exactly the part that
//! matters.
//!
//! There is today no `issue` subcommand on the CLI at all (`src/main.rs`'s
//! `Commands` enum has no `Issue` variant), so every test below fails via
//! clap's own "unrecognized subcommand" error rather than any assertion the
//! tests themselves make — the same honest RED `daemon_status.rs` documents
//! for itself. Each test calls [`assert_recognized_subcommand`] immediately
//! after every CLI invocation for exactly that reason: without it, a later
//! domain assertion's silence (e.g. "no gh calls were made") would be
//! indistinguishable from "clap rejected this before touching anything".
//!
//! The stub `gh` here is a genuine, stateful fixture (not a one-shot canned
//! reply): `issue comment`/`issue edit --add-label`/`issue edit
//! --add-assignee/--remove-assignee` persist into per-issue files under
//! `$GHSTUB_DIR`, and `issue view --json ...` reads them back — so a test
//! that claims twice in sequence (once as A, once as B) exercises the SAME
//! read-your-own-writes loop the real implementation will run against real
//! GitHub. `gh api user --jq .login` resolves to whatever
//! [`Fixture::set_login`] last wrote, standing in for "whoever `gh` is
//! currently authenticated as on this host".

use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(unix)]
use dot_agent_deck::agent_pty::DOT_AGENT_DECK_PANE_ID;
#[cfg(unix)]
use dot_agent_deck::worktree_reclaim::OWNER_MARKER_FILENAME;
use spec::spec;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A stateful synthetic `gh`, keyed by `--repo <owner/name>` (`/` → `_`) and
/// issue number, reading/writing canned data under `$GHSTUB_DIR`:
///   - `gh api user --jq .login` → the content of `$GHSTUB_DIR/login` (or
///     `stub-user` if unset), UNLESS `$GHSTUB_DIR/fail-api-user` exists, in
///     which case it fails like a real revoked/expired token would.
///   - `gh issue comment --repo R --body B -- N` → appends `{"body":"B"}` to
///     `$GHSTUB_DIR/<key>/issue-<n>/comments.jsonl`.
///   - `gh issue edit --repo R --add-label L -- N` → appends `L` to
///     `.../labels.txt` (idempotent).
///   - `gh issue edit --repo R --add-assignee A --remove-assignee B -- N` →
///     removes `B` then adds `A` to `.../assignees.txt` — either flag may be
///     absent, so a caller can issue the label/assignee writes as one
///     combined `issue edit` call or as separate calls; both shapes converge
///     on the same persisted state.
///   - `gh issue view --repo R --json ... -- N` → prints
///     `{"comments":[...],"labels":[...],"assignees":[...]}` assembled from
///     the three files above, always all three regardless of which `--json`
///     fields were requested (harmless: a real implementation only reads the
///     keys it asked for).
///
/// Every invocation is recorded verbatim (before any parsing) to
/// `$GHSTUB_DIR/gh-calls.log`, mirroring `tests/e2e_issue_dispatch.rs`'s
/// `GhStub` and `tests/worktree_reclaim.rs`'s stub — so a test can assert on
/// WHAT `gh` was asked to do without needing a new Rust type for a write path
/// that doesn't exist yet.
const GH_STUB_SCRIPT: &str = r#"#!/bin/sh
if [ "$1" = "api" ] && [ "$2" = "user" ]; then
    printf '%s\n' "$*" >> "$GHSTUB_DIR/gh-calls.log" 2>/dev/null || true
    if [ -f "$GHSTUB_DIR/fail-api-user" ]; then
        echo "gh: HTTP 401: Bad credentials (stubbed failure)" 1>&2
        exit 1
    fi
    if [ -f "$GHSTUB_DIR/login" ]; then
        cat "$GHSTUB_DIR/login"
    else
        printf 'stub-user\n'
    fi
    exit 0
fi

printf '%s\n' "$*" >> "$GHSTUB_DIR/gh-calls.log" 2>/dev/null || true

group="$1"
sub="$2"
shift 2 2>/dev/null || true

repo=""
issue=""
body=""
add_label=""
add_assignee=""
remove_assignee=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        --repo) shift; repo="$1" ;;
        --body) shift; body="$1" ;;
        --add-label) shift; add_label="$1" ;;
        --add-assignee) shift; add_assignee="$1" ;;
        --remove-assignee) shift; remove_assignee="$1" ;;
        --json) shift ;;
        --) ;;
        [0-9]*) issue="$1" ;;
        *) ;;
    esac
    shift
done
key=$(printf '%s' "$repo" | tr '/' '_')
issuedir="$GHSTUB_DIR/$key/issue-$issue"
mkdir -p "$issuedir" 2>/dev/null || true

if [ "$group" = "issue" ] && [ "$sub" = "comment" ]; then
    printf '{"body":"%s"}\n' "$body" >> "$issuedir/comments.jsonl"
    exit 0
fi

if [ "$group" = "issue" ] && [ "$sub" = "edit" ]; then
    if [ -n "$remove_assignee" ] && [ -f "$issuedir/assignees.txt" ]; then
        grep -vxF "$remove_assignee" "$issuedir/assignees.txt" > "$issuedir/assignees.txt.tmp" 2>/dev/null
        mv "$issuedir/assignees.txt.tmp" "$issuedir/assignees.txt" 2>/dev/null || true
    fi
    if [ -n "$add_assignee" ]; then
        grep -qxF "$add_assignee" "$issuedir/assignees.txt" 2>/dev/null || printf '%s\n' "$add_assignee" >> "$issuedir/assignees.txt"
    fi
    if [ -n "$add_label" ]; then
        grep -qxF "$add_label" "$issuedir/labels.txt" 2>/dev/null || printf '%s\n' "$add_label" >> "$issuedir/labels.txt"
    fi
    exit 0
fi

if [ "$group" = "issue" ] && [ "$sub" = "view" ]; then
    comments="[]"
    if [ -s "$issuedir/comments.jsonl" ]; then
        comments="[$(tr '\n' ',' < "$issuedir/comments.jsonl" | sed 's/,$//')]"
    fi
    labels="[]"
    if [ -s "$issuedir/labels.txt" ]; then
        labels="[$(sed 's/.*/{"name":"&"}/' "$issuedir/labels.txt" | tr '\n' ',' | sed 's/,$//')]"
    fi
    assignees="[]"
    if [ -s "$issuedir/assignees.txt" ]; then
        assignees="[$(sed 's/.*/{"login":"&"}/' "$issuedir/assignees.txt" | tr '\n' ',' | sed 's/,$//')]"
    fi
    printf '{"comments":%s,"labels":%s,"assignees":%s}\n' "$comments" "$labels" "$assignees"
    exit 0
fi

echo "gh stub: unhandled invocation: $group $sub $*" 1>&2
exit 1
"#;

struct Fixture {
    _scratch: tempfile::TempDir,
    repo: PathBuf,
    bindir: PathBuf,
    ghstub: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
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

fn worktree_git_dir(worktree: &Path) -> PathBuf {
    let out = Command::new("git")
        .current_dir(worktree)
        .args(["rev-parse", "--git-dir"])
        .output()
        .expect("git rev-parse --git-dir");
    let git_dir = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
    if git_dir.is_absolute() {
        git_dir
    } else {
        worktree.join(git_dir)
    }
}

fn combined(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Best-effort local hostname, via the same `hostname` binary the OS ships —
/// standing in for `src/issue_dispatch_run.rs::local_hostname`'s in-process
/// `gethostname(2)` call so a test can look for the CLI's own reported host
/// without depending on the exact resolution mechanism.
fn local_hostname() -> String {
    Command::new("hostname")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

impl Fixture {
    fn new() -> Self {
        let scratch = tempfile::tempdir().expect("scratch tempdir");
        let repo = scratch.path().join("repo");
        std::fs::create_dir_all(&repo).expect("create repo dir");
        git(&repo, &["init", "--initial-branch=main", "--quiet"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "user.name", "Test"]);
        std::fs::write(repo.join("README.md"), "seed\n").expect("write seed file");
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "--quiet", "-m", "seed"]);

        let bindir = scratch.path().join("bin");
        std::fs::create_dir_all(&bindir).expect("create bindir");
        let gh = bindir.join("gh");
        std::fs::write(&gh, GH_STUB_SCRIPT).expect("write gh stub");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755))
                .expect("chmod gh stub");
        }

        let ghstub = scratch.path().join("ghstub");
        std::fs::create_dir_all(&ghstub).expect("create ghstub dir");
        std::fs::write(ghstub.join("login"), "stub-user\n").expect("seed default login");

        Self {
            _scratch: scratch,
            repo,
            bindir,
            ghstub,
        }
    }

    /// Add a linked worktree at `<scratch>/<name>` on a new branch.
    fn add_worktree(&self, name: &str, branch: &str) -> PathBuf {
        let path = self._scratch.path().join(name);
        self.add_worktree_at(&path, branch);
        path
    }

    fn add_worktree_at(&self, path: &Path, branch: &str) {
        git(
            &self.repo,
            &[
                "worktree",
                "add",
                "-b",
                branch,
                &path.to_string_lossy(),
                "main",
            ],
        );
    }

    /// Write the `dot-agent-deck-owner` marker (fork #166/#425 format) so
    /// `owner_of` resolves `creator` for this worktree — mirroring
    /// `worktree_reclaim.rs`'s `Fixture::mark_owned_with_creator`.
    #[cfg(unix)]
    fn mark_owned(&self, worktree: &Path, creator: &str) {
        let git_dir = worktree_git_dir(worktree);
        std::fs::write(
            git_dir.join(OWNER_MARKER_FILENAME),
            format!("deck\ncreated-by: {creator}\n"),
        )
        .expect("write owner marker");
    }

    /// Set what `gh api user --jq .login` reports from here on — standing in
    /// for "whoever `gh` is currently authenticated as on this host".
    fn set_login(&self, login: &str) {
        std::fs::write(self.ghstub.join("login"), format!("{login}\n")).expect("write login");
    }

    /// Directly seed an issue as carrying `label` with NO discoverable claim
    /// comment — the hand-typed CLAUDE.md rule 14 claim (`gh issue edit
    /// --add-label in-progress`, typed by a human, never through this deck).
    /// Written straight to the stub's files, bypassing `gh` entirely, so it
    /// never appears in [`Fixture::gh_calls`].
    fn seed_label_only(&self, repo: &str, issue: u64, label: &str) {
        let key = repo.replace('/', "_");
        let dir = self.ghstub.join(&key).join(format!("issue-{issue}"));
        std::fs::create_dir_all(&dir).expect("create issue dir");
        std::fs::write(dir.join("labels.txt"), format!("{label}\n")).expect("seed label");
    }

    /// Run the REAL `dot-agent-deck` CLI as a subprocess in `cwd`, with the
    /// stub `gh` first on `PATH`. `pane_env` controls whether
    /// `DOT_AGENT_DECK_PANE_ID` is present (orchestration-shaped caller) or
    /// absent (human-shaped caller) — the M3 caller-identity gate's first
    /// signal.
    #[cfg(unix)]
    fn run(&self, cwd: &Path, args: &[&str], pane_env: bool) -> std::process::Output {
        let path = format!(
            "{}:{}",
            self.bindir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_dot-agent-deck"));
        cmd.current_dir(cwd)
            .args(args)
            .env("PATH", path)
            .env("GHSTUB_DIR", &self.ghstub);
        if pane_env {
            cmd.env(DOT_AGENT_DECK_PANE_ID, "claim-test-pane");
        } else {
            cmd.env_remove(DOT_AGENT_DECK_PANE_ID);
        }
        cmd.output().expect("run dot-agent-deck")
    }

    /// Every `gh` invocation recorded so far, one line per call, in call
    /// order — including across MULTIPLE `Fixture::run` invocations, since
    /// they all share this one `$GHSTUB_DIR/gh-calls.log`.
    fn gh_calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.ghstub.join("gh-calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The accumulated `comments.jsonl` lines for `repo`/`issue`, in the
    /// order `gh issue comment` calls appended them.
    fn comments(&self, repo: &str, issue: u64) -> Vec<String> {
        let key = repo.replace('/', "_");
        let path = self
            .ghstub
            .join(&key)
            .join(format!("issue-{issue}"))
            .join("comments.jsonl");
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The current `assignees.txt` content for `repo`/`issue`, one login per
    /// line — the replace-to-one state after every `--add-assignee`/
    /// `--remove-assignee` write so far.
    fn assignees(&self, repo: &str, issue: u64) -> Vec<String> {
        let key = repo.replace('/', "_");
        let path = self
            .ghstub
            .join(&key)
            .join(format!("issue-{issue}"))
            .join("assignees.txt");
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

/// Assert `out` was not rejected by clap's own generic unrecognized-subcommand
/// error — the honest RED shape today, since `issue claim` is not yet a real
/// subcommand (mirrors `daemon_status_003`'s and `worktree_reclaim_003`'s own
/// idiom). Every test below calls this immediately after every
/// [`Fixture::run`], so a later assertion's silence (e.g. "no gh calls were
/// made") is never "clap rejected this before touching anything" wearing a
/// domain assertion's clothes.
#[cfg(unix)]
fn assert_recognized_subcommand(out: &std::process::Output, label: &str) {
    assert_ne!(
        out.status.code(),
        Some(2),
        "{label}: exit code 2 is clap's own generic usage/parse-error code; `issue claim` is not \
         yet a recognized subcommand (PRD fork#235 — no `Issue` variant on `Commands` in \
         `src/main.rs`), so every assertion after this one would otherwise be vacuous rather \
         than evidence of the lock's actual behavior; status={:?} out={}",
        out.status,
        combined(out)
    );
    assert!(
        !combined(out).contains("Usage:"),
        "{label}: stderr still carries clap's own subcommand-usage banner, meaning `issue claim` \
         was not recognized as a real subcommand rather than being handled and making a real \
         decision; out={}",
        combined(out)
    );
}

/// Whether `line` (a `$GHSTUB_DIR/gh-calls.log` line, the stub's raw `$*`) is
/// a genuine `gh issue comment` WRITE — matched on the subcommand form
/// (`issue comment `), never by substring. `gh issue view --json
/// comments,labels,assignees` is a mandatory READ the lock must issue before
/// it can refuse and name the holder, but its own `--json` FIELD NAME
/// `comments` contains the substring `comment`, so a two-substring match
/// misclassifies that read as a write.
fn is_issue_comment_call(line: &str) -> bool {
    line.starts_with("issue comment ")
}

/// Whether any of `calls` is a write the lock must never make on a refusal:
/// an `--add-label` call, an `issue comment` call, or an
/// `--add-assignee`/`--remove-assignee` call.
fn any_claim_write(calls: &[String]) -> bool {
    calls.iter().any(|l| {
        l.contains("--add-label")
            || is_issue_comment_call(l)
            || l.contains("--add-assignee")
            || l.contains("--remove-assignee")
    })
}

/// Build a fixture with the standard `acme/widgets` repo and two
/// orchestration-owned worktrees — the two-holder setup shared by
/// `issue/claim/001`, `002`, `003` (`name_a`/`name_b` distinct) and `007`
/// (called with `name_b == name_a`, since that test's whole point is two
/// DIFFERENT worktrees claiming under the exact SAME typed name).
#[cfg(unix)]
fn two_orchestrations(name_a: &str, name_b: &str) -> (Fixture, &'static str, PathBuf, PathBuf) {
    let fx = Fixture::new();
    let repo = "acme/widgets";
    let wt_a = fx.add_worktree("wt-a", "orch-a-branch");
    fx.mark_owned(&wt_a, &format!("orchestration:{name_a}"));
    let wt_b = fx.add_worktree("wt-b", "orch-b-branch");
    fx.mark_owned(&wt_b, &format!("orchestration:{name_b}"));
    (fx, repo, wt_a, wt_b)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Scenario: Orchestration A claims issue 1 from its own worktree. A second,
/// DIFFERENT orchestration B then runs `issue claim 1` from its own worktree.
/// Assert B's claim exits non-zero, writes NOTHING (no label/assignee/comment
/// call is added to the gh call log during B's run), and B's stderr names A
/// and A's host — the centrepiece lock PRD fork#235 exists to add.
#[spec("issue/claim/001")]
#[test]
#[cfg(unix)]
fn issue_claim_001_second_orchestration_is_refused_and_writes_nothing() {
    let (fx, repo, wt_a, wt_b) = two_orchestrations("orch-A", "orch-B");

    fx.set_login("alice");
    let claim_a = fx.run(&wt_a, &["issue", "claim", "1", "--repo", repo], true);
    assert_recognized_subcommand(&claim_a, "A's initial claim");

    let calls_before_b = fx.gh_calls().len();
    fx.set_login("bob");
    let claim_b = fx.run(&wt_b, &["issue", "claim", "1", "--repo", repo], true);
    assert_recognized_subcommand(&claim_b, "B's refused claim");

    assert!(
        !claim_b.status.success(),
        "a second orchestration's claim on an issue already held by another identity must exit \
         non-zero; out={}",
        combined(&claim_b)
    );
    let text = combined(&claim_b);
    assert!(
        text.contains("orch-A"),
        "the refusal must name the current holder (`orch-A`); got:\n{text}"
    );
    let host = local_hostname();
    assert!(
        !host.is_empty() && text.contains(&host),
        "the refusal must name the holder's host, so a human can act on it (which machine may \
         still be running the other agent); expected host {host:?} in:\n{text}"
    );

    let new_calls: Vec<String> = fx.gh_calls().into_iter().skip(calls_before_b).collect();
    assert!(
        !any_claim_write(&new_calls),
        "a refused claim must write nothing — no label call, no assignee call, no comment call; \
         new gh calls during B's refused claim: {new_calls:?}"
    );
}

/// Scenario: With the issue already held by orchestration A, orchestration B
/// runs `issue claim --takeover` WITHOUT `--confirm-stopped`. Assert it still
/// exits non-zero, writes nothing, and its message instructs the caller to
/// re-run with `--confirm-stopped` — the two-step override is deliberate
/// friction, so an agent can never satisfy it in the same breath it discovers
/// the conflict.
#[spec("issue/claim/002")]
#[test]
#[cfg(unix)]
fn issue_claim_002_takeover_alone_still_refuses() {
    let (fx, repo, wt_a, wt_b) = two_orchestrations("orch-A", "orch-B");

    fx.set_login("alice");
    let claim_a = fx.run(&wt_a, &["issue", "claim", "2", "--repo", repo], true);
    assert_recognized_subcommand(&claim_a, "A's initial claim");

    let calls_before_b = fx.gh_calls().len();
    fx.set_login("bob");
    let claim_b = fx.run(
        &wt_b,
        &["issue", "claim", "2", "--repo", repo, "--takeover"],
        true,
    );
    assert_recognized_subcommand(&claim_b, "B's --takeover-alone claim");

    assert!(
        !claim_b.status.success(),
        "`--takeover` alone must still refuse; out={}",
        combined(&claim_b)
    );
    let text = combined(&claim_b);
    assert!(
        text.contains("--confirm-stopped"),
        "the refusal must instruct the caller to re-run with `--confirm-stopped`; got:\n{text}"
    );

    let new_calls: Vec<String> = fx.gh_calls().into_iter().skip(calls_before_b).collect();
    assert!(
        !any_claim_write(&new_calls),
        "`--takeover` alone must write nothing — an agent must not be able to satisfy the \
         override in the same breath it discovers the conflict; new gh calls: {new_calls:?}"
    );
}

/// Scenario: With the issue held by orchestration A, orchestration B runs
/// `issue claim --takeover --confirm-stopped`. Assert it succeeds; the
/// comment log ends up holding at least A's original claim AND B's takeover
/// comment, the LATEST of which still starts with `Claimed by ` (the literal
/// prefix `parse_claim_comment` finds claims by, so any other wording would
/// make it invisible and the system would still believe A holds the issue)
/// and names A in its tail; and the final assignee is B's human ONLY (A's
/// removed).
#[spec("issue/claim/003")]
#[test]
#[cfg(unix)]
fn issue_claim_003_takeover_confirm_stopped_succeeds_and_records_succession() {
    let (fx, repo, wt_a, wt_b) = two_orchestrations("orch-A", "orch-B");

    fx.set_login("alice");
    let claim_a = fx.run(&wt_a, &["issue", "claim", "3", "--repo", repo], true);
    assert_recognized_subcommand(&claim_a, "A's initial claim");

    fx.set_login("bob");
    let claim_b = fx.run(
        &wt_b,
        &[
            "issue",
            "claim",
            "3",
            "--repo",
            repo,
            "--takeover",
            "--confirm-stopped",
        ],
        true,
    );
    assert_recognized_subcommand(&claim_b, "B's takeover claim");
    assert!(
        claim_b.status.success(),
        "`--takeover --confirm-stopped` against a different holder must succeed; out={}",
        combined(&claim_b)
    );

    let calls = fx.gh_calls();
    let comment_calls: Vec<&String> = calls.iter().filter(|l| is_issue_comment_call(l)).collect();
    assert!(
        comment_calls.len() >= 2,
        "the comment log must hold both A's original claim and B's takeover comment, in order \
         (the deck APPENDS, never edits in place); observed comment-related gh calls: {calls:?}"
    );
    // The two claim-comment `gh` calls appear in the gh-calls.log in the
    // order they were actually invoked (A's run happened strictly before B's
    // in this test), so the LAST one is guaranteed to be B's regardless of
    // its exact wording.
    let last_comment = comment_calls.last().expect("at least one comment call");
    assert!(
        last_comment.contains("Claimed by "),
        "B's new comment must still start with `Claimed by ` — `parse_claim_comment` finds \
         claims via `.rfind` on that literal prefix, so wording it e.g. `Taken over by ...` \
         would be invisible to it and the system would still believe A holds the issue; got: \
         {last_comment}"
    );
    assert!(
        last_comment.contains("orch-A"),
        "B's new comment must name who it took over from in its tail; got: {last_comment}"
    );

    let assignees = fx.assignees(repo, 3);
    assert_eq!(
        assignees,
        vec!["bob".to_string()],
        "the assignee must end up as B's human ONLY — replace-to-one, A's login removed; got \
         {assignees:?}"
    );
}

/// Scenario: An issue carries the `in-progress` label with NO discoverable
/// claim comment — the hand-typed CLAUDE.md rule 14 claim, applied outside
/// any deck flow. Assert `issue claim` refuses (identity unknown) and writes
/// nothing.
#[spec("issue/claim/004")]
#[test]
#[cfg(unix)]
fn issue_claim_004_labelled_with_no_claim_comment_refuses() {
    let fx = Fixture::new();
    let repo = "acme/widgets";
    fx.seed_label_only(repo, 4, "in-progress");

    let wt = fx.add_worktree("wt-claimant", "claimant-branch");
    fx.mark_owned(&wt, "orchestration:orch-claimant");
    fx.set_login("carol");

    let out = fx.run(&wt, &["issue", "claim", "4", "--repo", repo], true);
    assert_recognized_subcommand(&out, "claim against a hand-labelled issue");
    assert!(
        !out.status.success(),
        "an issue labelled `in-progress` with no discoverable claim comment must refuse — the \
         holder's identity is unknown; out={}",
        combined(&out)
    );

    let calls = fx.gh_calls();
    assert!(
        !any_claim_write(&calls),
        "a refusal must write nothing; observed gh calls: {calls:?}"
    );
}

/// Scenario: With no `DOT_AGENT_DECK_PANE_ID` in the environment, a human
/// claims issue 5 — resolved as `human:<login>@<host>`. An orchestration then
/// runs `issue claim` on the SAME issue. Assert the orchestration's claim is
/// refused and its message names the human.
#[spec("issue/claim/005")]
#[test]
#[cfg(unix)]
fn issue_claim_005_human_claim_then_orchestration_is_refused_naming_human() {
    let fx = Fixture::new();
    let repo = "acme/widgets";
    fx.set_login("dave");
    let claim_human = fx.run(
        &fx.repo.clone(),
        &["issue", "claim", "5", "--repo", repo],
        false,
    );
    assert_recognized_subcommand(&claim_human, "human's initial claim");

    let wt_c = fx.add_worktree("wt-c", "orch-c-branch");
    fx.mark_owned(&wt_c, "orchestration:orch-C");
    fx.set_login("erin");
    let claim_orch = fx.run(&wt_c, &["issue", "claim", "5", "--repo", repo], true);
    assert_recognized_subcommand(&claim_orch, "orchestration claim after a human claim");

    assert!(
        !claim_orch.status.success(),
        "an orchestration claiming an issue already held by a human must be refused; out={}",
        combined(&claim_orch)
    );
    let text = combined(&claim_orch);
    assert!(
        text.contains("dave"),
        "the refusal must name the human holder (`human:dave@<host>`); got:\n{text}"
    );
}

/// Scenario: `DOT_AGENT_DECK_PANE_ID` is set (an agent pane, not a human
/// terminal) but the worktree carries NO owner marker — `write_owner_marker`
/// is best-effort by design, so a missing marker must never be read as "this
/// is a human". Assert the claim refuses, and specifically that it does NOT
/// fall back to `human:<login>` — every agent on one deck whose marker write
/// failed would otherwise resolve to the identical identity, and the lock
/// would read "held by me" and wave them all through while appearing to
/// work.
#[spec("issue/claim/006")]
#[test]
#[cfg(unix)]
fn issue_claim_006_pane_without_marker_refuses_without_downgrading_to_human() {
    let fx = Fixture::new();
    let repo = "acme/widgets";
    let wt = fx.add_worktree("wt-no-marker", "no-marker-branch");
    // Deliberately NOT marked owned via `fx.mark_owned`.
    fx.set_login("frank");

    let out = fx.run(&wt, &["issue", "claim", "6", "--repo", repo], true);
    assert_recognized_subcommand(&out, "pane env set, marker absent");
    assert!(
        !out.status.success(),
        "a pane with DOT_AGENT_DECK_PANE_ID set but no owner marker must refuse rather than \
         silently claiming; out={}",
        combined(&out)
    );
    let text = combined(&out).to_lowercase();
    assert!(
        !text.contains("human:frank") && !text.contains("human: frank"),
        "a missing owner marker must NOT downgrade to `human:frank` — every agent on one deck \
         whose marker write failed would otherwise resolve to the SAME identity, and the lock \
         would read \"held by me\" and wave them all through while appearing to work; got:\n{}",
        combined(&out)
    );

    let calls = fx.gh_calls();
    assert!(
        !any_claim_write(&calls),
        "a refusal must write nothing; observed gh calls: {calls:?}"
    );
}

/// Scenario: Two orchestrations share the exact SAME typed name but run from
/// TWO DIFFERENT worktrees — fork #201 records that orchestration-name
/// uniqueness is only advisory (two forms open at once are suggested the
/// same name and neither submit is refused), and its own text states plainly
/// "this is the case #74 is actually about". Comparing bare names would make
/// these two DISTINCT holders compare equal and hit the "held by this
/// identity → idempotent refresh, exit 0" row, waving both through in
/// exactly the scenario the lock exists for. Assert the SECOND same-named
/// orchestration's claim is REFUSED, not treated as an idempotent
/// self-refresh — the regression guard against anyone later "simplifying"
/// the identity back to a bare name.
#[spec("issue/claim/007")]
#[test]
#[cfg(unix)]
fn issue_claim_007_same_orchestration_name_different_worktree_is_refused_not_self_refresh() {
    let (fx, repo, wt_first, wt_second) = two_orchestrations("same-name", "same-name");

    fx.set_login("gina");
    let first = fx.run(&wt_first, &["issue", "claim", "7", "--repo", repo], true);
    assert_recognized_subcommand(&first, "first same-named orchestration's claim");

    fx.set_login("henry");
    let second = fx.run(&wt_second, &["issue", "claim", "7", "--repo", repo], true);
    assert_recognized_subcommand(&second, "second same-named orchestration's claim");

    assert!(
        !second.status.success(),
        "a SECOND orchestration sharing the first's exact typed name but running from a \
         DIFFERENT worktree must be REFUSED, not treated as an idempotent self-refresh of its \
         own claim — comparing bare names would make these two distinct holders compare equal \
         (fork #201: name uniqueness is only advisory, and this is the case fork #74 is \
         actually about); out={}",
        combined(&second)
    );

    let calls = fx.gh_calls();
    let comment_calls = calls.iter().filter(|l| is_issue_comment_call(l)).count();
    assert_eq!(
        comment_calls, 1,
        "only the FIRST same-named orchestration's claim may have posted a comment — a \
         self-refresh would post (or attempt) a second, and the assertion above already \
         requires the second run to have failed; observed gh calls: {calls:?}"
    );
}

/// Scenario: An orchestration whose worktree's absolute path is deliberately
/// forced to contain `/Users/` and `/home/`-shaped segments (regardless of
/// the host OS/CI runner) makes the FIRST, unlabelled claim on an issue.
/// Assert the posted claim comment carries a worktree DIGEST, never the raw
/// path — a claim comment is public, and a raw path leaks the OS username
/// and local directory layout.
#[spec("issue/claim/008")]
#[test]
#[cfg(unix)]
fn issue_claim_008_claim_comment_carries_worktree_digest_not_raw_path() {
    let fx = Fixture::new();
    let repo = "acme/widgets";
    let nested = fx
        ._scratch
        .path()
        .join("Users")
        .join("home")
        .join("leak-check-home");
    std::fs::create_dir_all(&nested).expect("create nested Users/home dir");
    let wt = nested.join("wt-leak");
    fx.add_worktree_at(&wt, "leak-branch");
    fx.mark_owned(&wt, "orchestration:orch-leak");
    fx.set_login("ivy");

    let out = fx.run(&wt, &["issue", "claim", "8", "--repo", repo], true);
    assert_recognized_subcommand(&out, "claim from a path containing /Users//home/ segments");
    assert!(
        out.status.success(),
        "an unlabelled issue's first claim must succeed; out={}",
        combined(&out)
    );

    let comments = fx.comments(repo, 8);
    assert!(
        !comments.is_empty(),
        "the claim must have posted a comment to inspect; observed gh calls: {:?}",
        fx.gh_calls()
    );
    let body = comments.join("\n");
    assert!(
        !body.contains("/Users/") && !body.contains("/home/"),
        "the claim comment must carry a worktree DIGEST, never a raw filesystem path — a public \
         comment leaking `/Users/<name>/...` or `/home/<name>/...` exposes the OS username and \
         local directory layout; worktree was {:?}, comment body was: {body:?}",
        wt
    );
}
