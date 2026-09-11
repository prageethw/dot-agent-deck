//! Tests for `dot-agent-deck worktree sync` (fork issue #744) — CLI-level
//! integration tests spawning the REAL binary as a subprocess, mirroring
//! `tests/worktree_reclaim.rs`'s own harness shape (fast tier: real git
//! subprocesses against real repositories in tempdirs, a synthetic `gh` on
//! `PATH`, no PTY, no daemon, no LLM).
//!
//! This file's `gh` stub additionally answers `gh repo view --json
//! defaultBranchRef` (fork issue #744's new default-branch resolution,
//! `worktree_reclaim::resolve_default_branch`), on top of the `pr list`
//! handling `tests/worktree_reclaim.rs` already established.
//!
//! The harder problem this file solves is making an isolated clone's
//! `origin` remote simultaneously (a) LOOK like a `git@github.com:...` URL
//! — required for `derive_repo_slug` to resolve a slug at all, which is the
//! gate `isolated_clone_report` already applies before ever calling `gh pr
//! list` for a candidate (fork#325 M4a auditor B3) — and (b) be genuinely
//! FETCHABLE against a real local repository, with no actual network
//! access, so `sync_merged_workspace_to_main`/`fetch_other_live_workspace`'s
//! real `git fetch`/`git merge --ff-only` calls have real ref advances to
//! observe (the same real-git-state discipline `orchestration/workspace/020`
//! already uses for its own direct-call unit test). `GIT_SSH_COMMAND`
//! pointed at a small POSIX shell script that ignores the SSH host argument
//! and execs the requested git server-side program (`git-upload-pack`)
//! directly against a real local repository is the standard technique for
//! this — verified empirically against this machine's own git before being
//! relied on here: `git remote get-url origin` still reports the literal
//! `git@github.com:...` string (no `insteadOf`-style rewrite is in play,
//! since none is configured), while `git fetch origin` genuinely reads from
//! the local repository the script redirects to.

use std::path::{Path, PathBuf};
use std::process::Command;

// Issue #322 (see `tests/worktree_reclaim.rs`'s own header for the full
// rationale): a disk-backed scratch dir, not the RAM-backed OS temp dir.
#[path = "../src/test_temp.rs"]
mod test_temp;

use spec::spec;

/// Synthetic `gh`, extending `tests/worktree_reclaim.rs`'s own `pr list`
/// stub shape with `repo view --json defaultBranchRef` support. Kept as a
/// SEPARATE script (not a shared `#[path]`-included one) because the two
/// files' fixtures diverge enough elsewhere (this file's fake-SSH origin
/// technique) that sharing would couple them for no real benefit — matching
/// `tests/worktree_reclaim.rs`'s own precedent of not sharing fixture code
/// with `tests/issue_claim.rs` despite both stubbing `gh api user`.
const GH_STUB_SCRIPT: &str = r#"#!/bin/sh
all_args="$*"
group="$1"
sub="$2"
shift 2 2>/dev/null || true

if [ "$group" = "repo" ] && [ "$sub" = "view" ]; then
    if [ -f "$GHSTUB_DIR/fail-repo-view" ]; then
        echo "gh stub: repo view stubbed to fail (simulating gh being unable to resolve the \
repository)" 1>&2
        exit 1
    fi
    repo=""
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --repo) shift; repo="$1" ;;
            --json) shift; ;;
            *)
                echo "gh stub: unrecognized flag \"$1\" in repo view -- full invocation: gh $all_args" 1>&2
                exit 1
                ;;
        esac
        shift
    done
    if [ -z "$repo" ]; then
        echo "gh stub: --repo is required for repo view -- full invocation: gh $all_args" 1>&2
        exit 1
    fi
    if [ -f "$GHSTUB_DIR/default-branch" ]; then
        db=$(cat "$GHSTUB_DIR/default-branch")
    else
        db="main"
    fi
    printf '{"defaultBranchRef":{"name":"%s"}}\n' "$db"
    exit 0
fi

if [ "$group" = "pr" ] && [ "$sub" = "list" ]; then
    head=""
    state=""
    repo=""
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --head) shift; head="$1" ;;
            --state) shift; state="$1" ;;
            --repo) shift; repo="$1" ;;
            --json) shift; ;;
            *)
                echo "gh stub: unrecognized flag \"$1\" in pr list -- full invocation: gh $all_args" 1>&2
                exit 1
                ;;
        esac
        shift
    done
    if [ -z "$repo" ]; then
        echo "gh stub: --repo is required -- full invocation: gh $all_args" 1>&2
        exit 1
    fi

    key=$(printf '%s' "$head" | tr '/' '_')
    file="$GHSTUB_DIR/pr-$key.json"
    if [ ! -f "$file" ]; then
        printf '[]\n'
        exit 0
    fi

    fixture_state=$(grep -o '"state":"[A-Z]*"' "$file" | head -n1 | cut -d'"' -f4)
    norm_state=$(printf '%s' "$state" | tr 'A-Z' 'a-z')
    case "$norm_state" in
        all) match=1 ;;
        merged) if [ "$fixture_state" = "MERGED" ]; then match=1; else match=0; fi ;;
        closed) if [ "$fixture_state" = "CLOSED" ]; then match=1; else match=0; fi ;;
        open|"") if [ "$fixture_state" = "OPEN" ]; then match=1; else match=0; fi ;;
        *)
            echo "gh stub: unrecognized --state value \"$state\" -- full invocation: gh $all_args" 1>&2
            exit 1
            ;;
    esac

    if [ "$match" = "1" ]; then
        cat "$file"
    else
        printf '[]\n'
    fi
    exit 0
fi

echo "gh stub: unhandled invocation: $group $sub $all_args" 1>&2
exit 1
"#;

/// A `GIT_SSH_COMMAND` script: ignores the SSH host argument (`$1`) entirely
/// and execs the requested git server-side program (`git-upload-pack` etc.)
/// directly against `$FAKE_SSH_REAL_REPO` — a real local repository standing
/// in for "the repository actually on GitHub". `$2` is the single
/// shell-quoted remote command git's ssh transport passes (e.g.
/// `git-upload-pack 'test-org/test-repo.git'`); only the leading command
/// word is trusted, the quoted path argument is discarded and replaced with
/// the real local path — this stub answers exactly one repository, so there
/// is nothing to route by name.
const FAKE_SSH_SCRIPT: &str = r#"#!/bin/sh
cmd_word=$(printf '%s' "$2" | awk '{print $1}')
case "$cmd_word" in
    git-upload-pack|git-receive-pack|git-upload-archive) ;;
    *)
        echo "fake-ssh: unrecognized remote command: $2" 1>&2
        exit 1
        ;;
esac
exec "$cmd_word" "$FAKE_SSH_REAL_REPO"
"#;

/// The fake GitHub remote URL every fixture repository (root and isolated
/// clone alike) carries as its `origin` — never dereferenced for real; `git
/// remote get-url`/`derive_repo_slug` read it literally, and
/// `GIT_SSH_COMMAND` (see [`FAKE_SSH_SCRIPT`]) redirects any actual fetch
/// against it to a real local repository instead.
const FAKE_ORIGIN_URL: &str = "git@github.com:test-org/test-repo.git";

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

fn git_output(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn head_sha(dir: &Path) -> String {
    git_output(dir, &["rev-parse", "HEAD"])
}

/// FNV-1a, 64-bit — a small, well-known, public hash function (not
/// crate-internal knowledge), reimplemented here byte-for-byte matching
/// `src/platform/lock/mod.rs::fnv1a64` (`pub(crate)`, unreachable from this
/// external integration-test crate), the same precedent
/// `tests/worktree_reclaim.rs::fnv1a64` already establishes.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

struct Fixture {
    _scratch: tempfile::TempDir,
    repo: PathBuf,
    bindir: PathBuf,
    ghstub: PathBuf,
    state_dir: PathBuf,
    ssh_script: PathBuf,
}

impl Fixture {
    /// A real git repo (`main` branch, one seed commit) whose `origin`
    /// remote is [`FAKE_ORIGIN_URL`] — never fetched directly; it exists
    /// only so `derive_repo_slug` has something to parse.
    fn new() -> Self {
        let scratch = test_temp::tempdir().expect("scratch tempdir");

        let repo = scratch.path().join("repo");
        std::fs::create_dir_all(&repo).expect("create repo dir");
        git(&repo, &["init", "--initial-branch=main", "--quiet"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "user.name", "Test"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("README.md"), "seed\n").expect("write seed file");
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "--quiet", "-m", "seed"]);
        git(&repo, &["remote", "add", "origin", FAKE_ORIGIN_URL]);

        let bindir = scratch.path().join("bin");
        std::fs::create_dir_all(&bindir).expect("create bindir");
        let gh = bindir.join("gh");
        std::fs::write(&gh, GH_STUB_SCRIPT).expect("write gh stub");

        let ssh_script = scratch.path().join("fake-ssh.sh");
        std::fs::write(&ssh_script, FAKE_SSH_SCRIPT).expect("write fake-ssh script");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755))
                .expect("chmod gh stub");
            std::fs::set_permissions(&ssh_script, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fake-ssh script");
        }

        let ghstub = scratch.path().join("ghstub");
        std::fs::create_dir_all(&ghstub).expect("create ghstub dir");

        let state_dir = scratch.path().join("state");
        std::fs::create_dir_all(&state_dir).expect("create state dir");

        Self {
            _scratch: scratch,
            repo,
            bindir,
            ghstub,
            state_dir,
            ssh_script,
        }
    }

    /// Provision a real, on-disk isolated clone: a genuine `git clone` of
    /// `self.repo`, checked out on a NEW branch, with its `origin` remote
    /// repointed at [`FAKE_ORIGIN_URL`] (matching production —
    /// `provision_isolated_clone_sync_resolved` points a fresh clone's
    /// origin at the SOURCE's own origin URL, never leaving the plain `git
    /// clone` local-path default in place — see
    /// `issue_dispatch_run.rs::point_isolated_clone_origin`'s own doc
    /// comment), plus a hand-written M4b provenance artifact matching
    /// `write_isolated_clone_provenance`'s on-disk format exactly, the same
    /// precedent `tests/worktree_reclaim.rs::Fixture::provision_isolated_clone`
    /// already establishes (that function is `pub(crate)` and unreachable
    /// from this external integration-test crate).
    fn provision_isolated_clone(&self, name: &str, branch: &str, creator: &str) -> PathBuf {
        let clone_dir = self._scratch.path().join(name);
        git(
            self._scratch.path(),
            &[
                "clone",
                "--quiet",
                "--branch",
                "main",
                &self.repo.to_string_lossy(),
                &clone_dir.to_string_lossy(),
            ],
        );
        git(&clone_dir, &["config", "user.email", "test@example.com"]);
        git(&clone_dir, &["config", "user.name", "Test"]);
        git(&clone_dir, &["config", "commit.gpgsign", "false"]);
        git(&clone_dir, &["checkout", "--quiet", "-b", branch]);
        git(
            &clone_dir,
            &["remote", "set-url", "origin", FAKE_ORIGIN_URL],
        );

        std::fs::write(
            clone_dir.join(".git").join("dot-agent-deck-owner"),
            "deck\n",
        )
        .expect("write owner marker");

        let canonical_repo = self.repo.canonicalize().expect("canonicalize repo");
        let canonical_clone = clone_dir.canonicalize().expect("canonicalize clone dir");
        let root_hash = fnv1a64(canonical_repo.to_string_lossy().as_bytes());
        let content = format!(
            "schema=2\nroot-hash={root_hash:016x}\nname={name}\ncreator={creator}\npath={}\n",
            canonical_clone.display()
        );
        let provenance_path = self.isolated_clone_provenance_path(&canonical_clone);
        std::fs::create_dir_all(
            provenance_path
                .parent()
                .expect("provenance path always has a parent"),
        )
        .expect("create provenance dir");
        std::fs::write(&provenance_path, content).expect("write provenance artifact");

        clone_dir
    }

    /// Same resolution as `tests/worktree_reclaim.rs`'s own
    /// `isolated_clone_provenance_path` helper — see that function's doc
    /// comment for why this is reimplemented here rather than called
    /// (`issue_dispatch_run::isolated_clone_provenance_path` is
    /// `pub(crate)`, unreachable from this crate).
    fn isolated_clone_provenance_path(&self, canonical_clone_dir: &Path) -> PathBuf {
        let hash = fnv1a64(canonical_clone_dir.to_string_lossy().as_bytes());
        let basename = canonical_clone_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("clone");
        self.state_dir
            .join("dot-agent-deck-isolated-clone-provenance")
            .join(format!("{basename}-{hash:016x}"))
    }

    /// Canned `gh pr list --head <branch>` reply — `headRepositoryOwner` is
    /// `"test-org"`, matching [`FAKE_ORIGIN_URL`]'s own owner.
    fn set_pr_state(&self, branch: &str, state: &str) {
        let key = branch.replace('/', "_");
        let body = format!(
            r#"[{{"state":"{state}","headRefName":"{branch}","headRepositoryOwner":{{"login":"test-org"}}}}]"#
        );
        std::fs::write(self.ghstub.join(format!("pr-{key}.json")), body).expect("write pr fixture");
    }

    /// Make the stub `gh repo view` fail from here on, like a real `gh`
    /// unable to resolve the repository would.
    fn fail_repo_view(&self) {
        std::fs::write(self.ghstub.join("fail-repo-view"), b"").expect("write fail-repo-view");
    }

    /// Simulate a merge landing on the real repository's own `main`: fetch
    /// `branch`'s commit from `clone_dir` directly into `self.repo` and
    /// fast-forward `self.repo`'s own checked-out `main` onto it — the same
    /// real-fast-forward-merge technique
    /// `orchestration/workspace/020`/`032` already use, with `self.repo`
    /// itself playing the "advanced origin" role (see this file's own
    /// header for why one repository can serve both purposes here).
    fn simulate_merge(&self, clone_dir: &Path, branch: &str) -> String {
        git(
            &self.repo,
            &[
                "fetch",
                "--quiet",
                &clone_dir.to_string_lossy(),
                &format!("{branch}:refs/heads/{branch}"),
            ],
        );
        git(&self.repo, &["merge", "--quiet", "--ff-only", branch]);
        git(&self.repo, &["branch", "-D", branch]);
        head_sha(&self.repo)
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
            .env("DOT_AGENT_DECK_STATE_DIR", &self.state_dir)
            .env(
                "GIT_SSH_COMMAND",
                self.ssh_script.to_string_lossy().into_owned(),
            )
            .env(
                "FAKE_SSH_REAL_REPO",
                self.repo.to_string_lossy().into_owned(),
            )
            .output()
            .expect("run dot-agent-deck")
    }
}

/// Scenario: fork issue #744 — `dot-agent-deck worktree sync` wires PRD
/// fork#544 M7's `sync_merged_workspace_to_main`/`fetch_other_live_workspace`
/// to a real CLI call site for the first time. Two sibling isolated clones:
/// one whose own branch is confirmed MERGED (a real fast-forward merge onto
/// the root repository's own `main`, via [`Fixture::simulate_merge`]) and
/// clean, and one whose own branch is NOT known to be merged (no PR fixture
/// at all). A single `worktree sync` run must auto-switch the first onto
/// `main`, fast-forwarding it to match the real advance exactly, while only
/// fetching the second — its checked-out branch and working tree must stay
/// completely untouched.
#[spec("orchestration/workspace/037")]
#[test]
#[cfg(unix)]
fn workspace_037_sync_switches_the_merged_clone_and_only_fetches_the_other() {
    let fx = Fixture::new();

    let merged_branch = "sync-037-merged";
    let merged_clone = fx.provision_isolated_clone("repo-sync-037-merged", merged_branch, "t#744");
    std::fs::write(merged_clone.join("feature.txt"), "the PR's own content\n")
        .expect("write feature file");
    git(&merged_clone, &["add", "feature.txt"]);
    git(&merged_clone, &["commit", "--quiet", "-m", "merged work"]);
    let feature_head = head_sha(&merged_clone);
    fx.set_pr_state(merged_branch, "MERGED");
    let advanced_main_head = fx.simulate_merge(&merged_clone, merged_branch);
    assert_eq!(
        advanced_main_head, feature_head,
        "setup: the root repository's main must now BE the feature commit (a real \
         fast-forward), not merely contain it"
    );

    let unmerged_branch = "sync-037-unmerged";
    let unmerged_clone =
        fx.provision_isolated_clone("repo-sync-037-unmerged", unmerged_branch, "t#744");
    let unmerged_head_before = head_sha(&unmerged_clone);
    // No `set_pr_state` call for this branch at all -- the stub answers `[]`
    // (no PR found), exactly like a real branch nobody has opened a PR for
    // yet.

    let out = fx.run(&["worktree", "sync"]);
    assert!(
        out.status.success(),
        "`worktree sync` must succeed; got {:?} stdout={} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert_eq!(
        git_output(&merged_clone, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "main",
        "the merged clone must be switched onto main, got:\n{stdout}"
    );
    assert_eq!(
        head_sha(&merged_clone),
        advanced_main_head,
        "the merged clone's local main must be fast-forwarded to match the real advance \
         exactly, got:\n{stdout}"
    );
    assert!(
        merged_clone.join("feature.txt").exists(),
        "the merged content must be present in the working tree after switching"
    );

    assert_eq!(
        git_output(&unmerged_clone, &["rev-parse", "--abbrev-ref", "HEAD"]),
        unmerged_branch,
        "the not-yet-merged clone must stay on its own branch -- only a read-only fetch may \
         run against it, got:\n{stdout}"
    );
    assert_eq!(
        head_sha(&unmerged_clone),
        unmerged_head_before,
        "the not-yet-merged clone's HEAD must be completely untouched"
    );

    assert!(
        stdout.contains("switched to main"),
        "the report must say the merged clone switched to main, got:\n{stdout}"
    );
    assert!(
        stdout.contains("fetched") && !stdout.contains(&format!("{unmerged_branch}: switched")),
        "the report must say the not-yet-merged clone was only fetched, never switched, got:\n{stdout}"
    );
}

/// Scenario: fork issue #744. An isolated clone whose own branch IS
/// confirmed MERGED, but the workspace carries local-only work the merge
/// never captured -- a genuine uncommitted change, exactly the case
/// `orchestration/workspace/021` already pins for `sync_merged_workspace_to_main`
/// directly. `worktree sync` must never discard it: the clone stays on its
/// own branch, the edit stays present, and the report says the row was left
/// untouched rather than silently switched.
#[spec("orchestration/workspace/038")]
#[test]
#[cfg(unix)]
fn workspace_038_merged_clone_with_uncommitted_change_is_left_untouched() {
    let fx = Fixture::new();

    let branch = "sync-038-merged-dirty";
    let clone_dir = fx.provision_isolated_clone("repo-sync-038", branch, "t#744");
    std::fs::write(clone_dir.join("feature.txt"), "the PR's own content\n")
        .expect("write feature file");
    git(&clone_dir, &["add", "feature.txt"]);
    git(&clone_dir, &["commit", "--quiet", "-m", "merged work"]);
    fx.set_pr_state(branch, "MERGED");
    fx.simulate_merge(&clone_dir, branch);

    // Local-only work the merge never captured -- a genuine uncommitted
    // edit, written AFTER the "merge" landed.
    std::fs::write(clone_dir.join("uncommitted.txt"), "local-only work\n")
        .expect("write uncommitted file");
    let head_before = head_sha(&clone_dir);

    let out = fx.run(&["worktree", "sync"]);
    assert!(
        out.status.success(),
        "`worktree sync` must succeed even when a row is left untouched; got {:?} stdout={} \
         stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert_eq!(
        git_output(&clone_dir, &["rev-parse", "--abbrev-ref", "HEAD"]),
        branch,
        "a clone with an uncommitted change must never be switched off its own branch, got:\n{stdout}"
    );
    assert_eq!(
        head_sha(&clone_dir),
        head_before,
        "a clone with an uncommitted change must have its HEAD completely untouched"
    );
    assert_eq!(
        std::fs::read_to_string(clone_dir.join("uncommitted.txt")).expect("read uncommitted file"),
        "local-only work\n",
        "the uncommitted edit itself must never be discarded"
    );
    assert!(
        stdout.contains("left untouched"),
        "the report must say this row was left untouched, got:\n{stdout}"
    );
}

/// Scenario: fork issue #744. `gh repo view` (the new default-branch
/// resolution this command adds) fails -- simulating `gh` being absent,
/// unauthenticated, or otherwise unable to resolve the repository. The
/// whole `worktree sync` run must fail cleanly, before touching any clone,
/// rather than guessing a branch name to fast-forward a merged clone onto.
#[spec("orchestration/workspace/039")]
#[test]
#[cfg(unix)]
fn workspace_039_gh_repo_view_failure_fails_the_whole_run_before_touching_any_clone() {
    let fx = Fixture::new();

    let branch = "sync-039-merged";
    let clone_dir = fx.provision_isolated_clone("repo-sync-039", branch, "t#744");
    std::fs::write(clone_dir.join("feature.txt"), "the PR's own content\n")
        .expect("write feature file");
    git(&clone_dir, &["add", "feature.txt"]);
    git(&clone_dir, &["commit", "--quiet", "-m", "merged work"]);
    fx.set_pr_state(branch, "MERGED");
    fx.simulate_merge(&clone_dir, branch);
    let head_before = head_sha(&clone_dir);

    fx.fail_repo_view();

    let out = fx.run(&["worktree", "sync"]);
    assert!(
        !out.status.success(),
        "`worktree sync` must fail when the default branch cannot be resolved; got {:?} \
         stdout={} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with("worktree sync:"),
        "the error must be reported under the `worktree sync:` prefix, got:\n{stderr}"
    );

    assert_eq!(
        git_output(&clone_dir, &["rev-parse", "--abbrev-ref", "HEAD"]),
        branch,
        "a failed default-branch resolution must never switch any clone -- got:\n{stderr}"
    );
    assert_eq!(
        head_sha(&clone_dir),
        head_before,
        "a failed default-branch resolution must leave every clone's HEAD untouched"
    );
}
