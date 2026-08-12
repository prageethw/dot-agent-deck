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
//! **Round 3 (this file).** `issue claim` is a REAL, already-wired
//! subcommand (`src/issue_claim.rs`, dispatched from `src/main.rs`'s
//! `IssueCmd::Claim`) — M1-M4 landed and were green at `375285d`. What is
//! RED this round is the IDENTITY it resolves: `src/issue_claim.rs`'s
//! `resolve_caller_identity` still derives identity from
//! `DOT_AGENT_DECK_PANE_ID` (round 2 — wrong, because those values are small
//! daemon-scoped integers that recycle across a daemon restart, CLAUDE.md
//! rule 23), while every test below is re-keyed onto the round-3 design: an
//! agent's identity IS the worktree it is running in — its absolute path
//! plus its git branch, exactly the two identifiers CLAUDE.md rule 1 already
//! obliges the orchestrator to create and name — rendered into the claim
//! comment in rule 23's own hand-typed prose format (`Claimed by the
//! orchestration working \`<path>\` on branch \`<branch>\`.`), so the
//! mechanised claim and the hand-written one are one artefact. A human
//! claiming outside any worktree is unchanged: `human:<login>@<host>`. So a
//! failure here is a genuine behavioral mismatch (a claim wrongly refused,
//! or wrongly allowed through, or a pane id treated as load-bearing when it
//! must not be) — not a clap parse error. See
//! `prds/235-issue-claim-lock.md`'s "Identity, round 2" section (which also
//! documents round 3, the design that stuck) for the authoritative design
//! this round tests against.
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
use dot_agent_deck::issue_dispatch::{
    Identity, claim_comment_body, derive_issue_paths, parse_claim_fields,
};
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
///   - `gh issue comment --repo R --body B -- N` → appends
///     `{"body":"B","author":{"login":"<current $GHSTUB_DIR/login>"}}` to
///     `$GHSTUB_DIR/<key>/issue-<n>/comments.jsonl` — a real `gh issue
///     comment` is always posted as whoever `gh` is currently authenticated
///     as, so the stub's own author field tracks [`Fixture::set_login`] the
///     same way `gh api user` does (round-4 author gate,
///     `issue/claim/022`-`025`: nothing before this needed a comment's
///     AUTHOR, only its body, so this field did not exist until now).
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
    author="stub-user"
    if [ -f "$GHSTUB_DIR/login" ]; then
        author=$(cat "$GHSTUB_DIR/login")
    fi
    printf '{"body":"%s","author":{"login":"%s"}}\n' "$body" "$author" >> "$issuedir/comments.jsonl"
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
    /// Round 3: this marker is INERT for identity purposes — dropped
    /// entirely, not merely decorative. Identity is the worktree's own
    /// absolute path plus its git branch (CLAUDE.md rule 23), both derivable
    /// straight from `git`, so no marker is required or consulted. Calls to
    /// this helper remain in the fixtures that use it purely as a regression
    /// guard (`issue/claim/007`): even when two DIFFERENT worktrees are
    /// marked with the IDENTICAL decorative `creator` string, their paths and
    /// branches still differ, so they must still compare as different
    /// identities.
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

    /// Point the fixture repo's `origin` remote at `url` — so
    /// `derive_repo_slug` (`src/worktree_reclaim.rs`) can resolve an
    /// `owner/name` slug from it when `--repo` is omitted
    /// (`issue/claim/015`). Every linked worktree shares this same remote
    /// config (it lives in the common git dir), so this must be called
    /// before any worktree that needs it to claim.
    fn set_origin(&self, url: &str) {
        git(&self.repo, &["remote", "add", "origin", url]);
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

    /// Directly seed an UNLABELLED issue (no `in-progress` label written —
    /// unlike [`Fixture::seed_claim_comment`]) with exactly ONE claim comment
    /// whose raw body is `body` and whose comment `author` is `author` —
    /// bypassing `gh` entirely. Round-4 author gate (`issue/claim/022`-`025`):
    /// `decide_claim_unlabelled_always_claims` never inspects `held` for the
    /// LOCK decision on an unlabelled issue, but `do_claim`'s `prior_login`
    /// extraction (the replace-to-one REMOVAL target) is unconditional, so a
    /// bogus claim comment on an issue this deck never labelled can still
    /// drive a `gh issue edit --remove-assignee` it should never have
    /// written. `author` lets a test place that comment's author on either
    /// side of the gate: matching [`Fixture::set_login`]'s later value
    /// (self-authored, `023`/`025`) or a DIFFERENT stranger's login (`022`).
    fn seed_claim_comment_unlabelled_by(&self, repo: &str, issue: u64, body: &str, author: &str) {
        let key = repo.replace('/', "_");
        let dir = self.ghstub.join(&key).join(format!("issue-{issue}"));
        std::fs::create_dir_all(&dir).expect("create issue dir");
        let line = format!(
            "{}\n",
            serde_json::json!({ "body": body, "author": { "login": author } })
        );
        std::fs::write(dir.join("comments.jsonl"), &line).expect("seed unlabelled claim comment");
    }

    /// Run the REAL `dot-agent-deck` CLI as a subprocess in `cwd`, with the
    /// stub `gh` first on `PATH`. `pane_id`, when `Some`, sets
    /// `DOT_AGENT_DECK_PANE_ID` to that EXACT value — present vs. absent
    /// still switches between an agent-shaped caller and a plain human
    /// terminal, but round 3 keys identity equality on `cwd`'s worktree
    /// (its absolute path plus its git branch), NOT on this string: two
    /// calls sharing a `pane_id` from DIFFERENT worktrees are DIFFERENT
    /// identities, and two calls from the SAME worktree under DIFFERENT
    /// `pane_id`s are the SAME identity (`issue/claim/014`'s regression
    /// guard against round 2's mistake — pane ids recycle across a daemon
    /// restart, CLAUDE.md rule 23). `None` removes the variable entirely (a
    /// plain human terminal). `Some("")`/`Some("   ")` sets it to a blank
    /// value — the pane-env-present-but-blank case `issue/claim/006` pins.
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

    /// Directly seed `login` as a REAL current GitHub assignee for
    /// `repo`/`issue` — bypassing `gh` entirely, standing in for an assignee
    /// set by an earlier claim (or by a human, by hand) before this test
    /// starts observing calls. PRD fork#235 FINAL round 5: the removal
    /// target is computed as `current assignees − {{claimant}}`, read
    /// straight from `gh issue view`'s own `assignees` field — never from
    /// any claim comment's content — so this helper lets a test set up "who
    /// is REALLY assigned" independently of "what a claim comment happens to
    /// say" (`issue/claim/023`, `027`).
    fn seed_assignee(&self, repo: &str, issue: u64, login: &str) {
        let key = repo.replace('/', "_");
        let dir = self.ghstub.join(&key).join(format!("issue-{issue}"));
        std::fs::create_dir_all(&dir).expect("create issue dir");
        let path = dir.join("assignees.txt");
        let mut existing = std::fs::read_to_string(&path).unwrap_or_default();
        if !existing.lines().any(|l| l == login) {
            if !existing.is_empty() && !existing.ends_with('\n') {
                existing.push('\n');
            }
            existing.push_str(login);
            existing.push('\n');
        }
        std::fs::write(&path, existing).expect("seed assignee");
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

/// The fixed branch names [`two_orchestrations`] gives `wt_a`/`wt_b` — round
/// 3's identity anchor (CLAUDE.md rule 23), so tests assert on these
/// literally rather than on the (now-inert) decorative marker name.
#[cfg(unix)]
const BRANCH_A: &str = "orch-a-branch";
#[cfg(unix)]
const BRANCH_B: &str = "orch-b-branch";

/// Build a fixture with the standard `acme/widgets` repo and two
/// agent-shaped worktrees on [`BRANCH_A`]/[`BRANCH_B`], each ALSO carrying an
/// owner marker (now fully INERT for identity — see [`Fixture::mark_owned`]'s
/// doc) — the two-holder setup shared by `issue/claim/001`, `002`, `003`
/// (`name_a`/`name_b` distinct) and `007` (called with `name_b == name_a`,
/// since that test's whole point is two DIFFERENT holders sharing the exact
/// SAME decorative name, which must still not make them compare equal).
/// Callers may pass the SAME or DISTINCT `pane_id`s to [`Fixture::run`] for
/// `wt_a`/`wt_b` — round 3 keys identity equality on the worktree (path +
/// branch), never on the pane id.
#[cfg(unix)]
fn two_orchestrations(name_a: &str, name_b: &str) -> (Fixture, &'static str, PathBuf, PathBuf) {
    let fx = Fixture::new();
    let repo = "acme/widgets";
    let wt_a = fx.add_worktree("wt-a", BRANCH_A);
    fx.mark_owned(&wt_a, &format!("orchestration:{name_a}"));
    let wt_b = fx.add_worktree("wt-b", BRANCH_B);
    fx.mark_owned(&wt_b, &format!("orchestration:{name_b}"));
    (fx, repo, wt_a, wt_b)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Scenario: Agent pane A (`pane-a`) claims issue 1 from its own worktree
/// (`wt-a`, branch [`BRANCH_A`]). A second, DIFFERENT agent pane B (`pane-b`)
/// then runs `issue claim 1` from ITS OWN worktree (`wt-b`, branch
/// [`BRANCH_B`]). Assert B's claim exits non-zero, writes NOTHING (no
/// label/assignee/comment call is added to the gh call log during B's run),
/// and B's stderr names A's worktree's absolute path and A's branch — round
/// 3's identity anchor (CLAUDE.md rule 23), replacing round 2's
/// `DOT_AGENT_DECK_PANE_ID`-keyed identity — the centrepiece lock PRD
/// fork#235 exists to add.
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
    let wt_a_str = wt_a.to_string_lossy().into_owned();
    assert!(
        text.contains(&wt_a_str),
        "the refusal must name the current holder's worktree ABSOLUTE PATH — round 3's identity \
         anchor (CLAUDE.md rule 23), replacing round 2's pane id; expected {wt_a_str:?} in:\n{text}"
    );
    assert!(
        text.contains(BRANCH_A),
        "the refusal must also name the current holder's BRANCH — the other half of round 3's \
         identity anchor; expected {BRANCH_A:?} in:\n{text}"
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
/// A's worktree absolute path and branch — round 3's identity anchor
/// (CLAUDE.md rule 23) — in its tail; and the final assignee list holds ONLY
/// B's human — A's IS removed. **Reverted from round 4's expectation** (PRD
/// fork#235 FINAL round 5, superseding the author gate entirely): round 4
/// read the removal target from A's claim comment's `, for @alice` clause,
/// gated on that comment's author — since A's comment was authored by
/// `alice`, not B's account, the gate refused the removal and both stayed
/// assigned. Round 5 deletes that whole mechanism at the root: the removal
/// target is now `current GitHub assignees − { the claimant }`, read
/// straight from `gh issue view`'s own `assignees` field, never from any
/// comment's content or authorship. `alice` genuinely IS a current GitHub
/// assignee (added by A's own earlier claim), so B's takeover removes her —
/// replace-to-one is "always exactly one" again, not "exactly one per deck,
/// converging". The auditor's round-5 finding was that the author gate did
/// not narrow this removal, it DISABLED it: for any deck-authored comment
/// the `, for @X` clause always names the authenticated account, so `X ==
/// author` always, and the gate's `author == login_now` check then made the
/// self-cancelling filter drop the removal on every legitimate run — its
/// only reachable trigger in production was the parse-corruption attack it
/// existed to stop.
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
    let wt_a_str = wt_a.to_string_lossy().into_owned();
    assert!(
        last_comment.contains(&wt_a_str) && last_comment.contains(BRANCH_A),
        "B's new comment must name who it took over from — A's worktree absolute path \
         ({wt_a_str:?}) and branch ({BRANCH_A:?}), round 3's identity anchor (CLAUDE.md rule 23) \
         — in its tail; got: {last_comment}"
    );

    let assignees = fx.assignees(repo, 3);
    assert_eq!(
        assignees,
        vec!["bob".to_string()],
        "PRD fork#235 round 5: the removal target is `current GitHub assignees − {{the \
         claimant}}`, read from `gh issue view`'s own `assignees` field — never from any claim \
         comment's content or authorship. `alice` genuinely IS a current assignee (A's own \
         earlier claim added her), so B's takeover removes her and adds `bob`; replace-to-one is \
         restored to \"always exactly one\", reverting round 4's author-gated \
         `[\"alice\", \"bob\"]` expectation, which depended on a gate the round-5 audit found \
         disabled removal entirely rather than narrowing it; got {assignees:?}"
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
/// name (written into each worktree's inert owner marker, `Fixture::mark_owned`)
/// but run from TWO DIFFERENT worktrees under TWO DIFFERENT pane ids — fork
/// #201 records that orchestration-name uniqueness is only advisory (two
/// forms open at once are suggested the same name and neither submit is
/// refused), and its own text states plainly "this is the case #74 is
/// actually about". Round 3 keys identity on the worktree (path + branch),
/// never on the pane id or the decorative name, so this name collision now
/// TRIVIALLY cannot make these two DISTINCT holders compare equal — but the
/// test still exists as the regression guard against anyone later
/// "simplifying" the comparison back onto the decorative name (or the pane
/// id — `issue/claim/014` guards the same comparison from the opposite
/// direction). Assert the SECOND same-named pane's claim is REFUSED, not
/// treated as an idempotent self-refresh.
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
         from a DIFFERENT worktree under a DIFFERENT pane id must be REFUSED, not treated as an \
         idempotent self-refresh of its own claim — comparing on the decorative name (or the pane \
         id) would make these two distinct holders compare equal (fork #201: name uniqueness is \
         only advisory, and this is the case fork #74 is actually about); round 3's worktree-keyed \
         identity makes this trivially correct; out={}",
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

/// Scenario: An agent pane (`DOT_AGENT_DECK_PANE_ID` set) claims from a
/// worktree carrying NO owner marker at all — the orchestrator's OWN
/// dominant real path, since CLAUDE.md rule 1 mandates the orchestrator
/// create worktrees by hand with `git worktree add`, which writes no marker.
/// Round 1 refused this unconditionally (reviewer F1): the caller M5
/// rewrites rules 14/23 around could never actually claim. Round 3 makes this
/// even more foundational than round 2 did: the worktree's path and branch
/// are derivable straight from `git`, so no marker is EVER required — a
/// marker is not merely "decoration only", it is now entirely unconsulted
/// for identity.
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
         dominant real path (CLAUDE.md rule 1) — must be able to claim; round 3 derives identity \
         purely from the worktree's own path and branch, so no marker is ever required; out={}",
        combined(&out)
    );

    let calls = fx.gh_calls();
    assert!(
        any_claim_write(&calls),
        "a successful claim must have written the label/assignee/comment; observed gh calls: \
         {calls:?}"
    );
}

/// Scenario: An agent claims issue 10 from its own worktree (`wt-mine`,
/// branch `mine-branch`) — succeeds. The SAME pane id then runs `issue claim`
/// again from a DIFFERENT worktree (`wt-other`, branch `other-branch`,
/// belonging to a DIFFERENT orchestration) — standing in for the agent `cd`
/// -ing into it, deliberately or by accident. Assert the SECOND call is
/// REFUSED, naming `wt-mine`'s path and branch as the holder, and writes
/// nothing. This is the round-2 regression flipped: round 2 keyed identity on
/// `DOT_AGENT_DECK_PANE_ID` alone, so the SAME pane id `cd`-ing into another
/// orchestration's worktree was (wrongly) treated as an idempotent
/// self-refresh — letting any agent assume another's identity just by
/// entering its directory. Round 3 makes `cd`-ing into another worktree the
/// rule 1 violation it actually is: the worktree the caller is IN is now the
/// unit of identity, so entering someone else's is entering someone else,
/// full stop — pane id is no longer load-bearing either way.
#[spec("issue/claim/010")]
#[test]
#[cfg(unix)]
fn issue_claim_010_different_worktree_is_refused_even_with_same_pane_id() {
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

    let calls_before_second = fx.gh_calls().len();
    let second = fx.run(
        &wt_other,
        &["issue", "claim", "10", "--repo", repo],
        Some("pane-cd"),
    );
    assert!(
        !second.status.success(),
        "the SAME pane id claiming from a DIFFERENT worktree — even entering another \
         orchestration's own worktree, as if by `cd` — must be REFUSED, not treated as the same \
         identity; round 3 keys identity on the worktree the caller is actually in (CLAUDE.md \
         rule 23), never on DOT_AGENT_DECK_PANE_ID, which round 2 wrongly relied on and which \
         recycles across a daemon restart; out={}",
        combined(&second)
    );
    let text = combined(&second);
    let wt_mine_str = wt_mine.to_string_lossy().into_owned();
    assert!(
        text.contains(&wt_mine_str) && text.contains("mine-branch"),
        "the refusal must name the ORIGINAL holder's worktree absolute path and branch; got:\n{text}"
    );

    let new_calls: Vec<String> = fx
        .gh_calls()
        .into_iter()
        .skip(calls_before_second)
        .collect();
    assert!(
        !any_claim_write(&new_calls),
        "a refused claim must write nothing; new gh calls during the second run: {new_calls:?}"
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
///
/// **Passes for a different reason under round 5** (PRD fork#235 FINAL round
/// 5, checked per that round's own instruction to verify `011`/`019` still
/// exercise what they claim): the fix this test originally pinned was an
/// EXPLICIT same-login skip guard (`remove = prior_login.filter(|prior|
/// *prior != login)`) in the writer. Round 5 deletes that guard along with
/// the whole prior-login-from-a-comment mechanism it special-cased — the
/// removal target is now `current assignees − {{claimant}}`, a set
/// difference computed from `gh issue view`'s own `assignees` field, which
/// STRUCTURALLY excludes the claimant from their own removal set by
/// construction, with no special-casing required at all. So this test still
/// pins a real, still-true property (a same-identity refresh never
/// unassigns), but the MECHANISM that makes it true has moved from an
/// explicit guard to a structural set-difference — a future reader must not
/// look for the old guard and conclude it went missing.
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

/// Scenario: An issue's ONLY claim comment is hostile — and, unlike the
/// ORIGINAL version of this test, genuinely round-3-parseable. That original
/// wrote its hostile body in the round-2 comment shape the round-3 parser
/// rejects outright (`parse_claim_fields_rejects_old_format_comment` pins
/// that rejection elsewhere), so `parse_claim_fields` returned `None`, the
/// takeover only ever ran through `RefuseNoIdentity`'s `--takeover
/// --confirm-stopped` escape hatch, and every assertion below held
/// TRIVIALLY — reviewer and auditor found this independently: "it passes
/// with the parser deleted." This version's FIRST line is genuinely
/// well-formed round-3 worktree-shape text; a raw embedded newline then
/// introduces a FORGED second `Claimed by` line carrying a `, for
/// @forgedvictim` clause and cc mentions, so the parser genuinely has
/// adversarial content to resist. Assert directly on the PARSED fields (so
/// this test can never again silently defang itself if the format moves):
/// the recognised identity and timestamp come from the FIRST line only,
/// never the forged second one, and the recognised login is never the
/// forged `@forgedvictim` reachable only by scanning past the injected
/// newline. A legitimate agent pane then takes over the issue: assert no
/// `gh` call ever carries the forged clause as an argv value (in particular
/// never as a `--remove-assignee` value), and the deck's OWN newly-posted
/// comment never carries a LIVE (non-code-spanned) mention — including one
/// embedded in the LEGITIMATE branch text itself, confirming the wrapping
/// code-span mechanism holds even for recognised (not forged) data — auditor
/// F2/F3/F5, reviewer F8/F9. This PRD's Threat model places FORGERY of a
/// claim comment's authorship out of scope (anyone with comment access can
/// write one), but the PARSER consuming an already-located claim comment's
/// BODY must still not be corrupted by adversarial content within it.
#[spec("issue/claim/012")]
#[test]
#[cfg(unix)]
fn issue_claim_012_hostile_comment_body_does_not_confuse_the_parser() {
    let fx = Fixture::new();
    let repo = "acme/widgets";
    let legit_path = "/legit/holder/path";
    // Embeds a FOURTH mention directly in the LEGITIMATE (recognised)
    // branch text — no backtick can survive inside it (the parser's own
    // first-backtick-terminates rule forbids that structurally), so this
    // proves the wrapping code span protects even data that DOES reach
    // `held.identity`, not only the forged/garbage region that never does.
    let legit_branch = "legit-branch-cc-@forgedmention4";
    let legit_ts = "2020-01-01T00:00:00Z";
    let hostile_body = format!(
        "Claimed by the orchestration `orch-hostile` working `{legit_path}` on branch \
         `{legit_branch}` at {legit_ts}.\n\
         GARBAGE_MARKER_ZZZ Claimed by orchestration:forged-second working `/forged/path` on \
         branch `forged-branch` at 2099-01-01T00:00:00Z, for @forgedvictim, cc @forgedmention2, \
         mention @forgedmention3."
    );

    // The round-3 parser must genuinely consume this body — if it stops
    // parsing (returns `None`), everything below passes vacuously exactly
    // like the original `012` did. This is the guard the task exists to add
    // so this test can never silently defang itself again.
    let parsed = parse_claim_fields(&hostile_body).expect(
        "the round-3 parser must genuinely consume this hostile body — if this returns None the \
         rest of this test would pass vacuously, exactly the '012 is vacuous' finding this \
         rewrite exists to fix; see prds/235-issue-claim-lock.md's round-3 section",
    );
    assert_eq!(
        parsed.identity,
        format!("worktree:{legit_path}@{legit_branch}"),
        "the recognised identity must come from the FIRST (legitimate) `Claimed by` occurrence \
         ONLY — a forged SECOND `Claimed by` line injected across a raw newline must never \
         override it; got {:?}",
        parsed.identity
    );
    assert!(
        !parsed.timestamp.contains("GARBAGE_MARKER_ZZZ") && !parsed.timestamp.contains('\n'),
        "the recognised claim's timestamp must be scoped to the first line only — a forged \
         `Claimed by` line injected across a raw newline must never extend the timestamp \
         capture past that newline; got {:?}",
        parsed.timestamp
    );
    assert_ne!(
        parsed.login.as_deref(),
        Some("forgedvictim"),
        "a forged `, for @victim` clause reachable only by scanning past an injected newline \
         must never become the recognised claim's login; got {:?}",
        parsed.login
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
         into a `gh` argv value; raw gh-calls.log:\n{raw_calls_after}"
    );
    assert!(
        !fx.gh_calls()
            .iter()
            .any(|l| l.contains("--remove-assignee") && l.contains("forgedvictim")),
        "a forged `, for @victim` clause must never reach a `--remove-assignee` argv value; \
         observed gh calls: {:?}",
        fx.gh_calls()
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
    for mention in [
        "forgedvictim",
        "forgedmention2",
        "forgedmention3",
        "forgedmention4",
    ] {
        assert!(
            !raw_mention_present(new_content, mention),
            "the deck's own new comment must not carry a LIVE `@{mention}` mention — neither one \
             reproduced from the hostile prior comment's forged/garbage region, nor one embedded \
             in the LEGITIMATE recognised branch text (`forgedmention4`) — GitHub notifies on any \
             bare `@user` outside a code span; new comment content: {new_content:?}"
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

/// Scenario: The same worktree claims issue 14, then claims it AGAIN with a
/// DIFFERENT `DOT_AGENT_DECK_PANE_ID` — simulating a daemon restart that
/// recycled the pane-id counter (CLAUDE.md rule 23, verified 2026-08-10:
/// `DOT_AGENT_DECK_PANE_ID` values are "small daemon-scoped integers … and
/// they recycle across a daemon restart"; confirmed in code —
/// `next_pane_id`/`src/spawn.rs`'s `PANE_COUNTER` is a process-global atomic
/// that resets with the daemon). Assert the SECOND claim is recognized as the
/// SAME identity and succeeds as an idempotent refresh, never a refusal. This
/// is round 2's own regression, guarded directly: a pane-id-keyed identity
/// would make a restarted daemon's recycled id compare EQUAL to a totally
/// different, unrelated prior holder and wave it through (the fork
/// #160/#163/#166 incident CLAUDE.md rule 23 exists to prevent) — here the
/// worktree is unchanged and its OWN prior claim is correctly recognized
/// regardless of which pane id happens to be assigned this time.
#[spec("issue/claim/014")]
#[test]
#[cfg(unix)]
fn issue_claim_014_identity_survives_a_pane_id_change() {
    let fx = Fixture::new();
    let repo = "acme/widgets";
    let wt = fx.add_worktree("wt-restart", "restart-branch");
    fx.mark_owned(&wt, "orchestration:orch-restart");
    fx.set_login("liam");

    let first = fx.run(
        &wt,
        &["issue", "claim", "14", "--repo", repo],
        Some("pane-6"),
    );
    assert!(
        first.status.success(),
        "the first claim must succeed; out={}",
        combined(&first)
    );

    // Simulate a daemon restart recycling the pane-id counter: a DIFFERENT
    // pane id, same worktree.
    let second = fx.run(
        &wt,
        &["issue", "claim", "14", "--repo", repo],
        Some("pane-9"),
    );
    assert!(
        second.status.success(),
        "the SAME worktree re-claiming with a DIFFERENT pane id (simulating a recycled \
         daemon-restart pane id, CLAUDE.md rule 23) must be recognized as the SAME identity and \
         succeed as an idempotent refresh, never a refusal; out={}",
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

/// Scenario: An agent claims issue 15 with `--repo` OMITTED, from a worktree
/// whose `origin` remote resolves to `acme/widgets`. Assert the claim
/// succeeds and its output names the DERIVED repo explicitly. A second,
/// different identity's `--repo`-omitted claim on the SAME issue is then
/// refused, and its output ALSO names the derived repo. Reviewer F11: this
/// fork's `origin` is the fork itself while plenty of issues live upstream,
/// so a silently-derived repo could target the WRONG tracker — a reader must
/// be able to tell, from the output alone, which repo a success or a refusal
/// is even about.
#[spec("issue/claim/015")]
#[test]
#[cfg(unix)]
fn issue_claim_015_repo_omitted_derives_from_origin_and_is_shown() {
    let fx = Fixture::new();
    fx.set_origin("git@github.com:acme/widgets.git");

    let wt = fx.add_worktree("wt-derive", "derive-branch");
    fx.mark_owned(&wt, "orchestration:orch-derive");
    fx.set_login("maya");

    let claim = fx.run(&wt, &["issue", "claim", "15"], Some("pane-derive"));
    assert!(
        claim.status.success(),
        "a claim with --repo omitted must succeed by deriving the repo from `origin`; out={}",
        combined(&claim)
    );
    let claim_text = combined(&claim);
    assert!(
        claim_text.contains("acme/widgets"),
        "the success output must name the DERIVED repo explicitly — a silently-derived repo \
         could target the wrong tracker (this fork's `origin` vs. upstream, reviewer F11); \
         got:\n{claim_text}"
    );

    let wt2 = fx.add_worktree("wt-derive-2", "derive-branch-2");
    fx.mark_owned(&wt2, "orchestration:orch-derive-2");
    fx.set_login("noah");
    let refused = fx.run(&wt2, &["issue", "claim", "15"], Some("pane-derive-2"));
    assert!(
        !refused.status.success(),
        "a second identity's claim on the same issue must still be refused when --repo is \
         omitted; out={}",
        combined(&refused)
    );
    let refused_text = combined(&refused);
    assert!(
        refused_text.contains("acme/widgets"),
        "the refusal must ALSO name the derived repo — the reader must be able to tell WHICH \
         tracker (this fork vs. upstream) the refusal is even about; got:\n{refused_text}"
    );
}

// ---------------------------------------------------------------------------
// PRD fork#235 round-3 hardening — RED for the re-review findings (reviewer
// R1-R3, auditor A1/A3/A5/A6/A8). Reviewer and auditor both endorse the
// round-3 DESIGN (the marker supply gap and the pane-ID recycling bug are
// genuinely gone); every test below pins an IMPLEMENTATION defect with a
// named fix, not a design flaw.
// ---------------------------------------------------------------------------

/// Scenario: A claim comment is seeded directly for issue 16, naming the
/// EXACT worktree absolute path and branch a legitimate agent pane will
/// itself resolve as its own identity when it claims from that SAME
/// worktree — standing in for "a second deck, on a DIFFERENT physical host,
/// whose worktree happens to share this path" (ordinary, not exotic: e.g.
/// `/workspaces/<repo>` under Codespaces/devcontainers). Auditor A1, a
/// blocker: the compared identity string (`worktree:{path}@{branch}`,
/// `Identity::Worktree`'s `Display`) carries NO host component at all, so
/// this pane's claim is today indistinguishable from an idempotent refresh
/// of its OWN prior claim and succeeds — #74 verbatim, just moved from the
/// pane-id anchor (round 2) to the worktree-path anchor (round 3). Assert
/// the claim is REFUSED.
#[spec("issue/claim/016")]
#[test]
#[cfg(unix)]
fn issue_claim_016_host_is_not_part_of_the_identity() {
    let fx = Fixture::new();
    let repo = "acme/widgets";
    let wt = fx.add_worktree("wt-shared-path", "shared-branch");
    fx.mark_owned(&wt, "orchestration:orch-local");

    // Canonicalize before seeding: `std::env::current_dir()` inside the
    // spawned CLI process resolves the PHYSICAL path (e.g. macOS's `/tmp`
    // symlinking to `/private/tmp`), so seeding the LEXICAL `tempdir()` path
    // verbatim would make this test's own claim get refused on some
    // platforms for an unrelated reason (the `issue/claim/018` path-
    // normalization gap) rather than the host gap this test exists to pin —
    // exactly the "passes/fails for the wrong reason" trap this PRD's round
    // exists to close.
    let wt_canonical = std::fs::canonicalize(&wt).expect("canonicalize worktree path");
    let wt_str = wt_canonical.to_string_lossy().into_owned();
    let held_body = format!(
        "Claimed by the orchestration `orch-remote-host` working `{wt_str}` on branch \
         `shared-branch` at 2020-01-01T00:00:00Z."
    );
    fx.seed_claim_comment(repo, 16, &held_body);

    fx.set_login("localuser");
    let out = fx.run(
        &wt,
        &["issue", "claim", "16", "--repo", repo],
        Some("pane-local"),
    );
    assert!(
        !out.status.success(),
        "the SAME worktree absolute path and branch claimed from a DIFFERENT host must be \
         REFUSED — round 3's identity (`worktree:{{path}}@{{branch}}`) carries no host \
         component at all (auditor A1), so two decks on two different machines with identical \
         worktree paths compare EQUAL and both take the idempotent-refresh row today (#74 \
         verbatim); out={}",
        combined(&out)
    );
}

/// Scenario: An agent claims issue 17 from its worktree ROOT (`wt-sub`,
/// branch `sub-branch`), succeeding. A second claim then runs `issue claim`
/// again from a SUBDIRECTORY of that SAME worktree (`wt-sub/src`) — standing
/// in for an agent that legitimately `cd`s into a subdirectory mid-task,
/// which CLAUDE.md rule 1's "every pane in one worktree shares that
/// worktree's identity" already promises. Reviewer R1 / auditor A5: identity
/// anchors on `cwd` VERBATIM, not the worktree ROOT, so `<wt>/src`'s resolved
/// identity (`worktree:<wt>/src@sub-branch`) differs textually from
/// `<wt>`'s own (`worktree:<wt>@sub-branch`) and today's second claim is
/// wrongly REFUSED as a conflict with itself — the case most likely to be
/// hit in practice, since agents routinely operate from subdirectories.
/// Assert the second claim succeeds as an idempotent refresh, never a
/// refusal.
#[spec("issue/claim/017")]
#[test]
#[cfg(unix)]
fn issue_claim_017_subdirectory_shares_the_worktree_root_identity() {
    let fx = Fixture::new();
    let repo = "acme/widgets";
    let wt = fx.add_worktree("wt-sub", "sub-branch");
    fx.mark_owned(&wt, "orchestration:orch-sub");
    fx.set_login("olga");

    let root_claim = fx.run(
        &wt,
        &["issue", "claim", "17", "--repo", repo],
        Some("pane-root"),
    );
    assert!(
        root_claim.status.success(),
        "the first claim, from the worktree ROOT, must succeed (sanity precondition); out={}",
        combined(&root_claim)
    );

    let subdir = wt.join("src");
    std::fs::create_dir_all(&subdir).expect("create subdirectory");
    let sub_claim = fx.run(
        &subdir,
        &["issue", "claim", "17", "--repo", repo],
        Some("pane-sub"),
    );
    assert!(
        sub_claim.status.success(),
        "a claim from a SUBDIRECTORY of the SAME worktree must be recognized as the SAME \
         identity and succeed as an idempotent refresh — CLAUDE.md rule 1 promises every pane in \
         one worktree shares that worktree's identity, but `resolve_caller_identity` anchors on \
         `cwd` VERBATIM rather than the worktree ROOT (reviewer R1 / auditor A5); out={}",
        combined(&sub_claim)
    );
    let text = combined(&sub_claim).to_lowercase();
    assert!(
        !text.contains("held by") && !text.contains("refus"),
        "a same-identity refresh must not be reported as a refusal/held-by-another message; \
         out={}",
        combined(&sub_claim)
    );
}

/// Scenario: A claim comment is seeded from an `Identity` built with the
/// worktree's SYMLINKED (lexical, unresolved) path — the shape
/// `derive_issue_paths`/`Identity::issue_dispatch` produce when the
/// dispatch's configured workspace root itself contains a symlink component
/// (e.g. `/tmp` -> `/private/tmp` on macOS is the natural, non-contrived
/// case reviewer R2 names). A legitimate CLI `issue claim` is then run with
/// its OWN `cwd` set through that SAME symlink, exactly as a pane the
/// dispatch flow spawns into that path would be — `std::env::current_dir()`
/// (`run_issue_claim_cli`) resolves the PHYSICAL path per POSIX `getcwd`
/// semantics, so its resolved identity differs textually from the
/// lexical-path identity seeded above even though both name the exact same
/// worktree on disk. Assert the CLI claim is recognized as an idempotent
/// refresh of the SAME identity, never a refusal (reviewer R2 / auditor A6).
#[spec("issue/claim/018")]
#[test]
#[cfg(unix)]
fn issue_claim_018_symlinked_path_normalizes_to_one_identity() {
    let fx = Fixture::new();
    let repo = "acme/widgets";
    let wt_real = fx.add_worktree("wt-normalize", "normalize-branch");
    let wt_link = wt_real
        .parent()
        .expect("worktree has a parent dir")
        .join("wt-normalize-link");
    std::os::unix::fs::symlink(&wt_real, &wt_link).expect("create symlink to worktree");

    // The "dispatch side" identity, built from the LEXICAL (symlinked,
    // unresolved) path — exactly the shape `derive_issue_paths` produces
    // from configured workspace text with no canonicalization applied.
    let dispatch_identity = Identity::worktree(&wt_link, "normalize-branch");
    let body = claim_comment_body(&dispatch_identity, "2020-01-01T00:00:00Z", None, None);
    fx.seed_claim_comment(repo, 18, &body);

    fx.set_login("petra");
    // The CLI side: spawned with its `cwd` set through the SAME symlink —
    // `std::env::current_dir()` inside the child resolves the PHYSICAL path.
    let out = fx.run(
        &wt_link,
        &["issue", "claim", "18", "--repo", repo],
        Some("pane-normalize"),
    );
    assert!(
        out.status.success(),
        "a claim written through the dispatch path (lexical/symlinked path) and one made from \
         the CLI in that SAME worktree (which `std::env::current_dir()` resolves to the \
         PHYSICAL path) must compare EQUAL and succeed as an idempotent refresh — today the CLI \
         resolves a physical `getcwd` while the dispatch path builds a lexical path from \
         configured text, so a symlinked workspace can never match its own dispatch's claim \
         (reviewer R2 / auditor A6); out={}",
        combined(&out)
    );
    let text = combined(&out).to_lowercase();
    assert!(
        !text.contains("held by") && !text.contains("refus"),
        "a same-identity refresh must not be reported as a refusal/held-by-another message; \
         out={}",
        combined(&out)
    );
}

/// Scenario: A scheduled issue-dispatch task's NAME (hand-edited config,
/// untrusted per `sanitize_claimant_name`'s own doc) contains a raw backtick
/// immediately followed by an `@mention`. `sanitize_clone_segment` — which
/// derives the task's worktree PATH component — strips only `/ \ \0 ..`
/// (auditor A3), so the backtick and mention both survive intact into the
/// real worktree directory name that becomes `Identity::Worktree.path`.
/// Assert the FIRST claim comment the deck itself posts for that identity —
/// built by `claim_comment_body`, which wraps `path` in its own backtick
/// pair with NO further sanitization — never lets that mention go LIVE (i.e.
/// escape the wrapping code span), even though no forged/hostile COMMENT is
/// involved at all; the task NAME alone is the attack surface, producing a
/// live `@mention` (or, structurally the same bug, a forged `Claimed by`
/// line or a forged `, for @victim` clause) with no adversarial comment
/// required. Also covers the BRANCH component, which the SAME rendering
/// interpolates with the SAME lack of escaping.
#[spec("issue/claim/020")]
#[test]
fn issue_claim_020_adversarial_task_name_cannot_inject_a_live_mention() {
    let workspace = Path::new("/ws");
    let malicious_task_name = "evil`cc @forgedmention5 done";
    let paths = derive_issue_paths(workspace, malicious_task_name, 20);
    // Sanity: `sanitize_clone_segment` really did leave the backtick/mention
    // intact in the derived path — otherwise this test would pass vacuously
    // because the attack surface it exists to pin was already closed by a
    // DIFFERENT function than the one under test.
    assert!(
        paths
            .worktree_dir
            .to_string_lossy()
            .contains("evil`cc @forgedmention5"),
        "sanitize_clone_segment must leave the backtick/mention intact in the derived path for \
         this test to be exercising anything real; got {:?}",
        paths.worktree_dir
    );

    let identity =
        Identity::issue_dispatch(malicious_task_name, 20, &paths.worktree_dir, &paths.branch);
    let body = claim_comment_body(&identity, "2020-01-01T00:00:00Z", None, None);
    assert!(
        !raw_mention_present(&body, "forgedmention5"),
        "an adversarial issue-dispatch task NAME must never produce a LIVE `@mention` in the \
         deck's own claim comment via its WORKTREE PATH component — `sanitize_clone_segment` \
         strips only `/ \\ \\0 ..`, not backticks, so the task name's own backtick closes \
         `claim_comment_body`'s wrapping code span early and everything after it renders as \
         live markdown with NO forged comment involved at all; got body: {body:?}"
    );

    // The branch component is interpolated by the SAME template with the
    // SAME lack of sanitization — cover it too, directly, since no current
    // caller derives `branch` from untrusted config the way `path` is here.
    let branch_identity = Identity::worktree(
        Path::new("/ws/clean/path"),
        "clean-branch`cc @forgedmention6 done",
    );
    let branch_body = claim_comment_body(&branch_identity, "2020-01-01T00:00:00Z", None, None);
    assert!(
        !raw_mention_present(&branch_body, "forgedmention6"),
        "the SAME code-span escape must never be reachable via the BRANCH component either; got \
         body: {branch_body:?}"
    );
}

/// Scenario: A claim comment is seeded for issue 21 whose timestamp field
/// carries raw ESC and CR control characters — reachable because
/// `extract_timestamp` bounds its capture only at the next comma (or end of
/// string), with no character restriction at all. A DIFFERENT identity then
/// runs a bare `issue claim` against it, which is refused (the holder is
/// someone else). Auditor A4: `run_issue_claim`'s `RefuseHeldByOther` arm
/// sanitizes `holder` (via `sanitize_claimant_name`, applied in
/// `decide_claim`) but interpolates `held.timestamp` RAW into the ` since
/// {timestamp}` clause of the refusal message printed to the operator's
/// terminal — the earlier sanitizer fix cleaned only its sibling field.
/// Assert the refusal's combined output carries no raw ESC/CR control
/// characters.
#[spec("issue/claim/021")]
#[test]
#[cfg(unix)]
fn issue_claim_021_refusal_output_carries_no_raw_control_characters() {
    let fx = Fixture::new();
    let repo = "acme/widgets";
    let held_body = "Claimed by the orchestration `orch-holder` working `/held/path` on branch \
         `held-branch` at 2020-01-01T00:00:00Z\u{1b}[31mCONTROL-INJECTED\u{1b}[0m\r, for \
         @holderlogin.";
    fx.seed_claim_comment(repo, 21, held_body);

    let wt = fx.add_worktree("wt-control", "control-branch");
    fx.mark_owned(&wt, "orchestration:orch-control");
    fx.set_login("quinn");

    let out = fx.run(
        &wt,
        &["issue", "claim", "21", "--repo", repo],
        Some("pane-control"),
    );
    assert!(
        !out.status.success(),
        "a claim against an issue held by a DIFFERENT identity must be refused (sanity \
         precondition); out={}",
        combined(&out)
    );
    let text = combined(&out);
    assert!(
        !text.contains('\u{1b}') && !text.contains('\r'),
        "the refusal message must never carry a raw ESC or CR control character to the \
         operator's terminal — `run_issue_claim`'s `RefuseHeldByOther` arm sanitizes `holder` \
         but interpolates `held.timestamp` RAW into the ` since {{timestamp}}` clause (auditor \
         A4); got:\n{text:?}"
    );
}

/// Build a well-formed, genuinely round-3-parseable worktree-shape claim
/// comment body naming `login` in its trailing `, for @<login>` clause — the
/// shared shape `issue/claim/022`/`023`/`025` seed, varying only `login` and
/// the seeded comment's `author` (see [`Fixture::seed_claim_comment_unlabelled_by`]).
/// Distinct from `stranger-orch`/`/some/other/path` in each caller so a
/// failure names which test it came from.
fn stranger_claim_body(login: &str) -> String {
    format!(
        "Claimed by the orchestration `stranger-orch` working `/some/other/path` on branch \
         `some-other-branch` at 2020-01-01T00:00:00Z, for @{login}."
    )
}

/// Scenario: an UNLABELLED issue already carries a well-formed claim comment
/// — on its FIRST line, no forged newline needed — naming `victim` in its
/// `, for @victim.` clause, authored by `eve`, a stranger unrelated to the
/// claiming agent. A legitimate agent (`legit`) then runs a bare `issue
/// claim` (which always succeeds on an unlabelled issue, regardless of
/// `held`). Assert the claim still succeeds, but NO `gh` call ever carries
/// `--remove-assignee victim`.
///
/// **Re-pointed for round 5** (PRD fork#235 FINAL round 5 — the removal
/// target comes from GitHub, not comment text): this test predates round 5
/// and originally pinned the round-4 author gate (`held.author ==
/// login_now`). Round 5 deletes that gate along with the whole mechanism it
/// guarded — `do_claim` no longer parses ANY login out of a comment for a
/// write, so this test's original mechanism ("the author gate blocks a
/// stranger's comment") no longer exists to pin at all. The property that
/// still matters, and that this test still genuinely exercises: `victim` was
/// never added as a REAL GitHub assignee (only named in a comment), so the
/// round-5 removal target — `current assignees − {{claimant}}`, read
/// straight from `gh issue view`'s own `assignees` field — never contains
/// `victim` regardless of who authored the comment naming him, or whether
/// any author-gate exists at all. A future reader must not read this as
/// still guarding a gate that has been removed. Reachable exactly as the
/// PRD's original threat-model amendment describes: "a stranger posts a
/// well-formed single-line claim comment ending `, for @maintainer.` on an
/// unlabelled issue" — round 5 kills the whole class at the root rather than
/// gating each exit.
#[spec("issue/claim/022")]
#[test]
#[cfg(unix)]
fn issue_claim_022_stranger_comment_cannot_drive_a_removal() {
    let fx = Fixture::new();
    let repo = "acme/widgets";
    fx.seed_claim_comment_unlabelled_by(repo, 22, &stranger_claim_body("victim"), "eve");

    let wt = fx.add_worktree("wt-022", "claim-022-branch");
    fx.mark_owned(&wt, "orchestration:orch-022");
    fx.set_login("legit");

    let out = fx.run(
        &wt,
        &["issue", "claim", "22", "--repo", repo],
        Some("pane-022"),
    );
    assert!(
        out.status.success(),
        "a bare claim on an unlabelled issue must still succeed regardless of any stray claim \
         comment already present; out={}",
        combined(&out)
    );
    assert!(
        !fx.gh_calls()
            .iter()
            .any(|l| l.contains("--remove-assignee") && l.contains("victim")),
        "PRD fork#235 round 5: a claim comment's content — no matter who authored it — is never \
         consulted for a removal write at all; the removal target is `current GitHub assignees − \
         {{claimant}}`, and `victim` was never added as a real assignee here, so `victim` is \
         structurally absent from that set regardless of the comment naming him; observed gh \
         calls: {:?}",
        fx.gh_calls()
    );
}

/// Scenario: the SAME shape as `022` — an UNLABELLED issue carrying a
/// well-formed claim comment, self-authored by `legit` (the SAME account the
/// claiming agent is currently authenticated as), naming `priorholder` in its
/// `, for @priorholder.` clause — but `priorholder` is ALSO seeded directly
/// as a REAL current GitHub assignee, standing in for a human who assigned
/// the issue by hand before any deck ever claimed it. Assert the removal
/// still happens (`--remove-assignee priorholder` reaches `gh`).
///
/// **Repurposed for round 5** (PRD fork#235 FINAL round 5): this test
/// originally pinned the round-4 author gate opening for a self-authored
/// comment (self-authorship was WHY the removal was allowed). Round 5
/// deletes the author gate — comment authorship no longer has any bearing on
/// a removal at all — so `priorholder`'s self-authored `, for @priorholder.`
/// clause is no longer why this removal happens; it is now pure decoration.
/// The property this test is repurposed to pin instead: replace-to-one
/// applies uniformly, even to an issue's FIRST claim, against whatever is
/// ALREADY in GitHub's own assignees field — including an assignee a human
/// set by hand, never through this deck at all. That is the round-5 PRD's
/// own stated accepted cost, made concrete: "the deck can overwrite an
/// assignee a human set by hand, because GitHub's assignee list does not
/// record who set it." This test is a regression guard, not RED-first: the
/// removal already happens under round-4 code too (the self-authored gate
/// was open), so it stays GREEN before AND after the coder's round-5 fix —
/// only the REASON it passes changes.
#[spec("issue/claim/023")]
#[test]
#[cfg(unix)]
fn issue_claim_023_removes_an_assignee_a_human_set_by_hand() {
    let fx = Fixture::new();
    let repo = "acme/widgets";
    fx.seed_claim_comment_unlabelled_by(repo, 23, &stranger_claim_body("priorholder"), "legit");
    fx.seed_assignee(repo, 23, "priorholder");

    let wt = fx.add_worktree("wt-023", "claim-023-branch");
    fx.mark_owned(&wt, "orchestration:orch-023");
    fx.set_login("legit");

    let out = fx.run(
        &wt,
        &["issue", "claim", "23", "--repo", repo],
        Some("pane-023"),
    );
    assert!(
        out.status.success(),
        "a bare claim on an unlabelled issue must succeed; out={}",
        combined(&out)
    );
    assert!(
        fx.gh_calls()
            .iter()
            .any(|l| l.contains("--remove-assignee") && l.contains("priorholder")),
        "PRD fork#235 round 5: `priorholder` is a REAL current GitHub assignee (seeded directly, \
         standing in for a human who assigned the issue by hand), so replace-to-one removes them \
         on the claimant's first claim regardless of any comment's authorship — this is the \
         round-5 PRD's own accepted cost (the deck can overwrite an assignee a human set by \
         hand) made concrete, not the now-deleted author gate; observed gh calls: {:?}",
        fx.gh_calls()
    );
}

/// Scenario: an UNLABELLED issue carries a well-formed, SELF-authored claim
/// comment (author `legit`, matching the claiming agent's own login) whose
/// `, for @<login>` clause names a MALFORMED login: `-baduser025` (leading
/// `-`, failing [`dot_agent_deck::issue_dispatch::validate_gh_login`]'s
/// `^[A-Za-z0-9][A-Za-z0-9-]*$` shape). Assert the claim succeeds but NO `gh`
/// call ever carries `-baduser025` as a `--remove-assignee` value.
///
/// **Re-pointed for round 5** (PRD fork#235 FINAL round 5): this test
/// originally pinned `parse_worktree_claim`'s parser-boundary
/// `validate_gh_login` check (cause 1 of the round-4 PRD's two independent
/// causes) — the fact that `-baduser025` never reaches a `gh` argv because
/// the PARSE drops it, independently of the (then still-open) author gate.
/// Round 5 removes that mechanism's relevance entirely: `do_claim` no longer
/// parses ANY login out of a comment for a write, valid or not, so this is
/// no longer "a malformed value slips past validation" — there is no
/// validation-of-a-parsed-value step left to slip past. The property that
/// still matters, and that this test still genuinely exercises: `-baduser025`
/// was never added as a REAL GitHub assignee (only named, malformed, in a
/// comment), so the round-5 removal target — `current assignees −
/// {{claimant}}` — never contains it regardless. A malformed string like this
/// could in fact never BE a real GitHub login to begin with, which makes the
/// round-5 guarantee even more robust than the round-4 parser check it
/// replaces: it does not merely reject bad shapes, it never reads comment
/// content for this purpose at all.
#[spec("issue/claim/025")]
#[test]
#[cfg(unix)]
fn issue_claim_025_malformed_parsed_login_is_dropped_not_passed_through() {
    let fx = Fixture::new();
    let repo = "acme/widgets";
    fx.seed_claim_comment_unlabelled_by(repo, 25, &stranger_claim_body("-baduser025"), "legit");

    let wt = fx.add_worktree("wt-025", "claim-025-branch");
    fx.mark_owned(&wt, "orchestration:orch-025");
    fx.set_login("legit");

    let out = fx.run(
        &wt,
        &["issue", "claim", "25", "--repo", repo],
        Some("pane-025"),
    );
    assert!(
        out.status.success(),
        "a bare claim on an unlabelled issue must succeed; out={}",
        combined(&out)
    );
    assert!(
        !fx.gh_calls_raw().contains("-baduser025"),
        "PRD fork#235 round 5: comment content is never consulted for a removal write at all, so \
         a malformed `-baduser025` (which could never be a real GitHub login anyway) never \
         reaches a `gh` argv — `-baduser025` was never added as a real assignee, so it is \
         structurally absent from `current assignees − {{claimant}}`; observed \
         gh-calls.log:\n{}",
        fx.gh_calls_raw()
    );
}

// ---------------------------------------------------------------------------
// PRD fork#235 FINAL round 5 — the removal target comes from GitHub, not
// comment text. The round-5 audit found the round-4 author gate did not
// narrow replace-to-one, it DISABLED it: for any deck-authored comment the
// `, for @X` clause always names the authenticated account, so `X == author`
// always, and the gate's `author == login_now` check then made the
// self-cancelling filter drop the removal on every legitimate run — its only
// reachable trigger in production was the parse-corruption attack it existed
// to stop. Round 5 deletes the gate and the whole comment-parsing-for-writes
// mechanism at the root: `remove = current GitHub assignees − {the
// claimant}`, read from `gh issue view`'s own `assignees` field. See
// `prds/235-issue-claim-lock.md`'s "FINAL (round 5)" section.
// ---------------------------------------------------------------------------

/// Scenario: An issue already holds a REAL GitHub assignee, `priorholder` —
/// seeded directly into the stub's assignee list, standing in for an
/// assignee set by an earlier claim before this test starts observing calls.
/// Its held claim comment (naming the identity a takeover must compare
/// against for the lock decision) carries a `, for @wronguser.` clause that
/// DISAGREES with the real assignees list — `wronguser` is not, and never
/// was, an actual assignee, and the comment's author is left unset (nobody's
/// account) so a round-4-shaped implementation could never treat it as
/// self-authored either. A new agent pane takes over
/// (`--takeover --confirm-stopped`, required because the issue is held by a
/// different identity). Assert the takeover succeeds, `priorholder` — the
/// REAL prior assignee — is removed and the claimant added, and no `gh` call
/// ever carries `--remove-assignee wronguser`: the removal target comes from
/// GitHub's own assignees field, never from any comment's `, for @` clause,
/// which is now rendered for a human reader only and never parsed back for a
/// write.
#[spec("issue/claim/027")]
#[test]
#[cfg(unix)]
fn issue_claim_027_removal_target_comes_from_the_assignees_field() {
    let fx = Fixture::new();
    let repo = "acme/widgets";
    let held_body = "Claimed by the orchestration `orch-prior-027` working `/held/prior-027` on \
         branch `prior-027-branch` at 2020-01-01T00:00:00Z, for @wronguser.";
    fx.seed_claim_comment(repo, 27, held_body);
    fx.seed_assignee(repo, 27, "priorholder");

    let wt = fx.add_worktree("wt-027", "claim-027-branch");
    fx.mark_owned(&wt, "orchestration:orch-027");
    fx.set_login("claimant027");

    let out = fx.run(
        &wt,
        &[
            "issue",
            "claim",
            "27",
            "--repo",
            repo,
            "--takeover",
            "--confirm-stopped",
        ],
        Some("pane-027"),
    );
    assert!(
        out.status.success(),
        "a takeover of an issue held by a different identity must succeed; out={}",
        combined(&out)
    );

    let assignees = fx.assignees(repo, 27);
    assert_eq!(
        assignees,
        vec!["claimant027".to_string()],
        "the REAL prior assignee (`priorholder`, seeded directly into GitHub's own assignees \
         field) must be removed and the claimant added — the removal target is `current \
         assignees − {{claimant}}`, computed from `gh issue view`'s `assignees` field, never \
         from the held comment's `, for @wronguser.` clause; got {assignees:?}"
    );
    assert!(
        !fx.gh_calls_raw().contains("--remove-assignee wronguser"),
        "`wronguser`, named only in the held comment's `, for @` clause and never a real \
         assignee, must never reach a `--remove-assignee` argv; raw gh-calls.log:\n{}",
        fx.gh_calls_raw()
    );
    assert!(
        fx.gh_calls()
            .iter()
            .any(|l| l.contains("--remove-assignee") && l.contains("priorholder")),
        "the REAL prior assignee `priorholder` must be the one actually removed; observed gh \
         calls: {:?}",
        fx.gh_calls()
    );
}

/// Scenario: an UNLABELLED issue carries a well-formed claim comment,
/// self-authored by `claimant028` — the SAME account about to run the claim,
/// so a round-4-shaped author gate would have OPENED for it — naming
/// `victim` in its `, for @victim.` clause. `victim` is never seeded as a
/// real GitHub assignee. A bare `issue claim` (which always succeeds on an
/// unlabelled issue) is asserted to succeed with NO `--remove-assignee
/// victim` ever reaching `gh`.
///
/// This is B1's original exploit — "a stranger posts a well-formed
/// single-line claim comment ending `, for @maintainer.`; the next claim
/// removes `maintainer`" — re-pointed at the round-5 design, and
/// deliberately the ONE quadrant `022`/`023`/`025` don't already cover:
/// self-authored (so the now-deleted round-4 gate would have let it through)
/// AND naming a login that is not a real assignee (so round 5's own removal
/// target excludes it). Under CURRENT (round-4) code this genuinely
/// removes `victim` — the self-authored gate opens and nothing yet checks
/// whether `victim` is a real assignee — so this test is RED before the
/// coder's round-5 fix lands and GREEN after: `victim` stays unreachable for
/// a STRUCTURAL reason (absent from `current assignees`), never because of
/// comment-authorship.
#[spec("issue/claim/028")]
#[test]
#[cfg(unix)]
fn issue_claim_028_comment_naming_a_non_assignee_removes_nobody() {
    let fx = Fixture::new();
    let repo = "acme/widgets";
    fx.seed_claim_comment_unlabelled_by(repo, 28, &stranger_claim_body("victim"), "claimant028");
    // Deliberately no `seed_assignee` call: `victim` is named only in the
    // comment — self-authored though it is — and is never a real GitHub
    // assignee.

    let wt = fx.add_worktree("wt-028", "claim-028-branch");
    fx.mark_owned(&wt, "orchestration:orch-028");
    fx.set_login("claimant028");

    let out = fx.run(
        &wt,
        &["issue", "claim", "28", "--repo", repo],
        Some("pane-028"),
    );
    assert!(
        out.status.success(),
        "a bare claim on an unlabelled issue must succeed; out={}",
        combined(&out)
    );
    assert!(
        !fx.gh_calls_raw().contains("--remove-assignee victim"),
        "PRD fork#235 round 5: `victim`, named only in a SELF-authored comment's `, for @` \
         clause and absent from GitHub's own assignees field, must never reach a \
         `--remove-assignee` argv — the removal target is `current assignees − {{claimant}}`, \
         and `victim` is structurally absent from that set; this must now hold even though the \
         now-deleted round-4 author gate would have opened for this exact comment (self-authored \
         by the claimant), proving the guarantee no longer depends on authorship at all; raw \
         gh-calls.log:\n{}",
        fx.gh_calls_raw()
    );
}
