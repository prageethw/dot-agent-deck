//! RED tests for PRD #422's `dot-agent-deck worktree list|reclaim` — reclaiming
//! merged worktrees behind a three-part gate (PR state, cleanliness, ownership).
//!
//! Fast tier on purpose: these drive the REAL binary as a subprocess against
//! real git repositories in tempdirs, with a synthetic `gh` on `PATH`. No PTY,
//! no daemon, no LLM, no `e2e` feature gate — mirroring `tests/daemon_status.rs`
//! (PRD #47) for the CLI-subprocess shape and `tests/e2e_issue_dispatch.rs` for
//! the stub-`gh` seam.
//!
//! The two scenarios that carry the PRD's whole argument are `002` (a
//! squash-merged branch, whose commits are NOT in `main`'s ancestry — the case
//! `git branch --merged` misses) and `004` (a branch that IS an ancestor of
//! `main` but has no PR at all — the case `git branch --merged` would wrongly
//! delete).

use std::path::{Path, PathBuf};
use std::process::Command;

use spec::spec;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Synthetic `gh` reading canned PR JSON from `$GHSTUB_DIR`, keyed by the
/// `--head <branch>` value with `/` → `_`. A missing file means "no PR for this
/// branch" and prints `[]`, which is what real `gh pr list` does.
const GH_STUB_SCRIPT: &str = r#"#!/bin/sh
group="$1"
sub="$2"
shift 2 2>/dev/null || true

head=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        --head) shift; head="$1" ;;
        *) ;;
    esac
    shift
done

if [ "$group" = "pr" ] && [ "$sub" = "list" ]; then
    key=$(printf '%s' "$head" | tr '/' '_')
    if [ -f "$GHSTUB_DIR/pr-$key.json" ]; then
        cat "$GHSTUB_DIR/pr-$key.json"
    else
        printf '[]\n'
    fi
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

impl Fixture {
    /// A real git repo with one commit on `main`, plus a stub `gh` on `PATH`.
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

        Self {
            _scratch: scratch,
            repo,
            bindir,
            ghstub,
        }
    }

    /// Add a worktree at `<scratch>/<name>` on a NEW branch carrying its own
    /// commit — so the branch is *not* an ancestor of `main`, exactly like a
    /// branch whose PR was squash-merged.
    fn add_worktree_with_commit(&self, name: &str, branch: &str) -> PathBuf {
        let path = self._scratch.path().join(name);
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
        std::fs::write(path.join("work.txt"), "work\n").expect("write work file");
        git(&path, &["add", "work.txt"]);
        git(&path, &["commit", "--quiet", "-m", "work"]);
        path
    }

    /// Add a worktree whose branch points at `main` — an ancestor, so
    /// `git branch --merged main` reports it as merged even with no PR.
    fn add_worktree_at_main(&self, name: &str, branch: &str) -> PathBuf {
        let path = self._scratch.path().join(name);
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
        path
    }

    /// Canned `gh pr list --head <branch>` reply.
    fn set_pr_state(&self, branch: &str, state: &str) {
        let key = branch.replace('/', "_");
        let body = format!(r#"[{{"state":"{state}","headRefName":"{branch}"}}]"#);
        std::fs::write(self.ghstub.join(format!("pr-{key}.json")), body).expect("write pr fixture");
    }

    /// Mark a worktree as deck-created. The marker lives in the worktree's OWN
    /// git metadata dir (`<repo>/.git/worktrees/<name>/`), which is outside the
    /// working tree — so it can never make the tree dirty, and it is removed
    /// along with the worktree by `git worktree remove`.
    fn mark_owned(&self, worktree: &Path) {
        let out = Command::new("git")
            .current_dir(worktree)
            .args(["rev-parse", "--git-dir"])
            .output()
            .expect("git rev-parse --git-dir");
        let git_dir = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
        let git_dir = if git_dir.is_absolute() {
            git_dir
        } else {
            worktree.join(git_dir)
        };
        std::fs::write(git_dir.join("dot-agent-deck-owner"), "deck\n").expect("write owner marker");
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        let path = format!(
            "{}:{}",
            self.bindir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Command::new(env!("CARGO_BIN_EXE_dot-agent-deck"))
            .current_dir(&self.repo)
            .args(args)
            .env("PATH", path)
            .env("GHSTUB_DIR", &self.ghstub)
            .output()
            .expect("run dot-agent-deck")
    }
}

fn combined(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Scenario: In a repo with one deck-owned worktree whose PR is MERGED and
/// whose tree is clean, `worktree list` succeeds and reports that worktree as
/// removable. Pins the basic happy path and that the command exists at all.
#[spec("worktree/reclaim/001")]
#[test]
#[cfg(unix)]
fn worktree_reclaim_001_lists_a_removable_worktree() {
    let fx = Fixture::new();
    let wt = fx.add_worktree_with_commit("wt-merged", "feat/merged");
    fx.set_pr_state("feat/merged", "MERGED");
    fx.mark_owned(&wt);

    let out = fx.run(&["worktree", "list"]);
    assert!(
        out.status.success(),
        "`worktree list` must succeed in a git repo; got {:?} out={}",
        out.status,
        combined(&out)
    );
    let text = combined(&out);
    assert!(
        text.contains("wt-merged"),
        "`worktree list` must name the worktree it examined; got:\n{text}"
    );
}

/// Scenario: A deck-owned worktree whose branch was SQUASH-merged — its commit
/// is not in `main`'s ancestry, so `git branch --merged` says "not merged" —
/// is still reclaimed, because the gate reads the PR's state instead. After
/// `reclaim`, the worktree directory is gone and the branch still exists.
#[spec("worktree/reclaim/002")]
#[test]
#[cfg(unix)]
fn worktree_reclaim_002_squash_merged_clean_owned_is_reclaimed_and_branch_survives() {
    let fx = Fixture::new();
    let wt = fx.add_worktree_with_commit("wt-squash", "feat/squashed");
    fx.set_pr_state("feat/squashed", "MERGED");
    fx.mark_owned(&wt);

    // Precondition: git ancestry does NOT consider this merged. If this ever
    // stops holding, the test is no longer exercising the squash-merge case.
    let merged = Command::new("git")
        .current_dir(&fx.repo)
        .args(["branch", "--merged", "main", "--list", "feat/squashed"])
        .output()
        .expect("git branch --merged");
    assert!(
        String::from_utf8_lossy(&merged.stdout).trim().is_empty(),
        "fixture precondition: `git branch --merged main` must NOT list the squash-merge \
         stand-in branch, or this test proves nothing about ancestry-vs-PR-state"
    );

    let out = fx.run(&["worktree", "reclaim", "--yes"]);
    assert!(
        out.status.success(),
        "`worktree reclaim` must succeed; got {:?} out={}",
        out.status,
        combined(&out)
    );
    assert!(
        !wt.exists(),
        "a deck-owned, MERGED, clean worktree must be removed even though its commits are \
         not in main's ancestry (squash merge) — {} still exists\n{}",
        wt.display(),
        combined(&out)
    );

    let branches = Command::new("git")
        .current_dir(&fx.repo)
        .args(["branch", "--list", "feat/squashed"])
        .output()
        .expect("git branch --list");
    assert!(
        !String::from_utf8_lossy(&branches.stdout).trim().is_empty(),
        "the branch must survive the worktree's removal — committed work stays recoverable"
    );
}

/// Scenario: A deck-owned worktree whose PR is MERGED but which holds an
/// untracked file is NEVER removed, and the report says it was kept because it
/// is dirty. The untracked file was never in the PR, so it is genuinely not in
/// `main` — the one case "the code is already merged" does not cover.
#[spec("worktree/reclaim/003")]
#[test]
#[cfg(unix)]
fn worktree_reclaim_003_dirty_worktree_is_kept_even_when_merged() {
    let fx = Fixture::new();
    let wt = fx.add_worktree_with_commit("wt-dirty", "feat/dirty");
    fx.set_pr_state("feat/dirty", "MERGED");
    fx.mark_owned(&wt);
    std::fs::write(wt.join("scratch-notes.txt"), "never committed\n").expect("write untracked");

    let out = fx.run(&["worktree", "reclaim", "--yes"]);
    assert!(
        wt.exists(),
        "a dirty worktree must never be removed, even with a MERGED PR and --yes — {} is gone\n{}",
        wt.display(),
        combined(&out)
    );
    let text = combined(&out).to_lowercase();
    assert!(
        text.contains("dirty") || text.contains("uncommitted") || text.contains("untracked"),
        "the report must say WHY the worktree was kept, not merely omit it; got:\n{}",
        combined(&out)
    );
}

/// Scenario: A worktree whose branch points at `main` — so it IS an ancestor
/// and `git branch --merged` reports it as merged — but which has NO pull
/// request at all is never removed. This is the destructive direction of the
/// naive ancestry check, and the one that would delete a live scratch worktree.
#[spec("worktree/reclaim/004")]
#[test]
#[cfg(unix)]
fn worktree_reclaim_004_ancestor_branch_without_a_pr_is_never_removed() {
    let fx = Fixture::new();
    let wt = fx.add_worktree_at_main("wt-scratch", "chore/scratch");
    fx.mark_owned(&wt); // owned, clean, and an ancestor: only "no PR" protects it

    let merged = Command::new("git")
        .current_dir(&fx.repo)
        .args(["branch", "--merged", "main", "--list", "chore/scratch"])
        .output()
        .expect("git branch --merged");
    assert!(
        !String::from_utf8_lossy(&merged.stdout).trim().is_empty(),
        "fixture precondition: `git branch --merged main` MUST list this branch, or the test \
         is not exercising the ancestry false-positive it exists for"
    );

    let out = fx.run(&["worktree", "reclaim", "--yes"]);
    assert!(
        wt.exists(),
        "a worktree with no PR must never be removed, even when its branch is an ancestor of \
         main and `git branch --merged` calls it merged — {} is gone\n{}",
        wt.display(),
        combined(&out)
    );
}

/// Scenario: A FOREIGN worktree (no ownership marker) that is merged and clean
/// is not removed by a bare `reclaim`. The output must name its exact path and
/// offer the specific command that would remove it, rather than describing the
/// situation in prose.
#[spec("worktree/reclaim/005")]
#[test]
#[cfg(unix)]
fn worktree_reclaim_005_foreign_worktree_is_asked_about_not_removed() {
    let fx = Fixture::new();
    let wt = fx.add_worktree_with_commit("wt-foreign", "feat/foreign");
    fx.set_pr_state("feat/foreign", "MERGED");
    // Deliberately NOT marked owned.

    let out = fx.run(&["worktree", "reclaim"]);
    assert!(
        wt.exists(),
        "a worktree the deck cannot prove it created must not be removed without explicit \
         authorisation — {} is gone\n{}",
        wt.display(),
        combined(&out)
    );
    let text = combined(&out);
    assert!(
        text.contains("wt-foreign"),
        "the pending decision must name the exact worktree path, not a count or a category; \
         got:\n{text}"
    );
    assert!(
        text.contains("--yes"),
        "the non-interactive path must emit the exact command that would proceed, ready to \
         copy; got:\n{text}"
    );
}

/// Scenario: `worktree list --json` emits a machine-readable document carrying
/// `schema_version` and one entry per worktree with a verdict, so the classifier
/// is scriptable rather than only human-readable.
#[spec("worktree/reclaim/006")]
#[test]
#[cfg(unix)]
fn worktree_reclaim_006_json_output_carries_schema_version_and_verdicts() {
    let fx = Fixture::new();
    let wt = fx.add_worktree_with_commit("wt-json", "feat/json");
    fx.set_pr_state("feat/json", "MERGED");
    fx.mark_owned(&wt);

    let out = fx.run(&["worktree", "list", "--json"]);
    assert!(
        out.status.success(),
        "`worktree list --json` must succeed; got {:?} out={}",
        out.status,
        combined(&out)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let doc: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout must parse as JSON ({e}); got:\n{stdout}"));
    assert!(
        doc.get("schema_version").is_some(),
        "the JSON document must carry `schema_version`; got:\n{stdout}"
    );
    assert!(
        stdout.contains("wt-json"),
        "the JSON document must include the examined worktree; got:\n{stdout}"
    );
}
