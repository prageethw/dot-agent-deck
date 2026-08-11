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
//! **Round 2 (this file).** `issue claim` is a REAL, already-wired
//! subcommand (`src/issue_claim.rs`, dispatched from `src/main.rs`'s
//! `IssueCmd::Claim`) — M1-M4 landed and were green at `375285d`. What is
//! RED this round is the IDENTITY it resolves: `src/issue_claim.rs`'s
//! `resolve_caller_identity` still derives identity from the worktree
//! ownership marker (round 1), while every test below is re-keyed onto the
//! round-2 design — identity is `agent:<pane-id>@<host>` from
//! `DOT_AGENT_DECK_PANE_ID`, no marker required, orchestration name rendered
//! as human-readable DECORATION only, never compared. So a failure here is a
//! genuine behavioral mismatch (a claim wrongly refused, or wrongly allowed
//! through, or a marker treated as load-bearing when it must not be) — not a
//! clap parse error. See `prds/235-issue-claim-lock.md`'s "Identity, round
//! 2" section for the authoritative design this round tests against.
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
//!
//! **Assignee-edit ordering (`issue/claim/011`).** The `--add-assignee
//! <X> --remove-assignee <X>` self-cancelling pair a same-identity refresh
//! emits behaves differently depending on which operation a real `gh`
//! applies last: applying the remove last (as this stub now does) nets the
//! issue UNASSIGNED — the real-`gh` defect reviewer finding F3 identified.
//! An earlier version of this stub applied remove-then-add (add last), which
//! netted ASSIGNED and therefore could never observe the defect — CI stayed
//! green while the underlying bug shipped. Fixed here so the stub is
//! faithful to what real `gh` actually does.

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
///     adds `A` THEN removes `B` (real-`gh`-ordering: whichever operation is
///     applied LAST wins when `A == B`, and a real `gh` applies the removal
///     last — see this file's own doc comment) — either flag may be absent,
///     so a caller can issue the label/assignee writes as one combined
///     `issue edit` call or as separate calls; both shapes converge on the
///     same persisted state.
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
    # Real-`gh`-ordering (reviewer F3, `issue/claim/011`): add is applied
    # FIRST, remove SECOND, so when `add_assignee == remove_assignee` (a
    # same-identity idempotent refresh) the remove wins and the issue ends
    # up UNASSIGNED, matching what a real `gh issue edit --add-assignee X
    # --remove-assignee X` actually does. For distinct values (a genuine
    # takeover) the final state is order-independent, so this changes
    # nothing for `issue/claim/003`.
    if [ -n "$add_assignee" ]; then
        grep -qxF "$add_assignee" "$issuedir/assignees.txt" 2>/dev/null || printf '%s\n' "$add_assignee" >> "$issuedir/assignees.txt"
    fi
    if [ -n "$remove_assignee" ] && [ -f "$issuedir/assignees.txt" ]; then
        grep -vxF "$remove_assignee" "$issuedir/assignees.txt" > "$issuedir/assignees.txt.tmp" 2>/dev/null
        mv "$issuedir/assignees.txt.tmp" "$issuedir/assignees.txt" 2>/dev/null || true
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

    /// Write the `dot-agent-deck-owner` marker (fork #166/#425 format) so a
    /// marker-reading implementation resolves `creator` for this worktree —
    /// mirroring `worktree_reclaim.rs`'s `Fixture::mark_owned_with_creator`.
    ///
    /// Round 2: this is DECORATION data only — the orchestration name it
    /// carries is rendered in a claim comment for a human to read, but never
    /// compared. Identity equality is on `DOT_AGENT_DECK_PANE_ID` alone (see
    /// [`Fixture::run`]'s `pane_id` parameter), so two calls sharing a
    /// worktree's marker are NOT automatically the same identity, and two
    /// calls with the SAME pane id but DIFFERENT markers (or no marker at
    /// all) ARE.
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

    /// Directly seed an issue as `in-progress`-labelled, with exactly ONE
    /// claim comment whose raw body is `body` — bypassing `gh` entirely, so
    /// `body` can be adversarial/malformed in ways this deck's OWN writer
    /// would never itself produce. Standing in for a comment the DECK's own
    /// `gh issue view` will read back as "the current holder" on its next
    /// invocation (`issue/claim/012`): this PRD's Threat model places
    /// forgery of the comment's AUTHOR out of scope, but the PARSER
    /// consuming its BODY still must not be corrupted by one. Returns the
    /// exact line written, so a caller can `strip_prefix` it off the file's
    /// later content to isolate whatever the deck itself appends next.
    fn seed_claim_comment(&self, repo: &str, issue: u64, body: &str) -> String {
        let key = repo.replace('/', "_");
        let dir = self.ghstub.join(&key).join(format!("issue-{issue}"));
        std::fs::create_dir_all(&dir).expect("create issue dir");
        std::fs::write(dir.join("labels.txt"), "in-progress\n").expect("seed label");
        let line = format!("{}\n", serde_json::json!({ "body": body }));
        std::fs::write(dir.join("comments.jsonl"), &line).expect("seed hostile comment");
        line
    }

    /// Run the REAL `dot-agent-deck` CLI as a subprocess in `cwd`, with the
    /// stub `gh` first on `PATH`. `pane_id`, when `Some`, sets
    /// `DOT_AGENT_DECK_PANE_ID` to that EXACT value (an agent-shaped caller —
    /// round 2 keys identity equality on this string, so two calls sharing a
    /// `pane_id` are the SAME identity regardless of `cwd`, and two calls
    /// with different `pane_id`s are different identities regardless of any
    /// worktree marker); `None` removes the variable entirely (a plain human
    /// terminal). `Some("")`/`Some("   ")` sets it to a blank value — the
    /// pane-env-present-but-blank case `issue/claim/006` pins.
    #[cfg(unix)]
    fn run(&self, cwd: &Path, args: &[&str], pane_id: Option<&str>) -> std::process::Output {
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
        match pane_id {
            Some(id) => {
                cmd.env(DOT_AGENT_DECK_PANE_ID, id);
            }
            None => {
                cmd.env_remove(DOT_AGENT_DECK_PANE_ID);
            }
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

    /// The raw, unparsed content of `$GHSTUB_DIR/gh-calls.log` — used
    /// instead of [`Fixture::gh_calls`] when an argv value under test may
    /// itself carry an embedded newline (`issue/claim/012`), which would
    /// otherwise corrupt line-based parsing.
    fn gh_calls_raw(&self) -> String {
        std::fs::read_to_string(self.ghstub.join("gh-calls.log")).unwrap_or_default()
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

/// Whether `body` carries `@handle` as a LIVE mention — i.e. OUTSIDE any
/// backtick-delimited code span, the shape GitHub actually notifies on.
/// Counts the backticks preceding each occurrence of `@handle`: an EVEN
/// count means the occurrence sits outside any open span (live, dangerous);
/// an ODD count means an odd number of code-span delimiters precede it, so
/// it is inside one (inert). This is the same code-span reasoning PRD #421's
/// C5/F3 fix and `sanitize_claimant_name` rely on — `issue/claim/012` uses it
/// to confirm the deck's OWN newly-posted comment never re-exposes a
/// hostile prior comment's embedded mention just because that comment's own
/// embedded backtick broke the deck's wrapping pair early.
fn raw_mention_present(body: &str, handle: &str) -> bool {
    let needle = format!("@{handle}");
    let mut search_from = 0;
    while let Some(idx) = body[search_from..].find(&needle) {
        let abs = search_from + idx;
        let backticks_before = body[..abs].matches('`').count();
        if backticks_before.is_multiple_of(2) {
            return true;
        }
        search_from = abs + needle.len();
    }
    false
}

/// Build a fixture with the standard `acme/widgets` repo and two
/// agent-shaped worktrees, each carrying an owner marker for DECORATION
/// (round 2 — see [`Fixture::mark_owned`]'s doc) — the two-holder setup
/// shared by `issue/claim/001`, `002`, `003` (`name_a`/`name_b` distinct) and
/// `007` (called with `name_b == name_a`, since that test's whole point is
/// two DIFFERENT holders sharing the exact SAME decorative name). Callers
/// must still pass DISTINCT `pane_id`s to [`Fixture::run`] for `wt_a` and
/// `wt_b` — identity equality is keyed on the pane id alone, not on which of
/// these two worktrees a call runs from.
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

/// Scenario: Agent pane A (`pane-a`) claims issue 1 from its own worktree. A
/// second, DIFFERENT agent pane B (`pane-b`) then runs `issue claim 1` from
/// its own worktree. Assert B's claim exits non-zero, writes NOTHING (no
/// label/assignee/comment call is added to the gh call log during B's run),
/// and B's stderr names A's decorative orchestration name and A's host — the
/// centrepiece lock PRD fork#235 exists to add, re-keyed round 2 onto
/// `DOT_AGENT_DECK_PANE_ID` rather than the worktree ownership marker.
#[spec("issue/claim/001")]
#[test]
#[cfg(unix)]
fn issue_claim_001_second_agent_pane_is_refused_and_writes_nothing() {
    let (fx, repo, wt_a, wt_b) = two_orchestrations("orch-A", "orch-B");

    fx.set_login("alice");
    let claim_a = fx.run(
        &wt_a,
        &["issue", "claim", "1", "--repo", repo],
        Some("pane-a"),
    );
    assert!(
        claim_a.status.success(),
        "A's own initial claim must succeed (sanity precondition); out={}",
        combined(&claim_a)
    );

    let calls_before_b = fx.gh_calls().len();
    fx.set_login("bob");
    let claim_b = fx.run(
        &wt_b,
        &["issue", "claim", "1", "--repo", repo],
        Some("pane-b"),
    );

    assert!(
        !claim_b.status.success(),
        "a second agent pane's claim on an issue already held by another identity must exit \
         non-zero; out={}",
        combined(&claim_b)
    );
    let text = combined(&claim_b);
    assert!(
        text.contains("orch-A"),
        "the refusal must name the current holder's decorative orchestration name (`orch-A`); \
         got:\n{text}"
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

/// Scenario: With the issue already held by agent pane A, agent pane B runs
/// `issue claim --takeover` WITHOUT `--confirm-stopped`. Assert it still
/// exits non-zero, writes nothing, and its message instructs the caller to
/// re-run with `--confirm-stopped` — the two-step override is deliberate
/// friction, so an agent can never satisfy it in the same breath it
/// discovers the conflict.
#[spec("issue/claim/002")]
#[test]
#[cfg(unix)]
fn issue_claim_002_takeover_alone_still_refuses() {
    let (fx, repo, wt_a, wt_b) = two_orchestrations("orch-A", "orch-B");

    fx.set_login("alice");
    let claim_a = fx.run(
        &wt_a,
        &["issue", "claim", "2", "--repo", repo],
        Some("pane-a"),
    );
    assert!(
        claim_a.status.success(),
        "A's own initial claim must succeed (sanity precondition); out={}",
        combined(&claim_a)
    );

    let calls_before_b = fx.gh_calls().len();
    fx.set_login("bob");
    let claim_b = fx.run(
        &wt_b,
        &["issue", "claim", "2", "--repo", repo, "--takeover"],
        Some("pane-b"),
    );

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

/// Scenario: With the issue held by agent pane A, agent pane B runs `issue
/// claim --takeover --confirm-stopped`. Assert it succeeds; the comment log
/// ends up holding at least A's original claim AND B's takeover comment, the
/// LATEST of which still starts with `Claimed by ` (the literal prefix
/// `parse_claim_comment` finds claims by, so any other wording would make it
/// invisible and the system would still believe A holds the issue) and names
/// A's decorative orchestration name in its tail; and the final assignee is
/// B's human ONLY (A's removed).
#[spec("issue/claim/003")]
#[test]
#[cfg(unix)]
fn issue_claim_003_takeover_confirm_stopped_succeeds_and_records_succession() {
    let (fx, repo, wt_a, wt_b) = two_orchestrations("orch-A", "orch-B");

    fx.set_login("alice");
    let claim_a = fx.run(
        &wt_a,
        &["issue", "claim", "3", "--repo", repo],
        Some("pane-a"),
    );
    assert!(
        claim_a.status.success(),
        "A's own initial claim must succeed (sanity precondition); out={}",
        combined(&claim_a)
    );

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
        Some("pane-b"),
    );
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
        "B's new comment must name who it took over from (A's decorative orchestration name) in \
         its tail; got: {last_comment}"
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

    let out = fx.run(
        &wt,
        &["issue", "claim", "4", "--repo", repo],
        Some("pane-claimant"),
    );
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
/// claims issue 5 — resolved as `human:<login>@<host>`. An agent pane then
/// runs `issue claim` on the SAME issue. Assert the agent's claim is refused
/// and its message names the human.
#[spec("issue/claim/005")]
#[test]
#[cfg(unix)]
fn issue_claim_005_human_claim_then_agent_pane_is_refused_naming_human() {
    let fx = Fixture::new();
    let repo = "acme/widgets";
    fx.set_login("dave");
    let claim_human = fx.run(
        &fx.repo.clone(),
        &["issue", "claim", "5", "--repo", repo],
        None,
    );
    assert!(
        claim_human.status.success(),
        "the human's initial claim must succeed (sanity precondition); out={}",
        combined(&claim_human)
    );

    let wt_c = fx.add_worktree("wt-c", "orch-c-branch");
    fx.mark_owned(&wt_c, "orchestration:orch-C");
    fx.set_login("erin");
    let claim_orch = fx.run(
        &wt_c,
        &["issue", "claim", "5", "--repo", repo],
        Some("pane-c"),
    );

    assert!(
        !claim_orch.status.success(),
        "an agent pane claiming an issue already held by a human must be refused; out={}",
        combined(&claim_orch)
    );
    let text = combined(&claim_orch);
    assert!(
        text.contains("dave"),
        "the refusal must name the human holder (`human:dave@<host>`); got:\n{text}"
    );
}

/// Scenario: `DOT_AGENT_DECK_PANE_ID` is SET but BLANK (empty, and
/// separately whitespace-only) — an agent-shaped caller whose pane id
/// somehow resolved to nothing, as opposed to `issue/claim/005`'s human case
/// where the variable is entirely ABSENT. Round 2 drops the worktree-marker
/// requirement entirely (`issue/claim/009` pins a marker-less pane claiming
/// successfully), so this test's meaning changed from round 1: it no longer
/// pins "marker absent → refuse" (there is no marker check left to pin).
/// What still must never happen is a blank pane id silently downgrading to
/// `human:<login>` — every agent on one deck whose pane id somehow resolved
/// blank would otherwise collapse to the SAME identity, and the lock would
/// read "held by me" and wave them all through while appearing to work,
/// exactly the failure mode this PRD exists to close.
#[spec("issue/claim/006")]
#[test]
#[cfg(unix)]
fn issue_claim_006_pane_env_set_but_blank_refuses_without_downgrading_to_human() {
    let fx = Fixture::new();
    let repo = "acme/widgets";
    fx.set_login("frank");

    for blank in ["", "   "] {
        let out = fx.run(
            &fx.repo.clone(),
            &["issue", "claim", "6", "--repo", repo],
            Some(blank),
        );
        assert!(
            !out.status.success(),
            "a pane env set to {blank:?} must refuse rather than silently claiming; out={}",
            combined(&out)
        );
        let text = combined(&out).to_lowercase();
        assert!(
            !text.contains("human:frank") && !text.contains("human: frank"),
            "a blank (but PRESENT) pane id must NOT downgrade to `human:frank` — every agent on \
             one deck whose pane id resolved blank would otherwise collapse to the SAME identity, \
             and the lock would read \"held by me\" and wave them all through while appearing to \
             work; pane env was {blank:?}, got:\n{}",
            combined(&out)
        );
    }

    let calls = fx.gh_calls();
    assert!(
        !any_claim_write(&calls),
        "a refusal must write nothing; observed gh calls: {calls:?}"
    );
}

/// Scenario: Two agent panes share the exact SAME decorative orchestration
/// name but run from TWO DIFFERENT worktrees under TWO DIFFERENT pane ids —
/// fork #201 records that orchestration-name uniqueness is only advisory
/// (two forms open at once are suggested the same name and neither submit is
/// refused), and its own text states plainly "this is the case #74 is
/// actually about". Comparing on the decorative name (instead of the pane
/// id) would make these two DISTINCT holders compare equal and hit the "held
/// by this identity → idempotent refresh, exit 0" row, waving both through
/// in exactly the scenario the lock exists for. Assert the SECOND
/// same-named pane's claim is REFUSED, not treated as an idempotent
/// self-refresh — the regression guard against anyone later "simplifying"
/// the comparison back onto the decorative name.
#[spec("issue/claim/007")]
#[test]
#[cfg(unix)]
fn issue_claim_007_same_decorative_name_different_pane_is_refused_not_self_refresh() {
    let (fx, repo, wt_first, wt_second) = two_orchestrations("same-name", "same-name");

    fx.set_login("gina");
    let first = fx.run(
        &wt_first,
        &["issue", "claim", "7", "--repo", repo],
        Some("pane-first"),
    );
    assert!(
        first.status.success(),
        "the first same-named pane's claim must succeed (sanity precondition); out={}",
        combined(&first)
    );

    fx.set_login("henry");
    let second = fx.run(
        &wt_second,
        &["issue", "claim", "7", "--repo", repo],
        Some("pane-second"),
    );

    assert!(
        !second.status.success(),
        "a SECOND agent pane sharing the first's exact decorative orchestration name but running \
         under a DIFFERENT pane id must be REFUSED, not treated as an idempotent self-refresh of \
         its own claim — comparing on the decorative name would make these two distinct holders \
         compare equal (fork #201: name uniqueness is only advisory, and this is the case fork \
         #74 is actually about); out={}",
        combined(&second)
    );

    let calls = fx.gh_calls();
    let comment_calls = calls.iter().filter(|l| is_issue_comment_call(l)).count();
    assert_eq!(
        comment_calls, 1,
        "only the FIRST same-named pane's claim may have posted a comment — a self-refresh would \
         post (or attempt) a second, and the assertion above already requires the second run to \
         have failed; observed gh calls: {calls:?}"
    );
}

/// Scenario: An agent pane whose worktree's absolute path is deliberately
/// forced to contain `/Users/` and `/home/`-shaped segments (regardless of
/// the host OS/CI runner) makes the FIRST, unlabelled claim on an issue.
/// Assert the posted claim comment never carries that raw path — a claim
/// comment is public, and a raw path leaks the OS username and local
/// directory layout. Round 2 keys identity on the pane id, not a worktree
/// digest, so there is no digest left to assert on here — what survives
/// unchanged from round 1 is the underlying "no path leak" requirement
/// itself.
#[spec("issue/claim/008")]
#[test]
#[cfg(unix)]
fn issue_claim_008_claim_comment_never_leaks_a_raw_worktree_path() {
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

    let out = fx.run(
        &wt,
        &["issue", "claim", "8", "--repo", repo],
        Some("pane-leak"),
    );
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
        "the claim comment must never carry a raw filesystem path — a public comment leaking \
         `/Users/<name>/...` or `/home/<name>/...` exposes the OS username and local directory \
         layout; worktree was {:?}, comment body was: {body:?}",
        wt
    );
}

/// Scenario: An agent pane (`DOT_AGENT_DECK_PANE_ID` set) claims from a
/// worktree carrying NO owner marker at all — the orchestrator's OWN
/// dominant real path, since CLAUDE.md rule 1 mandates the orchestrator
/// create worktrees by hand with `git worktree add`, which writes no marker.
/// Round 1 refused this unconditionally (reviewer F1): the caller M5
/// rewrites rules 14/23 around could never actually claim. Assert round 2
/// succeeds instead — a marker is decoration only, never required for
/// identity.
#[spec("issue/claim/009")]
#[test]
#[cfg(unix)]
fn issue_claim_009_pane_in_hand_made_worktree_without_marker_claims_successfully() {
    let fx = Fixture::new();
    let repo = "acme/widgets";
    let wt = fx.add_worktree("wt-hand-made", "hand-made-branch");
    // Deliberately NOT marked owned — mirrors rule 1's mandated flow: the
    // orchestrator creates worktrees by hand with `git worktree add`, which
    // writes no marker.
    fx.set_login("grace");

    let out = fx.run(
        &wt,
        &["issue", "claim", "9", "--repo", repo],
        Some("pane-hand-made"),
    );
    assert!(
        out.status.success(),
        "a deck-spawned pane in a marker-less, hand-made worktree — the orchestrator's own \
         dominant real path (CLAUDE.md rule 1) — must be able to claim; a marker is decoration \
         only in round 2, never required for identity; out={}",
        combined(&out)
    );

    let calls = fx.gh_calls();
    assert!(
        any_claim_write(&calls),
        "a successful claim must have written the label/assignee/comment; observed gh calls: \
         {calls:?}"
    );
}

/// Scenario: The SAME pane id claims issue 10 twice, from TWO DIFFERENT
/// working directories — its own worktree first, then a DIFFERENT
/// orchestration's own worktree second (standing in for the agent `cd`-ing
/// into it, deliberately or by accident). Assert the second call is
/// recognized as the SAME identity and succeeds as an idempotent refresh,
/// never as a refusal or an impersonation of the second worktree's
/// orchestration — `DOT_AGENT_DECK_PANE_ID` does not travel with `cd`, and
/// round 1's discarded pane-env value meant identity came entirely from
/// `cwd`, letting any agent assume another's identity this exact way.
#[spec("issue/claim/010")]
#[test]
#[cfg(unix)]
fn issue_claim_010_same_pane_id_different_cwd_is_idempotent_refresh_not_impersonation() {
    let fx = Fixture::new();
    let repo = "acme/widgets";
    let wt_mine = fx.add_worktree("wt-mine", "mine-branch");
    fx.mark_owned(&wt_mine, "orchestration:orch-mine");
    let wt_other = fx.add_worktree("wt-other", "other-branch");
    fx.mark_owned(&wt_other, "orchestration:orch-other");
    fx.set_login("henry");

    let first = fx.run(
        &wt_mine,
        &["issue", "claim", "10", "--repo", repo],
        Some("pane-cd"),
    );
    assert!(
        first.status.success(),
        "the first claim (from the agent's own worktree) must succeed; out={}",
        combined(&first)
    );

    let second = fx.run(
        &wt_other,
        &["issue", "claim", "10", "--repo", repo],
        Some("pane-cd"),
    );
    assert!(
        second.status.success(),
        "the SAME pane id claiming again from a DIFFERENT cwd — even another orchestration's own \
         worktree — must be recognized as the SAME identity and succeed as an idempotent refresh; \
         identity is keyed on DOT_AGENT_DECK_PANE_ID, which does not travel when an agent `cd`s, \
         never on cwd; out={}",
        combined(&second)
    );
    let text = combined(&second).to_lowercase();
    assert!(
        !text.contains("held by") && !text.contains("refus"),
        "a same-identity refresh must not be reported as a refusal/held-by-another message; \
         out={}",
        combined(&second)
    );
}

/// Scenario: The SAME pane id claims issue 11 twice in a row — an idempotent
/// refresh of its own claim. Assert the assignee ends up STILL SET to the
/// claiming human after the refresh. Today's writer emits `--add-assignee X
/// --remove-assignee X` (the same login for both) on a refresh, and under
/// real `gh`'s ordering (this file's stub is now fixed to match it —
/// `issue/claim/011` is the reason for that fix) that leaves the issue
/// UNASSIGNED — reviewer F3. Both this file's stub and the e2e stub
/// previously applied remove-before-add, which netted ASSIGNED and hid the
/// defect from CI entirely.
#[spec("issue/claim/011")]
#[test]
#[cfg(unix)]
fn issue_claim_011_idempotent_refresh_leaves_assignee_intact() {
    let fx = Fixture::new();
    let repo = "acme/widgets";
    let wt = fx.add_worktree("wt-refresh", "refresh-branch");
    fx.mark_owned(&wt, "orchestration:orch-refresh");
    fx.set_login("iris");

    let first = fx.run(
        &wt,
        &["issue", "claim", "11", "--repo", repo],
        Some("pane-refresh"),
    );
    assert!(
        first.status.success(),
        "the first claim must succeed; out={}",
        combined(&first)
    );
    let assignees_after_first = fx.assignees(repo, 11);
    assert_eq!(
        assignees_after_first,
        vec!["iris".to_string()],
        "the first claim must assign the claiming human; got {assignees_after_first:?}"
    );

    let second = fx.run(
        &wt,
        &["issue", "claim", "11", "--repo", repo],
        Some("pane-refresh"),
    );
    assert!(
        second.status.success(),
        "the SAME identity refreshing its own claim must succeed; out={}",
        combined(&second)
    );

    let assignees_after_second = fx.assignees(repo, 11);
    assert_eq!(
        assignees_after_second,
        vec!["iris".to_string()],
        "an idempotent refresh must leave the assignee INTACT — today's refresh path emits \
         `--add-assignee X --remove-assignee X` for the SAME login, and under real `gh`'s \
         ordering that nets the issue UNASSIGNED (reviewer F3); the writer must special-case a \
         same-identity refresh (e.g. skip the redundant remove, or skip the assignee write \
         entirely) rather than emit a self-cancelling pair; got {assignees_after_second:?}"
    );
}

/// Scenario: An issue's ONLY claim comment is hostile/malformed — crafted to
/// carry (a) an embedded newline followed by a forged second `Claimed by `
/// line, (b) a forged `, for @forgedvictim` clause, (c) a backtick inside
/// the claimed identity positioned to close a code span early, and (d) a
/// raw `@forgedmention` mention with a preceding space. A legitimate agent
/// pane then takes over the issue. Assert: no `gh` call the takeover makes
/// ever carries the forged/corrupted parse artifact as an argv value (in
/// particular never as a `--remove-assignee` value), and the deck's OWN
/// newly-posted comment never carries a LIVE (non-code-spanned) mention
/// reproduced from the hostile comment — auditor F2/F3/F5, reviewer F8/F9.
/// This PRD's Threat model places FORGERY of a claim comment's authorship
/// out of scope (anyone with comment access can write one), but the PARSER
/// consuming an already-located claim comment's BODY must still not be
/// corrupted by adversarial content within it.
#[spec("issue/claim/012")]
#[test]
#[cfg(unix)]
fn issue_claim_012_hostile_comment_body_does_not_confuse_the_parser() {
    let fx = Fixture::new();
    let repo = "acme/widgets";
    let hostile_identity = "orchestration:orch-hostile` cc @forgedmention@host-x:11112222";
    let hostile_body = format!(
        "Claimed by {hostile_identity} on `host-x` at 2020-01-01T00:00:00Z, for @forgedvictim.\n\
         GARBAGE_MARKER_ZZZ Claimed by orchestration:forged-second@host-y:33334444 on `host-y` \
         at 2099-01-01T00:00:00Z, for @forgedmention2, cc @forgedmention3."
    );
    let seeded_line = fx.seed_claim_comment(repo, 12, &hostile_body);

    let wt = fx.add_worktree("wt-takeover", "takeover-branch");
    fx.mark_owned(&wt, "orchestration:orch-legit");
    fx.set_login("legituser");

    let out = fx.run(
        &wt,
        &[
            "issue",
            "claim",
            "12",
            "--repo",
            repo,
            "--takeover",
            "--confirm-stopped",
        ],
        Some("pane-legit"),
    );
    assert!(
        out.status.success(),
        "a takeover of an issue held by a hostile/forged comment must still be able to succeed \
         (forgery of a claim comment's authorship is explicitly out of scope per this PRD's \
         Threat model) — what matters is that the hostile CONTENT cannot corrupt what the deck \
         itself does next; out={}",
        combined(&out)
    );

    // (a) The forged clause / corrupted parse artifact must never leak into
    // a `gh` argv value. Read the RAW log (not line-split) since a corrupted
    // multi-line parse, if it ever reached an argv, would itself carry a
    // literal embedded newline.
    let raw_calls_after = fx.gh_calls_raw();
    assert!(
        !raw_calls_after.contains("GARBAGE_MARKER_ZZZ"),
        "a forged clause reachable only by scanning past an injected newline must never leak \
         into a `gh` argv value (e.g. `--remove-assignee`); raw gh-calls.log:\n{raw_calls_after}"
    );

    // (b) The deck's OWN newly-posted comment must not carry a LIVE mention
    // reproduced from the hostile prior comment. Isolate exactly the bytes
    // the deck appended by stripping the KNOWN seeded prefix, rather than
    // relying on line-based parsing (which a corrupted echo could break).
    let raw_comments_after = std::fs::read_to_string(
        fx.ghstub
            .join(repo.replace('/', "_"))
            .join("issue-12")
            .join("comments.jsonl"),
    )
    .unwrap_or_default();
    let new_content = raw_comments_after
        .strip_prefix(&seeded_line)
        .unwrap_or(&raw_comments_after);
    assert!(
        !new_content.trim().is_empty(),
        "the takeover must have appended its own new comment; raw file content:\n{raw_comments_after}"
    );
    for mention in ["forgedmention", "forgedmention2", "forgedmention3"] {
        assert!(
            !raw_mention_present(new_content, mention),
            "the deck's own new comment must not carry a LIVE `@{mention}` mention reproduced \
             from the hostile prior comment — GitHub notifies on any bare `@user` outside a code \
             span, so echoing the hostile identity's own embedded backtick/mention verbatim would \
             make the deck itself page a real user it never intended to mention; new comment \
             content: {new_content:?}"
        );
    }
}

/// Scenario: An issue is labelled `in-progress` with no discoverable claim
/// comment (`issue/claim/004`'s state — `ClaimDecision::RefuseNoIdentity`).
/// Assert a bare claim still refuses (unchanged), but `--takeover
/// --confirm-stopped` ESCAPES it and succeeds. Reviewer/auditor F4: today
/// this state has no override path at all, and `do_claim`'s label-then-
/// comment write ordering can CREATE the state itself — a comment write
/// that fails after the label write already landed wedges the issue
/// permanently for both `issue claim` and `issue_dispatch`, since every
/// future claim attempt (with or without `--takeover`) hits the same
/// unconditional refusal. One transient failure must not be unrecoverable.
#[spec("issue/claim/013")]
#[test]
#[cfg(unix)]
fn issue_claim_013_refuse_no_identity_is_escapable_with_takeover_confirm_stopped() {
    let fx = Fixture::new();
    let repo = "acme/widgets";
    fx.seed_label_only(repo, 13, "in-progress");

    let wt = fx.add_worktree("wt-rescue", "rescue-branch");
    fx.mark_owned(&wt, "orchestration:orch-rescue");
    fx.set_login("kate");

    let bare = fx.run(
        &wt,
        &["issue", "claim", "13", "--repo", repo],
        Some("pane-rescue"),
    );
    assert!(
        !bare.status.success(),
        "a bare claim against a labelled-with-no-comment issue must still refuse \
         (issue/claim/004) — this is the state the escape hatch must be able to recover FROM; \
         out={}",
        combined(&bare)
    );

    let out = fx.run(
        &wt,
        &[
            "issue",
            "claim",
            "13",
            "--repo",
            repo,
            "--takeover",
            "--confirm-stopped",
        ],
        Some("pane-rescue"),
    );
    assert!(
        out.status.success(),
        "`RefuseNoIdentity` (labelled, but no claim comment names a holder — e.g. because a prior \
         claim's label write succeeded and its comment write then failed, or a human hand-typed \
         the label per CLAUDE.md rule 14) must be ESCAPABLE via `--takeover --confirm-stopped`, \
         the SAME override that recovers a known-holder refusal — otherwise one transient \
         comment-write failure permanently wedges the issue for both `issue claim` and \
         `issue_dispatch`; out={}",
        combined(&out)
    );
    let calls = fx.gh_calls();
    assert!(
        any_claim_write(&calls),
        "the rescue claim must actually write the label/assignee/comment; observed gh calls: \
         {calls:?}"
    );
}
